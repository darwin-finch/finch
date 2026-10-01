//! Production-boundary proof for the DOM manifest wire contract (#1141 part
//! 2): the say-turn card's manifest JSON matches a committed snapshot, the
//! manifest round-trips serde, and the committed TS types match what the
//! structs generate.

use finch_tui::{component_ui_manifest, UiManifest, MANIFEST_VERSION};
use finch_ui_model::{
    ComponentView, MessageId, OutputVm, ProgramSourceVm, SayTurnStatus, SayTurnView,
    WorkUnitViewModel,
};

/// The fixed identity the golden pins. Never change this value without
/// regenerating the committed snapshot and the schema doc in the same commit
/// — the wire contract moves with it.
const GOLDEN_MESSAGE_ID: &str = "7c9e6679-7425-40de-944b-e07fc1f90ae7";

fn say_view(show_program: bool) -> SayTurnView {
    SayTurnView {
        message_id: MessageId::from_uuid(uuid::Uuid::parse_str(GOLDEN_MESSAGE_ID).unwrap()),
        vm: WorkUnitViewModel {
            status: SayTurnStatus::Completed,
            program: ProgramSourceVm {
                language: "Co-Forth".to_string(),
                lines: vec![r#"(say "Hi, Shammah!")"#.to_string()],
            },
            output: Some(OutputVm {
                lines: vec!["Hi, Shammah! What would you like to work on?".to_string()],
            }),
            show_program,
        },
        reasoning: None,
        elapsed: std::time::Duration::from_millis(2350),
    }
}

fn golden_json() -> String {
    let manifest = component_ui_manifest(&ComponentView::Say(say_view(false)));
    serde_json::to_string_pretty(&manifest).expect("the manifest serializes")
}

/// INVARIANT (#1141 part 2): the say card's manifest JSON is the committed
/// wire contract — element type names, ids, and prop keys are pinned, so a
/// consumer built against the snapshot never breaks silently. Regenerate with
/// `FINCH_UPDATE_UI_MANIFEST_GOLDEN=1` and commit the diff together with the
/// schema doc bump.
#[test]
fn test_say_card_manifest_json_matches_the_committed_snapshot() {
    let actual = golden_json();
    let fixture_path = format!(
        "{}/tests/fixtures/ui_manifest_say_card.json",
        env!("CARGO_MANIFEST_DIR")
    );
    if std::env::var("FINCH_UPDATE_UI_MANIFEST_GOLDEN").is_ok() {
        std::fs::write(&fixture_path, &actual)
            .expect("writing the golden fixture requires a writable source tree");
        return;
    }
    let expected = std::fs::read_to_string(&fixture_path).unwrap_or_else(|error| {
        panic!(
            "cannot read the committed golden manifest at {fixture_path}: {error}; \
             regenerate with FINCH_UPDATE_UI_MANIFEST_GOLDEN=1"
        )
    });
    assert_eq!(
        actual, expected,
        "the say card's manifest JSON must match the committed snapshot \
         (the wire contract is pinned); regenerate only with intent"
    );
}

/// The manifest round-trips serde: a consumer can parse exactly what the
/// engine emits, and the parsed value equals the produced one.
#[test]
fn test_manifest_round_trips_serde() {
    let manifest = component_ui_manifest(&ComponentView::Say(say_view(false)));
    let serialized = serde_json::to_string(&manifest).expect("serialize");
    let parsed: UiManifest = serde_json::from_str(&serialized).expect("parse");
    assert_eq!(
        parsed, manifest,
        "the manifest must round-trip; serialized={serialized}"
    );
    assert_eq!(
        parsed.manifest_version, MANIFEST_VERSION,
        "the envelope carries the contract version"
    );
}

/// The card id comes from the message uuid. Only the labelled program control
/// carries semantic path `#1`; answer and source content are not action
/// targets, matching terminal routing.
#[test]
fn test_say_card_labelled_control_is_the_only_action_target() {
    let card = finch_tui::say_card_manifest(&say_view(false));
    assert_eq!(
        (card.element_type.as_str(), card.id.as_str()),
        ("SayTurnCard", GOLDEN_MESSAGE_ID),
        "the card element type and message-uuid id are pinned; got {card:?}"
    );
    let output = &card.children[0];
    let control = &card.children[1];
    assert_eq!(
        (output.element_type.as_str(), output.id.as_str()),
        ("Output", ""),
        "the answer is content, not an action target; got {output:?}"
    );
    assert_eq!(
        (control.element_type.as_str(), control.id.as_str()),
        (
            "ProgramDisclosureControl",
            format!("{GOLDEN_MESSAGE_ID}#1").as_str()
        ),
        "the explicit labelled control alone carries semantic path 1; got {control:?}"
    );
    assert_eq!(
        output.props["lines"],
        serde_json::json!(["Hi, Shammah! What would you like to work on?"]),
        "the answer child carries the VM output; output={output:?}"
    );
    assert_eq!(
        (&control.props["label"], &control.props["expanded"]),
        (
            &serde_json::json!("Show program"),
            &serde_json::json!(false)
        ),
        "the closed control exposes matching visible and structured state; control={control:?}"
    );
    assert_eq!(
        card.children.len(),
        2,
        "closed completed cards do not expose source content; card={card:?}"
    );
}

#[test]
fn test_say_card_open_manifest_preserves_answer_and_adds_exact_source_beneath_control() {
    let card = finch_tui::say_card_manifest(&say_view(true));
    assert_eq!(
        card.children
            .iter()
            .map(|child| child.element_type.as_str())
            .collect::<Vec<_>>(),
        vec!["Output", "ProgramDisclosureControl", "ProgramSource"],
        "the open DOM order mirrors the terminal: answer, control, exact source"
    );
    let output = &card.children[0];
    let control = &card.children[1];
    let program = &card.children[2];
    assert_eq!(
        (&control.props["label"], &control.props["expanded"]),
        (&serde_json::json!("Hide program"), &serde_json::json!(true)),
        "the open control exposes its inverse label and expanded state; control={control:?}"
    );
    assert_eq!(
        program.props["lines"],
        serde_json::json!([r#"(say "Hi, Shammah!")"#]),
        "the exact VM source is additive beneath the control; program={program:?}"
    );
    assert!(
        output.id.is_empty() && program.id.is_empty(),
        "answer and source content remain non-actionable; output={output:?} program={program:?}"
    );
}
/// The committed TS types exist and carry the wire types (they regenerate on
/// every `cargo test -p finch-tui --lib`; a deleted file or a rename shows up
/// here instead of silently breaking the external repo).
#[test]
fn test_committed_ts_types_carry_the_wire_types() {
    let dir = format!("{}/bindings/dom/ui_manifest", env!("CARGO_MANIFEST_DIR"));
    for (file, needle) in [
        ("UiManifest.ts", "export type UiManifest"),
        (
            "DynamicUiNode.ts",
            "export type DynamicUiNode = { element_type: string, id: string",
        ),
        ("ManifestSpan.ts", "export type ManifestSpan"),
        ("ManifestColor.ts", "export type ManifestColor"),
    ] {
        let path = format!("{dir}/{file}", dir = dir.clone());
        let source = std::fs::read_to_string(&path).unwrap_or_else(|error| {
            panic!(
                "the committed TS artifact {path} is missing ({error}); run \
                 `cargo test -p finch-tui --lib` to regenerate and commit it"
            )
        });
        assert!(
            source.contains(needle),
            "the TS artifact {path} must carry the wire type; got:\n{source}"
        );
    }
    let barrel = std::fs::read_to_string(format!(
        "{}/bindings/dom/index.ts",
        env!("CARGO_MANIFEST_DIR")
    ))
    .expect("the barrel file is committed");
    assert!(
        barrel.contains("FINCH_UI_MANIFEST_VERSION = 2"),
        "the barrel pins the contract version; got:\n{barrel}"
    );
}
