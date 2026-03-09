/// Parallel scatter/gather for Co-Forth.
///
/// `scatter" <code>"` sends a Forth snippet to all registered peers and collects
/// their output.  Peers are finch daemons exposing `POST /v1/forth/eval`.
///
/// All requests fire concurrently and results are labelled by peer address.

use futures::future::join_all;

pub struct PeerResult {
    pub peer:   String,
    pub output: String,
    /// Data stack from the remote VM after execution (top = last element).
    pub stack:  Vec<i64>,
    pub error:  Option<String>,
}

/// Send a bash command to every peer in `peers` concurrently via `POST /v1/exec`.
/// The command is run as `bash -c <cmd>` on each remote machine.
/// Never panics; errors are captured per-peer in `PeerResult::error`.
pub async fn scatter_exec_bash(peers: &[String], cmd: &str) -> Vec<PeerResult> {
    let tasks: Vec<_> = peers
        .iter()
        .map(|peer| {
            let peer = peer.clone();
            let cmd = cmd.to_string();
            async move {
                match exec_on_peer(&peer, &cmd).await {
                    Ok((output, error)) => PeerResult { peer, output, stack: Vec::new(), error },
                    Err(e) => PeerResult {
                        peer,
                        output: String::new(),
                        stack:  Vec::new(),
                        error:  Some(e.to_string()),
                    },
                }
            }
        })
        .collect();

    join_all(tasks).await
}

/// Send `code` to every peer in `peers` concurrently.  Never panics; errors are
/// captured per-peer in `PeerResult::error`.
pub async fn scatter_exec(peers: &[String], code: &str) -> Vec<PeerResult> {
    let tasks: Vec<_> = peers
        .iter()
        .map(|peer| {
            let peer = peer.clone();
            let code = code.to_string();
            async move {
                match eval_on_peer(&peer, &code).await {
                    Ok((output, stack, error)) => PeerResult { peer, output, stack, error },
                    Err(e) => PeerResult {
                        peer,
                        output: String::new(),
                        stack:  Vec::new(),
                        error:  Some(e.to_string()),
                    },
                }
            }
        })
        .collect();

    join_all(tasks).await
}

async fn exec_on_peer(addr: &str, cmd: &str) -> anyhow::Result<(String, Option<String>)> {
    let url = if addr.starts_with("http://") || addr.starts_with("https://") {
        format!("{addr}/v1/exec")
    } else {
        format!("http://{addr}/v1/exec")
    };

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()?;

    let resp = client
        .post(&url)
        .json(&serde_json::json!({ "cmd": "bash", "args": ["-c", cmd] }))
        .send()
        .await?
        .json::<serde_json::Value>()
        .await?;

    let stdout = resp["stdout"].as_str().unwrap_or("").to_string();
    let stderr = resp["stderr"].as_str().unwrap_or("").to_string();
    let exit_code = resp["exit_code"].as_i64().unwrap_or(0);
    let error_field = resp["error"].as_str().map(|s| s.to_string());

    // Non-zero exit or remote error → surface as error
    let error = if let Some(e) = error_field {
        Some(e)
    } else if exit_code != 0 {
        let msg = if stderr.is_empty() {
            format!("exit {exit_code}")
        } else {
            format!("exit {exit_code}: {}", stderr.trim())
        };
        Some(msg)
    } else {
        None
    };

    let output = if stdout.is_empty() && !stderr.is_empty() && exit_code == 0 {
        stderr
    } else {
        stdout
    };

    Ok((output, error))
}

async fn eval_on_peer(addr: &str, code: &str) -> anyhow::Result<(String, Vec<i64>, Option<String>)> {
    let url = if addr.starts_with("http://") || addr.starts_with("https://") {
        format!("{addr}/v1/forth/eval")
    } else {
        format!("http://{addr}/v1/forth/eval")
    };

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()?;

    let resp = client
        .post(&url)
        .json(&serde_json::json!({ "code": code }))
        .send()
        .await?
        .json::<serde_json::Value>()
        .await?;

    let output = resp["output"].as_str().unwrap_or("").to_string();
    let error  = resp["error"].as_str().map(|s| s.to_string());
    let stack: Vec<i64> = resp["stack"]
        .as_array()
        .map(|arr| arr.iter().filter_map(|v| v.as_i64()).collect())
        .unwrap_or_default();

    Ok((output, stack, error))
}
