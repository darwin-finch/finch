/// Finch Lisp — a Scheme-flavoured dialect that yields into the event loop.
///
/// ## Design
///
/// Every evaluation runs in a `tokio::spawn`ed task.  SSH and I/O operations
/// are `.await` points — the scheduler yields here, letting the event loop
/// dispatch other events (dialogs, tool results, renders) while the network
/// call is in flight.  This is the same pattern as `ShowDialog`, `BrainQuestion`,
/// and `PendingPosetRun` — just at a finer granularity.
///
/// ## Entry points
///
/// ```
/// // Parse + eval one or more top-level expressions.  Returns the last value.
/// let result = lisp::run("(+ 1 2)", &ctx).await?;
///
/// // Evaluate into a pre-existing environment (persists defines across calls).
/// let result = lisp::run_in(src, env, ctx).await?;
/// ```
///
/// ## SSH example
///
/// ```lisp
/// (define s (ssh-connect "192.168.1.10" 22 "alice" "secret"))
/// (display (ssh-exec s "uptime"))
/// (ssh-close s)
/// ```
///
/// ## Crypto example
///
/// ```lisp
/// (define key (random-bytes 32))
/// (define nonce (random-bytes 12))
/// (define ct (chacha20-seal key nonce (string->bytes "hello")))
/// (display (bytes->hex ct))
/// ```
pub mod env;
pub mod eval;
pub mod reader;
pub mod stdlib;
pub mod types;

use anyhow::Result;
use std::sync::Arc;

pub use env::EnvRef;
pub use types::Val;

use crate::ssh::SshSessionStore;

// ── Context ───────────────────────────────────────────────────────────────────

/// Shared context passed through every eval call.
///
/// Holds the SSH session store and (in the future) any other I/O resources the
/// Lisp evaluator needs to cross task boundaries.
pub struct LispCtx {
    pub ssh_sessions: SshSessionStore,
}

impl LispCtx {
    pub fn new() -> Self {
        Self {
            ssh_sessions: SshSessionStore::new(),
        }
    }
}

impl Default for LispCtx {
    fn default() -> Self {
        Self::new()
    }
}

// ── Public API ────────────────────────────────────────────────────────────────

/// Create a fresh global environment pre-loaded with all built-ins.
pub fn make_env() -> EnvRef {
    let env = env::Env::new_root();
    stdlib::register_all(&env);
    env
}

/// Parse `src`, evaluate all top-level expressions in a fresh env, return the
/// last value as a display string.
pub async fn run(src: &str, ctx: Arc<LispCtx>) -> Result<String> {
    let env = make_env();
    let val = run_in(src, env, ctx).await?;
    Ok(val.to_string())
}

/// Parse `src`, evaluate all top-level expressions in `env` (which persists
/// `define`d globals across calls), return the last value.
pub async fn run_in(src: &str, env: EnvRef, ctx: Arc<LispCtx>) -> Result<Val> {
    let exprs = reader::parse_str(src)?;
    if exprs.is_empty() {
        return Ok(Val::Nil);
    }
    let mut last = Val::Nil;
    for expr in exprs {
        last = eval::eval(expr, env.clone(), ctx.clone()).await?;
    }
    Ok(last)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx() -> Arc<LispCtx> {
        Arc::new(LispCtx::new())
    }

    #[tokio::test]
    async fn test_run_returns_string() {
        let result = run("(+ 3 4)", ctx()).await.unwrap();
        assert_eq!(result, "7");
    }

    #[tokio::test]
    async fn test_run_in_persists_defines() {
        let ctx = ctx();
        let env = make_env();
        run_in("(define x 100)", env.clone(), ctx.clone()).await.unwrap();
        let val = run_in("x", env.clone(), ctx.clone()).await.unwrap();
        assert_eq!(val, Val::Int(100));
    }

    #[tokio::test]
    async fn test_run_multiple_expressions() {
        let result = run("(define n 5) (* n n)", ctx()).await.unwrap();
        assert_eq!(result, "25");
    }

    #[tokio::test]
    async fn test_run_empty_string() {
        let result = run("", ctx()).await.unwrap();
        assert_eq!(result, "()"); // Nil
    }

    #[tokio::test]
    async fn test_run_crypto_sha256() {
        let result = run(
            r#"(bytes->hex (sha256 (string->bytes "abc")))"#,
            ctx(),
        ).await.unwrap();
        // SHA-256("abc") = ba7816bf...
        assert!(result.starts_with("ba7816bf"), "unexpected: {result}");
    }

    #[tokio::test]
    async fn test_run_ssh_session_not_found_without_connection() {
        // Connecting to a non-existent host returns an error, not a panic.
        let err = run(
            r#"(ssh-connect "127.0.0.1" 1 "nobody" "nopass")"#,
            ctx(),
        ).await;
        assert!(err.is_err());
    }
}
