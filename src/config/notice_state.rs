//! Where the licence notice records that it has been shown.
//!
//! Not in `config.toml`. Showing a weekly notice is application bookkeeping,
//! not a configuration change the user made, and recording it there meant
//! ordinary startup rewrote the user's configuration file — reformatting it,
//! dropping comments, and moving its mtime — for a reason the user never asked
//! for (#76). A separate runtime-state file keeps `config.toml` byte-for-byte
//! untouched by anything but an explicit save.

use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

/// The runtime-state file, beside the config it was extracted from.
pub(crate) fn notice_state_path() -> Result<PathBuf> {
    let home =
        dirs::home_dir().ok_or_else(|| anyhow::anyhow!("Could not determine home directory"))?;
    Ok(home.join(".finch").join("notice_state.toml"))
}

/// When the licence notice should next be shown.
#[derive(Debug, Default, Clone, serde::Serialize, serde::Deserialize)]
pub(crate) struct NoticeState {
    /// Suppress the startup notice until this ISO 8601 date.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) suppress_until: Option<String>,
}

impl NoticeState {
    /// Read the state file, treating absence and corruption alike as "nothing
    /// recorded".
    ///
    /// A malformed state file must not stop Finch starting, and must not be a
    /// reason to write to `config.toml`. The worst case is showing the notice
    /// once more than intended.
    pub(crate) fn load_from(path: &Path) -> Self {
        std::fs::read_to_string(path)
            .ok()
            .and_then(|text| toml::from_str(&text).ok())
            .unwrap_or_default()
    }

    /// Write the state file, creating its directory if needed.
    pub(crate) fn save_to(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("Failed to create {}", parent.display()))?;
        }
        let text = toml::to_string_pretty(self).context("Failed to serialize notice state")?;
        // Write-and-rename, not a plain write. Two Finch processes can start
        // at the same moment and both land here, and a half-written file is a
        // readable outcome of an interrupted `fs::write`. The corrupt path in
        // `load_from` recovers from that, but recovering means showing the
        // notice an extra time; renaming means it cannot happen at all. The
        // temporary carries the process id so two writers never share one.
        let temporary = path.with_extension(format!("toml.{}.tmp", std::process::id()));
        std::fs::write(&temporary, text)
            .with_context(|| format!("Failed to write {}", temporary.display()))?;
        std::fs::rename(&temporary, path).with_context(|| {
            format!(
                "Failed to replace {} with {}",
                path.display(),
                temporary.display()
            )
        })
    }
}

/// Forget any recorded suppression, so the next start shows the notice.
///
/// `finch license remove` used to achieve this for free: it wrote
/// `LicenseConfig::default()`, whose `notice_suppress_until` is `None`, and
/// the notice reappeared next start. Once the record moved out of the config
/// that stopped working — the state file wins, so removing a licence left the
/// user un-nagged until the old date expired. Review of #329 caught it;
/// deleting the file restores the previous behaviour exactly.
pub(crate) fn forget_recorded_suppression(state_path: &Path) {
    // Absent is the same as forgotten, so a failure here costs one missed
    // notice rather than anything durable.
    let _ = std::fs::remove_file(state_path);
}

/// Decide whether to show the notice, and record the decision — **without
/// touching `config.toml`**.
///
/// `config_suppress_until` is the legacy value that used to live in
/// `config.toml`. It is still read, so an existing installation's suppression
/// survives this change; it is never written back.
///
/// A state file that *carries a date* wins. One that exists but holds no date
/// — an empty file, or a serialized default, which `skip_serializing_if`
/// renders as empty — falls back to the config value, because `.or` runs on
/// the `Option` before parsing. That fallback is right, and it is not what an
/// earlier version of this sentence claimed.
pub(crate) fn claim_notice_showing(
    state_path: &Path,
    config_suppress_until: Option<&str>,
    today: chrono::NaiveDate,
    period: chrono::Duration,
) -> bool {
    use chrono::NaiveDate;

    let state = NoticeState::load_from(state_path);
    let recorded = state
        .suppress_until
        .as_deref()
        .or(config_suppress_until)
        .and_then(|s| NaiveDate::parse_from_str(s, "%Y-%m-%d").ok());

    if recorded.is_some_and(|until| today <= until) {
        return false;
    }

    let next = NoticeState {
        suppress_until: Some((today + period).format("%Y-%m-%d").to_string()),
    };
    // Non-fatal: failing to record it shows the notice again next start, which
    // is a far smaller cost than refusing to start or writing to the config.
    let _ = next.save_to(state_path);
    true
}

/// The whole startup decision, taking every path it touches as an argument.
///
/// This exists so the #76 invariant can be tested. The narrower
/// `claim_notice_showing` cannot express it: it takes the legacy date as an
/// `Option<&str>` and has no config path at all, so it is *structurally
/// incapable* of writing `config.toml` — which makes a "the config was not
/// touched" assertion against it vacuous. Review of #329 proved that by
/// restoring the original defect to the startup path and watching all 96 tests
/// pass.
///
/// The startup path calls this, so a test that points `config_path` and
/// `state_path` at a temporary directory exercises the same code the REPL runs.
pub(crate) fn claim_notice_showing_for(
    config: &crate::config::Config,
    config_path: &Path,
    state_path: &Path,
    today: chrono::NaiveDate,
) -> bool {
    let _ = config_path; // Deliberately unused: nothing here may write it.
    claim_notice_showing(
        state_path,
        config.license.notice_suppress_until.as_deref(),
        today,
        chrono::Duration::days(7),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{Duration, NaiveDate};

    fn day(s: &str) -> NaiveDate {
        NaiveDate::parse_from_str(s, "%Y-%m-%d").unwrap()
    }

    /// The whole point of #76: deciding to show the notice writes the state
    /// file and leaves `config.toml` byte-for-byte identical, mtime included.
    #[test]
    fn test_claiming_the_notice_does_not_touch_the_config_file() {
        let home = tempfile::tempdir().unwrap();
        let config = home.path().join("config.toml");
        let original = b"# hand-written, with a comment the serializer would drop\n[license]\nlicense_type = \"noncommercial\"\n";
        std::fs::write(&config, original).unwrap();
        let before_mtime = std::fs::metadata(&config).unwrap().modified().unwrap();
        // Coarse filesystem timestamps would hide a rewrite that happened
        // within the same tick, so put the write comfortably after the read.
        std::thread::sleep(std::time::Duration::from_millis(1100));

        let state = home.path().join("notice_state.toml");
        assert!(
            claim_notice_showing(&state, None, day("2026-09-03"), Duration::days(7)),
            "nothing recorded yet, so the notice is due"
        );

        assert_eq!(
            std::fs::read(&config).unwrap(),
            original,
            "config.toml must be byte-for-byte unchanged; rewriting it to record \
             a notice is the defect #76 is about"
        );
        assert_eq!(
            std::fs::metadata(&config).unwrap().modified().unwrap(),
            before_mtime,
            "config.toml mtime must not move either -- a rewrite with identical \
             bytes still tells every backup and sync tool the file changed"
        );
        assert!(state.exists(), "the state file is where the record belongs");
    }

    /// A second start inside the period is silent, and still writes nothing.
    #[test]
    fn test_a_second_start_inside_the_period_does_not_show_or_write() {
        let home = tempfile::tempdir().unwrap();
        let state = home.path().join("notice_state.toml");
        assert!(claim_notice_showing(
            &state,
            None,
            day("2026-09-03"),
            Duration::days(7)
        ));
        let recorded = std::fs::read(&state).unwrap();

        assert!(
            !claim_notice_showing(&state, None, day("2026-09-05"), Duration::days(7)),
            "two days later is still inside the seven-day period"
        );
        assert_eq!(
            std::fs::read(&state).unwrap(),
            recorded,
            "a start that shows nothing must record nothing"
        );
    }

    /// The period elapsing shows it again.
    #[test]
    fn test_the_notice_returns_after_the_period() {
        let home = tempfile::tempdir().unwrap();
        let state = home.path().join("notice_state.toml");
        assert!(claim_notice_showing(
            &state,
            None,
            day("2026-09-03"),
            Duration::days(7)
        ));
        assert!(
            !claim_notice_showing(&state, None, day("2026-09-10"), Duration::days(7)),
            "the boundary day is still suppressed"
        );
        assert!(
            claim_notice_showing(&state, None, day("2026-09-11"), Duration::days(7)),
            "the day after the recorded date shows it again"
        );
    }

    /// An existing installation's suppression survives the move.
    ///
    /// Without this, everyone who had been suppressed by the old
    /// `config.toml` field would see the notice again on the release that
    /// stops writing it -- a regression dressed as a fix.
    #[test]
    fn test_the_legacy_config_value_is_honoured_but_never_written_back() {
        let home = tempfile::tempdir().unwrap();
        let state = home.path().join("notice_state.toml");

        assert!(
            !claim_notice_showing(
                &state,
                Some("2026-09-10"),
                day("2026-09-03"),
                Duration::days(7)
            ),
            "a legacy suppression date must still suppress"
        );
        assert!(!state.exists(), "and suppressing writes nothing at all");

        assert!(
            claim_notice_showing(
                &state,
                Some("2026-09-01"),
                day("2026-09-03"),
                Duration::days(7)
            ),
            "an expired legacy date shows the notice"
        );
        let migrated = NoticeState::load_from(&state);
        assert_eq!(
            migrated.suppress_until.as_deref(),
            Some("2026-09-10"),
            "and records the next date in the state file, not the config"
        );
    }

    /// The #76 invariant, at the seam the startup path actually calls.
    ///
    /// This is the regression test. The one below it, which asserts bytes and
    /// mtime against `claim_notice_showing`, cannot fail for the right reason:
    /// that function receives the legacy date as an `Option<&str>` and never
    /// sees a config path, so it could not write `config.toml` even if it
    /// tried. Review of #329 restored the original defect to `event_loop.rs`
    /// and all 96 tests passed.
    ///
    /// `claim_notice_showing_for` takes both paths, so a restored
    /// `config.save_to(config_path)` inside it fails here.
    #[test]
    fn test_the_startup_decision_leaves_the_config_untouched() {
        let home = tempfile::tempdir().unwrap();
        let config_path = home.path().join("config.toml");
        let original =
            b"# a comment the serializer would drop\n[license]\nlicense_type = \"noncommercial\"\n";
        std::fs::write(&config_path, original).unwrap();
        let before = std::fs::metadata(&config_path).unwrap().modified().unwrap();
        std::thread::sleep(std::time::Duration::from_millis(1100));

        let config = crate::config::Config::new(Vec::new());
        let state_path = home.path().join("notice_state.toml");
        assert!(
            crate::config::claim_notice_showing_for(
                &config,
                &config_path,
                &state_path,
                day("2026-09-03"),
            ),
            "nothing recorded, so the notice is due"
        );

        assert_eq!(
            std::fs::read(&config_path).unwrap(),
            original,
            "the startup decision must not rewrite config.toml -- this is #76"
        );
        assert_eq!(
            std::fs::metadata(&config_path).unwrap().modified().unwrap(),
            before,
            "nor move its mtime: an identical-bytes rewrite still tells every \
             backup and sync tool the file changed"
        );

        // The record went to the state file, and nothing else was left beside
        // it -- a surviving `.tmp` would mean the atomic write did not complete.
        let written: Vec<String> = std::fs::read_dir(home.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|n| n != "config.toml")
            .collect();
        assert_eq!(
            written,
            vec!["notice_state.toml".to_string()],
            "the state file, and no temporary left behind"
        );
    }

    /// Removing a licence makes the notice due again.
    ///
    /// This used to happen for free: `LicenseRemove` wrote
    /// `LicenseConfig::default()` with `notice_suppress_until: None`, and the
    /// notice reappeared. Moving the record to a state file silently broke it,
    /// because the state file wins over the config's `None` — so a user who
    /// removed their licence stayed un-nagged until the old date expired.
    /// Nothing caught that; review of #329 did.
    #[test]
    fn test_removing_a_licence_makes_the_notice_due_again() {
        let home = tempfile::tempdir().unwrap();
        let state = home.path().join("notice_state.toml");

        assert!(claim_notice_showing(
            &state,
            None,
            day("2026-09-03"),
            Duration::days(7)
        ));
        assert!(
            !claim_notice_showing(&state, None, day("2026-09-04"), Duration::days(7)),
            "still inside the period, so suppressed"
        );

        forget_recorded_suppression(&state);

        assert!(
            claim_notice_showing(&state, None, day("2026-09-04"), Duration::days(7)),
            "removing the licence must make it due again on the same day"
        );
        // And forgetting when there is nothing recorded is not an error.
        forget_recorded_suppression(&home.path().join("absent.toml"));
    }

    /// The boundary matches the behaviour this replaced, exactly.
    ///
    /// The original predicate was `is_none_or(|d| today > d)` -- show when
    /// today is strictly after the recorded date. The replacement suppresses
    /// when `today <= until`, the same predicate negated. Nothing else pins
    /// that equivalence, so an edit turning `<=` into `<` would change how
    /// often every user sees the notice and no test would object.
    ///
    /// The old predicate is written out here rather than referenced, so this
    /// still means something now the code it came from is gone.
    #[test]
    fn test_the_boundary_matches_the_behaviour_it_replaced() {
        let home = tempfile::tempdir().unwrap();
        let recorded = day("2026-09-10");

        for (today, original_would_show) in [
            (day("2026-09-09"), false),
            (day("2026-09-10"), false),
            (day("2026-09-11"), true),
        ] {
            assert_eq!(
                today > recorded,
                original_would_show,
                "the reference predicate itself must be right, or this proves nothing"
            );
            let state = home.path().join(format!("state-{today}.toml"));
            assert_eq!(
                claim_notice_showing(&state, Some("2026-09-10"), today, Duration::days(7)),
                original_would_show,
                "on {today} the replacement must agree with `today > suppress_until`"
            );
        }
    }

    /// A corrupt state file is not a reason to fail or to touch the config.
    #[test]
    fn test_a_corrupt_state_file_is_treated_as_nothing_recorded() {
        let home = tempfile::tempdir().unwrap();
        let state = home.path().join("notice_state.toml");
        std::fs::write(&state, b"\xff\xfe not toml at all").unwrap();

        assert!(
            claim_notice_showing(&state, None, day("2026-09-03"), Duration::days(7)),
            "unreadable state means nothing is recorded, so show it"
        );
        assert_eq!(
            NoticeState::load_from(&state).suppress_until.as_deref(),
            Some("2026-09-10"),
            "and it is replaced with something readable"
        );
    }
}
