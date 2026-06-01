use anyhow::Context as _;
use futures_lite::io::AsyncWriteExt as _;
use std::{path::Path, process::Stdio, sync::Arc};

pub struct PullRequestFiles {
    pub id: String,
    pub number: u32,
    pub title: String,
    pub url: String,
    pub body: String,
    pub review_decision: Option<String>,
    pub review_requests: Vec<PullRequestReviewer>,
    pub status_checks: Vec<PullRequestStatusCheck>,
    pub files: Vec<PullRequestFile>,
    pub comments: Vec<PullRequestComment>,
}

#[derive(Clone, Debug)]
pub struct PullRequestDetails {
    pub number: u32,
    pub title: String,
    pub url: String,
    pub body: String,
    pub review_decision: Option<String>,
    pub review_requests: Vec<PullRequestReviewer>,
    pub status_checks: Vec<PullRequestStatusCheck>,
}

#[derive(Clone, Debug)]
pub struct PullRequestReviewer {
    pub login: String,
}

#[derive(Clone, Debug)]
pub struct PullRequestStatusCheck {
    pub name: String,
    pub status: Option<String>,
    pub conclusion: Option<String>,
}

pub struct PullRequestFile {
    pub path: String,
    pub viewed: bool,
}

#[derive(Clone, Debug)]
pub struct OpenPullRequest {
    pub number: u32,
    pub title: String,
    pub branch: String,
    pub author: Option<String>,
    pub is_draft: bool,
}

#[derive(Clone, Debug)]
pub struct PullRequestComment {
    pub path: String,
    pub start_line: Option<u32>,
    pub line: Option<u32>,
    pub original_start_line: Option<u32>,
    pub original_line: Option<u32>,
    pub url: String,
    pub body: String,
    pub author: Option<String>,
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

async fn gh_output_with_input(
    work_directory: Arc<Path>,
    args: Vec<String>,
    input: String,
) -> anyhow::Result<String> {
    let mut child = smol::process::Command::new("gh")
        .args(args)
        .current_dir(work_directory.as_ref())
        .stdin(Stdio::piped())
        .stderr(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .context("running gh")?;

    let mut stdin = child.stdin.take().context("opening gh stdin")?;
    stdin
        .write_all(input.as_bytes())
        .await
        .context("writing gh stdin")?;
    drop(stdin);

    let output = child.output().await.context("waiting for gh")?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("gh exited with status {}: {}", output.status, stderr.trim());
    }

    String::from_utf8(output.stdout).context("decoding gh output")
}

pub async fn fetch_open_pull_requests(
    work_directory: Arc<Path>,
    search_query: Option<String>,
) -> anyhow::Result<Vec<OpenPullRequest>> {
    let mut args = vec![
        "pr".into(),
        "list".into(),
        "--state".into(),
        "open".into(),
        "--limit".into(),
        "100".into(),
        "--json".into(),
        "number,title,headRefName,author,isDraft".into(),
    ];
    if let Some(search_query) = search_query {
        args.extend(["--search".into(), search_query]);
    }

    let pull_requests_json = gh_output(work_directory, args)
        .await
        .context("loading open pull requests")?;
    let pull_requests: serde_json::Value =
        serde_json::from_str(&pull_requests_json).context("parsing open pull requests JSON")?;
    let pull_requests = pull_requests
        .as_array()
        .context("open pull requests response was not an array")?;

    Ok(pull_requests
        .iter()
        .filter_map(|node| {
            let number = node
                .get("number")?
                .as_u64()
                .and_then(|number| u32::try_from(number).ok())?;
            let title = node.get("title")?.as_str()?.to_string();
            let branch = node.get("headRefName")?.as_str()?.to_string();
            let author = node
                .get("author")
                .and_then(|author| author.get("login"))
                .and_then(|login| login.as_str())
                .map(ToOwned::to_owned);
            let is_draft = node
                .get("isDraft")
                .and_then(|is_draft| is_draft.as_bool())
                .unwrap_or(false);
            Some(OpenPullRequest {
                number,
                title,
                branch,
                author,
                is_draft,
            })
        })
        .collect())
}

pub async fn fetch_current_pull_request_number(
    work_directory: Arc<Path>,
) -> anyhow::Result<Option<u32>> {
    let pull_request_json = match gh_output(
        work_directory,
        vec!["pr".into(), "view".into(), "--json".into(), "number".into()],
    )
    .await
    {
        Ok(output) => output,
        Err(error) if is_pull_request_not_found_error(&error) => return Ok(None),
        Err(error) => return Err(error).context("loading current pull request"),
    };
    let pull_request: serde_json::Value =
        serde_json::from_str(&pull_request_json).context("parsing current pull request JSON")?;
    let number = pull_request
        .get("number")
        .and_then(|number| number.as_u64())
        .and_then(|number| u32::try_from(number).ok());
    Ok(number)
}

pub async fn open_current_pull_request_in_browser(work_directory: Arc<Path>) -> anyhow::Result<()> {
    gh_output(
        work_directory,
        vec!["pr".into(), "view".into(), "--web".into()],
    )
    .await
    .context("opening current pull request in browser")?;
    Ok(())
}

pub async fn fetch_pull_request_template(work_directory: Arc<Path>) -> anyhow::Result<String> {
    let template_paths = [
        ".github/pull_request_template.md",
        ".github/PULL_REQUEST_TEMPLATE.md",
        "pull_request_template.md",
        "PULL_REQUEST_TEMPLATE.md",
    ];

    for template_path in template_paths {
        let path = work_directory.join(template_path);
        match smol::fs::read_to_string(&path).await {
            Ok(template) => return Ok(template),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("reading pull request template {}", path.display()));
            }
        }
    }

    Ok(String::new())
}

pub async fn create_pull_request(
    work_directory: Arc<Path>,
    body: String,
) -> anyhow::Result<String> {
    gh_output_with_input(
        work_directory,
        vec![
            "pr".into(),
            "create".into(),
            "--fill".into(),
            "--body-file".into(),
            "-".into(),
        ],
        body,
    )
    .await
    .context("creating pull request")
}

pub async fn update_current_pull_request_body(
    work_directory: Arc<Path>,
    body: String,
) -> anyhow::Result<String> {
    gh_output_with_input(
        work_directory,
        vec!["pr".into(), "edit".into(), "--body-file".into(), "-".into()],
        body,
    )
    .await
    .context("updating pull request description")
}

pub async fn fetch_pull_request_details(
    work_directory: Arc<Path>,
) -> anyhow::Result<PullRequestDetails> {
    let pull_request_json = gh_output(
        work_directory,
        vec![
            "pr".into(),
            "view".into(),
            "--json".into(),
            "number,title,url,body,reviewDecision,reviewRequests,statusCheckRollup".into(),
        ],
    )
    .await
    .context("loading pull request details")?;
    let pull_request: serde_json::Value =
        serde_json::from_str(&pull_request_json).context("parsing pull request details JSON")?;
    parse_pull_request_details(&pull_request)
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
            "id,number,title,url,body,reviewDecision,reviewRequests,statusCheckRollup".into(),
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
        .and_then(|number| number.as_u64())
        .and_then(|number| u32::try_from(number).ok())
        .context("pull request number missing")?;
    let pull_request_title = pull_request
        .get("title")
        .and_then(|title| title.as_str())
        .context("pull request title missing")?
        .to_string();
    let pull_request_url = pull_request
        .get("url")
        .and_then(|url| url.as_str())
        .context("pull request url missing")?
        .to_string();
    let body = pull_request
        .get("body")
        .and_then(|body| body.as_str())
        .unwrap_or_default()
        .to_string();
    let review_decision = pull_request
        .get("reviewDecision")
        .and_then(|review_decision| review_decision.as_str())
        .filter(|review_decision| !review_decision.is_empty())
        .map(ToOwned::to_owned);
    let review_requests = parse_review_requests(pull_request.get("reviewRequests"));
    let status_checks = parse_status_checks(pull_request.get("statusCheckRollup"));

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

    let mut comments = Vec::new();
    let mut page = 1;
    loop {
        let response_json = gh_output(
            work_directory.clone(),
            vec![
                "api".into(),
                "-X".into(),
                "GET".into(),
                format!("repos/{owner}/{name}/pulls/{pull_request_number}/comments"),
                "-f".into(),
                "per_page=100".into(),
                "-f".into(),
                format!("page={page}"),
            ],
        )
        .await
        .context("loading pull request comments")?;
        let response: serde_json::Value =
            serde_json::from_str(&response_json).context("parsing pull request comments JSON")?;
        let Some(nodes) = response.as_array() else {
            break;
        };
        if nodes.is_empty() {
            break;
        }

        comments.extend(nodes.iter().filter_map(|node| {
            let path = node.get("path")?.as_str()?.to_string();
            let url = node.get("html_url")?.as_str()?.to_string();
            let body = node
                .get("body")
                .and_then(|body| body.as_str())
                .unwrap_or_default()
                .to_string();
            let author = node
                .get("user")
                .and_then(|user| user.get("login"))
                .and_then(|login| login.as_str())
                .map(ToOwned::to_owned);
            let line = node
                .get("line")
                .and_then(|line| line.as_u64())
                .and_then(|line| u32::try_from(line).ok());
            let start_line = node
                .get("start_line")
                .and_then(|line| line.as_u64())
                .and_then(|line| u32::try_from(line).ok());
            let original_line = node
                .get("original_line")
                .and_then(|line| line.as_u64())
                .and_then(|line| u32::try_from(line).ok());
            let original_start_line = node
                .get("original_start_line")
                .and_then(|line| line.as_u64())
                .and_then(|line| u32::try_from(line).ok());
            Some(PullRequestComment {
                path,
                start_line,
                line,
                original_start_line,
                original_line,
                url,
                body,
                author,
            })
        }));
        page += 1;
    }

    Ok(PullRequestFiles {
        id: pull_request_id,
        number: pull_request_number,
        title: pull_request_title,
        url: pull_request_url,
        body,
        review_decision,
        review_requests,
        status_checks,
        files,
        comments,
    })
}

fn parse_pull_request_details(
    pull_request: &serde_json::Value,
) -> anyhow::Result<PullRequestDetails> {
    let number = pull_request
        .get("number")
        .and_then(|number| number.as_u64())
        .and_then(|number| u32::try_from(number).ok())
        .context("pull request number missing")?;
    let title = pull_request
        .get("title")
        .and_then(|title| title.as_str())
        .context("pull request title missing")?
        .to_string();
    let url = pull_request
        .get("url")
        .and_then(|url| url.as_str())
        .context("pull request url missing")?
        .to_string();
    let body = pull_request
        .get("body")
        .and_then(|body| body.as_str())
        .unwrap_or_default()
        .to_string();
    let review_decision = pull_request
        .get("reviewDecision")
        .and_then(|review_decision| review_decision.as_str())
        .filter(|review_decision| !review_decision.is_empty())
        .map(ToOwned::to_owned);
    let review_requests = parse_review_requests(pull_request.get("reviewRequests"));
    let status_checks = parse_status_checks(pull_request.get("statusCheckRollup"));

    Ok(PullRequestDetails {
        number,
        title,
        url,
        body,
        review_decision,
        review_requests,
        status_checks,
    })
}

fn parse_review_requests(review_requests: Option<&serde_json::Value>) -> Vec<PullRequestReviewer> {
    review_requests
        .and_then(|review_requests| review_requests.as_array())
        .into_iter()
        .flatten()
        .filter_map(|node| {
            let login = node
                .get("login")
                .or_else(|| node.get("slug"))
                .or_else(|| node.get("name"))
                .and_then(|login| login.as_str())?
                .to_string();
            Some(PullRequestReviewer { login })
        })
        .collect()
}

fn parse_status_checks(
    status_check_rollup: Option<&serde_json::Value>,
) -> Vec<PullRequestStatusCheck> {
    status_check_rollup
        .and_then(|rollup| rollup.as_array())
        .into_iter()
        .flatten()
        .filter_map(|node| {
            let name = node
                .get("name")
                .or_else(|| node.get("context"))
                .and_then(|name| name.as_str())?
                .to_string();
            let status = node
                .get("status")
                .and_then(|status| status.as_str())
                .map(ToOwned::to_owned);
            let conclusion = node
                .get("conclusion")
                .and_then(|conclusion| conclusion.as_str())
                .map(ToOwned::to_owned);
            Some(PullRequestStatusCheck {
                name,
                status,
                conclusion,
            })
        })
        .collect()
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
