//! Internationalization, branding constants, and localized message catalogs for Finch.

use std::borrow::Cow;

#[macro_use]
extern crate rust_i18n;

// Embed the compile-time translation bundles from the locales/ directory.
i18n!("locales", fallback = "en");

/// Brand/Product display name (e.g. "Finch").
pub const PRODUCT_NAME: &str = "Finch";

/// Executable/CLI binary name (e.g. "finch").
pub const BIN_NAME: &str = "finch";

/// Default configuration directory name (e.g. ".finch").
pub const CONFIG_DIR_NAME: &str = ".finch";

/// Supported and available locales in this build.
pub fn get_available_locales() -> Vec<Cow<'static, str>> {
    available_locales!()
}

/// Get the currently active locale.
pub fn current_locale() -> String {
    rust_i18n::locale().to_string()
}

/// Set the active locale.
pub fn switch_locale(locale_str: &str) {
    rust_i18n::set_locale(locale_str);
}

/// Translate a key with a given locale and parameter replacements.
pub fn translate(locale: &str, key: &str, args: &[(&str, &str)]) -> String {
    let raw = _rust_i18n_try_translate(locale, key).unwrap_or_else(|| Cow::Borrowed(key));
    if args.is_empty() {
        raw.into_owned()
    } else {
        let keys: Vec<&str> = args.iter().map(|(k, _)| *k).collect();
        let vals: Vec<Cow<'_, str>> = args.iter().map(|(_, v)| Cow::Borrowed(*v)).collect();
        rust_i18n::replace_patterns_cow(&raw, &keys, &vals)
    }
}

/// Macro for looking up translations in the Finch catalog.
///
/// Supports optional `locale = "..."` and parameter interpolation `name = val`.
#[macro_export]
macro_rules! t {
    ($key:expr, locale = $loc:expr $(, $name:ident = $val:expr)* $(,)?) => {
        {
            let args: &[(&str, &str)] = &[ $( (stringify!($name), &$val.to_string()) ),* ];
            $crate::translate($loc, $key, args)
        }
    };
    ($key:expr $(, $name:ident = $val:expr)* $(,)?) => {
        {
            let loc = $crate::current_locale();
            let args: &[(&str, &str)] = &[ $( (stringify!($name), &$val.to_string()) ),* ];
            $crate::translate(&loc, $key, args)
        }
    };
}

/// Detect best matching locale based on explicit override, environment variables,
/// system locale, and finally fallback to "en".
pub fn detect_locale(explicit_override: Option<&str>) -> String {
    if let Some(explicit) = explicit_override {
        if !explicit.trim().is_empty() {
            return normalize_locale(explicit);
        }
    }

    // Check environment variables in priority order
    for var in &[
        "FINCH_LOCALE",
        "FINCH_LANG",
        "LC_ALL",
        "LC_MESSAGES",
        "LANG",
    ] {
        if let Ok(val) = std::env::var(var) {
            let trimmed = val.trim();
            if !trimmed.is_empty() && trimmed != "C" && trimmed != "POSIX" {
                return normalize_locale(trimmed);
            }
        }
    }

    // Try system locale
    if let Some(sys) = sys_locale::get_locale() {
        return normalize_locale(&sys);
    }

    "en".to_string()
}

/// Normalize locale string (e.g., "en_US.UTF-8" -> "en", "es-ES" -> "es" if exact not present).
fn normalize_locale(raw: &str) -> String {
    // Strip encoding like .UTF-8 or @euro
    let clean = raw.split('.').next().unwrap_or(raw);
    let clean = clean.split('@').next().unwrap_or(clean);

    let available = get_available_locales();

    // 1. Direct match (e.g. "es", "en")
    if available.iter().any(|l| l.as_ref() == clean) {
        return clean.to_string();
    }

    // 2. Normalize separator (_ to -)
    let dash = clean.replace('_', "-");
    if available.iter().any(|l| l.as_ref() == dash) {
        return dash;
    }

    // 3. Fallback to base language (e.g., "es-419" or "es_MX" -> "es")
    let base = clean
        .split(|c| c == '_' || c == '-')
        .next()
        .unwrap_or(clean);
    if available.iter().any(|l| l.as_ref() == base) {
        return base.to_string();
    }

    // 4. Default fallback
    "en".to_string()
}

/// Initialize the i18n subsystem with optional explicit configuration.
pub fn init(explicit_locale: Option<&str>) -> String {
    let loc = detect_locale(explicit_locale);
    switch_locale(&loc);
    loc
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_constants() {
        assert_eq!(PRODUCT_NAME, "Finch");
        assert_eq!(BIN_NAME, "finch");
        assert_eq!(CONFIG_DIR_NAME, ".finch");
    }

    #[test]
    fn test_translations_en() {
        assert_eq!(t!("app.name", locale = "en"), "Finch");
        assert_eq!(t!("app.bin", locale = "en"), "finch");
        assert_eq!(
            t!("setup.starting", locale = "en", app = PRODUCT_NAME),
            "Starting Finch setup wizard...\n"
        );
        assert_eq!(
            t!("setup.cancelled", locale = "en"),
            "Setup cancelled; configuration was not changed."
        );
        assert_eq!(
            t!("daemon.starting", locale = "en", app = PRODUCT_NAME),
            "Starting Finch in daemon mode"
        );
    }

    #[test]
    fn test_translations_es() {
        assert_eq!(
            t!("setup.starting", locale = "es", app = PRODUCT_NAME),
            "Iniciando el asistente de configuración de Finch...\n"
        );
        assert_eq!(
            t!("setup.cancelled", locale = "es"),
            "Configuración cancelada; la configuración no fue modificada."
        );
        assert_eq!(
            t!("daemon.starting", locale = "es", app = PRODUCT_NAME),
            "Iniciando Finch en modo daemon"
        );
    }

    #[test]
    fn test_locale_normalization() {
        assert_eq!(normalize_locale("en_US.UTF-8"), "en");
        assert_eq!(normalize_locale("es_MX.UTF-8"), "es");
        assert_eq!(normalize_locale("fr_FR"), "en"); // fr not present, falls back to en
    }
}
