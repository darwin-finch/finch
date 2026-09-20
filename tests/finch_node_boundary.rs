use finch_node::{NodeSigningIdentity, NodeTlsIdentity};

#[test]
fn public_node_facade_preserves_signing_and_tls_identity_pairing() {
    let identity = NodeSigningIdentity::from_secret([73; 32]);
    let message = b"finch-node public boundary";
    let signature = identity.sign(message);

    NodeSigningIdentity::verify(identity.public_key_bytes(), message, signature)
        .expect("the public finch-node facade must verify its own signature");
    let tls = NodeTlsIdentity::from_signing_identity(&identity, "boundary.local")
        .expect("the public finch-node facade must derive TLS identity from the signing key");
    let config = tls
        .rustls_server_config()
        .expect("the public finch-node facade must construct a paired TLS server config");

    assert_eq!(
        config.alpn_protocols,
        vec![b"h2".to_vec(), b"http/1.1".to_vec()],
        "the extracted facade must preserve the production HTTP ALPN contract"
    );
    assert!(
        !tls.certificate_der().is_empty(),
        "the extracted facade must expose the derived certificate"
    );
}
