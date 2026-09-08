//! Provider setup through the real terminal and commit boundary.
//!
//! The reducer has detailed unit coverage in `src/cli/setup_wizard.rs`, but the
//! reported failure crossed the rendered wizard and persisted configuration.
//! This test therefore gives `finch setup` a real PTY, drives the same keys a
//! user presses, observes the rendered add/edit overlays, and reloads the file
//! written by the production setup ceremony.

#![cfg(unix)]

use finch::config::{CredentialLifecycle, CredentialProvider, ProviderCredential, ProviderEntry};
use finch::models::unified_loader::{InferenceProvider, ModelFamily, ModelSize};
use std::io::{Read, Write};
use std::os::fd::OwnedFd;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant, SystemTime};

const FRAME_DEADLINE: Duration = Duration::from_secs(30);
const EXIT_DEADLINE: Duration = Duration::from_secs(30);
const SUPERVISOR_AUTHORITY_FDS: &[i32] = &[9, 10, 11, 12, 108, 109, 110, 111, 112];

struct Fixture {
    _temp: tempfile::TempDir,
    home: PathBuf,
    config_path: PathBuf,
    expected_providers: Vec<ProviderEntry>,
    expected_credentials: Vec<ProviderCredential>,
}

impl Fixture {
    fn new() -> Self {
        Self::seed(false)
    }

    fn sole_subscription() -> Self {
        Self::seed(true)
    }

    fn seed(sole_subscription: bool) -> Self {
        let temp = tempfile::tempdir().expect("create disposable setup HOME");
        let home = temp.path().to_path_buf();
        let finch_dir = home.join(".finch");
        std::fs::create_dir_all(&finch_dir).expect("create disposable .finch directory");
        let config_path = finch_dir.join("config.toml");
        let mut providers = seeded_providers(temp.path());
        if sole_subscription {
            providers.truncate(1);
        }
        let credential = chatgpt_credential();
        #[derive(serde::Serialize)]
        struct SeedConfig {
            providers: Vec<ProviderEntry>,
            credentials: Vec<ProviderCredential>,
        }
        let encoded = toml::to_string_pretty(&SeedConfig {
            providers: providers.clone(),
            credentials: vec![credential.clone()],
        })
        .expect("serialize setup fixture");
        std::fs::write(&config_path, encoded).expect("seed setup config");

        Self {
            _temp: temp,
            home,
            config_path,
            expected_providers: providers,
            expected_credentials: vec![credential],
        }
    }
}

fn seeded_providers(root: &Path) -> Vec<ProviderEntry> {
    vec![
        ProviderEntry::Credentialed {
            provider: CredentialProvider::ChatgptSubscription,
            credential: finch::config::CredentialBinding {
                credential_ref: "chatgpt:boundary".into(),
                audience: Some(finch::config::AudienceBinding::standard(
                    finch::config::EndpointFamily::ChatgptSubscription,
                )),
                tenant: None,
                project: None,
                account: None,
                required_scopes: finch::providers::chatgpt_oauth::chatgpt_required_scopes(),
            },
            model: Some("gpt-5.6-sol".into()),
            base_url: None,
            chat_path: None,
            models_path: None,
            name: Some("ChatGPT Boundary".into()),
            reasoning_effort: Some(finch::config::ReasoningEffort::High),
        },
        ProviderEntry::Openai {
            api_key: "sk-test-boundary-existing-openai-key".into(),
            model: Some("gpt-4o".into()),
            base_url: None,
            chat_path: None,
            models_path: None,
            name: Some("OpenAI Boundary".into()),
            reasoning_effort: Some(finch::config::ReasoningEffort::Medium),
        },
        ProviderEntry::Ollama {
            model: "qwen2.5:7b".into(),
            base_url: "http://127.0.0.1:11434".into(),
            name: Some("Ollama Boundary".into()),
        },
        ProviderEntry::RemoteDaemon {
            address: "127.0.0.1:11435".into(),
            name: Some("Finch Boundary".into()),
        },
        ProviderEntry::Local {
            inference_provider: InferenceProvider::Onnx,
            execution_target: finch::config::ExecutionTarget::Cpu,
            model_family: ModelFamily::Qwen2,
            model_size: ModelSize::Small,
            model_repo: Some("boundary/local-model".into()),
            model_path: Some(root.join("models/boundary")),
            enabled: true,
            name: Some("Local Boundary".into()),
        },
    ]
}

fn chatgpt_credential() -> ProviderCredential {
    toml::from_str(
        r#"
name = "chatgpt:boundary"
kind = "oauth_device"
provider = "chatgpt_subscription"
issuer = "openai-chatgpt"
account = "boundary-account"
secret_ref = "oauth-store:chatgpt:boundary"
scopes = ["chatgpt.codex.invoke"]

[audience]
family = "chatgpt_subscription"

[lifecycle]
state = "active"
refreshable = true
"#,
    )
    .expect("deserialize a secret-free reusable ChatGPT credential")
}

struct Snapshot {
    bytes: Vec<u8>,
    modified: SystemTime,
}

impl Snapshot {
    fn read(path: &Path) -> Self {
        let metadata = std::fs::metadata(path)
            .unwrap_or_else(|error| panic!("read metadata for {}: {error}", path.display()));
        Self {
            bytes: std::fs::read(path)
                .unwrap_or_else(|error| panic!("read {}: {error}", path.display())),
            modified: metadata
                .modified()
                .unwrap_or_else(|error| panic!("read mtime for {}: {error}", path.display())),
        }
    }
}

struct Session {
    child: Child,
    master: OwnedFd,
    transcript: std::sync::Arc<std::sync::Mutex<Vec<u8>>>,
    reader: Option<std::thread::JoinHandle<()>>,
    reader_done: std::sync::mpsc::Receiver<()>,
}

impl Session {
    fn spawn(fixture: &Fixture) -> Self {
        let winsize = nix::pty::Winsize {
            ws_row: 40,
            ws_col: 120,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        let pty = nix::pty::openpty(&winsize, None).expect("open setup PTY");
        let slave_in = pty.slave.try_clone().expect("clone setup PTY stdin");
        let slave_out = pty.slave.try_clone().expect("clone setup PTY stdout");
        let slave_err = pty.slave.try_clone().expect("clone setup PTY stderr");

        let mut command = Command::new(env!("CARGO_BIN_EXE_finch"));
        command
            .arg("setup")
            .stdin(Stdio::from(slave_in))
            .stdout(Stdio::from(slave_out))
            .stderr(Stdio::from(slave_err))
            .env("HOME", &fixture.home)
            .env("XDG_CONFIG_HOME", fixture.home.join(".config"))
            .env("XDG_CACHE_HOME", fixture.home.join(".cache"))
            .env("XDG_DATA_HOME", fixture.home.join(".local/share"))
            .env("HF_HOME", fixture.home.join(".cache/huggingface"))
            .env("TERM", "xterm-256color")
            .env_remove("ANTHROPIC_API_KEY")
            .env_remove("OPENAI_API_KEY")
            .env_remove("GROK_API_KEY")
            .env_remove("XAI_API_KEY")
            .env_remove("GEMINI_API_KEY")
            .env_remove("GOOGLE_API_KEY")
            .env_remove("MISTRAL_API_KEY")
            .env_remove("GROQ_API_KEY")
            .env_remove("SHAMMAH_DEBUG")
            .env_remove("RUST_LOG");
        for (name, _) in std::env::vars_os() {
            let name_text = name.to_string_lossy();
            if name_text.starts_with("FINCH_BRAIN_TEST_") || name_text.starts_with("FINCH_TEST_") {
                command.env_remove(name);
            }
        }
        command.env("FINCH_BRAIN_TEST_NO_AUTO_SPAWN", "1");
        unsafe {
            command.pre_exec(|| {
                for fd in SUPERVISOR_AUTHORITY_FDS {
                    nix::libc::close(*fd);
                }
                Ok(())
            });
        }
        let child = command.spawn().expect("spawn finch setup under a PTY");
        drop(pty.slave);

        let transcript = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let master = pty.master;
        let read_fd = master.try_clone().expect("clone setup PTY reader");
        let sink = std::sync::Arc::clone(&transcript);
        let (done_tx, reader_done) = std::sync::mpsc::channel();
        let reader = std::thread::spawn(move || {
            let mut file = std::fs::File::from(read_fd);
            let mut buffer = [0u8; 8192];
            loop {
                match file.read(&mut buffer) {
                    Ok(0) | Err(_) => break,
                    Ok(read) => sink
                        .lock()
                        .expect("setup transcript poisoned")
                        .extend_from_slice(&buffer[..read]),
                }
            }
            let _ = done_tx.send(());
        });

        Self {
            child,
            master,
            transcript,
            reader: Some(reader),
            reader_done,
        }
    }

    fn checkpoint(&self) -> usize {
        self.transcript
            .lock()
            .expect("setup transcript poisoned")
            .len()
    }

    fn transcript(&self) -> String {
        String::from_utf8_lossy(&self.transcript.lock().expect("setup transcript poisoned"))
            .into_owned()
    }

    fn send(&mut self, bytes: &[u8]) {
        let mut file =
            std::fs::File::from(self.master.try_clone().expect("clone setup PTY writer"));
        file.write_all(bytes).expect("write setup keys to PTY");
        file.flush().expect("flush setup keys to PTY");
    }

    fn wait_for_after(&mut self, checkpoint: usize, context: &str, expected: &[&str]) -> String {
        let deadline = Instant::now() + FRAME_DEADLINE;
        loop {
            let raw = self
                .transcript
                .lock()
                .expect("setup transcript poisoned")
                .clone();
            let screen = rendered_terminal_screen(&raw);
            if raw.len() > checkpoint && expected.iter().all(|needle| screen.contains(needle)) {
                return screen;
            }
            if let Ok(Some(status)) = self.child.try_wait() {
                panic!(
                    "{context}: finch setup exited with {status:?} before rendering {expected:?}. \
                     Config path: {}. Terminal was:\n{}",
                    self.config_path_for_diagnostics(),
                    self.transcript()
                );
            }
            if Instant::now() >= deadline {
                panic!(
                    "{context}: finch setup did not render {expected:?} within \
                     {FRAME_DEADLINE:?}. Config path: {}. Terminal was:\n{}",
                    self.config_path_for_diagnostics(),
                    self.transcript()
                );
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    fn config_path_for_diagnostics(&self) -> &'static str {
        "$DISPOSABLE_HOME/.finch/config.toml"
    }

    fn wait_for_exit(&mut self, context: &str) -> std::process::ExitStatus {
        let deadline = Instant::now() + EXIT_DEADLINE;
        loop {
            match self.child.try_wait() {
                Ok(Some(status)) => {
                    self.drain_reader_after_exit(context);
                    return status;
                }
                Ok(None) => {}
                Err(error) => panic!("{context}: could not wait for finch setup: {error}"),
            }
            if Instant::now() >= deadline {
                let transcript = self.transcript();
                let _ = self.child.kill();
                let _ = self.child.wait();
                panic!(
                    "{context}: finch setup did not exit within {EXIT_DEADLINE:?}. \
                     Config path: {}. Terminal was:\n{transcript}",
                    self.config_path_for_diagnostics()
                );
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    fn drain_reader_after_exit(&mut self, context: &str) {
        if self.reader.is_none() {
            return;
        }
        self.reader_done
            .recv_timeout(Duration::from_secs(5))
            .unwrap_or_else(|error| {
                panic!(
                    "{context}: PTY output did not drain after finch setup exited: {error}. \
                     Terminal so far:\n{}",
                    self.transcript()
                )
            });
        self.reader
            .take()
            .expect("setup PTY reader exists before drain")
            .join()
            .expect("setup PTY reader must not panic");
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        if let Some(reader) = self.reader.take() {
            if self
                .reader_done
                .recv_timeout(Duration::from_secs(5))
                .is_ok()
            {
                let _ = reader.join();
            }
        }
    }
}

/// Reconstruct the current 120x40 terminal from the cursor-addressed output
/// Ratatui wrote. A transcript substring is not a frame: unchanged cells are
/// not emitted again, so checking raw bytes would miss text that remains on
/// screen from the preceding draw.
fn rendered_terminal_screen(raw: &[u8]) -> String {
    let raw = String::from_utf8_lossy(raw);
    let mut screen = vec![vec![' '; 120]; 40];
    let mut row = 0usize;
    let mut column = 0usize;
    let mut characters = raw.chars().peekable();
    while let Some(character) = characters.next() {
        if character == '\r' {
            column = 0;
            continue;
        }
        if character == '\n' {
            row = (row + 1).min(39);
            continue;
        }
        if character != '\u{1b}' {
            if !character.is_control() && row < 40 && column < 120 {
                screen[row][column] = character;
                column += 1;
            }
            continue;
        }
        match characters.peek() {
            Some('[') => {
                characters.next();
                let mut parameters = String::new();
                let mut final_byte = None;
                for byte in characters.by_ref() {
                    if ('\u{40}'..='\u{7e}').contains(&byte) {
                        final_byte = Some(byte);
                        break;
                    }
                    parameters.push(byte);
                }
                let values: Vec<usize> = parameters
                    .trim_start_matches('?')
                    .split(';')
                    .map(|value| value.parse().unwrap_or(0))
                    .collect();
                match final_byte {
                    Some('H' | 'f') => {
                        row = values.first().copied().unwrap_or(1).max(1).min(40) - 1;
                        column = values.get(1).copied().unwrap_or(1).max(1).min(120) - 1;
                    }
                    Some('A') => row = row.saturating_sub(values.first().copied().unwrap_or(1)),
                    Some('B') => row = (row + values.first().copied().unwrap_or(1)).min(39),
                    Some('C') => column = (column + values.first().copied().unwrap_or(1)).min(119),
                    Some('D') => {
                        column = column.saturating_sub(values.first().copied().unwrap_or(1))
                    }
                    Some('G') => column = values.first().copied().unwrap_or(1).max(1).min(120) - 1,
                    Some('J') if values.first() == Some(&2) => {
                        screen.iter_mut().for_each(|line| line.fill(' '));
                        row = 0;
                        column = 0;
                    }
                    Some('K') if values.first().copied().unwrap_or(0) == 2 => screen[row].fill(' '),
                    _ => {}
                }
            }
            Some(']') => {
                characters.next();
                while let Some(byte) = characters.next() {
                    if byte == '\u{7}' {
                        break;
                    }
                    if byte == '\u{1b}' && characters.peek() == Some(&'\\') {
                        characters.next();
                        break;
                    }
                }
            }
            Some(_) => {
                characters.next();
            }
            None => {}
        }
    }
    screen
        .into_iter()
        .map(|line| line.into_iter().collect::<String>())
        .collect::<Vec<_>>()
        .join("\n")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn open_models(session: &mut Session) {
    let checkpoint = session.checkpoint();
    session.send(b"\x1b[C");
    session.wait_for_after(
        checkpoint,
        "opening the production Models section",
        &["ChatGPT Boundary", "Enter: Edit provider", "A: Add"],
    );
}

#[derive(serde::Deserialize)]
struct SavedConfig {
    #[serde(default)]
    providers: Vec<ProviderEntry>,
    #[serde(default)]
    credentials: Vec<ProviderCredential>,
}

fn read_saved_config(path: &Path) -> SavedConfig {
    let encoded = std::fs::read_to_string(path)
        .unwrap_or_else(|error| panic!("read saved config {}: {error}", path.display()));
    toml::from_str(&encoded).unwrap_or_else(|error| {
        panic!(
            "the config written at {} must deserialize through the persisted provider schema: \
             {error}\nSaved file:\n{encoded}",
            path.display()
        )
    })
}

fn open_grok_add_dialog(session: &mut Session) {
    let checkpoint = session.checkpoint();
    session.send(b"a");
    let add_selector = session.wait_for_after(
        checkpoint,
        "opening the production add-provider selector",
        &["Add AI Provider", "Enter: Select", "Esc: Cancel"],
    );
    assert!(
        !add_selector.contains("Edit AI Provider"),
        "the add selector must not render the edit title. Frame was:\n{add_selector}"
    );

    let checkpoint = session.checkpoint();
    session.send(b"\x1b[B\r");
    let add_form = session.wait_for_after(
        checkpoint,
        "opening the production Grok add form",
        &["Add Cloud Provider", "Enter adds", "Esc back"],
    );
    assert!(
        !add_form.contains("Edit Provider") && !add_form.contains("Enter saves"),
        "the add form must render add-specific heading and instructions. Frame was:\n{add_form}"
    );
}

fn apply_grok_through_setup(
    fixture: &Fixture,
    verify_edit_renderer: bool,
) -> (SavedConfig, String) {
    let mut applied = Session::spawn(fixture);
    open_models(&mut applied);

    if verify_edit_renderer {
        let checkpoint = applied.checkpoint();
        applied.send(b"\r");
        let edit_form = applied.wait_for_after(
            checkpoint,
            "opening the production edit-provider form",
            &[
                "Edit AI Provider",
                "Edit Provider",
                "Enter saves",
                "cancels",
            ],
        );
        assert!(
            !edit_form.contains("Add AI Provider")
                && !edit_form.contains("Add Cloud Provider")
                && !edit_form.contains("Enter adds"),
            "the edit form must render edit-specific title, heading, and instructions. Frame was:\n{edit_form}"
        );
        let checkpoint = applied.checkpoint();
        applied.send(b"\x1b");
        applied.wait_for_after(
            checkpoint,
            "leaving the edit form without changing the provider",
            &["ChatGPT Boundary"],
        );
    }

    open_grok_add_dialog(&mut applied);
    // Grok opens focused on API Key. Fill the required model one row above,
    // return to the key row, then confirm the real add reducer.
    applied.send(b"\x1b[A");
    applied.send(b"grok-code-fast-1");
    applied.send(b"\x1b[B");
    applied.send(b"xai-test-boundary-new-provider-key");
    let checkpoint = applied.checkpoint();
    applied.send(b"\r");
    applied.wait_for_after(
        checkpoint,
        "confirming Grok through the production add reducer",
        &["ChatGPT Boundary", "grok"],
    );

    applied.send(&[0x13]);
    let status = applied.wait_for_exit("saving production setup");
    assert!(
        status.success(),
        "finch setup must save and exit successfully. Config path: {}. \
         Status: {status:?}. Terminal was:\n{}",
        fixture.config_path.display(),
        applied.transcript()
    );
    let transcript = applied.transcript();
    (read_saved_config(&fixture.config_path), transcript)
}

fn assert_exact_append(
    fixture: &Fixture,
    reloaded: &SavedConfig,
    transcript: &str,
    scenario: &str,
) {
    let diagnostics = format!(
        "scenario: {scenario}\nconfig path: {}\noriginal providers: {:#?}\n\
         reloaded providers: {:#?}\noriginal credentials: {:#?}\n\
         reloaded credentials: {:#?}\nterminal:\n{transcript}",
        fixture.config_path.display(),
        fixture.expected_providers,
        reloaded.providers,
        fixture.expected_credentials,
        reloaded.credentials,
    );
    assert_eq!(
        reloaded.providers.len(),
        fixture.expected_providers.len() + 1,
        "production setup must append exactly one provider. {diagnostics}"
    );
    assert_eq!(
        &reloaded.providers[..fixture.expected_providers.len()],
        fixture.expected_providers.as_slice(),
        "production setup must preserve every existing provider in its original order. \
         {diagnostics}"
    );
    assert!(
        matches!(
            reloaded.providers.last(),
            Some(ProviderEntry::Grok { api_key, model, name, .. })
                if api_key == "xai-test-boundary-new-provider-key"
                    && model.as_deref() == Some("grok-code-fast-1")
                    && name.as_deref() == Some("grok")
        ),
        "the one appended provider must be the Grok profile entered through the real form. \
         {diagnostics}"
    );
    assert_eq!(
        reloaded.credentials.as_slice(),
        fixture.expected_credentials.as_slice(),
        "production setup must preserve the named credential backing the subscription profile. \
         {diagnostics}"
    );
}

#[test]
fn test_setup_provider_add_and_cancel_cross_render_apply_save_reload_boundary() {
    if std::env::var_os("FINCH_BRAIN_TEST_ISOLATED").is_none() {
        eprintln!("skipped: this real-TUI regression requires scripts/test_brains.sh");
        return;
    }
    finch::brain::isolated_test_proof()
        .expect("scripts/test_brains.sh must provide authenticated supervisor authority");
    // A sole configured subscription is the exact hostile shape that the old
    // provider-count heuristic mistook for an unconfigured placeholder.
    let fixture = Fixture::sole_subscription();
    let before_cancel = Snapshot::read(&fixture.config_path);

    // First drive the reported defensive action: cancel after opening an add.
    let mut cancelled = Session::spawn(&fixture);
    open_models(&mut cancelled);
    open_grok_add_dialog(&mut cancelled);
    cancelled.send(b"partial-test-key");
    let checkpoint = cancelled.checkpoint();
    cancelled.send(&[0x03]);
    cancelled.wait_for_after(
        checkpoint,
        "requesting cancellation from inside the add form",
        &["Cancel setup?", "Discard all setup changes and cancel?"],
    );
    cancelled.send(b"y");
    let cancel_status = cancelled.wait_for_exit("cancelling production setup");
    let cancel_transcript = cancelled.transcript();
    assert!(
        !cancel_status.success() && cancel_transcript.contains("Setup cancelled"),
        "cancelling production setup must take the explicit cancellation outcome, not merely \
         terminate before writing. Config path: {}. Exit status: {cancel_status:?}. \
         Terminal was:\n{cancel_transcript}",
        fixture.config_path.display()
    );
    let after_cancel = Snapshot::read(&fixture.config_path);
    assert_eq!(
        after_cancel.bytes,
        before_cancel.bytes,
        "cancelling production setup must leave {} byte-for-byte unchanged. \
         Exit status: {cancel_status:?}. Terminal was:\n{}",
        fixture.config_path.display(),
        cancelled.transcript()
    );
    assert_eq!(
        after_cancel.modified,
        before_cancel.modified,
        "cancelling production setup must not rewrite {} with identical bytes. \
         Before mtime: {:?}; after mtime: {:?}; exit status: {cancel_status:?}. Terminal was:\n{}",
        fixture.config_path.display(),
        before_cancel.modified,
        after_cancel.modified,
        cancelled.transcript()
    );
    drop(cancelled);

    // The second invocation proves both rendered modes and the exact
    // single-existing-provider regression at the real commit boundary.
    let (reloaded, transcript) = apply_grok_through_setup(&fixture, true);
    assert_exact_append(
        &fixture,
        &reloaded,
        &transcript,
        "sole configured subscription",
    );
    assert!(
        matches!(
            fixture
                .expected_credentials
                .first()
                .map(|credential| &credential.lifecycle),
            Some(CredentialLifecycle::Active {
                refreshable: true,
                ..
            })
        ),
        "the fixture must use a reusable credential so this boundary cannot contact a live \
         provider. Config path: {}; terminal was:\n{transcript}",
        fixture.config_path.display()
    );

    // Exercise the same production apply/save boundary with the complete
    // mixture called out by the issue: subscription, key-based remote,
    // keyless providers, and local profile must all survive the append.
    let broad_fixture = Fixture::new();
    let (broad_reloaded, broad_transcript) = apply_grok_through_setup(&broad_fixture, false);
    assert_exact_append(
        &broad_fixture,
        &broad_reloaded,
        &broad_transcript,
        "mixed subscription, remote, keyless, and local providers",
    );

    // A new process must load the just-written file through
    // `load_persisted_config` before it can render these rows. This is the
    // production reload boundary; parsing above supplies the structural
    // assertions without mutating this test process's HOME.
    let mut reopened = Session::spawn(&broad_fixture);
    open_models(&mut reopened);
    reopened.wait_for_after(
        0,
        "reopening the production wizard from the config it just saved",
        &[
            "ChatGPT Boundary",
            "OpenAI Boundary",
            "Ollama Boundary",
            "Finch Boundary",
            "Local Qwen 2.5 Small",
            "grok",
        ],
    );
    let checkpoint = reopened.checkpoint();
    reopened.send(&[0x03]);
    reopened.wait_for_after(
        checkpoint,
        "cancelling the production reload session",
        &["Cancel setup?"],
    );
    reopened.send(b"y");
    reopened.wait_for_exit("closing the production reload session");
}
