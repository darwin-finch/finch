use finch_i18n::{t, PRODUCT_NAME};

#[test]
fn test_cross_crate_i18n() {
    finch_i18n::init(Some("en"));
    assert_eq!(PRODUCT_NAME, "Finch");
    assert_eq!(t!("app.name"), "Finch");
    assert_eq!(
        t!("setup.starting", app = PRODUCT_NAME),
        "Starting Finch setup wizard...\n"
    );

    // Spanish translation
    assert_eq!(
        t!("setup.starting", locale = "es", app = PRODUCT_NAME),
        "Iniciando el asistente de configuración de Finch...\n"
    );
}
