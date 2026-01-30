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
