//! Resolving the renderer's colour scheme from a saved configuration.
//!
//! `active_theme` names the preset the UI uses. The `[colors]` table is an
//! override layer on top of that preset: only the keys a user actually wrote
//! replace the theme's values, and only the values that differ from the theme
//! are written back on save.

use crate::theme::{ColorScheme, ColorTheme};

/// The scheme for a saved `active_theme` plus an optional `[colors]` table.
///
/// An unknown theme name falls back to the default preset. A table that
/// merely repeats a built-in preset is not an override: older builds wrote
/// the dark defaults on every save, and honouring that copy would pin the
/// renderer to dark after the user picks another theme. A table that cannot
/// be applied (wrong value type) is ignored with a warning rather than
/// failing startup over a cosmetic setting.
pub(crate) fn resolve_colors(active_theme: &str, saved: Option<toml::Value>) -> ColorScheme {
    let preset = ColorTheme::from_name(active_theme)
        .unwrap_or_default()
        .to_scheme();
    let Some(saved) = saved else {
        return preset;
    };
    if let Ok(full) = saved.clone().try_into::<ColorScheme>() {
        if full.is_builtin_preset() {
            return preset;
        }
    }
    let Ok(mut merged) = toml::Value::try_from(&preset) else {
        return preset;
    };
    overlay(&mut merged, saved);
    match merged.try_into::<ColorScheme>() {
        Ok(scheme) => scheme,
        Err(error) => {
            tracing::warn!(
                %error,
                "ignoring [colors] overrides that do not fit the colour scheme; using the '{active_theme}' theme"
            );
            preset
        }
    }
}

/// The `[colors]` table to save: only the values that differ from the
/// `active_theme` preset, or `None` when the scheme is the preset itself.
pub(crate) fn color_overrides(active_theme: &str, colors: &ColorScheme) -> Option<toml::Value> {
    let preset = ColorTheme::from_name(active_theme)
        .unwrap_or_default()
        .to_scheme();
    let preset = toml::Value::try_from(&preset).ok()?;
    let current = toml::Value::try_from(colors).ok()?;
    difference(&preset, current)
}

fn overlay(base: &mut toml::Value, overrides: toml::Value) {
    match (base, overrides) {
        (toml::Value::Table(base), toml::Value::Table(overrides)) => {
            for (key, value) in overrides {
                match base.get_mut(&key) {
                    Some(slot) => overlay(slot, value),
                    None => {
                        base.insert(key, value);
                    }
                }
            }
        }
        (slot, value) => *slot = value,
    }
}

fn difference(preset: &toml::Value, current: toml::Value) -> Option<toml::Value> {
    match (preset, current) {
        (toml::Value::Table(preset), toml::Value::Table(current)) => {
            let changed: toml::value::Table = current
                .into_iter()
                .filter_map(|(key, value)| {
                    let value = match preset.get(&key) {
                        Some(base) => difference(base, value)?,
                        None => value,
                    };
                    Some((key, value))
                })
                .collect();
            (!changed.is_empty()).then_some(toml::Value::Table(changed))
        }
        (preset, current) => (*preset != current).then_some(current),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme::ColorSpec;

    fn table(source: &str) -> toml::Value {
        toml::from_str(source).expect("test colour table must parse")
    }

    #[test]
    fn test_resolve_colors_uses_the_active_theme_when_no_table_is_saved() {
        for theme in ColorTheme::all() {
            let name = theme.name().to_lowercase();
            assert_eq!(
                resolve_colors(&name, None),
                theme.to_scheme(),
                "active_theme = {name:?} with no [colors] table must render that preset"
            );
        }
    }

    /// The reported failure: `active_theme = "light"` next to a `[colors]`
    /// table holding the dark defaults an earlier save had written. The
    /// renderer used the table and painted a dark canvas on a light theme.
    #[test]
    fn test_resolve_colors_ignores_a_saved_table_that_only_repeats_a_preset() {
        let baked_dark = toml::Value::try_from(ColorTheme::Dark.to_scheme()).unwrap();
        let resolved = resolve_colors("light", Some(baked_dark.clone()));
        assert_eq!(
            resolved,
            ColorTheme::Light.to_scheme(),
            "a saved copy of the dark preset must not override active_theme = \"light\"; saved table: {baked_dark:?}"
        );
    }

    #[test]
    fn test_resolve_colors_applies_only_the_overridden_keys_over_the_theme() {
        let resolved = resolve_colors("light", Some(table("[messages]\nuser = [200, 0, 100]\n")));
        let mut expected = ColorTheme::Light.to_scheme();
        expected.messages.user = ColorSpec::Rgb(200, 0, 100);
        assert_eq!(
            resolved, expected,
            "one overridden colour must leave every other role on the light preset"
        );
    }

    #[test]
    fn test_resolve_colors_falls_back_to_the_theme_for_an_unusable_table() {
        let resolved = resolve_colors("solarized", Some(table("background = true\n")));
        assert_eq!(
            resolved,
            ColorTheme::Solarized.to_scheme(),
            "a malformed override must not fail startup or leave the preset"
        );
    }

    #[test]
    fn test_resolve_colors_falls_back_to_the_default_preset_for_an_unknown_theme() {
        assert_eq!(
            resolve_colors("no-such-theme", None),
            ColorTheme::default().to_scheme()
        );
    }

    #[test]
    fn test_color_overrides_saves_nothing_for_an_unmodified_preset() {
        for theme in ColorTheme::all() {
            let name = theme.name().to_lowercase();
            assert_eq!(
                color_overrides(&name, &theme.to_scheme()),
                None,
                "saving the {name:?} preset must not write a [colors] table that later pins it"
            );
        }
    }

    #[test]
    fn test_color_overrides_round_trips_a_single_customised_colour() {
        let mut scheme = ColorTheme::Light.to_scheme();
        scheme.ui.cursor = ColorSpec::Named("magenta".to_string());
        let saved = color_overrides("light", &scheme).expect("a customised colour must be saved");
        assert_eq!(
            saved,
            table("[ui]\ncursor = \"magenta\"\n"),
            "only the customised colour belongs in the saved table"
        );
        assert_eq!(resolve_colors("light", Some(saved)), scheme);
    }
}
