//! Shared MCP protocol constants.

/// Newest protocol revision implemented by Finch.
pub const LATEST_PROTOCOL_VERSION: &str = "2026-07-28";

/// Newest revision which uses the initialize/initialized handshake.
pub const LATEST_LEGACY_PROTOCOL_VERSION: &str = "2025-11-25";

/// Protocol revisions Finch can negotiate on its stdio transport.
pub const SUPPORTED_PROTOCOL_VERSIONS: &[&str] = &[
    LATEST_PROTOCOL_VERSION,
    LATEST_LEGACY_PROTOCOL_VERSION,
    "2025-06-18",
    "2025-03-26",
    "2024-11-05",
];

/// Select Finch's newest revision from a server's advertised versions.
pub fn select_protocol_version<'a>(
    offered: impl IntoIterator<Item = &'a str>,
) -> Option<&'static str> {
    let offered: Vec<&str> = offered.into_iter().collect();
    SUPPORTED_PROTOCOL_VERSIONS
        .iter()
        .copied()
        .find(|supported| offered.contains(supported))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn selects_newest_mutually_supported_version() {
        assert_eq!(
            select_protocol_version(["2024-11-05", "2025-11-25"]),
            Some("2025-11-25")
        );
        assert_eq!(select_protocol_version(["not-a-version"]), None);
    }
}
