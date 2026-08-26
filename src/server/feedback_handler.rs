// Durable feedback endpoint handler
//
// Feedback is retained for a future supported learning implementation. It is
// deliberately separate from the legacy executable training queue.

use axum::{
    extract::State,
    http::StatusCode,
    response::{IntoResponse, Json, Response},
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tracing::{info, warn};

use crate::feedback::{FeedbackEntry, FeedbackLogger};

/// Request body for /v1/feedback endpoint
#[derive(Debug, Deserialize)]
pub struct FeedbackRequest {
    /// Original query
    pub query: String,
    /// Model response
    pub response: String,
    /// Weight for this example (1.0 = normal, 3.0 = medium, 10.0 = high)
    pub weight: f64,
    /// Optional feedback note
    #[serde(skip_serializing_if = "Option::is_none")]
    pub feedback: Option<String>,
}

/// Response body for /v1/feedback endpoint
#[derive(Debug, Serialize)]
pub struct FeedbackResponse {
    /// Status: "recorded", "error"
    pub status: String,
    /// Optional message
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

/// Handle POST /v1/feedback - durably retain explicit feedback
pub async fn handle_feedback(
    State(feedback_store): State<Arc<FeedbackLogger>>,
    Json(request): Json<FeedbackRequest>,
) -> Result<Json<FeedbackResponse>, Response> {
    info!(
        weight = request.weight,
        query_len = request.query.len(),
        response_len = request.response.len(),
        "Received feedback submission"
    );

    // Validate weight
    if !request.weight.is_finite() || request.weight <= 0.0 {
        warn!(weight = request.weight, "Invalid weight (must be > 0)");
        return Err((
            StatusCode::BAD_REQUEST,
            Json(FeedbackResponse {
                status: "error".to_string(),
                message: Some("Weight must be finite and greater than 0".to_string()),
            }),
        )
            .into_response());
    }

    let entry = FeedbackEntry::weighted(
        request.query,
        request.response,
        request.weight,
        request.feedback,
    );

    if let Err(error) = feedback_store.log(&entry) {
        warn!(%error, "Failed to persist feedback");
        return Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(FeedbackResponse {
                status: "error".to_string(),
                message: Some("Could not persist feedback".to_string()),
            }),
        )
            .into_response());
    }

    info!(path = %feedback_store.path().display(), "Feedback recorded; training is disabled");

    Ok(Json(FeedbackResponse {
        status: "recorded".to_string(),
        message: Some("Feedback saved; automatic training is disabled".to_string()),
    }))
}

/// Training status information
#[derive(Debug, Serialize)]
pub struct TrainingStatusResponse {
    /// Queue length (examples waiting to be processed)
    pub queue_length: usize,
    /// Whether training is currently active
    pub training_active: bool,
    /// Optional last training timestamp
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_training: Option<String>,
    /// Why training cannot currently be enabled.
    pub message: String,
}

/// Handle GET /v1/training/status - Get training queue status
pub async fn handle_training_status() -> Json<TrainingStatusResponse> {
    // TODO: Implement actual status tracking
    // For now, return placeholder data
    Json(TrainingStatusResponse {
        queue_length: 0,
        training_active: false,
        last_training: None,
        message: "Training is disabled pending a supported native implementation (issues #1, #7, and #74)".to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::response::IntoResponse;

    #[test]
    fn test_feedback_request_parsing() {
        let json = r#"{
            "query": "What is 2+2?",
            "response": "4",
            "weight": 10.0,
            "feedback": "Critical: Missing explanation"
        }"#;

        let request: FeedbackRequest = serde_json::from_str(json).unwrap();
        assert_eq!(request.query, "What is 2+2?");
        assert_eq!(request.weight, 10.0);
    }

    #[tokio::test(start_paused = true)]
    async fn feedback_is_durable_without_touching_legacy_training_state() {
        let temp = tempfile::tempdir().unwrap();
        let feedback_path = temp.path().join("feedback.jsonl");
        let legacy_queue = temp.path().join("training_queue.jsonl");
        let adapter_path = temp.path().join("adapters/latest.safetensors");
        std::fs::write(&legacy_queue, "legacy queued example\n").unwrap();

        let first_store = Arc::new(FeedbackLogger::at(&feedback_path).unwrap());
        let response = handle_feedback(
            State(first_store),
            Json(FeedbackRequest {
                query: "first query".into(),
                response: "first response".into(),
                weight: 3.0,
                feedback: Some("keep this".into()),
            }),
        )
        .await
        .unwrap();
        assert_eq!(response.0.status, "recorded");

        // Advancing beyond the former worker timeout cannot flush a training
        // queue or create an adapter because feedback owns no timer or worker.
        tokio::time::advance(std::time::Duration::from_secs(10 * 60)).await;
        assert_eq!(
            std::fs::read_to_string(&legacy_queue).unwrap(),
            "legacy queued example\n"
        );
        assert!(!adapter_path.exists());

        // Recreating the store models a daemon restart. Existing feedback is
        // appended, not replaced, and legacy training state stays preserved.
        let restarted_store = Arc::new(FeedbackLogger::at(&feedback_path).unwrap());
        let response = handle_feedback(
            State(restarted_store),
            Json(FeedbackRequest {
                query: "second query".into(),
                response: "second response".into(),
                weight: 1.0,
                feedback: None,
            }),
        )
        .await
        .unwrap();
        assert_eq!(response.0.status, "recorded");

        let lines: Vec<_> = std::fs::read_to_string(&feedback_path)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str::<FeedbackEntry>(line).unwrap())
            .collect();
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0].query, "first query");
        assert_eq!(lines[1].query, "second query");
        assert_eq!(
            std::fs::read_to_string(&legacy_queue).unwrap(),
            "legacy queued example\n"
        );
        assert!(!adapter_path.exists());
    }

    #[tokio::test]
    async fn invalid_feedback_is_rejected_without_recording_an_entry() {
        let temp = tempfile::tempdir().unwrap();
        let feedback_path = temp.path().join("feedback.jsonl");
        let store = Arc::new(FeedbackLogger::at(&feedback_path).unwrap());

        let response = handle_feedback(
            State(store),
            Json(FeedbackRequest {
                query: "query".into(),
                response: "response".into(),
                weight: 0.0,
                feedback: None,
            }),
        )
        .await
        .unwrap_err()
        .into_response();

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        assert!(feedback_path.exists());
        assert_eq!(std::fs::metadata(&feedback_path).unwrap().len(), 0);
    }

    #[tokio::test]
    async fn training_status_is_explicitly_disabled() {
        let status = handle_training_status().await.0;
        assert!(!status.training_active);
        assert_eq!(status.queue_length, 0);
        assert!(status.message.contains("disabled"));
    }
}
