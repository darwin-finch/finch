// Integration test for tabbed dialog functionality

use finch::cli::llm_dialogs::{Question, QuestionOption};
use finch::cli::tui::{TabbedDialog, TabbedDialogResult};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

#[test]
fn test_tabbed_dialog_creation() {
    // Create sample questions
    let questions = vec![
        Question {
            question: "What format do you prefer?".to_string(),
            header: "Format".to_string(),
            options: vec![
                QuestionOption {
                    label: "JSON".to_string(),
                    description: "Structured data format".to_string(),
                },
                QuestionOption {
                    label: "YAML".to_string(),
                    description: "Human-readable format".to_string(),
                },
            ],
            multi_select: false,
        },
        Question {
            question: "Which features do you want?".to_string(),
            header: "Features".to_string(),
            options: vec![
                QuestionOption {
                    label: "Authentication".to_string(),
                    description: "User login system".to_string(),
                },
                QuestionOption {
                    label: "Database".to_string(),
                    description: "Data persistence".to_string(),
                },
                QuestionOption {
                    label: "API".to_string(),
                    description: "REST endpoints".to_string(),
                },
            ],
            multi_select: true,
        },
    ];

    // Create tabbed dialog
    let dialog = TabbedDialog::new(questions, Some("Test Dialog".to_string()));

    // Verify initial state
    assert_eq!(dialog.current_tab_index(), 0);
    assert_eq!(dialog.tabs().len(), 2);
    assert!(!dialog.all_answered());

    println!("✓ Tabbed dialog created successfully");
}

#[test]
fn test_tab_navigation() {
    let questions = vec![
        Question {
            question: "Question 1?".to_string(),
            header: "Q1".to_string(),
            options: vec![
                QuestionOption {
                    label: "Option A".to_string(),
                    description: "First option".to_string(),
                },
                QuestionOption {
                    label: "Option B".to_string(),
                    description: "Second option".to_string(),
                },
            ],
            multi_select: false,
        },
        Question {
            question: "Question 2?".to_string(),
            header: "Q2".to_string(),
            options: vec![
                QuestionOption {
                    label: "Option X".to_string(),
                    description: "First option".to_string(),
                },
                QuestionOption {
                    label: "Option Y".to_string(),
                    description: "Second option".to_string(),
                },
            ],
            multi_select: false,
        },
    ];

    let mut dialog = TabbedDialog::new(questions, None);

    // Test right arrow (move to next tab)
    assert_eq!(dialog.current_tab_index(), 0);
    dialog.handle_key_event(KeyEvent::new(KeyCode::Right, KeyModifiers::NONE));
    assert_eq!(dialog.current_tab_index(), 1);

    // Test left arrow (move to previous tab)
    dialog.handle_key_event(KeyEvent::new(KeyCode::Left, KeyModifiers::NONE));
    assert_eq!(dialog.current_tab_index(), 0);

    println!("✓ Tab navigation works correctly");
}

#[test]
fn test_answer_submission() {
    let questions = vec![
        Question {
            question: "Pick one?".to_string(),
            header: "Choice".to_string(),
            options: vec![
                QuestionOption {
                    label: "Yes".to_string(),
                    description: "Affirmative".to_string(),
                },
                QuestionOption {
                    label: "No".to_string(),
                    description: "Negative".to_string(),
                },
            ],
            multi_select: false,
        },
    ];

    let mut dialog = TabbedDialog::new(questions, None);

    // Select an option (already at index 0 by default)
    // For single-tab dialog with pre-selected option, Enter immediately completes
    let result = dialog.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

    // Should return completed result immediately (single tab + option already selected)
    match result {
        Some(TabbedDialogResult::Completed(answers)) => {
            assert_eq!(answers.len(), 1);
            assert_eq!(answers.get("Pick one?"), Some(&"Yes".to_string()));
            println!("✓ Answer submission works correctly");
        }
        _ => panic!("Expected Completed result on first Enter for single-tab dialog"),
    }
}

#[test]
fn test_cancellation() {
    let questions = vec![
        Question {
            question: "Question?".to_string(),
            header: "Q".to_string(),
            options: vec![
                QuestionOption {
                    label: "A".to_string(),
                    description: "A".to_string(),
                },
                QuestionOption {
                    label: "B".to_string(),
                    description: "B".to_string(),
                },
            ],
            multi_select: false,
        },
    ];

    let mut dialog = TabbedDialog::new(questions, None);

    // Press Esc to cancel
    let result = dialog.handle_key_event(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));

    match result {
        Some(TabbedDialogResult::Cancelled) => {
            println!("✓ Cancellation works correctly");
        }
        _ => panic!("Expected Cancelled result"),
    }
}

#[test]
fn test_custom_input_mode() {
    let questions = vec![
        Question {
            question: "Custom input test?".to_string(),
            header: "Custom".to_string(),
            options: vec![
                QuestionOption {
                    label: "Option 1".to_string(),
                    description: "First".to_string(),
                },
                QuestionOption {
                    label: "Option 2".to_string(),
                    description: "Second".to_string(),
                },
            ],
            multi_select: false,
        },
    ];

    let mut dialog = TabbedDialog::new(questions, None);

    // Press 'o' to enter custom mode
    dialog.handle_key_event(KeyEvent::new(KeyCode::Char('o'), KeyModifiers::NONE));
    assert!(dialog.current_tab().custom_mode_active);

    // Type some text
    dialog.handle_key_event(KeyEvent::new(KeyCode::Char('H'), KeyModifiers::NONE));
    dialog.handle_key_event(KeyEvent::new(KeyCode::Char('i'), KeyModifiers::NONE));

    // Press Enter to submit custom text
    let result = dialog.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

    // Should mark as answered and move forward
    assert!(result.is_none() || matches!(result, Some(TabbedDialogResult::Completed(_))));

    println!("✓ Custom input mode works correctly");
}

#[test]
fn test_multi_select() {
    let questions = vec![
        Question {
            question: "Select multiple?".to_string(),
            header: "Multi".to_string(),
            options: vec![
                QuestionOption {
                    label: "A".to_string(),
                    description: "First".to_string(),
                },
                QuestionOption {
                    label: "B".to_string(),
                    description: "Second".to_string(),
                },
                QuestionOption {
                    label: "C".to_string(),
                    description: "Third".to_string(),
                },
            ],
            multi_select: true,
        },
    ];

    let mut dialog = TabbedDialog::new(questions, None);

    // Press Space to toggle selection (index 0)
    dialog.handle_key_event(KeyEvent::new(KeyCode::Char(' '), KeyModifiers::NONE));
    assert!(dialog.current_tab().selected_indices.contains(&0));

    // Move down and toggle (index 1)
    dialog.handle_key_event(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
    dialog.handle_key_event(KeyEvent::new(KeyCode::Char(' '), KeyModifiers::NONE));
    assert!(dialog.current_tab().selected_indices.contains(&1));

    // Press Enter to answer
    let result = dialog.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

    // Should complete with multiple selections
    match result {
        Some(TabbedDialogResult::Completed(answers)) => {
            let answer = answers.get("Select multiple?").unwrap();
            assert!(answer.contains("A"));
            assert!(answer.contains("B"));
            println!("✓ Multi-select works correctly: {}", answer);
        }
        _ => panic!("Expected Completed result with multi-select"),
    }
}
