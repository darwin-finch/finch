//! Crate-independence and public-contract smoke tests.

#[test]
fn package_graph_does_not_include_finch() {
    let manifest = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/Cargo.toml"));
    assert!(
        !manifest
            .lines()
            .any(|line| line.trim() == "finch = { path = \"../..\" }"
                || line.trim().starts_with("finch =")),
        "finch-providers must not depend on the Finch application crate"
    );
}

#[test]
fn public_contract_exports_provider_and_oauth_facades() {
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<finch_providers::ProviderRequest>();
    assert_send_sync::<finch_providers::oauth::OAuthDialectDescriptor>();
}
