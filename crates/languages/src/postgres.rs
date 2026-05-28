use anyhow::{Context as _, Result, bail};
use async_trait::async_trait;
use collections::HashMap;
use futures::StreamExt;
use gpui::AsyncApp;
use http_client::github::{GitHubLspBinaryVersion, latest_github_release};
use http_client::github_download::GithubBinaryMetadata;
use language::{LanguageName, LspAdapter, LspAdapterDelegate, LspInstaller, Toolchain};
use lsp::{LanguageServerBinary, LanguageServerName};
use smol::fs;
use std::{
    env::consts,
    ffi::{OsStr, OsString},
    future::Future,
    path::PathBuf,
    sync::Arc,
};
use util::{
    ResultExt,
    fs::{make_file_executable, remove_matching},
    maybe,
};

pub struct PostgresLspAdapter;

impl PostgresLspAdapter {
    const SERVER_NAME: LanguageServerName =
        LanguageServerName::new_static("postgres-language-server");
    const REPOSITORY: &str = "supabase-community/postgres-language-server";

    fn command_name() -> OsString {
        format!("postgres-language-server{}", consts::EXE_SUFFIX).into()
    }

    fn server_binary(path: PathBuf, env: Option<HashMap<String, String>>) -> LanguageServerBinary {
        LanguageServerBinary {
            path,
            env,
            arguments: vec!["lsp-proxy".into()],
        }
    }

    fn asset_name() -> Result<String> {
        let architecture = match consts::ARCH {
            "aarch64" => "aarch64",
            "x86_64" => "x86_64",
            other => bail!(
                "Postgres Language Server does not provide prebuilt binaries for {other}. Consider installing the binary manually."
            ),
        };
        let os = match consts::OS {
            "macos" => "apple-darwin",
            "linux" => "unknown-linux-gnu",
            "windows" => "pc-windows-msvc",
            other => bail!(
                "Postgres Language Server does not provide prebuilt binaries for {other}. Consider installing the binary manually."
            ),
        };
        let extension = if consts::OS == "windows" { ".exe" } else { "" };
        Ok(format!(
            "postgres-language-server_{architecture}-{os}{extension}"
        ))
    }
}

impl LspInstaller for PostgresLspAdapter {
    type BinaryVersion = GitHubLspBinaryVersion;

    async fn fetch_latest_server_version(
        &self,
        delegate: &Arc<dyn LspAdapterDelegate>,
        pre_release: bool,
        _: &mut AsyncApp,
    ) -> Result<Self::BinaryVersion> {
        let release =
            latest_github_release(Self::REPOSITORY, true, pre_release, delegate.http_client())
                .await?;
        let asset_name = Self::asset_name()?;
        let asset = release
            .assets
            .into_iter()
            .find(|asset| asset.name == asset_name)
            .with_context(|| format!("no asset found matching `{asset_name}`"))?;
        Ok(GitHubLspBinaryVersion {
            name: release.tag_name,
            url: asset.browser_download_url,
            digest: asset.digest,
        })
    }

    async fn check_if_user_installed(
        &self,
        delegate: &Arc<dyn LspAdapterDelegate>,
        _: Option<Toolchain>,
        _: &AsyncApp,
    ) -> Option<LanguageServerBinary> {
        let path = delegate
            .which(OsStr::new("postgres-language-server"))
            .await?;
        let env = delegate.shell_env().await;
        Some(Self::server_binary(path, Some(env)))
    }

    fn check_if_version_installed(
        &self,
        version: &Self::BinaryVersion,
        container_dir: &PathBuf,
        delegate: &Arc<dyn LspAdapterDelegate>,
    ) -> impl Send + Future<Output = Option<LanguageServerBinary>> + use<> {
        let name = version.name.clone();
        let container_dir = container_dir.clone();
        let delegate = delegate.clone();

        async move {
            let binary_path = container_dir
                .join(format!("postgres-language-server-{name}"))
                .join(Self::command_name());
            let env = delegate.shell_env().await;
            let binary = Self::server_binary(binary_path.clone(), Some(env));

            let metadata_path = binary_path.with_extension("metadata");
            let metadata = GithubBinaryMetadata::read_from_file(&metadata_path)
                .await
                .ok()?;
            if metadata.metadata_version != 1 {
                return None;
            }
            delegate
                .try_exec(LanguageServerBinary {
                    path: binary_path,
                    env: None,
                    arguments: vec!["--version".into()],
                })
                .await
                .ok()?;
            Some(binary)
        }
    }

    fn fetch_server_binary(
        &self,
        version: Self::BinaryVersion,
        container_dir: PathBuf,
        delegate: &Arc<dyn LspAdapterDelegate>,
    ) -> impl Send + Future<Output = Result<LanguageServerBinary>> + use<> {
        let delegate = delegate.clone();

        async move {
            let GitHubLspBinaryVersion { name, url, digest } = version;
            let destination_dir = container_dir.join(format!("postgres-language-server-{name}"));
            fs::create_dir_all(&destination_dir)
                .await
                .with_context(|| format!("creating {destination_dir:?}"))?;
            let binary_path = destination_dir.join(Self::command_name());

            let mut response = delegate
                .http_client()
                .get(&url, Default::default(), true)
                .await
                .with_context(|| format!("downloading release from {url}"))?;
            let mut file = fs::File::create(&binary_path)
                .await
                .with_context(|| format!("creating {binary_path:?}"))?;
            futures::io::copy(response.body_mut(), &mut file)
                .await
                .with_context(|| format!("writing {binary_path:?}"))?;
            make_file_executable(&binary_path).await?;

            delegate
                .try_exec(LanguageServerBinary {
                    path: binary_path.clone(),
                    env: None,
                    arguments: vec!["--version".into()],
                })
                .await
                .with_context(|| format!("running {binary_path:?}"))?;

            remove_matching(&container_dir, |path| path != destination_dir).await;
            GithubBinaryMetadata::write_to_file(
                &GithubBinaryMetadata {
                    metadata_version: 1,
                    digest,
                },
                &binary_path.with_extension("metadata"),
            )
            .await?;

            let env = delegate.shell_env().await;
            Ok(Self::server_binary(binary_path, Some(env)))
        }
    }

    async fn cached_server_binary(
        &self,
        container_dir: PathBuf,
        delegate: &dyn LspAdapterDelegate,
    ) -> Option<LanguageServerBinary> {
        get_cached_server_binary(container_dir, delegate).await
    }
}

#[async_trait(?Send)]
impl LspAdapter for PostgresLspAdapter {
    fn name(&self) -> LanguageServerName {
        Self::SERVER_NAME
    }

    fn language_ids(&self) -> HashMap<LanguageName, String> {
        let mut language_ids = HashMap::default();
        language_ids.insert(LanguageName::new("SQL"), "sql".to_string());
        language_ids
    }
}

async fn get_cached_server_binary(
    container_dir: PathBuf,
    delegate: &dyn LspAdapterDelegate,
) -> Option<LanguageServerBinary> {
    maybe!(async {
        let mut entries = fs::read_dir(&container_dir)
            .await
            .with_context(|| format!("listing {container_dir:?}"))?;
        let mut cached_path = None;
        while let Some(entry) = entries.next().await {
            let path = entry?.path();
            if path.is_dir()
                && path.file_name().is_some_and(|name| {
                    name.to_string_lossy()
                        .starts_with("postgres-language-server-")
                })
            {
                cached_path = Some(path.join(PostgresLspAdapter::command_name()));
            }
        }
        let binary_path = cached_path.context("no cached postgres-language-server binary found")?;
        let env = delegate.shell_env().await;
        anyhow::Ok(PostgresLspAdapter::server_binary(binary_path, Some(env)))
    })
    .await
    .log_err()
}
