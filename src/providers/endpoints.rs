//! Endpoint construction shared by provider transport and model discovery.
//!
//! A configured path may be either relative to an API base URL or a complete
//! URL. Complete URLs are preserved verbatim. Relative paths avoid duplicating
//! a leading API version already present at the end of the base URL.

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderEndpoints {
    pub chat_url: String,
    pub models_url: String,
}

impl ProviderEndpoints {
    pub fn new(base_url: &str, chat_path: &str, models_path: &str) -> Self {
        Self {
            chat_url: resolve_endpoint(base_url, chat_path),
            models_url: resolve_endpoint(base_url, models_path),
        }
    }
}

pub fn resolve_endpoint(base_url: &str, path_or_url: &str) -> String {
    if path_or_url.starts_with("https://") || path_or_url.starts_with("http://") {
        return path_or_url.to_string();
    }

    let base = base_url.trim_end_matches('/');
    let mut path = path_or_url.trim_start_matches('/');

    // `https://host/v1` + `/v1/models` must not become `/v1/v1/models`.
    if let Some((first, remainder)) = path.split_once('/') {
        if base.ends_with(&format!("/{first}")) {
            path = remainder;
        }
    } else if base.ends_with(&format!("/{path}")) {
        return base.to_string();
    }

    if path.is_empty() {
        base.to_string()
    } else {
        format!("{base}/{path}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base_ending_in_v1_does_not_duplicate_v1() {
        assert_eq!(
            resolve_endpoint("https://compatible.example/v1", "/v1/chat/completions"),
            "https://compatible.example/v1/chat/completions"
        );
        assert_eq!(
            resolve_endpoint("https://compatible.example/v1/", "v1/models"),
            "https://compatible.example/v1/models"
        );
    }

    #[test]
    fn complete_configured_urls_are_preserved_exactly() {
        let endpoints = ProviderEndpoints::new(
            "https://ignored.example/v1",
            "https://gateway.example/api/coding/paas/v4/chat/completions?preview=1",
            "https://catalog.example/custom/models?account=work",
        );
        assert_eq!(
            endpoints.chat_url,
            "https://gateway.example/api/coding/paas/v4/chat/completions?preview=1"
        );
        assert_eq!(
            endpoints.models_url,
            "https://catalog.example/custom/models?account=work"
        );
    }
}
