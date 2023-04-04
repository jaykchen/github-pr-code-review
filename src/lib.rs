use dotenv::dotenv;
use flowsnet_platform_sdk::write_error_log;
use github_flows::{
    get_octo, listen_to_event, octocrab::models::events::payload::PullRequestEventAction,
    EventPayload,
};
use http_req::{
    request::{Method, Request},
    uri::Uri,
};
use openai_flows::{chat_completion, ChatModel, ChatOptions};
use slack_flows::send_message_to_channel;
use std::env;
use tiktoken_rs::cl100k_base;

#[no_mangle]
#[tokio::main(flavor = "current_thread")]
pub async fn run() -> anyhow::Result<()> {
    // dotenv().ok();

    let login: String = match env::var("login") {
        Err(_) => "jaykchen".to_string(),
        Ok(name) => name,
    };

    let owner: String = match env::var("owner") {
        Err(_) => "jaykchen".to_string(),
        Ok(name) => name,
    };

    let repo: String = match env::var("repo") {
        Err(_) => "a-test".to_string(),
        Ok(name) => name,
    };

    let openai_key_name: String = match env::var("openai_key_name") {
        Err(_) => "jaykchen".to_string(),
        Ok(name) => name,
    };

    listen_to_event(&login, &owner, &repo, vec!["pull_request"], |payload| {
        handler(&login, &owner, &repo, &openai_key_name, payload)
    })
    .await;

    Ok(())
}

async fn handler(
    login: &str,
    owner: &str,
    repo: &str,
    openai_key_name: &str,
    payload: EventPayload,
) {
    let octo = get_octo(Some(String::from(login)));
    let issues = octo.issues(owner, repo);

    let bpe = cl100k_base().unwrap();
    let mut pull = None;

    match payload {
        EventPayload::PullRequestEvent(e) => {
            if e.action == PullRequestEventAction::Closed {
                write_error_log!("Closed event");
                return;
            }
            pull = Some(e.pull_request);
        }

        _ => (),
    };

    let (_title, pull_number, _contributor) = match pull {
        Some(p) => (
            p.title.unwrap_or("".to_string()),
            p.number,
            p.user.unwrap().login,
        ),
        None => return,
    };
    let chat_id = &format!("PR#{}", pull_number);

    // let patch_url =
    //     "https://patch-diff.githubusercontent.com/raw/WasmEdge/WasmEdge/pull/2368.patch".to_string();
    let patch_url = format!(
        "https://patch-diff.githubusercontent.com/raw/{owner}/{repo}/pull/{pull_number}.patch"
    );
    send_message_to_channel("ik8", "ch_in", patch_url.to_string());
    let patch_uri = Uri::try_from(patch_url.as_str()).unwrap();
    let mut writer = Vec::new();
    let _ = Request::new(&patch_uri)
        .method(Method::GET)
        .header("Accept", "application/vnd.github+json")
        .header("User-Agent", "Flows Network Connector")
        .send(&mut writer)
        .map_err(|_e| {})
        .unwrap();
    let patch_as_text = String::from_utf8_lossy(&writer);

    let mut current_commit = String::new();
    let mut commits: Vec<String> = Vec::new();
    for line in patch_as_text.lines() {
        if line.starts_with("From ") {
            // Detected a new commit
            if !current_commit.is_empty() {
                // Store the previous commit
                commits.push(current_commit.clone());
            }
            // Start a new commit
            current_commit.clear();
        }
        // Append the line to the current commit if the current commit is less than 3800 tokens (the
        // max token size is 4096)
        let tokens = bpe.encode_ordinary(&current_commit);

        if tokens.len() < 3800 {
            current_commit.push_str(&line);
            current_commit.push('\n');
        }
    }
    if !current_commit.is_empty() {
        // Store the last commit
        let head = current_commit.lines().next().unwrap();
        send_message_to_channel("ik8", "ch_mid", head.to_string());

        commits.push(current_commit.clone());
    }
    // write_error_log!(&format!("Num of commits = {}", commits.len()));

    if commits.len() < 1 {
        write_error_log!("Cannot parse any commit from the patch file");
        return;
    }

    let mut reviews: Vec<String> = Vec::new();
    let mut reviews_text = String::new();
    for (_i, commit) in commits.iter().enumerate() {
        let prompt = r#"You will act as a reviewer for GitHub Pull Requests. The next message is a GitHub patch for a single commit. Please review and provide feedback about the patch in the following format, filling the information and keeping the format intact:
        # Code Review Request for Pull Request

        **PR Number:** [insert PR number]
        **PR Title:** [insert PR title]
        **Code Version:** [insert code version]
        **File Name(s):** [insert file name(s) of files changed in the PR]
        **Code Overview:** [insert brief description of what the code changes do]
        **Review Type:** [general review]
        **Review Goals:** [identify bugs, improve readability, optimize performance]
        **Additional Comments:** [alignment with the roadmap and objectives of the whole program]"#;

        let co = ChatOptions {
            model: ChatModel::GPT35Turbo,
            restart: true,
            system_prompt: Some(prompt),
        };
        if let Some(r) = chat_completion(openai_key_name, chat_id, &commit, &co) {
            write_error_log!("Got a patch review");
            reviews_text.push_str("------\n");
            reviews_text.push_str(&r.choice);
            reviews_text.push('\n');
            reviews.push(r.choice);
        }
    }

    let mut resp = String::new();
    resp.push_str("Hello, I am a [serverless review bot](https://github.com/flows-network/github-pr-summary/) on [flows.network](https://flows.network/). Here are my reviews of code commits in this PR.\n\n------\n\n");
    if reviews.len() > 1 {
        let prompt = "In the next messge, I will provide a set of reviews for code patches. Each review starts with a ------ line. Please write a summary of all the reviews";
        let co = ChatOptions {
            model: ChatModel::GPT35Turbo,
            restart: true,
            system_prompt: Some(prompt),
        };
        if let Some(r) = chat_completion(openai_key_name, chat_id, &reviews_text, &co) {
            write_error_log!("Got the overall summary");
            resp.push_str(&r.choice);
            resp.push_str("\n\n## Details\n\n");
        }
    }
    for (i, review) in reviews.iter().enumerate() {
        resp.push_str(&format!("### Commit {}\n", i + 1));
        resp.push_str(&review);
        resp.push_str("\n\n");
    }
    // Send the entire response to GitHub PR
    issues.create_comment(pull_number, resp).await.unwrap();
}
