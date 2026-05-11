use anyhow::Context as _;
use std::{path::Path, process::Stdio, sync::Arc};

pub struct PullRequestFiles {
    pub id: String,
    pub title: String,
    pub files: Vec<PullRequestFile>,
}

pub struct PullRequestFile {
    pub path: String,
    pub viewed: bool,
}

async fn gh_output(work_directory: Arc<Path>, args: Vec<String>) -> anyhow::Result<String> {
    let output = smol::process::Command::new("gh")
        .args(args)
        .current_dir(work_directory.as_ref())
        .stdin(Stdio::null())
        .stderr(Stdio::piped())
        .output()
        .await
        .context("running gh")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("gh exited with status {}: {}", output.status, stderr.trim());
    }

    String::from_utf8(output.stdout).context("decoding gh output")
}

pub async fn fetch_pull_request_files(
    work_directory: Arc<Path>,
) -> anyhow::Result<PullRequestFiles> {
    let pull_request_json = gh_output(
        work_directory.clone(),
        vec![
            "pr".into(),
            "view".into(),
            "--json".into(),
            "id,number,title".into(),
        ],
    )
    .await
    .context("loading current pull request")?;
    let pull_request: serde_json::Value =
        serde_json::from_str(&pull_request_json).context("parsing pull request JSON")?;
    let pull_request_id = pull_request
        .get("id")
        .and_then(|id| id.as_str())
        .context("pull request id missing")?
        .to_string();
    let pull_request_number = pull_request
        .get("number")
        .and_then(|number| number.as_i64())
        .context("pull request number missing")?;
    let pull_request_title = pull_request
        .get("title")
        .and_then(|title| title.as_str())
        .context("pull request title missing")?
        .to_string();

    let repository_json = gh_output(
        work_directory.clone(),
        vec![
            "repo".into(),
            "view".into(),
            "--json".into(),
            "owner,name".into(),
        ],
    )
    .await
    .context("loading GitHub repository")?;
    let repository: serde_json::Value =
        serde_json::from_str(&repository_json).context("parsing repository JSON")?;
    let owner = repository
        .get("owner")
        .and_then(|owner| owner.get("login"))
        .and_then(|login| login.as_str())
        .context("repository owner missing")?
        .to_string();
    let name = repository
        .get("name")
        .and_then(|name| name.as_str())
        .context("repository name missing")?
        .to_string();

    let query = r#"
        query($owner: String!, $name: String!, $number: Int!, $after: String) {
          repository(owner: $owner, name: $name) {
            pullRequest(number: $number) {
              files(first: 100, after: $after) {
                nodes {
                  path
                  viewerViewedState
                }
                pageInfo {
                  hasNextPage
                  endCursor
                }
              }
            }
          }
        }
    "#;

    let mut files = Vec::new();
    let mut after = None::<String>;
    loop {
        let mut args = vec![
            "api".into(),
            "graphql".into(),
            "-f".into(),
            format!("query={query}"),
            "-f".into(),
            format!("owner={owner}"),
            "-f".into(),
            format!("name={name}"),
            "-F".into(),
            format!("number={pull_request_number}"),
        ];
        if let Some(after) = after.as_ref() {
            args.extend(["-f".into(), format!("after={after}")]);
        }

        let response_json = gh_output(work_directory.clone(), args)
            .await
            .context("loading pull request files")?;
        let response: serde_json::Value =
            serde_json::from_str(&response_json).context("parsing pull request files JSON")?;
        let files_json = response
            .pointer("/data/repository/pullRequest/files")
            .context("pull request files missing")?;
        if let Some(nodes) = files_json.get("nodes").and_then(|nodes| nodes.as_array()) {
            files.extend(nodes.iter().filter_map(|node| {
                let path = node.get("path")?.as_str()?.to_string();
                let viewed = node
                    .get("viewerViewedState")
                    .and_then(|state| state.as_str())
                    .is_some_and(|state| state == "VIEWED");
                Some(PullRequestFile { path, viewed })
            }));
        }

        let Some(page_info) = files_json.get("pageInfo") else {
            break;
        };
        if !page_info
            .get("hasNextPage")
            .and_then(|has_next_page| has_next_page.as_bool())
            .unwrap_or(false)
        {
            break;
        }
        after = page_info
            .get("endCursor")
            .and_then(|end_cursor| end_cursor.as_str())
            .map(ToOwned::to_owned);
        if after.is_none() {
            break;
        }
    }

    Ok(PullRequestFiles {
        id: pull_request_id,
        title: pull_request_title,
        files,
    })
}

pub fn is_pull_request_not_found_error(error: &anyhow::Error) -> bool {
    error.chain().any(|cause| {
        let message = cause.to_string().to_lowercase();
        message.contains("no pull requests found")
            || message.contains("no pull request found")
            || message.contains("no open pull requests")
            || message.contains("could not find any pull requests")
            || message.contains("no git remotes found")
    })
}

pub async fn set_pull_request_file_viewed(
    work_directory: Arc<Path>,
    pull_request_id: String,
    path: String,
    viewed: bool,
) -> anyhow::Result<()> {
    let mutation = if viewed {
        r#"
            mutation($pullRequestId: ID!, $path: String!) {
              markFileAsViewed(input: { pullRequestId: $pullRequestId, path: $path }) {
                clientMutationId
              }
            }
        "#
    } else {
        r#"
            mutation($pullRequestId: ID!, $path: String!) {
              unmarkFileAsViewed(input: { pullRequestId: $pullRequestId, path: $path }) {
                clientMutationId
              }
            }
        "#
    };

    gh_output(
        work_directory,
        vec![
            "api".into(),
            "graphql".into(),
            "-f".into(),
            format!("query={mutation}"),
            "-f".into(),
            format!("pullRequestId={pull_request_id}"),
            "-f".into(),
            format!("path={path}"),
        ],
    )
    .await?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_pull_request_not_found_error_detection() {
        let error = anyhow::anyhow!(
            "gh exited with status 1: no pull requests found for branch \"feature\""
        )
        .context("loading current pull request");

        assert!(is_pull_request_not_found_error(&error));

        let error = anyhow::anyhow!(
            "gh exited with status 1: no open pull requests found for branch \"feature\""
        )
        .context("loading current pull request");

        assert!(is_pull_request_not_found_error(&error));

        let error = anyhow::anyhow!("gh exited with status 1: no git remotes found")
            .context("loading current pull request");

        assert!(is_pull_request_not_found_error(&error));

        let error = anyhow::anyhow!("gh exited with status 1: authentication required")
            .context("loading current pull request");

        assert!(!is_pull_request_not_found_error(&error));
    }
}
