// Retry logic with exponential backoff

use anyhow::Result;
use std::time::Duration;
use tokio::time::sleep;

const MAX_RETRIES: u32 = 3;
const BASE_DELAY_MS: u64 = 1000;

/// Execute a function with exponential backoff retry logic
pub async fn with_retry<F, Fut, T>(f: F) -> Result<T>
where
    F: Fn() -> Fut,
    Fut: std::future::Future<Output = Result<T>>,
{
    let mut last_error = None;

    for attempt in 0..MAX_RETRIES {
        match f().await {
            Ok(result) => return Ok(result),
            Err(e) => {
                last_error = Some(e);

                if attempt < MAX_RETRIES - 1 {
                    let delay = Duration::from_millis(BASE_DELAY_MS * 2u64.pow(attempt));
                    tracing::warn!(
                        "Request failed (attempt {}/{}), retrying in {:?}",
                        attempt + 1,
                        MAX_RETRIES,
                        delay
                    );
                    sleep(delay).await;
                }
            }
        }
    }

    Err(last_error.unwrap())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::sync::Arc;

    #[tokio::test]
    async fn test_succeeds_on_first_try_no_retries() {
        let result = with_retry(|| async { Ok::<i32, anyhow::Error>(42) }).await;
        assert_eq!(result.unwrap(), 42);
    }

    #[tokio::test]
    async fn test_returns_string_value_immediately() {
        let result = with_retry(|| async { Ok::<_, anyhow::Error>("hello".to_string()) }).await;
        assert_eq!(result.unwrap(), "hello");
    }

    // Use start_paused so tokio auto-advances through sleep() calls instantly
    #[tokio::test(start_paused = true)]
    async fn test_retries_twice_then_succeeds() {
        let call_count = Arc::new(AtomicU32::new(0));
        let cc = Arc::clone(&call_count);

        let result = with_retry(|| {
            let cc = Arc::clone(&cc);
            async move {
                let n = cc.fetch_add(1, Ordering::SeqCst);
                if n < 2 {
                    Err(anyhow::anyhow!("transient"))
                } else {
                    Ok(99i32)
                }
            }
        })
        .await;

        assert_eq!(result.unwrap(), 99);
        // Failed twice (n=0,1), succeeded on third call (n=2)
        assert_eq!(call_count.load(Ordering::SeqCst), 3);
    }

    #[tokio::test(start_paused = true)]
    async fn test_exhausts_all_retries_returns_last_error() {
        let call_count = Arc::new(AtomicU32::new(0));
        let cc = Arc::clone(&call_count);

        let result: anyhow::Result<i32> = with_retry(|| {
            let cc = Arc::clone(&cc);
            async move {
                cc.fetch_add(1, Ordering::SeqCst);
                Err(anyhow::anyhow!("persistent error"))
            }
        })
        .await;

        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("persistent error"));
        // MAX_RETRIES = 3: exactly 3 attempts
        assert_eq!(call_count.load(Ordering::SeqCst), MAX_RETRIES);
    }

    #[tokio::test(start_paused = true)]
    async fn test_exactly_max_retries_attempts() {
        let call_count = Arc::new(AtomicU32::new(0));
        let cc = Arc::clone(&call_count);

        let _: anyhow::Result<()> = with_retry(|| {
            let cc = Arc::clone(&cc);
            async move {
                cc.fetch_add(1, Ordering::SeqCst);
                Err(anyhow::anyhow!("fail"))
            }
        })
        .await;

        assert_eq!(
            call_count.load(Ordering::SeqCst),
            MAX_RETRIES,
            "should attempt exactly MAX_RETRIES={MAX_RETRIES} times"
        );
    }

    #[tokio::test]
    async fn test_first_try_success_returns_ok_not_err() {
        // Verify the happy path produces Ok, not Err
        let result: anyhow::Result<u8> = with_retry(|| async { Ok(7) }).await;
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), 7);
    }
}
