/// Parallel scatter/gather for Co-Forth.
///
/// `scatter" <code>"` sends a Forth snippet to all registered peers and collects
/// their output.  Peers are finch daemons exposing `POST /v1/forth/eval`.
///
/// All requests fire concurrently and results are labelled by peer address.

use futures::future::join_all;

pub struct PeerResult {
    pub peer:         String,
    pub output:       String,
    /// Data stack from the remote VM after execution (top = last element).
    pub stack:        Vec<i64>,
    pub error:        Option<String>,
    /// Wall-clock milliseconds the remote peer spent executing.
    pub compute_ms:   u64,
    /// Set when this machine's compute debt crossed the threshold on that peer.
    pub debt_warning: Option<String>,
    /// Forth code the peer wants executed on the caller after this response.
    pub forth_back:   Option<String>,
}

/// Send a bash command to every peer in `peers` concurrently via `POST /v1/exec`.
/// The command is run as `bash -c <cmd>` on each remote machine.
/// `peer_tokens`: optional per-peer auth tokens (addr → token).
/// Never panics; errors are captured per-peer in `PeerResult::error`.
pub async fn scatter_exec_bash(
    peers: &[String],
    cmd: &str,
    peer_tokens: &std::collections::HashMap<String, String>,
) -> Vec<PeerResult> {
    let tasks: Vec<_> = peers
        .iter()
        .map(|peer| {
            let peer  = peer.clone();
            let cmd   = cmd.to_string();
            let token = peer_tokens.get(&peer).cloned();
            async move {
                match exec_on_peer(&peer, &cmd, token.as_deref()).await {
                    Ok((output, error)) => PeerResult { peer, output, stack: Vec::new(), error, compute_ms: 0, debt_warning: None, forth_back: None },
                    Err(e) => PeerResult {
                        peer,
                        output:       String::new(),
                        stack:        Vec::new(),
                        error:        Some(e.to_string()),
                        compute_ms:   0,
                        debt_warning: None,
                        forth_back:   None,
                    },
                }
            }
        })
        .collect();

    join_all(tasks).await
}

/// Send `code` to every peer in `peers` concurrently.  Never panics; errors are
/// captured per-peer in `PeerResult::error`.
/// `caller` is this machine's registry address for debt tracking.
/// `peer_tokens`: optional per-peer auth tokens (addr → token).
pub async fn scatter_exec(
    peers: &[String],
    code: &str,
    caller: Option<&str>,
    peer_tokens: &std::collections::HashMap<String, String>,
) -> Vec<PeerResult> {
    let caller = caller.map(|s| s.to_string());
    let tasks: Vec<_> = peers
        .iter()
        .map(|peer| {
            let peer   = peer.clone();
            let code   = code.to_string();
            let caller = caller.clone();
            let token  = peer_tokens.get(&peer).cloned();
            async move {
                match eval_on_peer(&peer, &code, caller.as_deref(), token.as_deref()).await {
                    Ok((output, stack, error, compute_ms, debt_warning, forth_back)) =>
                        PeerResult { peer, output, stack, error, compute_ms, debt_warning, forth_back },
                    Err(e) => PeerResult {
                        peer,
                        output:       String::new(),
                        stack:        Vec::new(),
                        error:        Some(e.to_string()),
                        compute_ms:   0,
                        debt_warning: None,
                        forth_back:   None,
                    },
                }
            }
        })
        .collect();

    join_all(tasks).await
}

/// Send a plain-text message to every peer via POST /v1/forth/push.
/// The peer displays it in their TUI without running any Forth.
pub async fn scatter_push(peers: &[String], text: &str, from: Option<&str>) {
    let tasks: Vec<_> = peers
        .iter()
        .map(|peer| {
            let peer = peer.clone();
            let text = text.to_string();
            let from = from.map(|s| s.to_string());
            async move {
                let url = if peer.starts_with("http://") || peer.starts_with("https://") {
                    format!("{peer}/v1/forth/push")
                } else {
                    format!("http://{peer}/v1/forth/push")
                };
                let body = match &from {
                    Some(f) => serde_json::json!({ "text": text, "from": f }),
                    None    => serde_json::json!({ "text": text }),
                };
                let client = reqwest::Client::builder()
                    .timeout(std::time::Duration::from_secs(10))
                    .build()
                    .unwrap_or_default();
                let _ = client.post(&url).json(&body).send().await;
            }
        })
        .collect();
    join_all(tasks).await;
}

async fn exec_on_peer(addr: &str, cmd: &str, token: Option<&str>) -> anyhow::Result<(String, Option<String>)> {
    let url = if addr.starts_with("http://") || addr.starts_with("https://") {
        format!("{addr}/v1/exec")
    } else {
        format!("http://{addr}/v1/exec")
    };

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()?;

    let mut req = client.post(&url);
    if let Some(t) = token {
        req = req.header(crate::peer_token::HEADER, t);
    }
    let resp = req
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

/// Send a word definition to every peer in `peers` concurrently via `POST /v1/forth/define`.
/// The peer compiles and persists the definition.  Never panics; errors captured per-peer.
pub async fn define_on_peers(
    peers: &[String],
    source: &str,
    peer_tokens: &std::collections::HashMap<String, String>,
) -> Vec<PeerResult> {
    let tasks: Vec<_> = peers
        .iter()
        .map(|peer| {
            let peer   = peer.clone();
            let source = source.to_string();
            let token  = peer_tokens.get(&peer).cloned();
            async move {
                match define_on_peer(&peer, &source, token.as_deref()).await {
                    Ok(output) => PeerResult { peer, output, stack: Vec::new(), error: None, compute_ms: 0, debt_warning: None, forth_back: None },
                    Err(e)     => PeerResult {
                        peer,
                        output:       String::new(),
                        stack:        Vec::new(),
                        error:        Some(e.to_string()),
                        compute_ms:   0,
                        debt_warning: None,
                        forth_back:   None,
                    },
                }
            }
        })
        .collect();
    join_all(tasks).await
}

async fn define_on_peer(addr: &str, source: &str, token: Option<&str>) -> anyhow::Result<String> {
    let url = if addr.starts_with("http://") || addr.starts_with("https://") {
        format!("{addr}/v1/forth/define")
    } else {
        format!("http://{addr}/v1/forth/define")
    };

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()?;

    let mut req = client.post(&url);
    if let Some(t) = token {
        req = req.header(crate::peer_token::HEADER, t);
    }
    let resp = req
        .json(&serde_json::json!({ "source": source }))
        .send()
        .await?
        .json::<serde_json::Value>()
        .await?;

    if let Some(e) = resp["error"].as_str() {
        anyhow::bail!("{}", e);
    }
    Ok(resp["output"].as_str().unwrap_or("").to_string())
}

/// `caller` is this machine's registry address — sent so the remote peer can
/// record the debit and issue a debt warning if the threshold is crossed.
async fn eval_on_peer(addr: &str, code: &str, caller: Option<&str>, token: Option<&str>) -> anyhow::Result<(String, Vec<i64>, Option<String>, u64, Option<String>, Option<String>)> {
    let url = if addr.starts_with("http://") || addr.starts_with("https://") {
        format!("{addr}/v1/forth/eval")
    } else {
        format!("http://{addr}/v1/forth/eval")
    };

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()?;

    let mut body = serde_json::json!({ "code": code });
    if let Some(c) = caller {
        body["caller"] = serde_json::Value::String(c.to_string());
    }

    let mut req = client.post(&url);
    if let Some(t) = token {
        req = req.header(crate::peer_token::HEADER, t);
    }
    let resp = req
        .json(&body)
        .send()
        .await?
        .json::<serde_json::Value>()
        .await?;

    let output       = resp["output"].as_str().unwrap_or("").to_string();
    let error        = resp["error"].as_str().map(|s| s.to_string());
    let compute_ms   = resp["compute_ms"].as_u64().unwrap_or(0);
    let debt_warning = resp["debt_warning"].as_str().map(|s| s.to_string());
    let forth_back   = resp["forth_back"].as_str().map(|s| s.to_string());
    let stack: Vec<i64> = resp["stack"]
        .as_array()
        .map(|arr| arr.iter().filter_map(|v| v.as_i64()).collect())
        .unwrap_or_default();

    Ok((output, stack, error, compute_ms, debt_warning, forth_back))
}
