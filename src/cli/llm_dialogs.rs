// LLM-Prompted User Dialogs
//
// Implements Claude Code-style AskUserQuestion functionality, allowing the LLM
// to prompt the user with structured questions during execution.
//
// Architecture:
// - LLM calls AskUserQuestion tool when clarification needed
// - Tool input contains questions with options
// - TUI displays dialog using DialogWidget
// - Collected answers returned to LLM
// - Conversation continues with user's choices

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// A single option in a question
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QuestionOption {
    /// Display label for this option (e.g., "Summary", "Detailed")
    pub label: String,

    /// Description explaining what this option means
    pub description: String,
}

/// A single question to ask the user
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Question {
    /// The question text (e.g., "How should I format the output?")
    pub question: String,

    /// Short header/label for display (e.g., "Format", max 12 chars)
    pub header: String,

    /// Available options (2-4 options required)
    pub options: Vec<QuestionOption>,

    /// Whether user can select multiple options (default: false)
    #[serde(default)]
    pub multi_select: bool,
}

/// Input to AskUserQuestion tool
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AskUserQuestionInput {
    /// Questions to ask (1-4 questions)
    pub questions: Vec<Question>,
}

/// Output from AskUserQuestion tool
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AskUserQuestionOutput {
    /// The questions that were asked (echoed back)
    pub questions: Vec<Question>,

    /// Answers provided by user (question text → selected label(s))
    /// For single-select: value is the label string
    /// For multi-select: value is comma-separated labels
    pub answers: HashMap<String, String>,
}

/// Validation errors for AskUserQuestion input
#[derive(Debug, Clone)]
pub enum ValidationError {
    /// No questions provided
    NoQuestions,

    /// Too many questions (max 4)
    TooManyQuestions(usize),

    /// Question missing required fields
    InvalidQuestion(String),

    /// Too few options (min 2)
    TooFewOptions(String),

    /// Too many options (max 4)
    TooManyOptions(String),

    /// Header too long (max 12 chars)
    HeaderTooLong(String),
}

impl std::fmt::Display for ValidationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoQuestions => write!(f, "At least one question is required"),
            Self::TooManyQuestions(n) => write!(f, "Too many questions: {} (max 4)", n),
            Self::InvalidQuestion(q) => write!(f, "Invalid question: {}", q),
            Self::TooFewOptions(q) => write!(f, "Question '{}' has too few options (min 2)", q),
            Self::TooManyOptions(q) => write!(f, "Question '{}' has too many options (max 4)", q),
            Self::HeaderTooLong(h) => write!(f, "Header '{}' too long (max 12 chars)", h),
        }
    }
}

impl std::error::Error for ValidationError {}

/// Validate AskUserQuestion input
pub fn validate_input(input: &AskUserQuestionInput) -> Result<(), ValidationError> {
    // Check question count
    if input.questions.is_empty() {
        return Err(ValidationError::NoQuestions);
    }

    if input.questions.len() > 4 {
        return Err(ValidationError::TooManyQuestions(input.questions.len()));
    }

    // Validate each question
    for question in &input.questions {
        // Check required fields
        if question.question.trim().is_empty() {
            return Err(ValidationError::InvalidQuestion(
                "Empty question text".to_string(),
            ));
        }

        if question.header.trim().is_empty() {
            return Err(ValidationError::InvalidQuestion(format!(
                "Question '{}' has empty header",
                question.question
            )));
        }

        // Check header length
        if question.header.len() > 12 {
            return Err(ValidationError::HeaderTooLong(question.header.clone()));
        }

        // Check option count
        if question.options.len() < 2 {
            return Err(ValidationError::TooFewOptions(question.question.clone()));
        }

        if question.options.len() > 4 {
            return Err(ValidationError::TooManyOptions(question.question.clone()));
        }

        // Check option content
        for option in &question.options {
            if option.label.trim().is_empty() {
                return Err(ValidationError::InvalidQuestion(format!(
                    "Question '{}' has option with empty label",
                    question.question
                )));
            }
        }
    }

    Ok(())
}

/// Convert Question to Dialog format for display
pub fn question_to_dialog(question: &Question) -> crate::cli::tui::Dialog {
    use crate::cli::tui::{Dialog, DialogOption};

    // Convert our QuestionOptions to DialogOptions
    let dialog_options: Vec<DialogOption> = question
        .options
        .iter()
        .map(|opt| DialogOption::with_description(opt.label.clone(), opt.description.clone()))
        .collect();

    if question.multi_select {
        // Multi-select dialog with custom text option
        Dialog::multiselect_with_custom(question.header.clone(), dialog_options)
            .with_help(&question.question)
    } else {
        // Single-select dialog with custom text option
        Dialog::select_with_custom(question.header.clone(), dialog_options)
            .with_help(&question.question)
    }
}

/// Extract user's answer from dialog result
/// Returns the selected label(s) as a comma-separated string
pub fn extract_answer(
    question: &Question,
    dialog_result: &crate::cli::tui::DialogResult,
) -> Option<String> {
    use crate::cli::tui::DialogResult;

    match dialog_result {
        DialogResult::Selected(idx) => {
            // Single-select: get selected index
            if *idx < question.options.len() {
                Some(question.options[*idx].label.clone())
            } else {
                None
            }
        }
        DialogResult::MultiSelected(indices) => {
            // Multi-select: join all selected labels
            let labels: Vec<String> = indices
                .iter()
                .filter_map(|&idx| {
                    if idx < question.options.len() {
                        Some(question.options[idx].label.clone())
                    } else {
                        None
                    }
                })
                .collect();

            if labels.is_empty() {
                None
            } else {
                Some(labels.join(", "))
            }
        }
        DialogResult::CustomText(text) => {
            // User provided custom text via 'o' key
            Some(text.clone())
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_validate_input_valid() {
        let input = AskUserQuestionInput {
            questions: vec![Question {
                question: "How should I proceed?".to_string(),
                header: "Action".to_string(),
                options: vec![
                    QuestionOption {
                        label: "Option A".to_string(),
                        description: "Description A".to_string(),
                    },
                    QuestionOption {
                        label: "Option B".to_string(),
                        description: "Description B".to_string(),
                    },
                ],
                multi_select: false,
            }],
        };

        assert!(validate_input(&input).is_ok());
    }

    #[test]
    fn test_validate_input_no_questions() {
        let input = AskUserQuestionInput { questions: vec![] };

        assert!(matches!(
            validate_input(&input),
            Err(ValidationError::NoQuestions)
        ));
    }

    #[test]
    fn test_validate_input_too_many_questions() {
        let input = AskUserQuestionInput {
            questions: vec![
                Question {
                    question: "Q1".to_string(),
                    header: "H1".to_string(),
                    options: vec![
                        QuestionOption {
                            label: "A".to_string(),
                            description: "D".to_string(),
                        },
                        QuestionOption {
                            label: "B".to_string(),
                            description: "D".to_string(),
                        },
                    ],
                    multi_select: false,
                },
                Question {
                    question: "Q2".to_string(),
                    header: "H2".to_string(),
                    options: vec![
                        QuestionOption {
                            label: "A".to_string(),
                            description: "D".to_string(),
                        },
                        QuestionOption {
                            label: "B".to_string(),
                            description: "D".to_string(),
                        },
                    ],
                    multi_select: false,
                },
                Question {
                    question: "Q3".to_string(),
                    header: "H3".to_string(),
                    options: vec![
                        QuestionOption {
                            label: "A".to_string(),
                            description: "D".to_string(),
                        },
                        QuestionOption {
                            label: "B".to_string(),
                            description: "D".to_string(),
                        },
                    ],
                    multi_select: false,
                },
                Question {
                    question: "Q4".to_string(),
                    header: "H4".to_string(),
                    options: vec![
                        QuestionOption {
                            label: "A".to_string(),
                            description: "D".to_string(),
                        },
                        QuestionOption {
                            label: "B".to_string(),
                            description: "D".to_string(),
                        },
                    ],
                    multi_select: false,
                },
                Question {
                    question: "Q5".to_string(),
                    header: "H5".to_string(),
                    options: vec![
                        QuestionOption {
                            label: "A".to_string(),
                            description: "D".to_string(),
                        },
                        QuestionOption {
                            label: "B".to_string(),
                            description: "D".to_string(),
                        },
                    ],
                    multi_select: false,
                },
            ],
        };

        assert!(matches!(
            validate_input(&input),
            Err(ValidationError::TooManyQuestions(5))
        ));
    }

    #[test]
    fn test_validate_input_too_few_options() {
        let input = AskUserQuestionInput {
            questions: vec![Question {
                question: "Choose one".to_string(),
                header: "Choice".to_string(),
                options: vec![QuestionOption {
                    label: "Only one".to_string(),
                    description: "Description".to_string(),
                }],
                multi_select: false,
            }],
        };

        assert!(matches!(
            validate_input(&input),
            Err(ValidationError::TooFewOptions(_))
        ));
    }

    #[test]
    fn test_validate_input_header_too_long() {
        let input = AskUserQuestionInput {
            questions: vec![Question {
                question: "Question".to_string(),
                header: "This header is way too long for display".to_string(),
                options: vec![
                    QuestionOption {
                        label: "A".to_string(),
                        description: "D".to_string(),
                    },
                    QuestionOption {
                        label: "B".to_string(),
                        description: "D".to_string(),
                    },
                ],
                multi_select: false,
            }],
        };

        assert!(matches!(
            validate_input(&input),
            Err(ValidationError::HeaderTooLong(_))
        ));
    }

    #[test]
    fn test_extract_answer_single_select() {
        use crate::cli::tui::DialogResult;

        let question = Question {
            question: "Choose".to_string(),
            header: "Choice".to_string(),
            options: vec![
                QuestionOption {
                    label: "Option A".to_string(),
                    description: "First".to_string(),
                },
                QuestionOption {
                    label: "Option B".to_string(),
                    description: "Second".to_string(),
                },
            ],
            multi_select: false,
        };

        let result = DialogResult::Selected(1);
        let answer = extract_answer(&question, &result);

        assert_eq!(answer, Some("Option B".to_string()));
    }

    #[test]
    fn test_extract_answer_multi_select() {
        use crate::cli::tui::DialogResult;

        let question = Question {
            question: "Choose multiple".to_string(),
            header: "Choices".to_string(),
            options: vec![
                QuestionOption {
                    label: "Option A".to_string(),
                    description: "First".to_string(),
                },
                QuestionOption {
                    label: "Option B".to_string(),
                    description: "Second".to_string(),
                },
                QuestionOption {
                    label: "Option C".to_string(),
                    description: "Third".to_string(),
                },
            ],
            multi_select: true,
        };

        let result = DialogResult::MultiSelected(vec![0, 2]);
        let answer = extract_answer(&question, &result);

        assert_eq!(answer, Some("Option A, Option C".to_string()));
    }
}
