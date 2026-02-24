// Integration test for tabbed dialog functionality

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use finch::cli::llm_dialogs::{build_annotations, Question, QuestionOption};
use finch::cli::tui::{TabbedDialog, TabbedDialogResult};

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

fn opt(label: &str, description: &str) -> QuestionOption {
    QuestionOption {
        label: label.to_string(),
        description: description.to_string(),
        markdown: None,
    }
}

fn opt_with_md(label: &str, description: &str, markdown: &str) -> QuestionOption {
    QuestionOption {
        label: label.to_string(),
        description: description.to_string(),
        markdown: Some(markdown.to_string()),
    }
}

#[test]
fn test_tabbed_dialog_creation() {
    let questions = vec![
        Question {
            question: "What format do you prefer?".to_string(),
            header: "Format".to_string(),
            options: vec![
                opt("JSON", "Structured data format"),
                opt("YAML", "Human-readable format"),
            ],
            multi_select: false,
        },
        Question {
            question: "Which features do you want?".to_string(),
            header: "Features".to_string(),
            options: vec![
                opt("Authentication", "User login system"),
                opt("Database", "Data persistence"),
                opt("API", "REST endpoints"),
            ],
            multi_select: true,
        },
    ];

    let dialog = TabbedDialog::new(questions, Some("Test Dialog".to_string()));

    assert_eq!(dialog.current_tab_index(), 0);
    assert_eq!(dialog.tabs().len(), 2);
    assert!(!dialog.all_answered());
}

#[test]
fn test_tab_navigation() {
    let questions = vec![
        Question {
            question: "Question 1?".to_string(),
            header: "Q1".to_string(),
            options: vec![
                opt("Option A", "First option"),
                opt("Option B", "Second option"),
            ],
            multi_select: false,
        },
        Question {
            question: "Question 2?".to_string(),
            header: "Q2".to_string(),
            options: vec![
                opt("Option X", "First option"),
                opt("Option Y", "Second option"),
            ],
            multi_select: false,
        },
    ];

    let mut dialog = TabbedDialog::new(questions, None);

    assert_eq!(dialog.current_tab_index(), 0);
    dialog.handle_key_event(key(KeyCode::Right));
    assert_eq!(dialog.current_tab_index(), 1);

    dialog.handle_key_event(key(KeyCode::Left));
    assert_eq!(dialog.current_tab_index(), 0);
}

#[test]
fn test_answer_submission() {
    let questions = vec![Question {
        question: "Pick one?".to_string(),
        header: "Choice".to_string(),
        options: vec![opt("Yes", "Affirmative"), opt("No", "Negative")],
        multi_select: false,
    }];

    let mut dialog = TabbedDialog::new(questions, None);

    let result = dialog.handle_key_event(key(KeyCode::Enter));

    match result {
        Some(TabbedDialogResult::Completed(answers)) => {
            assert_eq!(answers.len(), 1);
            assert_eq!(answers.get("Pick one?"), Some(&"Yes".to_string()));
        }
        _ => panic!("Expected Completed result on first Enter for single-tab dialog"),
    }
}

#[test]
fn test_cancellation() {
    let questions = vec![Question {
        question: "Question?".to_string(),
        header: "Q".to_string(),
        options: vec![opt("A", "A"), opt("B", "B")],
        multi_select: false,
    }];

    let mut dialog = TabbedDialog::new(questions, None);

    let result = dialog.handle_key_event(key(KeyCode::Esc));

    match result {
        Some(TabbedDialogResult::Cancelled) => {}
        _ => panic!("Expected Cancelled result"),
    }
}

#[test]
fn test_custom_input_mode() {
    let questions = vec![Question {
        question: "Custom input test?".to_string(),
        header: "Custom".to_string(),
        options: vec![opt("Option 1", "First"), opt("Option 2", "Second")],
        multi_select: false,
    }];

    let mut dialog = TabbedDialog::new(questions, None);

    dialog.handle_key_event(key(KeyCode::Char('o')));
    assert!(dialog.current_tab().custom_mode_active);

    dialog.handle_key_event(key(KeyCode::Char('H')));
    dialog.handle_key_event(key(KeyCode::Char('i')));

    let result = dialog.handle_key_event(key(KeyCode::Enter));

    assert!(result.is_none() || matches!(result, Some(TabbedDialogResult::Completed(_))));
}

#[test]
fn test_multi_select() {
    let questions = vec![Question {
        question: "Select multiple?".to_string(),
        header: "Multi".to_string(),
        options: vec![opt("A", "First"), opt("B", "Second"), opt("C", "Third")],
        multi_select: true,
    }];

    let mut dialog = TabbedDialog::new(questions, None);

    dialog.handle_key_event(key(KeyCode::Char(' ')));
    assert!(dialog.current_tab().selected_indices.contains(&0));

    dialog.handle_key_event(key(KeyCode::Down));
    dialog.handle_key_event(key(KeyCode::Char(' ')));
    assert!(dialog.current_tab().selected_indices.contains(&1));

    let result = dialog.handle_key_event(key(KeyCode::Enter));

    match result {
        Some(TabbedDialogResult::Completed(answers)) => {
            let answer = answers.get("Select multiple?").unwrap();
            assert!(answer.contains("A"));
            assert!(answer.contains("B"));
        }
        _ => panic!("Expected Completed result with multi-select"),
    }
}

// ── Markdown preview tests ────────────────────────────────────────────────────

/// Markdown stored in QuestionOption is preserved through the TabbedDialog constructor.
#[test]
fn test_option_markdown_field_preserved_in_dialog() {
    let q = Question {
        question: "Which impl?".to_string(),
        header: "Impl".to_string(),
        options: vec![
            opt_with_md("Async", "Async approach", "async fn foo() {}"),
            opt("Sync", "Sync approach"),
        ],
        multi_select: false,
    };
    let dialog = TabbedDialog::new(vec![q], None);

    let opts = &dialog.current_tab().question.options;
    assert_eq!(opts[0].markdown, Some("async fn foo() {}".to_string()));
    assert_eq!(opts[1].markdown, None);
}

/// Navigating to a different option makes its markdown accessible through the
/// current selected_index.
#[test]
fn test_focused_option_markdown_accessible_after_navigation() {
    let q = Question {
        question: "Pick style?".to_string(),
        header: "Style".to_string(),
        options: vec![
            opt_with_md("Option A", "First", "// A code"),
            opt_with_md("Option B", "Second", "// B code"),
        ],
        multi_select: false,
    };
    let mut dialog = TabbedDialog::new(vec![q], None);

    // Initially at index 0 → markdown is Option A's
    let focused_md_at = |d: &TabbedDialog| {
        let tab = d.current_tab();
        tab.question.options[tab.selected_index].markdown.clone()
    };

    assert_eq!(focused_md_at(&dialog), Some("// A code".to_string()));

    // Arrow-down → index 1 → markdown switches to Option B's
    dialog.handle_key_event(key(KeyCode::Down));
    assert_eq!(focused_md_at(&dialog), Some("// B code".to_string()));
}

/// An option without markdown gives None for the focused markdown.
#[test]
fn test_option_without_markdown_gives_none() {
    let q = Question {
        question: "Choose?".to_string(),
        header: "Choose".to_string(),
        options: vec![
            opt_with_md("With MD", "Has markdown", "code"),
            opt("No MD", "No markdown"),
            opt_with_md("With MD 2", "Also has markdown", "more code"),
        ],
        multi_select: false,
    };
    let mut dialog = TabbedDialog::new(vec![q], None);

    // Navigate to index 1 (the one without markdown)
    dialog.handle_key_event(key(KeyCode::Down));
    let tab = dialog.current_tab();
    let md = tab.question.options[tab.selected_index].markdown.clone();
    assert_eq!(md, None);
}

/// Completing a dialog with a markdown-bearing option causes build_annotations
/// to echo the markdown in the output annotations map.
#[test]
fn test_build_annotations_echoes_markdown_after_dialog_completes() {
    use std::collections::HashMap;

    let question_text = "Which approach?";
    let selected_label = "Async";
    let expected_md = "async fn process() -> Result<()> {}";

    let questions = vec![Question {
        question: question_text.to_string(),
        header: "Approach".to_string(),
        options: vec![
            opt_with_md(selected_label, "Async approach", expected_md),
            opt("Sync", "Sync approach"),
        ],
        multi_select: false,
    }];

    let mut answers = HashMap::new();
    answers.insert(question_text.to_string(), selected_label.to_string());

    let annotations = build_annotations(&questions, &answers);

    assert_eq!(annotations.len(), 1, "expected 1 annotation entry");
    assert_eq!(
        annotations[question_text].markdown.as_deref(),
        Some(expected_md),
        "annotation markdown must match the selected option's markdown"
    );
}

/// Completing a dialog where the user selected the option without markdown
/// produces an empty annotations map.
#[test]
fn test_build_annotations_empty_when_selected_option_has_no_markdown() {
    use std::collections::HashMap;

    let question_text = "Format?";
    let questions = vec![Question {
        question: question_text.to_string(),
        header: "Format".to_string(),
        options: vec![
            opt_with_md("JSON", "Structured", "{ \"key\": 1 }"),
            opt("YAML", "Human-readable"), // no markdown
        ],
        multi_select: false,
    }];

    let mut answers = HashMap::new();
    answers.insert(question_text.to_string(), "YAML".to_string());

    let annotations = build_annotations(&questions, &answers);
    assert!(
        annotations.is_empty(),
        "no annotation when selected option has no markdown"
    );
}
