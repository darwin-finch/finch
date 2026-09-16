use super::*;
use crate::LlmProvider;
use crate::ToolInputSchema;
use std::sync::atomic::{AtomicUsize, Ordering};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

const VALID_PNG_BASE64: &str =
    "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNk+A8AAQUBAScY42YAAAAASUVORK5CYII=";

fn production_credential() -> ProviderCredential {
    toml::from_str(
        r#"name = "work"
kind = "oauth_device"
provider = "chatgpt_subscription"
issuer = "openai-chatgpt"
account = "account-1"
secret_ref = "oauth-store:work"

[audience]
family = "chatgpt_subscription"
"#,
    )
    .expect("production credential fixture must deserialize")
}

#[tokio::test]
async fn test_production_omitted_reasoning_uses_medium_in_request_and_debug_metadata() {
    let mut server = mockito::Server::new_async().await;
    let models = server
        .mock("GET", "/backend-api/codex/models")
        .match_query(mockito::Matcher::UrlEncoded(
            "client_version".into(),
            CHATGPT_CATALOG_CLIENT_VERSION.into(),
        ))
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(catalog_body())
        .create_async()
        .await;
    let inference = server
        .mock("POST", RESPONSES_PATH)
        .match_body(mockito::Matcher::PartialJson(json!({
            "reasoning": {"effort": "medium", "context": "all_turns"}
        })))
        .with_status(200)
        .with_header("content-type", "text/event-stream")
        .with_header("openai-model", DEFAULT_MODEL)
        .with_body(completed_sse(DEFAULT_MODEL))
        .create_async()
        .await;
    let mut provider = ChatGptSubscriptionProvider::production(
        &production_credential(),
        Some(DEFAULT_MODEL),
        None,
    )
    .expect("omitted reasoning must construct the production subscription provider");
    provider.source = Arc::new(StaticSource::new());
    provider.base = validate_base(&format!("{}/backend-api/codex", server.url()), true)
        .expect("loopback production-boundary fixture must have a valid base URL");
    provider.allow_loopback = true;
    let request = ProviderRequest::new(vec![Message::user("hello")])
        .with_model(DEFAULT_MODEL)
        .with_tools(vec![tool()]);

    assert_eq!(
        provider.requested_reasoning_effort(&request),
        Some(ReasoningEffort::Medium),
        "omitted subscription reasoning selected the wrong effective effort"
    );
    let debug = format!("{provider:?}");
    assert!(
        debug.contains("reasoning_effort: Medium"),
        "provider debug metadata omitted the effective reasoning effort: {debug}"
    );
    assert!(
        !debug.contains("account-1") && !debug.contains("oauth-store:work"),
        "provider debug metadata exposed credential identity: {debug}"
    );

    let response = provider
        .send_message(&request)
        .await
        .expect("production provider failed to dispatch the omitted reasoning default");
    models.assert_async().await;
    inference.assert_async().await;
    assert_eq!(
        response.text(),
        "hello",
        "production-boundary fixture returned the wrong response: {response:?}"
    );
}

#[test]
fn test_production_explicit_reasoning_efforts_reach_the_request_exactly() {
    for effort in [
        ReasoningEffort::Low,
        ReasoningEffort::Medium,
        ReasoningEffort::High,
        ReasoningEffort::Xhigh,
        ReasoningEffort::Max,
    ] {
        let provider = ChatGptSubscriptionProvider::production(
            &production_credential(),
            Some(DEFAULT_MODEL),
            Some(effort),
        )
        .unwrap_or_else(|error| {
            panic!(
                "explicit reasoning effort {} was rejected: {error:#}",
                effort.as_str()
            )
        });
        let request = ProviderRequest::new(vec![Message::user("hello")]).with_model(DEFAULT_MODEL);
        let body = encode_responses_lite(&request, provider.reasoning_effort)
            .expect("validated explicit reasoning must serialize");

        assert_eq!(
            provider.requested_reasoning_effort(&request),
            Some(effort),
            "explicit effort {} changed before dispatch",
            effort.as_str()
        );
        assert_eq!(
            body["reasoning"]["effort"],
            effort.as_str(),
            "explicit effort {} changed at the request boundary: {body}",
            effort.as_str()
        );
    }
}

#[test]
fn test_production_unsupported_reasoning_names_value_and_allowed_efforts() {
    for effort in [ReasoningEffort::None, ReasoningEffort::Minimal] {
        let error = ChatGptSubscriptionProvider::production(
            &production_credential(),
            Some(DEFAULT_MODEL),
            Some(effort),
        )
        .expect_err("unproven subscription reasoning effort was accepted");
        let diagnostic = error.to_string();
        assert!(
            diagnostic.contains(effort.as_str())
                && diagnostic.contains("low, medium, high, xhigh, max"),
            "unsupported effort {} returned an unactionable diagnostic: {error:#}",
            effort.as_str()
        );
    }
}

struct StaticSource {
    generation: Mutex<String>,
    leases: AtomicUsize,
    refreshes: AtomicUsize,
}

impl StaticSource {
    fn new() -> Self {
        Self {
            generation: Mutex::new("generation-1".to_string()),
            leases: AtomicUsize::new(0),
            refreshes: AtomicUsize::new(0),
        }
    }

    async fn current(&self) -> ChatGptCredentialLease {
        ChatGptCredentialLease {
            access_token: "subscription-secret".to_string(),
            account: "account-1".to_string(),
            generation: self.generation.lock().await.clone(),
        }
    }
}

#[async_trait]
impl ChatGptCredentialSource for StaticSource {
    async fn lease(&self, _cancel: &CancellationToken) -> Result<ChatGptCredentialLease> {
        self.leases.fetch_add(1, Ordering::SeqCst);
        Ok(self.current().await)
    }

    async fn refresh_after_unauthorized(
        &self,
        rejected_generation: &str,
        _cancel: &CancellationToken,
    ) -> Result<ChatGptCredentialLease> {
        self.refreshes.fetch_add(1, Ordering::SeqCst);
        let mut generation = self.generation.lock().await;
        if generation.as_str() == rejected_generation {
            *generation = "generation-2".to_string();
        }
        Ok(ChatGptCredentialLease {
            access_token: "refreshed-subscription-secret".to_string(),
            account: "account-1".to_string(),
            generation: generation.clone(),
        })
    }
}

fn catalog_body() -> String {
    catalog_body_with_context_windows(1_050_000, 1_050_000)
}

fn catalog_body_with_context_windows(sol: u64, alias: u64) -> String {
    json!({
        "models":[
            {
                "slug":"gpt-5.6-sol",
                "supported_in_api":true,
                "use_responses_lite":true,
                "input_modalities":["text","image"],
                "context_window":sol
            },
            {
                "slug":"gpt-5.6",
                "supported_in_api":true,
                "use_responses_lite":true,
                "input_modalities":["text","image"],
                "context_window":alias
            }
        ]
    })
    .to_string()
}

fn single_model_catalog_body(slug: &str, context_window: u64) -> String {
    json!({
        "models":[{
            "slug":slug,
            "supported_in_api":true,
            "use_responses_lite":true,
            "input_modalities":["text","image"],
            "context_window":context_window
        }]
    })
    .to_string()
}

fn completed_sse_with_model_provenance(model: Option<&str>) -> String {
    let created = match model {
        Some(model) => {
            json!({"type":"response.created","sequence_number":1,"response":{"headers":{"openai-model":model}}})
        }
        None => json!({"type":"response.created","sequence_number":1,"response":{}}),
    };
    format!(
        concat!(
            "event: response.created\ndata: {}\n\n",
            "event: response.output_item.done\ndata: {}\n\n",
            "event: response.output_text.delta\ndata: {}\n\n",
            "event: response.output_item.done\ndata: {}\n\n",
            "event: response.output_item.done\ndata: {}\n\n",
            "event: response.completed\ndata: {}\n\n",
            "data: [DONE]\n\n"
        ),
        created,
        json!({"type":"response.output_item.done","sequence_number":2,"output_index":0,"item":{"type":"reasoning","summary":[],"encrypted_content":"opaque-1"}}),
        json!({"type":"response.output_text.delta","sequence_number":3,"item_id":"message-1","output_index":1,"content_index":0,"delta":"hello"}),
        json!({"type":"response.output_item.done","sequence_number":4,"output_index":1,"item":{"type":"message","role":"assistant","content":[{"type":"output_text","text":"hello"}]}}),
        json!({"type":"response.output_item.done","sequence_number":5,"output_index":2,"item":{"type":"function_call","call_id":"call-2","name":"read","namespace":"functions","arguments":"{\"path\":\"README.md\"}"}}),
        json!({"type":"response.completed","sequence_number":6,"response":{"id":"resp-1","usage":{"input_tokens":12,"output_tokens":7}}})
    )
}

fn completed_sse(model: &str) -> String {
    completed_sse_with_model_provenance(Some(model))
}

fn completed_sse_with_function_call(
    model: &str,
    wire_name: &str,
    namespace: Option<&str>,
) -> String {
    completed_sse(model)
        .split_inclusive('\n')
        .map(|line| {
            let Some(data) = line
                .strip_prefix("data: ")
                .and_then(|data| data.strip_suffix('\n'))
            else {
                return line.to_string();
            };
            let Ok(mut event) = serde_json::from_str::<Value>(data) else {
                return line.to_string();
            };
            if event["item"]["type"] != "function_call" {
                return line.to_string();
            }
            event["item"]["name"] = json!(wire_name);
            event["item"]["arguments"] = json!("{\"task\":\"say your name\"}");
            match namespace {
                Some(namespace) => event["item"]["namespace"] = json!(namespace),
                None => {
                    event["item"]
                        .as_object_mut()
                        .expect("function-call fixture item must be an object")
                        .remove("namespace");
                }
            }
            format!("data: {event}\n")
        })
        .collect()
}

fn completed_sse_with_function_namespace_projection(
    model: &str,
    streamed_namespace: Option<&str>,
    terminal_namespace: Option<&str>,
) -> String {
    let mut terminal_call = json!({
        "type":"function_call",
        "call_id":"call-2",
        "name":"finch_spawn_agent",
        "arguments":"{\"task\":\"say your name\"}"
    });
    if let Some(namespace) = terminal_namespace {
        terminal_call["namespace"] = json!(namespace);
    }
    completed_sse_with_function_call(model, "finch_spawn_agent", streamed_namespace)
        .split_inclusive('\n')
        .map(|line| {
            let Some(data) = line
                .strip_prefix("data: ")
                .and_then(|data| data.strip_suffix('\n'))
            else {
                return line.to_string();
            };
            let Ok(mut event) = serde_json::from_str::<Value>(data) else {
                return line.to_string();
            };
            if event["type"] != "response.completed" {
                return line.to_string();
            }
            event["response"]["output"] = json!([
                {
                    "type":"reasoning",
                    "summary":[],
                    "encrypted_content":"opaque-1"
                },
                {
                    "type":"message",
                    "role":"assistant",
                    "content":[{"type":"output_text","text":"hello"}]
                },
                terminal_call
            ]);
            format!("data: {event}\n")
        })
        .collect()
}

fn completed_sse_with_audited_passive_fields(model: &str) -> String {
    let mut response = json!({
        "id":"resp-passive-fields",
        "object":"response",
        "created_at":1_777_777_776.5,
        "completed_at":1_777_777_777.5,
        "status":"completed",
        "error":null,
        "incomplete_details":null,
        "instructions":null,
        "max_output_tokens":null,
        "max_tool_calls":null,
        "model":model,
        "parallel_tool_calls":false,
        "previous_response_id":null,
        "reasoning":{},
        "store":false,
        "temperature":1.0,
        "text":{},
        "tool_choice":"auto",
        "tools":[],
        "top_p":1.0,
        "truncation":"disabled"
    })
    .as_object()
    .expect("passive-field response fixture must be an object")
    .clone();
    response.extend(
            json!({
                "usage":{"input_tokens":12,"output_tokens":7,"total_tokens":19},
                "user":null,
                "metadata":{"fixture":"audited-passive-fields"},
                "service_tier":"default",
                "prompt_cache_key":null,
                "prompt_cache_diagnostics":{"type":"cache_hit"},
                "prompt_cache_options":{"mode":"implicit","ttl":"30m","comparison_response_id":"resp-cache-comparison"},
                "prompt_cache_retention":"24h",
                "safety_identifier":null,
                "headers":{},
                "usage_metadata":{},
                "end_turn":true,
                "background":null,
                "conversation":{"id":"conv-fixture"},
                "moderation":{},
                "prompt":{},
                "top_logprobs":0,
                "frequency_penalty":0.0,
                "presence_penalty":0.0,
                "tool_usage":[{"name":"read","calls":1}]
            })
            .as_object()
            .expect("passive-field response extension fixture must be an object")
                .clone(),
        );
    response.insert(
            "output".to_string(),
            json!([
                {"type":"reasoning","summary":[],"encrypted_content":"opaque-1"},
                {"id":"message-terminal","type":"message","status":"completed","role":"assistant","phase":"final_answer","internal_chat_message_metadata_passthrough":{"source":"terminal"},"content":[{"type":"output_text","text":"hello"}]}
            ]),
        );
    let completed = json!({
        "type":"response.completed",
        "sequence_number":5,
        "obfuscation":"padding-terminal",
        "safety_buffering":{"enabled":false},
        "response":response
    });
    format!(
        concat!(
            "event: response.created\ndata: {}\n\n",
            "event: response.output_item.done\ndata: {}\n\n",
            "event: response.output_text.delta\ndata: {}\n\n",
            "event: response.output_item.done\ndata: {}\n\n",
            "event: response.completed\ndata: {}\n\n",
            "data: [DONE]\n\n"
        ),
        json!({"type":"response.created","sequence_number":1,"response":{"headers":{"openai-model":model}},"obfuscation":"padding-created","safety_buffering":{"enabled":false}}),
        json!({"type":"response.output_item.done","sequence_number":2,"output_index":0,"item":{"type":"reasoning","summary":[],"encrypted_content":"opaque-1"},"obfuscation":"padding-reasoning"}),
        json!({"type":"response.output_text.delta","sequence_number":3,"item_id":"message-1","output_index":1,"content_index":0,"delta":"hello","logprobs":[],"obfuscation":"padding-delta"}),
        json!({"type":"response.output_item.done","sequence_number":4,"output_index":1,"item":{"id":"message-stream","type":"message","status":"completed","role":"assistant","phase":"final_answer","internal_chat_message_metadata_passthrough":{"source":"stream"},"content":[{"type":"output_text","text":"hello","annotations":[],"logprobs":[]}]},"obfuscation":"padding-message"}),
        completed
    )
}

fn completed_sse_with_terminal_output(model: &str, terminal_output: Value) -> String {
    format!(
        concat!(
            "event: response.created\ndata: {}\n\n",
            "event: response.output_item.done\ndata: {}\n\n",
            "event: response.output_text.delta\ndata: {}\n\n",
            "event: response.output_item.done\ndata: {}\n\n",
            "event: response.completed\ndata: {}\n\n",
            "data: [DONE]\n\n"
        ),
        json!({"type":"response.created","sequence_number":1,"response":{"headers":{"openai-model":model}}}),
        json!({"type":"response.output_item.done","sequence_number":2,"output_index":0,"item":{"type":"reasoning","summary":[],"encrypted_content":"opaque-1"}}),
        json!({"type":"response.output_text.delta","sequence_number":3,"item_id":"message-1","output_index":1,"content_index":0,"delta":"hello"}),
        json!({"type":"response.output_item.done","sequence_number":4,"output_index":1,"item":{"id":"message-stream","type":"message","status":"completed","role":"assistant","phase":"final_answer","content":[{"type":"output_text","text":"hello"}]}}),
        json!({"type":"response.completed","sequence_number":5,"response":{"id":"resp-terminal-subset","status":"completed","model":model,"output":terminal_output,"usage":{"input_tokens":12,"output_tokens":7}}})
    )
}

fn completed_sse_with_streamed_message_and_terminal_response(
    model: &str,
    terminal_response: Value,
) -> String {
    format!(
        concat!(
            "event: response.created\ndata: {}\n\n",
            "event: response.output_text.delta\ndata: {}\n\n",
            "event: response.output_item.done\ndata: {}\n\n",
            "event: response.completed\ndata: {}\n\n",
            "data: [DONE]\n\n"
        ),
        json!({"type":"response.created","sequence_number":1,"response":{"headers":{"openai-model":model}}}),
        json!({"type":"response.output_text.delta","sequence_number":2,"item_id":"message-1","output_index":0,"content_index":0,"delta":"hello"}),
        json!({"type":"response.output_item.done","sequence_number":3,"output_index":0,"item":{"type":"message","role":"assistant","content":[{"type":"output_text","text":"hello"}]}}),
        json!({"type":"response.completed","sequence_number":4,"response":terminal_response})
    )
}

fn completed_sse_with_streamed_message_and_empty_terminal_output(model: &str) -> String {
    completed_sse_with_streamed_message_and_terminal_response(
        model,
        json!({
            "id":"resp-empty-terminal-output",
            "status":"completed",
            "model":model,
            "output":[],
            "usage":{"input_tokens":12,"output_tokens":7}
        }),
    )
}

async fn run_streamed_message_terminal_response(
    terminal_response: Value,
) -> (
    Result<ProviderResponse, String>,
    Vec<Result<StreamChunk, String>>,
) {
    run_streamed_message_sse(completed_sse_with_streamed_message_and_terminal_response(
        DEFAULT_MODEL,
        terminal_response,
    ))
    .await
}

async fn run_streamed_message_sse(
    response_body: String,
) -> (
    Result<ProviderResponse, String>,
    Vec<Result<StreamChunk, String>>,
) {
    let mut server = mockito::Server::new_async().await;
    let models = server
        .mock("GET", "/backend-api/codex/models")
        .match_query(mockito::Matcher::UrlEncoded(
            "client_version".into(),
            CHATGPT_CATALOG_CLIENT_VERSION.into(),
        ))
        .with_status(200)
        .with_body(catalog_body())
        .expect(1)
        .create_async()
        .await;
    let inference = server
        .mock("POST", RESPONSES_PATH)
        .with_status(200)
        .with_header("content-type", "text/event-stream")
        .with_header("openai-model", DEFAULT_MODEL)
        .with_header("x-codex-primary-used-percent", "25.5")
        .with_body(response_body)
        .expect(2)
        .create_async()
        .await;
    let provider = ChatGptSubscriptionProvider::for_test(
        Arc::new(StaticSource::new()),
        &format!("{}/backend-api/codex", server.url()),
        DEFAULT_MODEL,
    )
    .expect("terminal-response fixture must construct a provider");
    let request = ProviderRequest::new(vec![Message::user("hello")]);

    let (buffered, streaming) = tokio::join!(
        provider.send_message(&request),
        provider.send_message_stream(&request)
    );

    models.assert_async().await;
    inference.assert_async().await;
    let buffered = buffered.map_err(|error| error.to_string());
    let mut receiver = streaming.expect("stream setup must consume the complete SSE fixture");
    let mut outcome = Vec::new();
    while let Some(chunk) = receiver.recv().await {
        outcome.push(chunk.map_err(|error| error.to_string()));
    }
    (buffered, outcome)
}

fn completed_sse_with_streamed_tool_and_terminal_output(
    model: &str,
    terminal_output: Value,
) -> String {
    format!(
        concat!(
            "event: response.created\ndata: {}\n\n",
            "event: response.output_item.done\ndata: {}\n\n",
            "event: response.output_text.delta\ndata: {}\n\n",
            "event: response.output_item.done\ndata: {}\n\n",
            "event: response.output_item.done\ndata: {}\n\n",
            "event: response.completed\ndata: {}\n\n",
            "data: [DONE]\n\n"
        ),
        json!({"type":"response.created","sequence_number":1,"response":{"headers":{"openai-model":model}}}),
        json!({"type":"response.output_item.done","sequence_number":2,"output_index":0,"item":{"type":"reasoning","summary":[],"encrypted_content":"opaque-1"}}),
        json!({"type":"response.output_text.delta","sequence_number":3,"item_id":"message-1","output_index":1,"content_index":0,"delta":"hello"}),
        json!({"type":"response.output_item.done","sequence_number":4,"output_index":1,"item":{"type":"message","role":"assistant","content":[{"type":"output_text","text":"hello"}]}}),
        json!({"type":"response.output_item.done","sequence_number":5,"output_index":2,"item":{"type":"function_call","call_id":"call-2","name":"read","namespace":"functions","arguments":"{\"path\":\"README.md\"}"}}),
        json!({"type":"response.completed","sequence_number":6,"response":{"id":"resp-terminal-negative","status":"completed","model":model,"output":terminal_output,"usage":{"input_tokens":12,"output_tokens":7}}})
    )
}

async fn assert_terminal_snapshot_rejected_at_provider_boundary(
    case: &str,
    terminal_output: Value,
    expected_diagnostic: &str,
) {
    let mut server = mockito::Server::new_async().await;
    let models = server
        .mock("GET", "/backend-api/codex/models")
        .match_query(mockito::Matcher::UrlEncoded(
            "client_version".into(),
            CHATGPT_CATALOG_CLIENT_VERSION.into(),
        ))
        .with_status(200)
        .with_body(catalog_body())
        .expect(1)
        .create_async()
        .await;
    let inference = server
        .mock("POST", RESPONSES_PATH)
        .with_status(200)
        .with_header("content-type", "text/event-stream")
        .with_header("openai-model", DEFAULT_MODEL)
        .with_header("x-codex-primary-used-percent", "25.5")
        .with_body(completed_sse_with_streamed_tool_and_terminal_output(
            DEFAULT_MODEL,
            terminal_output,
        ))
        .expect(2)
        .create_async()
        .await;
    let provider = ChatGptSubscriptionProvider::for_test(
        Arc::new(StaticSource::new()),
        &format!("{}/backend-api/codex", server.url()),
        DEFAULT_MODEL,
    )
    .unwrap_or_else(|error| panic!("{case} fixture failed to construct a provider: {error:#}"));
    let request = ProviderRequest::new(vec![Message::user("hello")]).with_tools(vec![tool()]);

    let (buffered, streaming) = tokio::join!(
        provider.send_message(&request),
        provider.send_message_stream(&request)
    );

    models.assert_async().await;
    inference.assert_async().await;
    let buffered_error = buffered
        .err()
        .unwrap_or_else(|| panic!("{case} terminal semantic drift passed the buffered boundary"));
    assert!(
        buffered_error.to_string().contains(expected_diagnostic),
        "{case} buffered rejection was not actionable: {buffered_error:#}"
    );

    let mut receiver = streaming.unwrap_or_else(|error| {
        panic!("{case} stream setup failed before the SSE fixture was consumed: {error:#}")
    });
    let mut outcome = Vec::new();
    while let Some(chunk) = receiver.recv().await {
        outcome.push(chunk.map_err(|error| error.to_string()));
    }
    assert_eq!(
        outcome.iter().filter(|chunk| chunk.is_err()).count(),
        1,
        "{case} must emit exactly one terminal stream error; outcome={outcome:?}"
    );
    assert!(
        matches!(outcome.last(), Some(Err(error)) if error.contains(expected_diagnostic)),
        "{case} did not end with its actionable stream error; outcome={outcome:?}"
    );
    assert!(
        outcome.iter().all(|chunk| !matches!(
            chunk,
            Ok(StreamChunk::ResponseMetadata { .. }
                | StreamChunk::Usage { .. }
                | StreamChunk::Allowance { .. }
                | StreamChunk::ContentBlockComplete(_))
        )),
        "{case} published terminal metadata, usage, allowance, or completed content after \
             semantic reconciliation failed; outcome={outcome:?}"
    );
}

fn tool() -> ToolDefinition {
    ToolDefinition {
        name: "read".to_string(),
        description: "Read a file".to_string(),
        input_schema: ToolInputSchema {
            schema_type: "object".to_string(),
            properties: json!({"path":{"type":"string"}}),
            required: vec!["path".to_string()],
        },
    }
}

fn named_tool(name: &str) -> ToolDefinition {
    ToolDefinition {
        name: name.to_string(),
        description: format!("Run {name}"),
        input_schema: ToolInputSchema {
            schema_type: "object".to_string(),
            properties: json!({"task":{"type":"string"}}),
            required: vec!["task".to_string()],
        },
    }
}

async fn run_function_call_boundary(
    wire_name: &str,
    namespace: Option<&str>,
    advertise_spawn_agent: bool,
    response_body: Option<String>,
) -> (
    Result<ProviderResponse, String>,
    Vec<Result<StreamChunk, String>>,
) {
    let mut server = mockito::Server::new_async().await;
    let models = server
        .mock("GET", "/backend-api/codex/models")
        .match_query(mockito::Matcher::UrlEncoded(
            "client_version".into(),
            CHATGPT_CATALOG_CLIENT_VERSION.into(),
        ))
        .with_status(200)
        .with_body(catalog_body())
        .expect(1)
        .create_async()
        .await;
    let inference = server
        .mock("POST", RESPONSES_PATH)
        .with_status(200)
        .with_header("content-type", "text/event-stream")
        .with_header("openai-model", DEFAULT_MODEL)
        .with_body(response_body.unwrap_or_else(|| {
            completed_sse_with_function_call(DEFAULT_MODEL, wire_name, namespace)
        }))
        .expect(2)
        .create_async()
        .await;
    let provider = ChatGptSubscriptionProvider::for_test(
        Arc::new(StaticSource::new()),
        &format!("{}/backend-api/codex", server.url()),
        DEFAULT_MODEL,
    )
    .expect("function-call fixture must construct a provider");
    let tools = if advertise_spawn_agent {
        vec![named_tool("spawn_agent"), tool()]
    } else {
        vec![tool()]
    };
    let request = ProviderRequest::new(vec![Message::user("delegate")]).with_tools(tools);

    let (buffered, streaming) = tokio::join!(
        provider.send_message(&request),
        provider.send_message_stream(&request)
    );

    models.assert_async().await;
    inference.assert_async().await;
    let buffered = buffered.map_err(|error| error.to_string());
    let mut receiver = streaming.expect("stream setup must consume the function-call fixture");
    let mut outcome = Vec::new();
    while let Some(chunk) = receiver.recv().await {
        outcome.push(chunk.map_err(|error| error.to_string()));
    }
    (buffered, outcome)
}

#[tokio::test]
async fn test_buffered_and_streaming_bind_chatgpt_collaboration_function_calls() {
    for namespace in [None, Some("functions"), Some("collaboration")] {
        let (buffered, streaming) =
            run_function_call_boundary("finch_spawn_agent", namespace, true, None).await;
        let buffered = buffered.unwrap_or_else(|error| {
            panic!(
                "advertised collaboration function failed at the buffered boundary for \
                     namespace {namespace:?}: {error}"
            )
        });
        assert!(
            buffered.content.iter().any(|block| matches!(
                block,
                ContentBlock::ToolUse { id, name, input }
                    if id == "call-2"
                        && name == "spawn_agent"
                        && input == &json!({"task":"say your name"})
            )),
            "buffered collaboration function was not rebound to the advertised local tool; \
                 namespace={namespace:?}, response={buffered:?}"
        );
        assert!(
            streaming.iter().any(|chunk| matches!(
                chunk,
                Ok(StreamChunk::ContentBlockComplete(ContentBlock::ToolUse {
                    id,
                    name,
                    input,
                })) if id == "call-2"
                    && name == "spawn_agent"
                    && input == &json!({"task":"say your name"})
            )),
            "streaming collaboration function was not rebound to the advertised local tool; \
                 namespace={namespace:?}, outcome={streaming:?}"
        );
        assert!(
            streaming.iter().all(Result::is_ok),
            "accepted collaboration function emitted a stream error; namespace={namespace:?}, \
                 outcome={streaming:?}"
        );
    }
}

#[tokio::test]
async fn test_terminal_reconciliation_accepts_equivalent_function_namespaces() {
    for (streamed_namespace, terminal_namespace) in [
        (None, Some("functions")),
        (Some("functions"), None),
        (Some("collaboration"), Some("functions")),
        (Some("functions"), Some("collaboration")),
    ] {
        let response_body = completed_sse_with_function_namespace_projection(
            DEFAULT_MODEL,
            streamed_namespace,
            terminal_namespace,
        );
        let (buffered, streaming) = run_function_call_boundary(
            "finch_spawn_agent",
            streamed_namespace,
            true,
            Some(response_body),
        )
        .await;
        let buffered = buffered.unwrap_or_else(|error| {
            panic!(
                "equivalent streamed and terminal function namespaces failed buffered \
                     reconciliation; streamed={streamed_namespace:?}, \
                     terminal={terminal_namespace:?}, error={error}"
            )
        });
        assert!(
            buffered.content.iter().any(|block| matches!(
                block,
                ContentBlock::ToolUse { id, name, input }
                    if id == "call-2"
                        && name == "spawn_agent"
                        && input == &json!({"task":"say your name"})
            )),
            "equivalent namespace reconciliation changed buffered tool semantics; \
                 streamed={streamed_namespace:?}, terminal={terminal_namespace:?}, \
                 response={buffered:?}"
        );
        assert!(
            streaming.iter().all(Result::is_ok),
            "equivalent namespace reconciliation emitted a streaming error; \
                 streamed={streamed_namespace:?}, terminal={terminal_namespace:?}, \
                 outcome={streaming:?}"
        );
        assert!(
            streaming.iter().any(|chunk| matches!(
                chunk,
                Ok(StreamChunk::ContentBlockComplete(ContentBlock::ToolUse {
                    id,
                    name,
                    input,
                })) if id == "call-2"
                    && name == "spawn_agent"
                    && input == &json!({"task":"say your name"})
            )),
            "equivalent namespace reconciliation did not complete the local tool call; \
                 streamed={streamed_namespace:?}, terminal={terminal_namespace:?}, \
                 outcome={streaming:?}"
        );
    }
}

#[tokio::test]
async fn test_buffered_and_streaming_reject_invalid_chatgpt_function_namespaces() {
    for (wire_name, namespace, advertise_spawn_agent, expected_error) in [
        (
            "finch_spawn_agent",
            Some("other"),
            true,
            "ChatGPT function call namespace was invalid",
        ),
        (
            "spawn_agent",
            Some("functions"),
            true,
            "ChatGPT requested a reserved native function name",
        ),
        (
            "read",
            Some("collaboration"),
            true,
            "ChatGPT function call namespace was invalid",
        ),
        (
            "finch_spawn_agent",
            Some("functions"),
            false,
            "ChatGPT requested a function Finch did not advertise",
        ),
    ] {
        let (buffered, streaming) =
            run_function_call_boundary(wire_name, namespace, advertise_spawn_agent, None).await;
        let buffered_error = buffered.expect_err(
            "invalid ChatGPT function namespace unexpectedly crossed the buffered boundary",
        );
        assert!(
            buffered_error.contains(expected_error),
            "buffered invalid function call returned the wrong diagnostic; wire_name={wire_name}, \
                 namespace={namespace:?}, expected={expected_error:?}, actual={buffered_error:?}"
        );
        let stream_errors = streaming
            .iter()
            .filter_map(|chunk| chunk.as_ref().err())
            .collect::<Vec<_>>();
        assert_eq!(
            stream_errors.len(),
            1,
            "invalid function call did not terminate with exactly one streaming error; \
                 wire_name={wire_name}, namespace={namespace:?}, outcome={streaming:?}"
        );
        assert!(
            stream_errors[0].contains(expected_error),
            "streaming invalid function call returned the wrong diagnostic; \
                 wire_name={wire_name}, namespace={namespace:?}, expected={expected_error:?}, \
                 actual={stream_errors:?}"
        );
        assert!(
            streaming.iter().all(|chunk| !matches!(
                chunk,
                Ok(StreamChunk::ResponseMetadata { .. }
                    | StreamChunk::Usage { .. }
                    | StreamChunk::Allowance { .. }
                    | StreamChunk::ContentBlockComplete(_))
            )),
            "invalid function call published terminal state or completed content; \
                 wire_name={wire_name}, namespace={namespace:?}, outcome={streaming:?}"
        );
    }
}

async fn stalling_subscription_server(
    send_stream_headers: bool,
) -> (String, tokio::sync::oneshot::Receiver<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let (closed_tx, closed_rx) = tokio::sync::oneshot::channel();
    tokio::spawn(async move {
        let (mut catalog_socket, _) = listener.accept().await.unwrap();
        let mut request = vec![0u8; 16 * 1024];
        let _ = catalog_socket.read(&mut request).await;
        let catalog = catalog_body();
        catalog_socket
                .write_all(
                    format!(
                        "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
                        catalog.len(),
                        catalog
                    )
                    .as_bytes(),
                )
                .await
                .unwrap();
        catalog_socket.flush().await.unwrap();
        drop(catalog_socket);

        let (mut response_socket, _) = listener.accept().await.unwrap();
        let _ = response_socket.read(&mut request).await;
        if send_stream_headers {
            response_socket
                .write_all(
                    concat!(
                        "HTTP/1.1 200 OK\r\n",
                        "content-type: text/event-stream\r\n",
                        "openai-model: gpt-5.6-sol\r\n",
                        "connection: close\r\n\r\n"
                    )
                    .as_bytes(),
                )
                .await
                .unwrap();
            response_socket.flush().await.unwrap();
        }
        let mut byte = [0u8; 1];
        while matches!(response_socket.read(&mut byte).await, Ok(1)) {}
        let _ = closed_tx.send(());
    });
    (format!("http://{address}/backend-api/codex"), closed_rx)
}

async fn held_open_subscription_server(
    body: String,
) -> (
    String,
    tokio::sync::oneshot::Receiver<()>,
    tokio::sync::oneshot::Receiver<()>,
) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("backpressure fixture must bind a kernel-assigned loopback port");
    let address = listener
        .local_addr()
        .expect("backpressure fixture must expose its loopback address");
    let (body_sent_tx, body_sent_rx) = tokio::sync::oneshot::channel();
    let (closed_tx, closed_rx) = tokio::sync::oneshot::channel();
    tokio::spawn(async move {
        let mut request = vec![0u8; 16 * 1024];
        let (mut catalog_socket, _) = listener
            .accept()
            .await
            .expect("backpressure fixture must accept the catalog request");
        let _ = catalog_socket.read(&mut request).await;
        let catalog = catalog_body();
        catalog_socket
                .write_all(
                    format!(
                        "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
                        catalog.len(),
                        catalog
                    )
                    .as_bytes(),
                )
                .await
                .expect("backpressure fixture must write the catalog response");
        catalog_socket
            .flush()
            .await
            .expect("backpressure fixture must flush the catalog response");
        drop(catalog_socket);

        let (mut response_socket, _) = listener
            .accept()
            .await
            .expect("backpressure fixture must accept the inference request");
        let _ = response_socket.read(&mut request).await;
        response_socket
            .write_all(
                format!(
                    concat!(
                        "HTTP/1.1 200 OK\r\n",
                        "content-type: text/event-stream\r\n",
                        "openai-model: {}\r\n",
                        "transfer-encoding: chunked\r\n",
                        "connection: close\r\n\r\n",
                        "{:X}\r\n{}\r\n"
                    ),
                    DEFAULT_MODEL,
                    body.len(),
                    body
                )
                .as_bytes(),
            )
            .await
            .expect("backpressure fixture must write its held-open SSE body");
        response_socket
            .flush()
            .await
            .expect("backpressure fixture must flush its held-open SSE body");
        let _ = body_sent_tx.send(());

        let mut byte = [0u8; 1];
        loop {
            match response_socket.read(&mut byte).await {
                Ok(0) => break,
                Ok(_) => {}
                Err(error) => panic!(
                    "backpressure fixture must observe peer EOF, not a socket error: {error}"
                ),
            }
        }
        let _ = closed_tx.send(());
    });
    (
        format!("http://{address}/backend-api/codex"),
        body_sent_rx,
        closed_rx,
    )
}

fn text_delta_sse(count: usize, duplicate_final_sequence: bool) -> String {
    let mut body = format!(
        "event: response.created\ndata: {}\n\n",
        json!({"type":"response.created","sequence_number":1,"response":{"headers":{"openai-model":DEFAULT_MODEL}}})
    );
    for index in 0..count {
        body.push_str(&format!(
            "event: response.output_text.delta\ndata: {}\n\n",
            json!({
                "type":"response.output_text.delta",
                "sequence_number":index + 2,
                "item_id":"message-1",
                "output_index":0,
                "content_index":0,
                "delta":format!("delta-{index}")
            })
        ));
    }
    if duplicate_final_sequence {
        body.push_str(&format!(
            "event: response.output_text.delta\ndata: {}\n\n",
            json!({
                "type":"response.output_text.delta",
                "sequence_number":count + 1,
                "item_id":"message-1",
                "output_index":0,
                "content_index":0,
                "delta":"must-not-project"
            })
        ));
    }
    body
}

fn many_terminal_blocks_sse(count: usize) -> String {
    let output = (0..count)
        .map(|index| {
            json!({
                "type":"message",
                "role":"assistant",
                "content":[{"type":"output_text","text":format!("block-{index}")}]
            })
        })
        .collect::<Vec<_>>();
    format!(
        concat!(
            "event: response.created\ndata: {}\n\n",
            "event: response.completed\ndata: {}\n\n",
            "data: [DONE]\n\n"
        ),
        json!({"type":"response.created","sequence_number":1,"response":{"headers":{"openai-model":DEFAULT_MODEL}}}),
        json!({
            "type":"response.completed",
            "sequence_number":2,
            "response":{
                "id":"resp-terminal-backpressure",
                "output":output,
                "usage":{"input_tokens":1,"output_tokens":count}
            }
        })
    )
}

fn completed_sse_after_deltas(count: usize) -> String {
    let mut body = format!(
        "event: response.created\ndata: {}\n\n",
        json!({"type":"response.created","sequence_number":1,"response":{"headers":{"openai-model":DEFAULT_MODEL}}})
    );
    let mut text = String::new();
    for index in 0..count {
        let delta = format!("delta-{index}");
        text.push_str(&delta);
        body.push_str(&format!(
            "event: response.output_text.delta\ndata: {}\n\n",
            json!({
                "type":"response.output_text.delta",
                "sequence_number":index + 2,
                "item_id":"message-1",
                "output_index":0,
                "content_index":0,
                "delta":delta
            })
        ));
    }
    body.push_str(&format!(
        concat!(
            "event: response.output_item.done\ndata: {}\n\n",
            "event: response.completed\ndata: {}\n\n",
            "data: [DONE]\n\n"
        ),
        json!({
            "type":"response.output_item.done",
            "sequence_number":count + 2,
            "output_index":0,
            "item":{
                "type":"message",
                "role":"assistant",
                "content":[{"type":"output_text","text":text}]
            }
        }),
        json!({
            "type":"response.completed",
            "sequence_number":count + 3,
            "response":{
                "id":"resp-backpressure-complete",
                "status":"completed",
                "model":DEFAULT_MODEL,
                "output":[],
                "usage":{"input_tokens":12,"output_tokens":7}
            }
        })
    ));
    body
}

fn observe_stream_producer(
    provider: ChatGptSubscriptionProvider,
) -> (
    ChatGptSubscriptionProvider,
    mpsc::UnboundedReceiver<StreamProducerEvent>,
) {
    let (events_tx, events_rx) = mpsc::unbounded_channel();
    let observer = Arc::new(move |event| {
        let _ = events_tx.send(event);
    });
    (provider.with_stream_producer_observer(observer), events_rx)
}

/// Waits for the spawned streaming producer task to run to completion and
/// returns every send it attempted. Termination is observed through the
/// producer's own drop guard, so a producer that publishes its terminal
/// outcome and then parks forever fails here instead of leaking silently.
async fn wait_for_producer_finished(
    events: &mut mpsc::UnboundedReceiver<StreamProducerEvent>,
    case: &str,
) -> Vec<StreamChunk> {
    let mut attempts = Vec::new();
    let mut observer_closed = false;
    let mut panicked = false;
    let finished = tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            match events.recv().await {
                Some(StreamProducerEvent::Finished { panicking }) => {
                    panicked = panicking;
                    return;
                }
                Some(StreamProducerEvent::SendAttempt(chunk)) => attempts.push(chunk),
                None => {
                    observer_closed = true;
                    return;
                }
            }
        }
    })
    .await;
    assert!(
            finished.is_ok(),
            "{case}: streaming producer task did not terminate within 2s; observed_send_attempts={}; attempts={attempts:?}",
            attempts.len()
        );
    assert!(
            !observer_closed,
            "{case}: producer observer closed before the task reported termination; observed_send_attempts={}; attempts={attempts:?}",
            attempts.len()
        );
    // A panicking producer releases the transport and fires this guard
    // exactly as a clean one does, so on the paths that never drain the
    // receiver -- where no `outcome` vector exists to contradict it --
    // termination alone is not evidence of correct termination.
    assert!(
            !panicked,
            "{case}: the streaming producer task unwound instead of returning; the transport was released by a panic, not by the cancellation path under test; observed_send_attempts={}; attempts={attempts:?}",
            attempts.len()
        );
    attempts
}

async fn fragmented_subscription_server(body: String) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let mut request = vec![0u8; 16 * 1024];
        let (mut catalog_socket, _) = listener.accept().await.unwrap();
        let _ = catalog_socket.read(&mut request).await;
        let catalog = catalog_body();
        catalog_socket
                .write_all(
                    format!(
                        "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
                        catalog.len(),
                        catalog
                    )
                    .as_bytes(),
                )
                .await
                .unwrap();
        catalog_socket.flush().await.unwrap();
        drop(catalog_socket);

        let (mut response_socket, _) = listener.accept().await.unwrap();
        let _ = response_socket.read(&mut request).await;
        response_socket
                .write_all(
                    format!(
                        "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\nopenai-model: {DEFAULT_MODEL}\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
                        body.len()
                    )
                    .as_bytes(),
                )
                .await
                .unwrap();
        for byte in body.bytes() {
            response_socket.write_all(&[byte]).await.unwrap();
            response_socket.flush().await.unwrap();
            tokio::task::yield_now().await;
        }
    });
    format!("http://{address}/backend-api/codex")
}

async fn subscription_stream_outcome(
    body: String,
    header_model: &str,
) -> Vec<std::result::Result<StreamChunk, String>> {
    let mut server = mockito::Server::new_async().await;
    let _models = server
        .mock("GET", "/backend-api/codex/models")
        .match_query(mockito::Matcher::UrlEncoded(
            "client_version".into(),
            CHATGPT_CATALOG_CLIENT_VERSION.into(),
        ))
        .with_status(200)
        .with_body(catalog_body())
        .create_async()
        .await;
    let _inference = server
        .mock("POST", RESPONSES_PATH)
        .with_status(200)
        .with_header("content-type", "text/event-stream")
        .with_header("openai-model", header_model)
        .with_body(body)
        .create_async()
        .await;
    let provider = ChatGptSubscriptionProvider::for_test(
        Arc::new(StaticSource::new()),
        &format!("{}/backend-api/codex", server.url()),
        DEFAULT_MODEL,
    )
    .unwrap();
    let mut receiver = provider
        .send_message_stream(&ProviderRequest::new(vec![Message::user("hello")]))
        .await
        .unwrap();
    let mut outcome = Vec::new();
    while let Some(chunk) = receiver.recv().await {
        outcome.push(chunk.map_err(|error| error.to_string()));
    }
    outcome
}

#[test]
fn canonical_request_preserves_ordered_reasoning_tools_results_and_lite_shape() {
    let request = ProviderRequest::new(vec![
        Message::with_content(
            "user",
            vec![
                ContentBlock::text("inspect"),
                ContentBlock::image("image/png", VALID_PNG_BASE64),
            ],
        ),
        Message::with_content(
            "assistant",
            vec![
                ContentBlock::opaque_reasoning("encrypted-turn"),
                ContentBlock::ToolUse {
                    id: "call-1".to_string(),
                    name: "read".to_string(),
                    input: json!({"path":"README.md"}),
                },
            ],
        ),
        Message::with_content(
            "user",
            vec![ContentBlock::ToolResult {
                tool_use_id: "call-1".to_string(),
                content: "contents".to_string(),
                is_error: None,
            }],
        ),
    ])
    .with_model(DEFAULT_MODEL)
    .with_system("developer instructions")
    .with_tools(vec![tool()]);
    let body = encode_responses_lite(&request, ReasoningEffort::High).unwrap();
    assert_eq!(body["input"][0]["type"], "additional_tools");
    assert_eq!(body["input"][0]["role"], "developer");
    assert_eq!(
        body["input"][0]["id"],
        "at_06f0d744-9e74-54f8-9371-312adc3c666b"
    );
    assert_eq!(body["input"][0]["tools"][0]["type"], "namespace");
    assert_eq!(body["input"][0]["tools"][0]["name"], "functions");
    assert_eq!(body["input"][0]["tools"][0]["description"], "");
    assert_eq!(body["input"][0]["tools"][0]["tools"][0]["type"], "function");
    assert_eq!(body["input"][1]["role"], "developer");
    assert_eq!(
        body["input"][1]["id"],
        "msg_e26db3f8-834d-58b1-9d5e-5e9465345d82"
    );
    assert_eq!(body["input"][2]["content"][1]["type"], "input_image");
    assert_eq!(
        body["input"][2]["content"][1]["image_url"],
        format!("data:image/png;base64,{VALID_PNG_BASE64}")
    );
    assert_eq!(body["input"][3]["type"], "reasoning");
    assert_eq!(body["input"][4]["type"], "function_call");
    assert_eq!(body["input"][4]["namespace"], "functions");
    assert_eq!(body["input"][5]["type"], "function_call_output");
    assert_eq!(body["reasoning"]["context"], "all_turns");
    assert_eq!(body["include"], json!(["reasoning.encrypted_content"]));
    assert_eq!(body["store"], false);
    assert_eq!(body["stream"], true);
    assert!(body.get("instructions").is_none());
    assert!(body.get("tools").is_none());
    assert!(body.get("previous_response_id").is_none());
    assert!(body.get("prompt_cache_key").is_none());
    assert!(body.get("client_metadata").is_none());
}

#[test]
fn collaboration_tools_use_reserved_wire_aliases_and_replay_symmetrically() {
    let request = ProviderRequest::new(vec![
        Message::user("delegate"),
        Message::with_content(
            "assistant",
            vec![ContentBlock::ToolUse {
                id: "call-agent".to_string(),
                name: "spawn_agent".to_string(),
                input: json!({"task":"say your name"}),
            }],
        ),
    ])
    .with_model(DEFAULT_MODEL)
    .with_tools(vec![named_tool("spawn_agent"), tool()]);
    let body = encode_responses_lite(&request, ReasoningEffort::High)
        .expect("advertised collaboration tool should map to a safe wire alias");
    let tools = body["input"][0]["tools"][0]["tools"]
        .as_array()
        .expect("additional-tools item should contain a function array");
    assert_eq!(
        tools
            .iter()
            .filter(|tool| tool["name"] == "finch_spawn_agent")
            .count(),
        1,
        "spawn_agent was not advertised exactly once under its reserved Finch wire alias; \
             tools={tools:?}"
    );
    assert!(
        tools.iter().any(|tool| tool["name"] == "read"),
        "ordinary tools should retain their local name on the wire; tools={tools:?}"
    );
    let replay = body["input"]
        .as_array()
        .expect("request input should be an array")
        .iter()
        .find(|item| item["type"] == "function_call")
        .expect("assistant tool history should produce a function-call replay item");
    assert_eq!(
            replay["name"], "finch_spawn_agent",
            "collaboration tool history did not replay with the advertised wire alias; replay={replay:?}"
        );
    assert_eq!(
            replay["namespace"], "functions",
            "collaboration tool history did not replay in the advertised functions namespace; replay={replay:?}"
        );

    let collision = ProviderRequest::new(vec![Message::user("delegate")])
        .with_model(DEFAULT_MODEL)
        .with_tools(vec![named_tool("finch_spawn_agent")]);
    let error = encode_responses_lite(&collision, ReasoningEffort::High)
        .expect_err("a local tool must not claim Finch's reserved collaboration wire alias");
    assert!(
        error.to_string().contains("reserved wire tool name"),
        "reserved wire alias collision returned the wrong diagnostic: {error:#}"
    );

    let malformed_namespace = parse_output_item(
        &json!({
            "type":"function_call",
            "call_id":"malformed-namespace",
            "name":"read",
            "namespace":false,
            "arguments":"{\"path\":\"README.md\"}"
        }),
        &mut Vec::new(),
        &mut HashSet::new(),
        &test_tool_bindings(&["read"]),
    )
    .expect_err("a non-string function namespace must be rejected");
    assert_eq!(
        malformed_namespace.to_string(),
        "ChatGPT function call namespace was invalid",
        "malformed function namespace returned the wrong diagnostic"
    );
}

#[test]
fn responses_lite_prompt_item_ids_are_stable_and_payload_bound() {
    let request = ProviderRequest::new(vec![Message::user("hello")])
        .with_model(DEFAULT_MODEL)
        .with_system("developer instructions")
        .with_tools(vec![tool()]);
    let first = encode_responses_lite(&request, ReasoningEffort::High).unwrap();
    let retry = encode_responses_lite(&request, ReasoningEffort::High).unwrap();
    assert_eq!(first["input"][0]["id"], retry["input"][0]["id"]);
    assert_eq!(first["input"][1]["id"], retry["input"][1]["id"]);

    let changed = encode_responses_lite(
        &request.with_system("different developer instructions"),
        ReasoningEffort::High,
    )
    .unwrap();
    assert_eq!(first["input"][0]["id"], changed["input"][0]["id"]);
    assert_ne!(first["input"][1]["id"], changed["input"][1]["id"]);
    assert!(first["input"][0]["id"]
        .as_str()
        .is_some_and(|id| id.starts_with("at_") && id.len() == 39));
    assert!(first["input"][1]["id"]
        .as_str()
        .is_some_and(|id| id.starts_with("msg_") && id.len() == 40));
}

#[test]
fn catalog_and_capability_alias_are_pinned_to_responses_lite() {
    let catalog = parse_catalog(catalog_body().as_bytes()).unwrap();
    assert!(catalog.models.contains_key(DEFAULT_MODEL));
    assert!(catalog.models.contains_key(MODEL_ALIAS));
    for model in [DEFAULT_MODEL, MODEL_ALIAS] {
        let capability = subscription_capabilities(model);
        assert_eq!(
            capability.wire_protocol.protocol,
            Some(WireProtocol::OpenAiChatGptResponsesLite)
        );
        assert!(capability.image_input.is_supported());
        assert!(capability.continuation.is_supported());
        assert_eq!(capability.context_window.max_tokens, None);
    }
    assert!(subscription_capabilities("gpt-4o")
        .wire_protocol
        .protocol
        .is_none());
}

#[test]
fn catalog_accepts_and_preserves_authoritative_bounded_context_windows() {
    for (sol, alias) in [(200_000, 262_144), (1_000_000, 1_050_000)] {
        let catalog =
            parse_catalog(catalog_body_with_context_windows(sol, alias).as_bytes()).unwrap();
        assert_eq!(catalog.models[DEFAULT_MODEL].context_window, sol as usize);
        assert_eq!(catalog.models[MODEL_ALIAS].context_window, alias as usize);
    }
    for slug in [DEFAULT_MODEL, MODEL_ALIAS] {
        let catalog = parse_catalog(single_model_catalog_body(slug, 1_000_000).as_bytes())
            .expect("either exact selectable identifier is sufficient");
        assert_eq!(catalog.models.len(), 1);
        assert_eq!(catalog.models[slug].context_window, 1_000_000);
    }
}

#[test]
fn catalog_rejects_missing_malformed_zero_and_excessive_context_windows() {
    let assert_invalid = |catalog: Value| {
        let error = parse_catalog(catalog.to_string().as_bytes())
            .err()
            .expect("invalid context window must fail");
        assert!(error.is::<SubscriptionCatalogContextWindowInvalid>());
        error.to_string()
    };

    let mut missing: Value = serde_json::from_str(&catalog_body()).unwrap();
    missing["models"][0]
        .as_object_mut()
        .unwrap()
        .remove("context_window");
    assert!(assert_invalid(missing).contains("missing or malformed"));

    let mut malformed: Value = serde_json::from_str(&catalog_body()).unwrap();
    malformed["models"][0]["context_window"] = json!("1000000");
    assert!(assert_invalid(malformed).contains("missing or malformed"));

    let mut zero: Value = serde_json::from_str(&catalog_body()).unwrap();
    zero["models"][0]["context_window"] = json!(0);
    let zero_error = assert_invalid(zero);
    assert!(zero_error.contains("context window 0"));
    assert!(!zero_error.contains("models"));

    let mut excessive: Value = serde_json::from_str(&catalog_body()).unwrap();
    excessive["models"][0]["context_window"] = json!(MAX_CATALOG_CONTEXT_WINDOW + 1);
    let excessive_error = assert_invalid(excessive);
    assert!(excessive_error.contains(&(MAX_CATALOG_CONTEXT_WINDOW + 1).to_string()));
    assert!(!excessive_error.contains("models"));
}

#[test]
fn catalog_context_metadata_does_not_weaken_slug_api_or_modality_checks() {
    let mut wrong_slug: Value =
        serde_json::from_str(&single_model_catalog_body(DEFAULT_MODEL, 1_000_000)).unwrap();
    wrong_slug["models"][0]["slug"] = json!("gpt-5.6-sol-impostor");
    let wrong_slug_error = parse_catalog(wrong_slug.to_string().as_bytes())
        .err()
        .expect("wrong slug must fail");
    assert!(wrong_slug_error.is::<SubscriptionCatalogNoSelectableModel>());

    let mut unsupported_api: Value =
        serde_json::from_str(&single_model_catalog_body(DEFAULT_MODEL, 1_000_000)).unwrap();
    unsupported_api["models"][0]["supported_in_api"] = json!(false);
    let unsupported_error = parse_catalog(unsupported_api.to_string().as_bytes())
        .err()
        .expect("unsupported API model must fail");
    assert!(unsupported_error.is::<SubscriptionCatalogNoSelectableModel>());

    let mut missing_image: Value =
        serde_json::from_str(&single_model_catalog_body(DEFAULT_MODEL, 1_000_000)).unwrap();
    missing_image["models"][0]["input_modalities"] = json!(["text"]);
    assert!(parse_catalog(missing_image.to_string().as_bytes())
        .err()
        .expect("missing image modality must fail")
        .to_string()
        .contains("catalog capabilities drifted"));

    let mut missing_text: Value =
        serde_json::from_str(&single_model_catalog_body(DEFAULT_MODEL, 1_000_000)).unwrap();
    missing_text["models"][0]["input_modalities"] = json!(["image"]);
    let missing_text_error = parse_catalog(missing_text.to_string().as_bytes())
        .err()
        .expect("missing text modality must fail");
    assert!(missing_text_error.is::<SubscriptionCatalogNoSelectableModel>());
}

#[test]
fn test_compatibility_version_is_pinned_to_audited_codex_not_finch_package() {
    assert_eq!(CHATGPT_CATALOG_CLIENT_VERSION, "0.151.0");
    assert_ne!(CHATGPT_CATALOG_CLIENT_VERSION, env!("CARGO_PKG_VERSION"));
}

#[test]
fn chatgpt_user_agent_is_static_bounded_and_has_no_user_identity() {
    assert_eq!(
        FINCH_CHATGPT_USER_AGENT,
        concat!(
            "finch/",
            env!("CARGO_PKG_VERSION"),
            " (+https://darwin-finch.github.io/)"
        )
    );
    assert!(FINCH_CHATGPT_USER_AGENT.len() <= 256);
    for private_value in [
        "shammah",
        "Shammahs-MacBook-Air.local",
        "brain-identifier",
        "account-identifier",
        "credential-identifier",
    ] {
        assert!(!FINCH_CHATGPT_USER_AGENT.contains(private_value));
    }
}

#[tokio::test]
async fn empty_catalog_is_typed_actionable_and_secret_free() {
    let access_secret = "empty-catalog-access-secret";
    let account_secret = "empty-catalog-account-secret";
    let mut server = mockito::Server::new_async().await;
    let models = server
        .mock("GET", "/backend-api/codex/models")
        .match_query(mockito::Matcher::UrlEncoded(
            "client_version".into(),
            "0.151.0".into(),
        ))
        .match_header("authorization", format!("Bearer {access_secret}").as_str())
        .match_header("chatgpt-account-id", account_secret)
        .match_header("originator", "finch")
        .match_header("user-agent", FINCH_CHATGPT_USER_AGENT)
        .with_status(200)
        .with_body(json!({"models": []}).to_string())
        .create_async()
        .await;
    let provider = ChatGptSubscriptionProvider::for_test(
        Arc::new(StaticSource::new()),
        &format!("{}/backend-api/codex", server.url()),
        DEFAULT_MODEL,
    )
    .unwrap();
    let error = provider
        .account_catalog(
            &ChatGptCredentialLease {
                access_token: access_secret.into(),
                account: account_secret.into(),
                generation: "generation-empty-catalog".into(),
            },
            &CancellationToken::new(),
        )
        .await
        .err()
        .expect("empty catalog must fail");
    assert!(error.is::<SubscriptionCatalogUnavailable>());
    let rendered = error.to_string();
    assert!(rendered.contains("pinned Codex compatibility version 0.151.0"));
    assert!(rendered.contains("entitlement or server compatibility filtering"));
    assert!(!rendered.contains(access_secret));
    assert!(!rendered.contains(account_secret));
    models.assert_async().await;
}

#[test]
fn catalog_ignores_unrelated_models_and_accepts_one_pinned_identifier() {
    let mut catalog: Value = serde_json::from_str(&catalog_body()).unwrap();
    catalog["models"].as_array_mut().unwrap().push(json!({
        "slug":"unrelated-account-model",
        "context_window":"attacker-controlled-not-a-number"
    }));
    let parsed = parse_catalog(catalog.to_string().as_bytes()).unwrap();
    assert_eq!(parsed.models.len(), 2);

    catalog["models"]
        .as_array_mut()
        .unwrap()
        .retain(|model| model["slug"] != MODEL_ALIAS);
    let parsed = parse_catalog(catalog.to_string().as_bytes())
        .expect("one exact selectable identifier is sufficient");
    assert_eq!(parsed.models.len(), 1);
    assert!(parsed.models.contains_key(DEFAULT_MODEL));
}

#[test]
fn test_actual_model_uses_authoritative_header_or_requested_route_and_never_payload_model() {
    let allowed = empty_tool_bindings();
    let terminal = json!({
        "id":"resp-1",
        "status":"completed",
        "model":"attacker-payload-model",
        "output":[],
        "usage":{"input_tokens":1,"output_tokens":2,"total_tokens":3}
    });
    let mut accumulator = StreamAccumulator::default();
    let completed = parse_completed(
        terminal.as_object().unwrap(),
        DEFAULT_MODEL,
        Some(DEFAULT_MODEL),
        &allowed,
        &mut accumulator,
    )
    .expect("a valid authoritative header must complete successfully");
    assert_eq!(
        completed.model, DEFAULT_MODEL,
        "authoritative header model was not retained; observed_model={}",
        completed.model
    );

    let mut accumulator = StreamAccumulator::default();
    let completed = parse_completed(
        terminal.as_object().unwrap(),
        DEFAULT_MODEL,
        None,
        &allowed,
        &mut accumulator,
    )
    .expect("missing authoritative model provenance must retain the validated requested route");
    assert_eq!(
        completed.model, DEFAULT_MODEL,
        "missing provenance must not copy the untrusted terminal payload model"
    );
}

#[test]
fn test_terminal_background_accepts_null_and_false_but_rejects_other_values() {
    for (case, value) in [("null", Value::Null), ("false", json!(false))] {
        let terminal = json!({
            "id":"resp-background",
            "status":"completed",
            "model":DEFAULT_MODEL,
            "output":[],
            "background":value
        });
        parse_completed(
            terminal.as_object().unwrap(),
            DEFAULT_MODEL,
            Some(DEFAULT_MODEL),
            &empty_tool_bindings(),
            &mut StreamAccumulator::default(),
        )
        .unwrap_or_else(|error| panic!("documented {case} background state failed: {error:#}"));
    }

    for (case, value) in [("true", json!(true)), ("string", json!("false"))] {
        let background = json!({
            "id":"resp-background",
            "status":"completed",
            "model":DEFAULT_MODEL,
            "output":[],
            "background":value
        });
        let error = parse_completed(
            background.as_object().unwrap(),
            DEFAULT_MODEL,
            Some(DEFAULT_MODEL),
            &empty_tool_bindings(),
            &mut StreamAccumulator::default(),
        )
        .err()
        .unwrap_or_else(|| panic!("{case} background execution unexpectedly succeeded"));
        assert!(
            error.to_string().contains("background state was invalid"),
            "{case} background state returned an unhelpful diagnostic: {error:#}"
        );
    }
}

#[test]
fn test_terminal_tool_usage_is_bounded_without_requiring_an_object() {
    let terminal = json!({
        "id":"resp-tool-usage",
        "status":"completed",
        "model":DEFAULT_MODEL,
        "output":[],
        "tool_usage":[{"name":"read","calls":1}]
    });
    parse_completed(
        terminal.as_object().unwrap(),
        DEFAULT_MODEL,
        Some(DEFAULT_MODEL),
        &empty_tool_bindings(),
        &mut StreamAccumulator::default(),
    )
    .expect("bounded passive tool usage must not require a provider-specific shape");

    for value in [json!(true), json!("x".repeat(MAX_TOOL_ARGUMENT_BYTES - 2))] {
        let mut bounded = terminal.clone();
        bounded["tool_usage"] = value;
        parse_completed(
            bounded.as_object().unwrap(),
            DEFAULT_MODEL,
            Some(DEFAULT_MODEL),
            &empty_tool_bindings(),
            &mut StreamAccumulator::default(),
        )
        .expect("opaque tool usage at or below the inclusive byte limit must be accepted");
    }

    let mut excessive = terminal;
    excessive["tool_usage"] = json!("x".repeat(MAX_TOOL_ARGUMENT_BYTES - 1));
    let error = parse_completed(
        excessive.as_object().unwrap(),
        DEFAULT_MODEL,
        Some(DEFAULT_MODEL),
        &empty_tool_bindings(),
        &mut StreamAccumulator::default(),
    )
    .err()
    .expect("oversized passive tool usage must remain bounded");
    assert!(
        error
            .to_string()
            .contains("tool usage metadata exceeded the size limit"),
        "oversized tool usage returned an unhelpful diagnostic: {error:#}"
    );
}

#[test]
fn test_terminal_unknown_fields_remain_fail_closed() {
    let sentinel = "sk-proj-SensitiveToken123";
    let terminal = json!({
        "id":"resp-unknown-terminal",
        "status":"completed",
        "model":DEFAULT_MODEL,
        "output":[],
        sentinel:true
    });
    let error = parse_completed(
        terminal.as_object().unwrap(),
        DEFAULT_MODEL,
        Some(DEFAULT_MODEL),
        &empty_tool_bindings(),
        &mut StreamAccumulator::default(),
    )
    .err()
    .expect("unaudited terminal semantics must remain fail closed");
    assert_eq!(
        error.to_string(),
        "ChatGPT subscription terminal response contained an unknown field",
        "unknown terminal semantics must report only the static containing object"
    );
    assert!(
        !error.to_string().contains(sentinel),
        "unknown terminal semantics reflected response-body field data"
    );
}

#[test]
fn test_usage_extra_requires_object_metadata() {
    for value in [json!(false), json!("opaque")] {
        let usage = json!({
            "input_tokens":12,
            "output_tokens":7,
            "total_tokens":19,
            "extra":value
        });
        let error = parse_usage(Some(&usage))
            .err()
            .expect("non-object usage extra metadata must remain fail closed");
        assert_eq!(
            error.to_string(),
            "ChatGPT response usage extra metadata was invalid",
            "non-object usage extra metadata returned an unhelpful diagnostic"
        );
    }
}

#[tokio::test]
async fn test_buffered_and_streaming_reject_non_object_usage_extra_before_terminal_effects() {
    let terminal = json!({
        "id":"resp-invalid-usage-extra",
        "status":"completed",
        "model":DEFAULT_MODEL,
        "output":[],
        "usage":{
            "input_tokens":12,
            "output_tokens":7,
            "total_tokens":19,
            "extra":false
        }
    });
    let (buffered, outcome) = run_streamed_message_terminal_response(terminal).await;
    let expected = "ChatGPT response usage extra metadata was invalid";
    assert_eq!(
        buffered.err().as_deref(),
        Some(expected),
        "non-object usage extra crossed the buffered provider boundary"
    );
    assert!(
        matches!(
            outcome.as_slice(),
            [Ok(StreamChunk::TextDelta(delta)), Err(error)]
                if delta == "hello" && error == expected
        ),
        "non-object usage extra must end with one error and no terminal effects; \
             outcome={outcome:?}"
    );
}

#[tokio::test]
async fn test_buffered_and_streaming_bound_multiline_usage_extra_event() {
    let first_items = "0,".repeat(300_000);
    let second_items = "0,".repeat(300_000);
    let response_body = format!(
        concat!(
            "event: response.created\ndata: {}\n\n",
            "event: response.output_text.delta\ndata: {}\n\n",
            "event: response.output_item.done\ndata: {}\n\n",
            "event: response.completed\n",
            "data: {{\"type\":\"response.completed\",\"sequence_number\":4,",
            "\"response\":{{\"id\":\"resp-oversized-usage-extra\",",
            "\"status\":\"completed\",\"model\":\"{}\",\"output\":[],",
            "\"usage\":{{\"input_tokens\":12,\"output_tokens\":7,",
            "\"total_tokens\":19,\"extra\":{{\"items\":[{}\n",
            "data: {}0]}}}}}}}}\n\n"
        ),
        json!({"type":"response.created","sequence_number":1,"response":{"headers":{"openai-model":DEFAULT_MODEL}}}),
        json!({"type":"response.output_text.delta","sequence_number":2,"item_id":"message-1","output_index":0,"content_index":0,"delta":"hello"}),
        json!({"type":"response.output_item.done","sequence_number":3,"output_index":0,"item":{"type":"message","role":"assistant","content":[{"type":"output_text","text":"hello"}]}}),
        DEFAULT_MODEL,
        first_items,
        second_items,
    );
    let completed_event = response_body
        .split("event: response.completed\n")
        .nth(1)
        .expect("oversized completed event fixture must exist");
    assert!(
        completed_event.len() > MAX_SSE_EVENT_BYTES,
        "oversized completed event fixture drifted below the aggregate limit"
    );
    assert!(
        completed_event
            .lines()
            .all(|line| line.len() <= MAX_SSE_LINE_BYTES),
        "aggregate event fixture accidentally exceeded the per-line limit"
    );

    let (buffered, outcome) = run_streamed_message_sse(response_body).await;
    let expected = "ChatGPT subscription stream event exceeded the size limit";
    assert_eq!(
        buffered.err().as_deref(),
        Some(expected),
        "oversized multiline usage event crossed the buffered provider boundary"
    );
    assert!(
        matches!(
            outcome.as_slice(),
            [Ok(StreamChunk::TextDelta(delta)), Err(error)]
                if delta == "hello" && error == expected
        ),
        "oversized multiline usage event must end with one error and no terminal effects; \
             outcome={outcome:?}"
    );
}

#[tokio::test]
async fn test_buffered_and_streaming_accept_large_usage_extra_within_event_bound() {
    let large_extra = json!({"p":"x".repeat(128 * 1024 - 8)});
    assert_eq!(
        serde_json::to_vec(&large_extra).unwrap().len(),
        128 * 1024,
        "large usage-extra fixture drifted"
    );
    let terminal = json!({
        "id":"resp-large-usage-extra",
        "status":"completed",
        "model":DEFAULT_MODEL,
        "output":[],
        "usage":{
            "input_tokens":12,
            "output_tokens":7,
            "total_tokens":19,
            "extra":large_extra
        }
    });
    let (buffered, outcome) = run_streamed_message_terminal_response(terminal).await;
    let buffered = buffered.unwrap_or_else(|error| {
        panic!("large usage extra metadata within the event bound failed: {error}")
    });
    assert_eq!(
        buffered
            .usage
            .as_ref()
            .map(|usage| (usage.input_tokens, usage.output_tokens)),
        Some((12, 7)),
        "large passive metadata changed buffered token accounting"
    );
    assert!(
        matches!(
            outcome.as_slice(),
            [
                Ok(StreamChunk::TextDelta(delta)),
                Ok(StreamChunk::ResponseMetadata { model }),
                Ok(StreamChunk::Usage {
                    input_tokens: 12,
                    output_tokens: 7,
                }),
                Ok(StreamChunk::Allowance {
                    primary_used_percent: Some(primary),
                    secondary_used_percent: None,
                }),
                Ok(StreamChunk::ContentBlockComplete(ContentBlock::Text { text })),
            ] if delta == "hello"
                && model == DEFAULT_MODEL
                && *primary == 25.5
                && text == "hello"
        ),
        "large passive metadata changed exact ordered streaming effects; outcome={outcome:?}"
    );
}

#[tokio::test]
async fn test_buffered_and_streaming_accept_bounded_usage_extra_metadata() {
    let terminal = json!({
        "id":"resp-usage-extra",
        "status":"completed",
        "model":DEFAULT_MODEL,
        "output":[],
        "usage":{
            "input_tokens":12,
            "output_tokens":7,
            "total_tokens":19,
            "extra":{"label":"example","items":[0,null,true]}
        }
    });
    let (buffered, outcome) = run_streamed_message_terminal_response(terminal).await;
    let buffered = buffered.unwrap_or_else(|error| {
        panic!("bounded official usage extra metadata failed buffered parsing: {error}")
    });
    assert!(
        matches!(buffered.content.as_slice(), [ContentBlock::Text { text }] if text == "hello"),
        "usage extra metadata changed buffered semantic content; response={buffered:?}"
    );
    assert_eq!(
        buffered
            .usage
            .as_ref()
            .map(|usage| (usage.input_tokens, usage.output_tokens)),
        Some((12, 7)),
        "usage extra metadata changed token accounting"
    );
    assert_eq!(
        buffered.model, DEFAULT_MODEL,
        "usage extra metadata changed buffered model provenance"
    );
    assert!(
        matches!(
            outcome.as_slice(),
            [
                Ok(StreamChunk::TextDelta(delta)),
                Ok(StreamChunk::ResponseMetadata { model }),
                Ok(StreamChunk::Usage {
                    input_tokens: 12,
                    output_tokens: 7,
                }),
                Ok(StreamChunk::Allowance {
                    primary_used_percent: Some(primary),
                    secondary_used_percent: None,
                }),
                Ok(StreamChunk::ContentBlockComplete(ContentBlock::Text { text })),
            ] if delta == "hello"
                && model == DEFAULT_MODEL
                && *primary == 25.5
                && text == "hello"
        ),
        "usage extra metadata did not preserve exact ordered streaming effects; \
             outcome={outcome:?}"
    );
}

#[tokio::test]
async fn test_buffered_and_streaming_accept_bounded_usage_attribution_metadata() {
    let terminal = json!({
        "id":"resp-usage-attribution",
        "status":"completed",
        "model":DEFAULT_MODEL,
        "output":[],
        "usage":{
            "input_tokens":12,
            "output_tokens":7,
            "total_tokens":19,
            "attribution":{
                "items":{
                    "dynamic-attribution-id":{
                        "input_tokens":9001,
                        "output_tokens":8002
                    }
                }
            }
        }
    });
    let (buffered, outcome) = run_streamed_message_terminal_response(terminal).await;
    let buffered = buffered.unwrap_or_else(|error| {
        panic!("bounded usage attribution metadata failed buffered parsing: {error}")
    });
    assert!(
        matches!(buffered.content.as_slice(), [ContentBlock::Text { text }] if text == "hello"),
        "usage attribution metadata changed buffered semantic content; response={buffered:?}"
    );
    assert_eq!(
        buffered
            .usage
            .as_ref()
            .map(|usage| (usage.input_tokens, usage.output_tokens)),
        Some((12, 7)),
        "usage attribution metadata changed token accounting"
    );
    assert_eq!(
        buffered.model, DEFAULT_MODEL,
        "usage attribution metadata changed buffered model provenance"
    );
    assert!(
        matches!(
            outcome.as_slice(),
            [
                Ok(StreamChunk::TextDelta(delta)),
                Ok(StreamChunk::ResponseMetadata { model }),
                Ok(StreamChunk::Usage {
                    input_tokens: 12,
                    output_tokens: 7,
                }),
                Ok(StreamChunk::Allowance {
                    primary_used_percent: Some(primary),
                    secondary_used_percent: None,
                }),
                Ok(StreamChunk::ContentBlockComplete(ContentBlock::Text { text })),
            ] if delta == "hello"
                && model == DEFAULT_MODEL
                && *primary == 25.5
                && text == "hello"
        ),
        "usage attribution metadata did not preserve exact ordered streaming effects; \
             outcome={outcome:?}"
    );
}

#[tokio::test]
async fn test_buffered_and_streaming_reject_invalid_usage_attribution_before_terminal_effects() {
    for (case, attribution, expected) in [
        (
            "non-object",
            json!(false),
            "ChatGPT response usage attribution metadata was invalid",
        ),
        (
            "oversized-object",
            json!({"payload":"x".repeat(MAX_USAGE_METADATA_BYTES)}),
            "ChatGPT response usage attribution metadata exceeded the size limit",
        ),
    ] {
        let terminal = json!({
            "id":format!("resp-invalid-usage-attribution-{case}"),
            "status":"completed",
            "model":DEFAULT_MODEL,
            "output":[],
            "usage":{
                "input_tokens":12,
                "output_tokens":7,
                "total_tokens":19,
                "attribution":attribution
            }
        });
        let (buffered, outcome) = run_streamed_message_terminal_response(terminal).await;
        assert_eq!(
            buffered.err().as_deref(),
            Some(expected),
            "{case} usage attribution crossed the buffered provider boundary"
        );
        assert!(
            matches!(
                outcome.as_slice(),
                [Ok(StreamChunk::TextDelta(delta)), Err(error)]
                    if delta == "hello" && error == expected
            ),
            "{case} usage attribution must end with one error and no terminal effects; \
                 outcome={outcome:?}"
        );
    }
}

#[tokio::test]
async fn test_buffered_and_streaming_unknown_usage_fields_report_only_static_location() {
    let sentinel = "sk-proj-SensitiveToken123";
    let mut usage = json!({
        "input_tokens":12,
        "output_tokens":7,
        "total_tokens":19
    });
    usage
        .as_object_mut()
        .expect("usage fixture must be an object")
        .insert(sentinel.to_string(), Value::Null);
    let terminal = json!({
        "id":"resp-unknown-usage",
        "status":"completed",
        "model":DEFAULT_MODEL,
        "output":[],
        "usage":usage
    });
    let (buffered, outcome) = run_streamed_message_terminal_response(terminal).await;
    let expected = "ChatGPT subscription response usage contained an unknown field";
    let buffered_error = buffered
        .err()
        .expect("unknown usage semantics passed the buffered provider boundary");
    assert_eq!(buffered_error, expected);
    assert!(
        !buffered_error.contains(sentinel),
        "buffered error reflected response-body field data"
    );
    assert!(
        matches!(
            outcome.as_slice(),
            [Ok(StreamChunk::TextDelta(delta)), Err(error)]
                if delta == "hello" && error == expected && !error.contains(sentinel)
        ),
        "unknown usage semantics must end with exactly one static-location error and no \
             terminal metadata, usage, allowance, or completed content; outcome={outcome:?}"
    );
}

#[test]
fn test_terminal_prompt_cache_diagnostics_are_object_shaped_and_bounded() {
    let exactly_bounded = json!({"p":"x".repeat(MAX_TOOL_ARGUMENT_BYTES - 8)});
    assert_eq!(
        serde_json::to_vec(&exactly_bounded).unwrap().len(),
        MAX_TOOL_ARGUMENT_BYTES,
        "prompt cache diagnostics exact-bound fixture drifted"
    );
    validate_documented_response_metadata(
        json!({"prompt_cache_diagnostics":exactly_bounded})
            .as_object()
            .unwrap(),
    )
    .expect("prompt cache diagnostics at the inclusive byte limit must be accepted");

    for (case, value) in [
        ("non-object", json!(false)),
        (
            "oversized-object",
            json!({"payload":"x".repeat(MAX_TOOL_ARGUMENT_BYTES)}),
        ),
    ] {
        let terminal = json!({
            "id":"resp-cache-diagnostics",
            "status":"completed",
            "model":DEFAULT_MODEL,
            "output":[],
            "prompt_cache_diagnostics":value
        });
        let error = parse_completed(
            terminal.as_object().unwrap(),
            DEFAULT_MODEL,
            Some(DEFAULT_MODEL),
            &empty_tool_bindings(),
            &mut StreamAccumulator::default(),
        )
        .err()
        .unwrap_or_else(|| panic!("{case} prompt cache diagnostics unexpectedly succeeded"));
        assert!(
            error
                .to_string()
                .contains("prompt_cache_diagnostics was invalid"),
            "{case} prompt cache diagnostics returned an unhelpful diagnostic: {error:#}"
        );
    }
}

#[test]
fn test_terminal_prompt_cache_options_and_retention_are_strict() {
    for (case, metadata) in [
        (
            "explicit-without-comparison",
            json!({"prompt_cache_options":{"mode":"explicit","ttl":"30m"}}),
        ),
        (
            "in-memory-retention",
            json!({"prompt_cache_retention":"in_memory"}),
        ),
        (
            "bounded-cache-key",
            json!({"prompt_cache_key":"k".repeat(256)}),
        ),
    ] {
        validate_documented_response_metadata(metadata.as_object().unwrap())
            .unwrap_or_else(|error| panic!("valid {case} metadata failed: {error:#}"));
    }

    for (case, metadata) in [
        (
            "invalid-mode",
            json!({"prompt_cache_options":{"mode":"future","ttl":"30m"}}),
        ),
        (
            "missing-ttl",
            json!({"prompt_cache_options":{"mode":"implicit"}}),
        ),
        (
            "unknown-option",
            json!({"prompt_cache_options":{"mode":"implicit","ttl":"30m","future":true}}),
        ),
        (
            "invalid-comparison-id",
            json!({"prompt_cache_options":{"mode":"implicit","ttl":"30m","comparison_response_id":""}}),
        ),
        (
            "invalid-retention",
            json!({"prompt_cache_retention":"forever"}),
        ),
        ("non-string-cache-key", json!({"prompt_cache_key":false})),
        (
            "oversized-cache-key",
            json!({"prompt_cache_key":"k".repeat(257)}),
        ),
    ] {
        let error = validate_documented_response_metadata(metadata.as_object().unwrap())
            .err()
            .unwrap_or_else(|| panic!("{case} prompt cache metadata unexpectedly succeeded"));
        assert!(
            error.to_string().contains("invalid")
                || error.to_string().contains("unknown field")
                || error.to_string().contains("exceeded the size limit"),
            "{case} prompt cache metadata returned an unhelpful diagnostic: {error:#}"
        );
    }
}

#[test]
fn test_output_text_metadata_is_array_shaped_and_count_bounded() {
    for field in ["annotations", "logprobs"] {
        let mut accepted = json!({
            "type":"message",
            "role":"assistant",
            "content":[{"type":"output_text","text":"hello"}]
        });
        accepted["content"][0][field] = json!(vec![Value::Null; 256]);
        parse_output_item(
            &accepted,
            &mut Vec::new(),
            &mut HashSet::new(),
            &empty_tool_bindings(),
        )
        .unwrap_or_else(|error| {
            panic!("{field} metadata at the inclusive count limit failed: {error:#}")
        });

        for (case, value) in [
            ("non-array", json!(false)),
            ("257 entries", json!(vec![Value::Null; 257])),
        ] {
            let mut rejected = json!({
                "type":"message",
                "role":"assistant",
                "content":[{"type":"output_text","text":"hello"}]
            });
            rejected["content"][0][field] = value;
            let error = parse_output_item(
                &rejected,
                &mut Vec::new(),
                &mut HashSet::new(),
                &empty_tool_bindings(),
            )
            .err()
            .unwrap_or_else(|| panic!("{case} {field} metadata unexpectedly succeeded"));
            assert!(
                error.to_string().contains("metadata was invalid")
                    || error
                        .to_string()
                        .contains("metadata exceeded the size limit"),
                "{case} {field} metadata returned an unhelpful diagnostic: {error:#}"
            );
        }
    }
}

#[test]
fn test_event_obfuscation_must_be_string_padding() {
    let event = json!({"type":"response.created","obfuscation":{"future":true}});
    let error = exact_event_keys(
        event.as_object().unwrap(),
        &["type"],
        "response created event",
    )
    .err()
    .expect("structured obfuscation must not bypass event validation");
    assert!(
        error.to_string().contains("padding was invalid"),
        "structured obfuscation returned an unhelpful diagnostic: {error:#}"
    );
}

#[test]
fn test_unknown_event_fields_report_only_static_location() {
    let sentinel = "sk-proj-SensitiveToken123";
    let event = json!({"type":"response.created",sentinel:null});
    let error = exact_event_keys(
        event.as_object().expect("event fixture must be an object"),
        &["type"],
        "response created event",
    )
    .err()
    .expect("unknown event semantics must remain fail closed");
    assert_eq!(
        error.to_string(),
        "ChatGPT subscription response created event contained an unknown field",
        "unknown event semantics must report only the static event location"
    );
    assert!(
        !error.to_string().contains(sentinel),
        "unknown event semantics reflected response-body field data"
    );
}

#[test]
fn test_terminal_output_rejects_semantic_drift_after_passive_normalization() {
    let streamed = json!({
        "type":"message",
        "role":"assistant",
        "content":[{"type":"output_text","text":"hello","annotations":[],"logprobs":[]}]
    });
    let terminal = json!({
        "id":"resp-semantic-drift",
        "status":"completed",
        "model":DEFAULT_MODEL,
        "output":[{
            "type":"message",
            "role":"assistant",
            "content":[{"type":"output_text","text":"different"}]
        }]
    });
    let mut accumulator = StreamAccumulator::default();
    accumulator.output_items.insert(0, streamed);
    let error = parse_completed(
        terminal.as_object().unwrap(),
        DEFAULT_MODEL,
        Some(DEFAULT_MODEL),
        &empty_tool_bindings(),
        &mut accumulator,
    )
    .err()
    .expect("terminal text drift must remain fail closed");
    assert!(
        error.to_string().contains(
            "terminal output item 0 (message) did not match streamed output item 0 \
                 (message); terminal_count=1, streamed_count=1"
        ),
        "terminal semantic drift returned an unhelpful diagnostic: {error:#}"
    );
}

#[test]
fn malformed_unknown_and_misordered_terminal_events_fail_closed() {
    let mut accumulator = StreamAccumulator::default();
    let unknown = json!({"type":"response.future","sequence_number":1});
    assert!(parse_event(
        unknown,
        DEFAULT_MODEL,
        None,
        &empty_tool_bindings(),
        &mut accumulator,
    )
    .is_err());
    let malformed = json!({
        "type":"response.output_text.delta",
        "sequence_number":1,
        "item_id":"item-1",
        "output_index":0,
        "content_index":0,
        "unexpected":"secret"
    });
    assert!(parse_event(
        malformed,
        DEFAULT_MODEL,
        None,
        &empty_tool_bindings(),
        &mut accumulator,
    )
    .is_err());
    let terminal = json!({
        "id":"resp",
        "status":"in_progress",
        "model":DEFAULT_MODEL,
        "output":[]
    });
    assert!(parse_completed(
        terminal.as_object().unwrap(),
        DEFAULT_MODEL,
        None,
        &empty_tool_bindings(),
        &mut accumulator,
    )
    .is_err());
}

#[test]
fn test_metadata_events_accept_a_routed_model_and_reject_within_response_drift() {
    for kind in ["response.metadata", "codex.response.metadata"] {
        let mut accumulator = StreamAccumulator::default();
        let event = json!({
            "type":kind,
            "sequence_number":1,
            "response_id":"resp-1",
            "headers":{"OpenAI-Model":DEFAULT_MODEL},
            "metadata":{}
        });
        let parsed = parse_event(
            event,
            DEFAULT_MODEL,
            None,
            &empty_tool_bindings(),
            &mut accumulator,
        )
        .unwrap_or_else(|error| panic!("{kind} rejected a valid model header: {error:#}"));
        assert!(
            parsed.is_none(),
            "{kind} unexpectedly completed a response; completed={}",
            parsed.is_some()
        );
        assert_eq!(
            accumulator.actual_model.as_deref(),
            Some(DEFAULT_MODEL),
            "{kind} did not retain its observed model; accumulator={:?}",
            accumulator.actual_model
        );
    }

    let mut accumulator = StreamAccumulator::default();
    let routed = json!({
        "type":"response.metadata",
        "sequence_number":1,
        "headers":{"openai-model":"gpt-4o"}
    });
    let parsed = parse_event(
        routed,
        DEFAULT_MODEL,
        None,
        &empty_tool_bindings(),
        &mut accumulator,
    )
    .expect("a bounded serving-model identifier may differ from the requested route");
    assert!(
        parsed.is_none(),
        "routed model metadata unexpectedly completed a response; completed={}",
        parsed.is_some()
    );
    assert_eq!(
        accumulator.actual_model.as_deref(),
        Some("gpt-4o"),
        "routed model metadata was not retained; accumulator={:?}",
        accumulator.actual_model
    );

    let drift = json!({
        "type":"response.metadata",
        "sequence_number":2,
        "headers":{"openai-model":DEFAULT_MODEL}
    });
    let error = parse_event(
        drift,
        DEFAULT_MODEL,
        None,
        &empty_tool_bindings(),
        &mut accumulator,
    )
    .err()
    .expect("contradictory model identities within one response must fail")
    .to_string();
    assert!(
        error.contains("changed during the response"),
        "model-identity drift returned an unhelpful diagnostic: {error}"
    );

    for (case, model) in [
        ("empty", String::new()),
        ("whitespace", "bad model".to_string()),
        ("over-limit", "m".repeat(257)),
    ] {
        let error = StreamAccumulator::default()
            .observe_model(&model)
            .err()
            .unwrap_or_else(|| panic!("{case} actual-model identifier was accepted"));
        assert!(
            error.to_string().contains("actual model was invalid"),
            "{case} actual-model identifier returned an unhelpful diagnostic: {error:#}"
        );
    }
}

#[test]
fn test_outer_model_headers_accept_identical_duplicates_and_reject_drift() {
    let mut identical = reqwest::header::HeaderMap::new();
    identical.append("openai-model", "gpt-4o".parse().expect("valid header"));
    identical.append("openai-model", "gpt-4o".parse().expect("valid header"));
    let mut accumulator = StreamAccumulator::default();
    observe_outer_model_headers(&identical, &mut accumulator)
        .expect("identical outer model headers must remain valid");
    assert_eq!(
        accumulator.actual_model.as_deref(),
        Some("gpt-4o"),
        "identical duplicate headers did not retain their model; accumulator={:?}",
        accumulator.actual_model
    );

    let mut conflicting = identical;
    conflicting.append(
        "openai-model",
        DEFAULT_MODEL.parse().expect("valid default-model header"),
    );
    let mut accumulator = StreamAccumulator::default();
    let error = observe_outer_model_headers(&conflicting, &mut accumulator)
        .err()
        .expect("conflicting outer model headers must fail");
    assert!(
        error.to_string().contains("changed during the response"),
        "conflicting outer model headers returned an unhelpful diagnostic: {error:#}"
    );
}

#[test]
fn production_refresh_lock_is_shared_by_named_credential_and_account() {
    let first = shared_refresh_lock("credential-a", "account-a");
    let second = shared_refresh_lock("credential-a", "account-a");
    let other_account = shared_refresh_lock("credential-a", "account-b");
    let other_credential = shared_refresh_lock("credential-b", "account-a");
    assert!(Arc::ptr_eq(&first, &second));
    assert!(!Arc::ptr_eq(&first, &other_account));
    assert!(!Arc::ptr_eq(&first, &other_credential));
}

#[tokio::test]
async fn test_catalog_dispatch_uses_the_pinned_compatibility_version() {
    let mut server = mockito::Server::new_async().await;
    let models = server
        .mock("GET", "/backend-api/codex/models")
        .match_query(mockito::Matcher::UrlEncoded(
            "client_version".into(),
            CHATGPT_CATALOG_CLIENT_VERSION.into(),
        ))
        .match_header("authorization", "Bearer subscription-secret")
        .match_header("chatgpt-account-id", "account-1")
        .match_header("originator", "finch")
        .match_header("user-agent", FINCH_CHATGPT_USER_AGENT)
        .match_header("version", CHATGPT_CATALOG_CLIENT_VERSION)
        .with_status(200)
        .with_body(catalog_body())
        .create_async()
        .await;
    let inference = server
        .mock("POST", RESPONSES_PATH)
        .with_status(200)
        .with_header("content-type", "text/event-stream")
        .with_header("openai-model", DEFAULT_MODEL)
        .with_body(completed_sse(DEFAULT_MODEL))
        .create_async()
        .await;
    let provider = ChatGptSubscriptionProvider::for_test(
        Arc::new(StaticSource::new()),
        &format!("{}/backend-api/codex", server.url()),
        DEFAULT_MODEL,
    )
    .expect("catalog-version fixture must construct its provider");
    let outcome = provider
        .send_message(&ProviderRequest::new(vec![Message::user("hello")]).with_tools(vec![tool()]))
        .await;
    models.assert_async().await;
    inference.assert_async().await;
    let response = outcome.unwrap_or_else(|error| {
        panic!("catalog request with pinned compatibility version failed: {error:#}")
    });
    assert_eq!(
        response.model, DEFAULT_MODEL,
        "catalog-version boundary changed terminal model provenance; response={response:?}"
    );
}

#[tokio::test]
async fn test_buffered_and_streaming_accept_successful_sse_without_content_type() {
    let mut server = mockito::Server::new_async().await;
    let models = server
        .mock("GET", "/backend-api/codex/models")
        .match_query(mockito::Matcher::UrlEncoded(
            "client_version".into(),
            CHATGPT_CATALOG_CLIENT_VERSION.into(),
        ))
        .with_status(200)
        .with_body(catalog_body())
        .expect(1)
        .create_async()
        .await;
    let inference = server
        .mock("POST", RESPONSES_PATH)
        .with_status(200)
        .with_header("openai-model", DEFAULT_MODEL)
        .with_body(completed_sse(DEFAULT_MODEL))
        .expect(2)
        .create_async()
        .await;
    let provider = ChatGptSubscriptionProvider::for_test(
        Arc::new(StaticSource::new()),
        &format!("{}/backend-api/codex", server.url()),
        DEFAULT_MODEL,
    )
    .expect("missing-content-type fixture must construct a provider");
    let request = ProviderRequest::new(vec![Message::user("hello")]).with_tools(vec![tool()]);

    let (buffered, streaming) = tokio::join!(
        provider.send_message(&request),
        provider.send_message_stream(&request)
    );

    models.assert_async().await;
    inference.assert_async().await;
    let buffered = buffered.unwrap_or_else(|error| {
        panic!("valid headerless SSE failed through the buffered boundary: {error:#}")
    });
    assert_eq!(
        buffered.model, DEFAULT_MODEL,
        "headerless buffered SSE changed model provenance; response={buffered:?}"
    );
    let mut receiver = streaming.unwrap_or_else(|error| {
        panic!("valid headerless SSE failed through the streaming boundary: {error:#}")
    });
    let mut streamed_text = String::new();
    let mut terminal_metadata = None;
    while let Some(chunk) = receiver.recv().await {
        match chunk.unwrap_or_else(|error| {
            panic!("valid headerless SSE emitted a stream error: {error:#}")
        }) {
            StreamChunk::TextDelta(delta) => streamed_text.push_str(&delta),
            StreamChunk::ResponseMetadata { model } => terminal_metadata = Some(model),
            _ => {}
        }
    }
    assert_eq!(streamed_text, "hello", "headerless SSE lost streamed text");
    assert_eq!(
        terminal_metadata.as_deref(),
        Some(DEFAULT_MODEL),
        "headerless SSE omitted terminal model metadata"
    );
}

#[tokio::test]
async fn test_buffered_and_streaming_report_omitted_or_routed_model_provenance() {
    for (case, provenance, reported_model) in [
        ("omitted", None, DEFAULT_MODEL),
        ("routed", Some("gpt-4o"), "gpt-4o"),
    ] {
        let mut server = mockito::Server::new_async().await;
        let models = server
            .mock("GET", "/backend-api/codex/models")
            .match_query(mockito::Matcher::UrlEncoded(
                "client_version".into(),
                CHATGPT_CATALOG_CLIENT_VERSION.into(),
            ))
            .with_status(200)
            .with_body(catalog_body())
            .expect(1)
            .create_async()
            .await;
        let mut inference = server
            .mock("POST", RESPONSES_PATH)
            .with_status(200)
            .with_header("content-type", "text/event-stream")
            .with_body(completed_sse_with_model_provenance(provenance))
            .expect(2);
        if let Some(model) = provenance {
            inference = inference.with_header("openai-model", model);
        }
        let inference = inference.create_async().await;
        let provider = ChatGptSubscriptionProvider::for_test(
            Arc::new(StaticSource::new()),
            &format!("{}/backend-api/codex", server.url()),
            DEFAULT_MODEL,
        )
        .unwrap_or_else(|error| panic!("{case} model fixture failed to construct: {error:#}"));
        let request = ProviderRequest::new(vec![Message::user("hello")]).with_tools(vec![tool()]);

        let (buffered, streaming) = tokio::join!(
            provider.send_message(&request),
            provider.send_message_stream(&request)
        );

        models.assert_async().await;
        inference.assert_async().await;
        let buffered = buffered.unwrap_or_else(|error| {
            panic!("{case} model provenance failed through the buffered boundary: {error:#}")
        });
        assert_eq!(
            buffered.model, reported_model,
            "buffered response reported the wrong model for {case} provenance; \
                 response={buffered:?}"
        );
        assert!(
            buffered
                .content
                .iter()
                .any(|block| matches!(block, ContentBlock::Text { text } if text == "hello")),
            "{case} model-provenance response lost buffered text; response={buffered:?}"
        );

        let mut receiver = streaming.unwrap_or_else(|error| {
            panic!("{case} model provenance failed through the streaming boundary: {error:#}")
        });
        let mut streamed_text = String::new();
        let mut terminal_model = None;
        while let Some(chunk) = receiver.recv().await {
            match chunk.unwrap_or_else(|error| {
                panic!("{case} model provenance emitted a stream error: {error:#}")
            }) {
                StreamChunk::TextDelta(delta) => streamed_text.push_str(&delta),
                StreamChunk::ResponseMetadata { model } => terminal_model = Some(model),
                _ => {}
            }
        }
        assert_eq!(
            streamed_text, "hello",
            "{case} model-provenance streaming response lost parsed text"
        );
        assert_eq!(
            terminal_model.as_deref(),
            Some(reported_model),
            "streaming response reported the wrong model for {case} provenance"
        );
    }
}

#[tokio::test]
async fn test_buffered_and_streaming_reject_malformed_model_provenance_before_terminal_effects() {
    let mut server = mockito::Server::new_async().await;
    let models = server
        .mock("GET", "/backend-api/codex/models")
        .match_query(mockito::Matcher::UrlEncoded(
            "client_version".into(),
            CHATGPT_CATALOG_CLIENT_VERSION.into(),
        ))
        .with_status(200)
        .with_body(catalog_body())
        .expect(1)
        .create_async()
        .await;
    let malformed = format!(
        "event: response.created\ndata: {}\n\n",
        json!({"type":"response.created","sequence_number":1,"response":{"headers":{"openai-model":"bad model"}}})
    );
    let inference = server
        .mock("POST", RESPONSES_PATH)
        .with_status(200)
        .with_header("content-type", "text/event-stream")
        .with_body(malformed)
        .expect(2)
        .create_async()
        .await;
    let provider = ChatGptSubscriptionProvider::for_test(
        Arc::new(StaticSource::new()),
        &format!("{}/backend-api/codex", server.url()),
        DEFAULT_MODEL,
    )
    .expect("malformed-model-provenance fixture must construct a provider");
    let request = ProviderRequest::new(vec![Message::user("hello")]);

    let (buffered, streaming) = tokio::join!(
        provider.send_message(&request),
        provider.send_message_stream(&request)
    );

    models.assert_async().await;
    inference.assert_async().await;
    let buffered_error = buffered
        .err()
        .expect("malformed model provenance must fail the buffered response");
    assert!(
        buffered_error
            .to_string()
            .contains("actual model was invalid"),
        "malformed buffered provenance returned an unhelpful diagnostic: {buffered_error:#}"
    );

    let mut receiver =
        streaming.expect("stream setup must succeed before malformed SSE provenance is consumed");
    let mut outcome = Vec::new();
    while let Some(chunk) = receiver.recv().await {
        outcome.push(chunk.map_err(|error| error.to_string()));
    }
    assert_eq!(
        outcome.len(),
        1,
        "malformed streaming provenance must emit exactly one terminal error and no effects; \
             outcome={outcome:?}"
    );
    assert!(
        matches!(&outcome[0], Err(error) if error.contains("actual model was invalid")),
        "malformed streaming provenance must emit its actionable terminal error and no effects; \
             outcome={outcome:?}"
    );
}

#[tokio::test]
async fn test_buffered_and_streaming_accept_audited_passive_response_fields() {
    let mut server = mockito::Server::new_async().await;
    let models = server
        .mock("GET", "/backend-api/codex/models")
        .match_query(mockito::Matcher::UrlEncoded(
            "client_version".into(),
            CHATGPT_CATALOG_CLIENT_VERSION.into(),
        ))
        .with_status(200)
        .with_body(catalog_body())
        .expect(1)
        .create_async()
        .await;
    let inference = server
        .mock("POST", RESPONSES_PATH)
        .with_status(200)
        .with_header("content-type", "text/event-stream")
        .with_header("openai-model", DEFAULT_MODEL)
        .with_body(completed_sse_with_audited_passive_fields(DEFAULT_MODEL))
        .expect(2)
        .create_async()
        .await;
    let provider = ChatGptSubscriptionProvider::for_test(
        Arc::new(StaticSource::new()),
        &format!("{}/backend-api/codex", server.url()),
        DEFAULT_MODEL,
    )
    .expect("audited-passive-field fixture must construct a provider");
    let request = ProviderRequest::new(vec![Message::user("hello")]);

    let (buffered, streaming) = tokio::join!(
        provider.send_message(&request),
        provider.send_message_stream(&request)
    );

    models.assert_async().await;
    inference.assert_async().await;
    let buffered = buffered.unwrap_or_else(|error| {
        panic!("audited passive fields failed through the buffered boundary: {error:#}")
    });
    assert_eq!(
        buffered.model, DEFAULT_MODEL,
        "audited passive fields changed buffered model provenance; response={buffered:?}"
    );
    assert!(
        buffered
            .content
            .iter()
            .any(|block| matches!(block, ContentBlock::Text { text } if text == "hello")),
        "audited passive fields lost buffered response text; response={buffered:?}"
    );

    let mut receiver = streaming.unwrap_or_else(|error| {
        panic!("audited passive fields failed through the streaming boundary: {error:#}")
    });
    let mut streamed_text = String::new();
    let mut terminal_metadata = None;
    while let Some(chunk) = receiver.recv().await {
        match chunk.unwrap_or_else(|error| {
            panic!("audited passive fields emitted a stream error: {error:#}")
        }) {
            StreamChunk::TextDelta(delta) => streamed_text.push_str(&delta),
            StreamChunk::ResponseMetadata { model } => terminal_metadata = Some(model),
            _ => {}
        }
    }
    assert_eq!(
        streamed_text, "hello",
        "audited passive fields lost streamed text"
    );
    assert_eq!(
        terminal_metadata.as_deref(),
        Some(DEFAULT_MODEL),
        "audited passive fields omitted terminal model metadata"
    );
}

#[tokio::test]
async fn test_buffered_and_streaming_accept_terminal_snapshot_omitting_streamed_reasoning() {
    let mut server = mockito::Server::new_async().await;
    let models = server
        .mock("GET", "/backend-api/codex/models")
        .match_query(mockito::Matcher::UrlEncoded(
            "client_version".into(),
            CHATGPT_CATALOG_CLIENT_VERSION.into(),
        ))
        .with_status(200)
        .with_body(catalog_body())
        .expect(1)
        .create_async()
        .await;
    let inference = server
        .mock("POST", RESPONSES_PATH)
        .with_status(200)
        .with_header("content-type", "text/event-stream")
        .with_header("openai-model", DEFAULT_MODEL)
        .with_body(completed_sse_with_terminal_output(
            DEFAULT_MODEL,
            json!([{
                "id":"message-terminal",
                "type":"message",
                "status":"completed",
                "role":"assistant",
                "phase":"final_answer",
                "content":[{"type":"output_text","text":"hello"}]
            }]),
        ))
        .expect(2)
        .create_async()
        .await;
    let provider = ChatGptSubscriptionProvider::for_test(
        Arc::new(StaticSource::new()),
        &format!("{}/backend-api/codex", server.url()),
        DEFAULT_MODEL,
    )
    .expect("terminal-subset fixture must construct a provider");
    let request = ProviderRequest::new(vec![Message::user("hello")]);

    let (buffered, streaming) = tokio::join!(
        provider.send_message(&request),
        provider.send_message_stream(&request)
    );

    models.assert_async().await;
    inference.assert_async().await;
    let buffered = buffered.unwrap_or_else(|error| {
        panic!(
            "a terminal snapshot that omits already-validated streamed reasoning failed at \
                 the buffered provider boundary: {error:#}"
        )
    });
    assert!(
        buffered
            .content
            .iter()
            .any(|block| matches!(block, ContentBlock::Text { text } if text == "hello")),
        "terminal-subset reconciliation lost buffered text; response={buffered:?}"
    );
    assert!(
        buffered
            .content
            .iter()
            .any(|block| matches!(block, ContentBlock::OpaqueReasoning { .. })),
        "terminal-subset reconciliation lost the authoritative streamed reasoning item; \
             response={buffered:?}"
    );

    let mut receiver = streaming.unwrap_or_else(|error| {
        panic!(
            "a terminal snapshot that omits already-validated streamed reasoning failed \
                 before the streaming provider boundary: {error:#}"
        )
    });
    let mut outcome = Vec::new();
    while let Some(chunk) = receiver.recv().await {
        outcome.push(chunk.map_err(|error| error.to_string()));
    }
    assert!(
        outcome.iter().all(Result::is_ok),
        "terminal-subset reconciliation emitted a streaming error after valid text; \
             outcome={outcome:?}"
    );
    assert!(
        outcome
            .iter()
            .any(|chunk| matches!(chunk, Ok(StreamChunk::TextDelta(text)) if text == "hello")),
        "terminal-subset reconciliation lost the streamed text delta; outcome={outcome:?}"
    );
    assert!(
        outcome.iter().any(|chunk| matches!(
            chunk,
            Ok(StreamChunk::ContentBlockComplete(
                ContentBlock::OpaqueReasoning { .. }
            ))
        )),
        "terminal-subset reconciliation lost the completed streamed reasoning item; \
             outcome={outcome:?}"
    );
}

#[tokio::test]
async fn test_buffered_and_streaming_accept_empty_terminal_output_after_streamed_message() {
    let mut server = mockito::Server::new_async().await;
    let models = server
        .mock("GET", "/backend-api/codex/models")
        .match_query(mockito::Matcher::UrlEncoded(
            "client_version".into(),
            CHATGPT_CATALOG_CLIENT_VERSION.into(),
        ))
        .with_status(200)
        .with_body(catalog_body())
        .expect(1)
        .create_async()
        .await;
    let inference = server
        .mock("POST", RESPONSES_PATH)
        .with_status(200)
        .with_header("content-type", "text/event-stream")
        .with_header("openai-model", DEFAULT_MODEL)
        .with_body(completed_sse_with_streamed_message_and_empty_terminal_output(DEFAULT_MODEL))
        .expect(2)
        .create_async()
        .await;
    let provider = ChatGptSubscriptionProvider::for_test(
        Arc::new(StaticSource::new()),
        &format!("{}/backend-api/codex", server.url()),
        DEFAULT_MODEL,
    )
    .expect("empty-terminal-output fixture must construct a provider");
    let request = ProviderRequest::new(vec![Message::user("hello")]);

    let (buffered, streaming) = tokio::join!(
        provider.send_message(&request),
        provider.send_message_stream(&request)
    );

    models.assert_async().await;
    inference.assert_async().await;
    let buffered = buffered.unwrap_or_else(|error| {
        panic!(
            "an empty terminal output snapshot rejected a validated streamed message at the \
                 buffered provider boundary: {error:#}"
        )
    });
    assert!(
        matches!(
            buffered.content.as_slice(),
            [ContentBlock::Text { text }] if text == "hello"
        ),
        "empty-terminal-output reconciliation lost buffered text; response={buffered:?}"
    );
    assert_eq!(
        buffered.model, DEFAULT_MODEL,
        "empty-terminal-output reconciliation lost model metadata"
    );
    assert_eq!(
        buffered
            .usage
            .as_ref()
            .map(|usage| (usage.input_tokens, usage.output_tokens)),
        Some((12, 7)),
        "empty-terminal-output reconciliation lost terminal usage"
    );

    let mut receiver = streaming.unwrap_or_else(|error| {
        panic!(
            "an empty terminal output snapshot failed before the streaming provider \
                 boundary: {error:#}"
        )
    });
    let mut outcome = Vec::new();
    while let Some(chunk) = receiver.recv().await {
        outcome.push(chunk.map_err(|error| error.to_string()));
    }
    assert!(
        matches!(
            outcome.as_slice(),
            [
                Ok(StreamChunk::TextDelta(delta)),
                Ok(StreamChunk::ResponseMetadata { model }),
                Ok(StreamChunk::Usage {
                    input_tokens: 12,
                    output_tokens: 7,
                }),
                Ok(StreamChunk::ContentBlockComplete(ContentBlock::Text { text })),
            ] if delta == "hello" && model == DEFAULT_MODEL && text == "hello"
        ),
        "empty-terminal-output reconciliation did not emit exactly one ordered text delta, \
             model identity, usage record, and completed message with no extra terminal effects; \
             outcome={outcome:?}"
    );
}

#[tokio::test]
async fn test_buffered_and_streaming_reject_non_reasoning_terminal_snapshot_drift() {
    let reasoning = json!({
        "type":"reasoning",
        "summary":[],
        "encrypted_content":"opaque-1"
    });
    let message = json!({
        "type":"message",
        "role":"assistant",
        "content":[{"type":"output_text","text":"hello"}]
    });
    let function_call = json!({
        "type":"function_call",
        "call_id":"call-2",
        "name":"read",
        "namespace":"functions",
        "arguments":"{\"path\":\"README.md\"}"
    });

    assert_terminal_snapshot_rejected_at_provider_boundary(
        "omitted streamed function call",
        json!([reasoning.clone(), message.clone()]),
        "terminal snapshot omitted streamed output item 2 (function_call); \
             terminal_count=2, streamed_count=3",
    )
    .await;
    assert_terminal_snapshot_rejected_at_provider_boundary(
        "duplicate message masking a streamed function call",
        json!([reasoning.clone(), message.clone(), message.clone()]),
        "terminal output item 2 (message) did not match streamed output item 2 \
             (function_call); terminal_count=3, streamed_count=3",
    )
    .await;
    assert_terminal_snapshot_rejected_at_provider_boundary(
        "reordered message and function call",
        json!([reasoning, function_call, message]),
        "terminal output item 1 (function_call) did not match streamed output item 1 \
             (message); terminal_count=3, streamed_count=3",
    )
    .await;
}

#[tokio::test]
async fn test_successful_sse_with_explicit_empty_content_type_is_rejected() {
    let mut server = mockito::Server::new_async().await;
    let models = server
        .mock("GET", "/backend-api/codex/models")
        .match_query(mockito::Matcher::UrlEncoded(
            "client_version".into(),
            CHATGPT_CATALOG_CLIENT_VERSION.into(),
        ))
        .with_status(200)
        .with_body(catalog_body())
        .create_async()
        .await;
    let inference = server
        .mock("POST", RESPONSES_PATH)
        .with_status(200)
        .with_header("content-type", "")
        .with_body(completed_sse(DEFAULT_MODEL))
        .create_async()
        .await;
    let provider = ChatGptSubscriptionProvider::for_test(
        Arc::new(StaticSource::new()),
        &format!("{}/backend-api/codex", server.url()),
        DEFAULT_MODEL,
    )
    .expect("empty-content-type fixture must construct a provider");

    let error = provider
        .send_message(&ProviderRequest::new(vec![Message::user("hello")]))
        .await
        .expect_err("an explicit empty Content-Type was treated as an omitted header");

    models.assert_async().await;
    inference.assert_async().await;
    assert!(
        error.to_string().contains("not an event stream"),
        "explicit empty Content-Type returned the wrong diagnostic: {error:#}"
    );
}

#[tokio::test]
async fn test_buffered_inference_uses_the_pinned_compatibility_version() {
    let mut server = mockito::Server::new_async().await;
    let models = server
        .mock("GET", "/backend-api/codex/models")
        .match_query(mockito::Matcher::UrlEncoded(
            "client_version".into(),
            "0.151.0".into(),
        ))
        .match_header("authorization", "Bearer subscription-secret")
        .match_header("chatgpt-account-id", "account-1")
        .match_header("originator", "finch")
        .match_header("user-agent", FINCH_CHATGPT_USER_AGENT)
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_header("etag", "account-etag")
        .with_body(catalog_body())
        .create_async()
        .await;
    let expected = json!({
        "model": DEFAULT_MODEL,
        "input": [
            {
                "id": "at_06f0d744-9e74-54f8-9371-312adc3c666b",
                "type": "additional_tools",
                "role": "developer",
                "tools": [{
                    "type": "namespace",
                    "name": "functions",
                    "description": "",
                    "tools": [{
                        "type": "function",
                        "name": "read",
                        "description": "Read a file",
                        "strict": false,
                        "parameters": {
                            "type": "object",
                            "properties": {"path":{"type":"string"}},
                            "required": ["path"],
                            "additionalProperties": false
                        }
                    }]
                }]
            },
            {"type":"message","role":"user","content":[{"type":"input_text","text":"hello"}]}
        ],
        "tool_choice": "auto",
        "parallel_tool_calls": false,
        "reasoning": {"effort":"high","context":"all_turns"},
        "store": false,
        "stream": true,
        "include": ["reasoning.encrypted_content"]
    });
    let inference = server
        .mock("POST", RESPONSES_PATH)
        .match_header("authorization", "Bearer subscription-secret")
        .match_header("chatgpt-account-id", "account-1")
        .match_header("originator", "finch")
        .match_header("user-agent", FINCH_CHATGPT_USER_AGENT)
        .match_header("version", CHATGPT_CATALOG_CLIENT_VERSION)
        .match_header("x-openai-internal-codex-responses-lite", "true")
        .match_body(mockito::Matcher::Json(expected))
        .with_status(200)
        .with_header("content-type", "text/event-stream")
        .with_header("openai-model", DEFAULT_MODEL)
        .with_header("x-codex-primary-used-percent", "25.5")
        .with_body(completed_sse(DEFAULT_MODEL))
        .create_async()
        .await;
    let source = Arc::new(StaticSource::new());
    let provider = ChatGptSubscriptionProvider::for_test(
        source.clone(),
        &format!("{}/backend-api/codex", server.url()),
        DEFAULT_MODEL,
    )
    .unwrap();
    let outcome = provider
        .send_message(
            &ProviderRequest::new(vec![Message::user("hello")])
                .with_model(DEFAULT_MODEL)
                .with_tools(vec![tool()]),
        )
        .await;
    models.assert_async().await;
    inference.assert_async().await;
    let response = outcome.unwrap_or_else(|error| {
        panic!("buffered inference with pinned compatibility version failed: {error:#}")
    });
    assert_eq!(response.model, DEFAULT_MODEL);
    assert_eq!(response.usage.unwrap().output_tokens, 7);
    assert_eq!(response.allowance.unwrap().primary_used_percent, Some(25.5));
    assert!(matches!(
        response.content.first(),
        Some(ContentBlock::OpaqueReasoning { encrypted_content }) if encrypted_content == "opaque-1"
    ));
    assert!(matches!(
        response.content.last(),
        Some(ContentBlock::ToolUse { id, .. }) if id == "call-2"
    ));
    assert_eq!(source.refreshes.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn test_streaming_inference_uses_the_pinned_compatibility_version() {
    let mut server = mockito::Server::new_async().await;
    let models = server
        .mock("GET", "/backend-api/codex/models")
        .match_query(mockito::Matcher::UrlEncoded(
            "client_version".into(),
            CHATGPT_CATALOG_CLIENT_VERSION.into(),
        ))
        .with_status(200)
        .with_body(single_model_catalog_body(DEFAULT_MODEL, 1_000_000))
        .expect(1)
        .create_async()
        .await;
    let inference = server
        .mock("POST", RESPONSES_PATH)
        .match_header("version", CHATGPT_CATALOG_CLIENT_VERSION)
        .with_status(200)
        .with_header("content-type", "text/event-stream")
        .with_header("openai-model", DEFAULT_MODEL)
        .with_body(completed_sse(DEFAULT_MODEL))
        .expect(2)
        .create_async()
        .await;
    let provider = ChatGptSubscriptionProvider::for_test(
        Arc::new(StaticSource::new()),
        &format!("{}/backend-api/codex", server.url()),
        DEFAULT_MODEL,
    )
    .unwrap();
    let request = ProviderRequest::new(vec![Message::user("hello")]).with_tools(vec![tool()]);
    let (nonstream, stream) = tokio::join!(
        provider.send_message(&request),
        provider.send_message_stream(&request)
    );
    models.assert_async().await;
    inference.assert_async().await;
    let nonstream = nonstream.unwrap_or_else(|error| {
        panic!("buffered half of streaming parity dispatch failed: {error:#}")
    });
    let mut receiver = stream.unwrap_or_else(|error| {
        panic!("streaming inference with pinned compatibility version failed: {error:#}")
    });
    let mut streamed_blocks = Vec::new();
    let mut streamed_model = None;
    let mut streamed_usage = None;
    let mut streamed_text = String::new();
    while let Some(chunk) = receiver.recv().await {
        match chunk.unwrap() {
            StreamChunk::TextDelta(delta) => streamed_text.push_str(&delta),
            StreamChunk::ContentBlockComplete(block) => streamed_blocks.push(block),
            StreamChunk::ResponseMetadata { model } => streamed_model = Some(model),
            StreamChunk::Usage {
                input_tokens,
                output_tokens,
            } => streamed_usage = Some((input_tokens, output_tokens)),
            _ => {}
        }
    }
    assert_eq!(
        serde_json::to_value(streamed_blocks).unwrap(),
        serde_json::to_value(&nonstream.content).unwrap()
    );
    assert_eq!(streamed_model.as_deref(), Some(nonstream.model.as_str()));
    assert_eq!(streamed_usage, Some((12, 7)));
    assert_eq!(streamed_text, "hello");
}

#[tokio::test]
async fn buffered_and_streaming_require_the_exact_requested_catalog_entry() {
    let mut server = mockito::Server::new_async().await;
    let models = server
        .mock("GET", "/backend-api/codex/models")
        .match_query(mockito::Matcher::UrlEncoded(
            "client_version".into(),
            CHATGPT_CATALOG_CLIENT_VERSION.into(),
        ))
        .with_status(200)
        .with_body(single_model_catalog_body(MODEL_ALIAS, 1_000_000))
        .expect(1)
        .create_async()
        .await;
    let provider = ChatGptSubscriptionProvider::for_test(
        Arc::new(StaticSource::new()),
        &format!("{}/backend-api/codex", server.url()),
        DEFAULT_MODEL,
    )
    .unwrap();
    let request = ProviderRequest::new(vec![Message::user("hello")]);

    let buffered_error = provider
        .send_message(&request)
        .await
        .err()
        .expect("buffered request must require its exact catalog entry");
    assert!(buffered_error.is::<SubscriptionRequestedModelUnavailable>());
    assert!(!buffered_error.to_string().contains("subscription-secret"));
    assert!(!buffered_error.to_string().contains("account-1"));

    let streaming_error = provider
        .send_message_stream(&request)
        .await
        .err()
        .expect("streaming request must require its exact catalog entry");
    assert!(streaming_error.is::<SubscriptionRequestedModelUnavailable>());
    assert!(!streaming_error.to_string().contains("subscription-secret"));
    assert!(!streaming_error.to_string().contains("account-1"));
    models.assert_async().await;
}

#[tokio::test]
async fn byte_fragmented_sse_preserves_ordered_opaque_and_tool_items() {
    let base = fragmented_subscription_server(completed_sse(DEFAULT_MODEL)).await;
    let provider =
        ChatGptSubscriptionProvider::for_test(Arc::new(StaticSource::new()), &base, DEFAULT_MODEL)
            .unwrap();
    let response = provider
        .send_message(&ProviderRequest::new(vec![Message::user("hello")]).with_tools(vec![tool()]))
        .await
        .unwrap();
    assert!(matches!(
        response.content.as_slice(),
        [
            ContentBlock::OpaqueReasoning { .. },
            ContentBlock::Text { .. },
            ContentBlock::ToolUse { .. }
        ]
    ));
}

#[tokio::test]
async fn one_pre_stream_unauthorized_refreshes_same_account_once() {
    let mut server = mockito::Server::new_async().await;
    let initial_catalog = server
        .mock("GET", "/backend-api/codex/models")
        .match_query(mockito::Matcher::UrlEncoded(
            "client_version".into(),
            CHATGPT_CATALOG_CLIENT_VERSION.into(),
        ))
        .match_header("authorization", "Bearer subscription-secret")
        .with_status(200)
        .with_body(catalog_body())
        .create_async()
        .await;
    let refreshed_catalog = server
        .mock("GET", "/backend-api/codex/models")
        .match_query(mockito::Matcher::UrlEncoded(
            "client_version".into(),
            CHATGPT_CATALOG_CLIENT_VERSION.into(),
        ))
        .match_header("authorization", "Bearer refreshed-subscription-secret")
        .with_status(200)
        .with_body(catalog_body())
        .expect(1)
        .create_async()
        .await;
    let first = server
        .mock("POST", RESPONSES_PATH)
        .match_header("authorization", "Bearer subscription-secret")
        .with_status(401)
        .with_body("do-not-log-this-secret")
        .expect(1)
        .create_async()
        .await;
    let second = server
        .mock("POST", RESPONSES_PATH)
        .match_header("authorization", "Bearer refreshed-subscription-secret")
        .with_status(200)
        .with_header("content-type", "text/event-stream")
        .with_header("openai-model", DEFAULT_MODEL)
        .with_body(completed_sse(DEFAULT_MODEL))
        .expect(1)
        .create_async()
        .await;
    let source = Arc::new(StaticSource::new());
    let provider = ChatGptSubscriptionProvider::for_test(
        source.clone(),
        &format!("{}/backend-api/codex", server.url()),
        DEFAULT_MODEL,
    )
    .unwrap();
    provider
        .send_message(&ProviderRequest::new(vec![Message::user("hello")]).with_tools(vec![tool()]))
        .await
        .unwrap();
    assert_eq!(source.refreshes.load(Ordering::SeqCst), 1);
    initial_catalog.assert_async().await;
    refreshed_catalog.assert_async().await;
    first.assert_async().await;
    second.assert_async().await;
}

#[tokio::test]
async fn one_catalog_unauthorized_refreshes_before_inference_only_once() {
    let mut server = mockito::Server::new_async().await;
    let rejected_catalog = server
        .mock("GET", "/backend-api/codex/models")
        .match_query(mockito::Matcher::UrlEncoded(
            "client_version".into(),
            CHATGPT_CATALOG_CLIENT_VERSION.into(),
        ))
        .match_header("authorization", "Bearer subscription-secret")
        .with_status(401)
        .with_body("catalog-auth-secret")
        .expect(1)
        .create_async()
        .await;
    let refreshed_catalog = server
        .mock("GET", "/backend-api/codex/models")
        .match_query(mockito::Matcher::UrlEncoded(
            "client_version".into(),
            CHATGPT_CATALOG_CLIENT_VERSION.into(),
        ))
        .match_header("authorization", "Bearer refreshed-subscription-secret")
        .with_status(200)
        .with_body(catalog_body())
        .expect(1)
        .create_async()
        .await;
    let inference = server
        .mock("POST", RESPONSES_PATH)
        .match_header("authorization", "Bearer refreshed-subscription-secret")
        .with_status(200)
        .with_header("content-type", "text/event-stream")
        .with_header("openai-model", DEFAULT_MODEL)
        .with_body(completed_sse(DEFAULT_MODEL))
        .expect(1)
        .create_async()
        .await;
    let source = Arc::new(StaticSource::new());
    let provider = ChatGptSubscriptionProvider::for_test(
        source.clone(),
        &format!("{}/backend-api/codex", server.url()),
        DEFAULT_MODEL,
    )
    .unwrap();
    provider
        .send_message(&ProviderRequest::new(vec![Message::user("hello")]).with_tools(vec![tool()]))
        .await
        .unwrap();
    assert_eq!(source.refreshes.load(Ordering::SeqCst), 1);
    rejected_catalog.assert_async().await;
    refreshed_catalog.assert_async().await;
    inference.assert_async().await;
}

#[tokio::test]
async fn stream_receiver_drop_cancels_and_releases_subscription_transport() {
    let (base, closed) = stalling_subscription_server(true).await;
    let provider =
        ChatGptSubscriptionProvider::for_test(Arc::new(StaticSource::new()), &base, DEFAULT_MODEL)
            .unwrap();
    let receiver = provider
        .send_message_stream(&ProviderRequest::new(vec![Message::user("hello")]))
        .await
        .unwrap();
    drop(receiver);
    tokio::time::timeout(Duration::from_secs(2), closed)
        .await
        .expect("subscription transport was not released after receiver drop")
        .unwrap();
}

#[tokio::test]
async fn caller_cancellation_reaches_and_releases_subscription_transport() {
    let (base, closed) = stalling_subscription_server(true).await;
    let provider =
        ChatGptSubscriptionProvider::for_test(Arc::new(StaticSource::new()), &base, DEFAULT_MODEL)
            .unwrap();
    let cancel = CancellationToken::new();
    let mut receiver = provider
        .send_message_stream(
            &ProviderRequest::new(vec![Message::user("hello")])
                .with_cancellation_token(cancel.clone()),
        )
        .await
        .unwrap();
    cancel.cancel();
    let error = tokio::time::timeout(Duration::from_secs(2), receiver.recv())
        .await
        .expect("cancelled subscription stream did not terminate")
        .expect("cancelled subscription stream omitted its error")
        .unwrap_err()
        .to_string();
    assert!(error.contains("cancelled"));
    tokio::time::timeout(Duration::from_secs(2), closed)
        .await
        .expect("subscription transport was not released after caller cancellation")
        .unwrap();
}

#[tokio::test]
async fn test_stream_receiver_preserves_32_chunk_ordinary_capacity() {
    let (base, closed) = stalling_subscription_server(true).await;
    let provider =
        ChatGptSubscriptionProvider::for_test(Arc::new(StaticSource::new()), &base, DEFAULT_MODEL)
            .expect("capacity fixture must construct a subscription provider");
    let receiver = provider
        .send_message_stream(&ProviderRequest::new(vec![Message::user("hello")]))
        .await
        .expect("capacity fixture must start the production streaming boundary");
    assert_eq!(
        receiver.capacity(),
        STREAM_BUFFER_CAPACITY,
        "reserved terminal slot changed the caller-visible ordinary capacity; max_capacity={}",
        receiver.max_capacity()
    );
    assert_eq!(
            receiver.max_capacity(),
            STREAM_CHANNEL_CAPACITY,
            "subscription channel must expose 32 ordinary slots plus one reserved terminal slot; capacity={}",
            receiver.capacity()
        );

    drop(receiver);
    tokio::time::timeout(Duration::from_secs(2), closed)
        .await
        .expect("capacity fixture retained its HTTP transport after receiver drop")
        .expect("capacity fixture dropped its transport-close milestone");
}

#[tokio::test]
async fn test_backpressured_cancellation_releases_transport_and_terminates_once() {
    const DELTA_COUNT: usize = 40;
    const ORDINARY_CAPACITY: usize = STREAM_BUFFER_CAPACITY;
    let (base, body_sent, closed) =
        held_open_subscription_server(text_delta_sse(DELTA_COUNT, false)).await;
    let (provider, mut events) = observe_stream_producer(
        ChatGptSubscriptionProvider::for_test(Arc::new(StaticSource::new()), &base, DEFAULT_MODEL)
            .expect("backpressure fixture must construct a subscription provider"),
    );
    let cancel = CancellationToken::new();
    let mut receiver = provider
        .send_message_stream(
            &ProviderRequest::new(vec![Message::user("hello")])
                .with_cancellation_token(cancel.clone()),
        )
        .await
        .expect("backpressure fixture must start the production streaming boundary");
    tokio::time::timeout(Duration::from_secs(2), body_sent)
        .await
        .expect("backpressure fixture did not send more than 32 valid SSE deltas")
        .expect("backpressure fixture dropped its body-sent milestone");
    tokio::time::timeout(Duration::from_secs(2), async {
        while receiver.capacity() != 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap_or_else(|_| {
        panic!(
            "subscription producer did not reach backpressure; capacity={}; max_capacity={}",
            receiver.capacity(),
            receiver.max_capacity()
        )
    });

    cancel.cancel();
    tokio::time::timeout(Duration::from_secs(2), closed)
        .await
        .expect("backpressured cancellation retained the ChatGPT HTTP transport")
        .expect("backpressure fixture dropped its transport-close milestone");

    // Task termination is proven while the receiver is still completely
    // undrained: a producer that publishes its terminal error and then
    // parks behind backpressure never reports Finished.
    let attempts = wait_for_producer_finished(&mut events, "backpressured cancellation").await;
    assert!(
            attempts.len() > ORDINARY_CAPACITY,
            "backpressured cancellation never attempted the send that exceeds the ordinary budget; observed_send_attempts={}; attempts={attempts:?}",
            attempts.len()
        );
    assert!(
            receiver.is_closed(),
            "backpressured cancellation terminated with a live producer sender; capacity={}; max_capacity={}; observed_send_attempts={}",
            receiver.capacity(),
            receiver.max_capacity(),
            attempts.len()
        );
    assert_eq!(
            receiver.capacity(),
            0,
            "backpressured cancellation must terminate with 32 ordinary chunks queued plus the reserved terminal slot consumed; max_capacity={}; observed_send_attempts={}",
            receiver.max_capacity(),
            attempts.len()
        );

    let mut outcome = Vec::new();
    tokio::time::timeout(Duration::from_secs(2), async {
        while let Some(chunk) = receiver.recv().await {
            outcome.push(chunk.map_err(|error| error.to_string()));
        }
    })
    .await
    .unwrap_or_else(|_| {
        panic!("backpressured producer did not terminate after cancellation; outcome={outcome:?}")
    });
    assert_eq!(
            outcome.len(),
            ORDINARY_CAPACITY + 1,
            "cancellation must preserve the queued prefix and emit one terminal error; outcome={outcome:?}"
        );
    for (index, chunk) in outcome[..ORDINARY_CAPACITY].iter().enumerate() {
        assert!(
            matches!(chunk, Ok(StreamChunk::TextDelta(delta)) if delta == &format!("delta-{index}")),
            "backpressure changed or reordered delta {index}; outcome={outcome:?}"
        );
    }
    assert!(
        matches!(outcome.last(), Some(Err(error)) if error.contains("cancelled")),
        "cancellation must be the one final stream outcome; outcome={outcome:?}"
    );
}

#[tokio::test]
async fn test_backpressured_receiver_drop_releases_transport() {
    let (base, body_sent, closed) = held_open_subscription_server(text_delta_sse(40, false)).await;
    let (provider, mut events) = observe_stream_producer(
        ChatGptSubscriptionProvider::for_test(Arc::new(StaticSource::new()), &base, DEFAULT_MODEL)
            .expect("receiver-drop fixture must construct a subscription provider"),
    );
    let receiver = provider
        .send_message_stream(&ProviderRequest::new(vec![Message::user("hello")]))
        .await
        .expect("receiver-drop fixture must start the production streaming boundary");
    tokio::time::timeout(Duration::from_secs(2), body_sent)
        .await
        .expect("receiver-drop fixture did not send more than 32 valid SSE deltas")
        .expect("receiver-drop fixture dropped its body-sent milestone");
    tokio::time::timeout(Duration::from_secs(2), async {
        while receiver.capacity() != 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap_or_else(|_| {
        panic!(
            "receiver-drop fixture did not reach backpressure; capacity={}; max_capacity={}",
            receiver.capacity(),
            receiver.max_capacity()
        )
    });

    drop(receiver);
    tokio::time::timeout(Duration::from_secs(2), closed)
        .await
        .expect("dropping a full subscription receiver retained the HTTP transport")
        .expect("receiver-drop fixture dropped its transport-close milestone");
    let attempts = wait_for_producer_finished(&mut events, "backpressured receiver drop").await;
    assert!(
            attempts.len() > STREAM_BUFFER_CAPACITY,
            "backpressured receiver drop never attempted the send that exceeds the ordinary budget; observed_send_attempts={}; attempts={attempts:?}",
            attempts.len()
        );
}

#[tokio::test]
async fn test_full_queue_protocol_error_releases_transport_before_delivery() {
    const ORDINARY_CAPACITY: usize = STREAM_BUFFER_CAPACITY;
    let (base, body_sent, closed) =
        held_open_subscription_server(text_delta_sse(ORDINARY_CAPACITY, true)).await;
    let (provider, mut events) = observe_stream_producer(
        ChatGptSubscriptionProvider::for_test(Arc::new(StaticSource::new()), &base, DEFAULT_MODEL)
            .expect("full-error fixture must construct a subscription provider"),
    );
    let mut receiver = provider
        .send_message_stream(&ProviderRequest::new(vec![Message::user("hello")]))
        .await
        .expect("full-error fixture must start the production streaming boundary");
    tokio::time::timeout(Duration::from_secs(2), body_sent)
        .await
        .expect("full-error fixture did not send its malformed SSE suffix")
        .expect("full-error fixture dropped its body-sent milestone");
    tokio::time::timeout(Duration::from_secs(2), closed)
        .await
        .expect("a full output queue retained HTTP after the protocol error")
        .expect("full-error fixture dropped its transport-close milestone");
    wait_for_producer_finished(&mut events, "full-queue protocol error").await;
    assert!(
        receiver.is_closed(),
        "full-queue protocol error left a sender alive before drain; capacity={}; max_capacity={}",
        receiver.capacity(),
        receiver.max_capacity()
    );
    assert_eq!(
            receiver.capacity(),
            0,
            "protocol error did not fill the ordinary queue plus reserved terminal slot; max_capacity={}",
            receiver.max_capacity()
        );

    let mut outcome = Vec::new();
    tokio::time::timeout(Duration::from_secs(2), async {
        while let Some(chunk) = receiver.recv().await {
            outcome.push(chunk.map_err(|error| error.to_string()));
        }
    })
    .await
    .unwrap_or_else(|_| {
        panic!("full-queue protocol error did not terminate exactly once; outcome={outcome:?}")
    });
    assert_eq!(
        outcome.len(),
        ORDINARY_CAPACITY + 1,
        "full queue must contain 32 deltas and one terminal error; outcome={outcome:?}"
    );
    for (index, chunk) in outcome[..ORDINARY_CAPACITY].iter().enumerate() {
        assert!(
            matches!(chunk, Ok(StreamChunk::TextDelta(delta)) if delta == &format!("delta-{index}")),
            "protocol failure changed or reordered delta {index}; outcome={outcome:?}"
        );
    }
    assert!(
        matches!(outcome.last(), Some(Err(error)) if error.contains("strictly increasing")),
        "protocol failure must be the one final stream outcome; outcome={outcome:?}"
    );
}

#[tokio::test]
async fn test_cancellation_during_full_terminal_projection_ends_with_one_error() {
    const BLOCK_COUNT: usize = 40;
    const ORDINARY_CAPACITY: usize = STREAM_BUFFER_CAPACITY;
    let mut server = mockito::Server::new_async().await;
    let models = server
        .mock("GET", "/backend-api/codex/models")
        .match_query(mockito::Matcher::UrlEncoded(
            "client_version".into(),
            CHATGPT_CATALOG_CLIENT_VERSION.into(),
        ))
        .with_status(200)
        .with_body(catalog_body())
        .expect(1)
        .create_async()
        .await;
    let inference = server
        .mock("POST", RESPONSES_PATH)
        .with_status(200)
        .with_header("content-type", "text/event-stream")
        .with_header("openai-model", DEFAULT_MODEL)
        .with_body(many_terminal_blocks_sse(BLOCK_COUNT))
        .expect(1)
        .create_async()
        .await;
    let provider = ChatGptSubscriptionProvider::for_test(
        Arc::new(StaticSource::new()),
        &format!("{}/backend-api/codex", server.url()),
        DEFAULT_MODEL,
    )
    .expect("terminal-backpressure fixture must construct a subscription provider");
    let cancel = CancellationToken::new();
    let mut receiver = provider
        .send_message_stream(
            &ProviderRequest::new(vec![Message::user("hello")])
                .with_cancellation_token(cancel.clone()),
        )
        .await
        .expect("terminal-backpressure fixture must start the production stream");
    tokio::time::timeout(Duration::from_secs(2), async {
        while receiver.capacity() != 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap_or_else(|_| {
        panic!(
            "terminal projection did not fill its ordinary queue; capacity={}; max_capacity={}",
            receiver.capacity(),
            receiver.max_capacity()
        )
    });

    cancel.cancel();
    let mut outcome = Vec::new();
    tokio::time::timeout(Duration::from_secs(2), async {
        while let Some(chunk) = receiver.recv().await {
            outcome.push(chunk.map_err(|error| error.to_string()));
        }
    })
    .await
    .unwrap_or_else(|_| {
        panic!("cancelled terminal projection did not terminate; outcome={outcome:?}")
    });
    assert_eq!(
        outcome.len(),
        ORDINARY_CAPACITY + 1,
        "terminal projection must end with one cancellation error; outcome={outcome:?}"
    );
    assert!(
        matches!(outcome.first(), Some(Ok(StreamChunk::ResponseMetadata { model })) if model == DEFAULT_MODEL),
        "terminal projection omitted or reordered response metadata; outcome={outcome:?}"
    );
    assert!(
        matches!(outcome.get(1), Some(Ok(StreamChunk::Usage { input_tokens: 1, output_tokens })) if *output_tokens == BLOCK_COUNT as u32),
        "terminal projection omitted or reordered usage; outcome={outcome:?}"
    );
    for (index, chunk) in outcome[2..ORDINARY_CAPACITY].iter().enumerate() {
        assert!(
            matches!(chunk, Ok(StreamChunk::ContentBlockComplete(ContentBlock::Text { text })) if text == &format!("block-{index}")),
            "terminal projection changed or reordered block {index}; outcome={outcome:?}"
        );
    }
    assert!(
        matches!(outcome.last(), Some(Err(error)) if error.contains("cancelled")),
        "terminal cancellation must be the one final stream outcome; outcome={outcome:?}"
    );
    models.assert_async().await;
    inference.assert_async().await;
}

#[tokio::test]
async fn test_terminal_metadata_usage_and_allowance_sends_cancel_when_full() {
    for (delta_count, target) in [
        (32usize, "metadata"),
        (31usize, "usage"),
        (30usize, "allowance"),
    ] {
        let mut server = mockito::Server::new_async().await;
        let models = server
            .mock("GET", "/backend-api/codex/models")
            .match_query(mockito::Matcher::UrlEncoded(
                "client_version".into(),
                CHATGPT_CATALOG_CLIENT_VERSION.into(),
            ))
            .with_status(200)
            .with_body(catalog_body())
            .expect(1)
            .create_async()
            .await;
        let inference = server
            .mock("POST", RESPONSES_PATH)
            .with_status(200)
            .with_header("content-type", "text/event-stream")
            .with_header("openai-model", DEFAULT_MODEL)
            .with_header("x-codex-primary-used-percent", "25.5")
            .with_body(completed_sse_after_deltas(delta_count))
            .expect(1)
            .create_async()
            .await;
        let (provider, mut events) = observe_stream_producer(
            ChatGptSubscriptionProvider::for_test(
                Arc::new(StaticSource::new()),
                &format!("{}/backend-api/codex", server.url()),
                DEFAULT_MODEL,
            )
            .expect("terminal-callsite fixture must construct a subscription provider"),
        );
        let cancel = CancellationToken::new();
        let mut receiver = provider
            .send_message_stream(
                &ProviderRequest::new(vec![Message::user("hello")])
                    .with_cancellation_token(cancel.clone()),
            )
            .await
            .unwrap_or_else(|error| {
                panic!("{target} fixture failed to start the production stream: {error:#}")
            });

        tokio::time::timeout(Duration::from_secs(2), async {
                loop {
                    match events.recv().await {
                        Some(StreamProducerEvent::SendAttempt(chunk))
                            if matches!(
                                (target, &chunk),
                                ("metadata", StreamChunk::ResponseMetadata { .. })
                                    | ("usage", StreamChunk::Usage { .. })
                                    | ("allowance", StreamChunk::Allowance { .. })
                            ) =>
                        {
                            break;
                        }
                        Some(StreamProducerEvent::SendAttempt(_)) => {}
                        Some(StreamProducerEvent::Finished { panicking }) => {
                            panic!(
                                "{target} producer finished before attempting its blocked send; unwound={panicking}"
                            )
                        }
                        None => panic!("{target} observer closed before its blocked send"),
                    }
                }
            })
            .await
            .unwrap_or_else(|_| {
                panic!(
                    "{target} send was not attempted while the queue was full; capacity={}; max_capacity={}",
                    receiver.capacity(),
                    receiver.max_capacity()
                )
            });
        assert_eq!(
            receiver.capacity(),
            0,
            "{target} send attempt was not pending behind 32 ordinary chunks; max_capacity={}",
            receiver.max_capacity()
        );

        cancel.cancel();
        wait_for_producer_finished(&mut events, target).await;
        assert!(
            receiver.is_closed(),
            "{target} cancellation left a producer sender alive before drain; capacity={}",
            receiver.capacity()
        );
        let mut outcome = Vec::new();
        tokio::time::timeout(Duration::from_secs(2), async {
            while let Some(chunk) = receiver.recv().await {
                outcome.push(chunk.map_err(|error| error.to_string()));
            }
        })
        .await
        .unwrap_or_else(|_| {
            panic!("{target} cancellation did not reach terminal None; outcome={outcome:?}")
        });
        assert_eq!(
                outcome.len(),
                STREAM_BUFFER_CAPACITY + 1,
                "{target} cancellation must preserve 32 ordinary chunks plus one error; outcome={outcome:?}"
            );
        for (index, chunk) in outcome[..delta_count].iter().enumerate() {
            assert!(
                matches!(chunk, Ok(StreamChunk::TextDelta(delta)) if delta == &format!("delta-{index}")),
                "{target} cancellation changed delta {index}; outcome={outcome:?}"
            );
        }
        let mut next = delta_count;
        if target != "metadata" {
            assert!(
                matches!(outcome.get(next), Some(Ok(StreamChunk::ResponseMetadata { model })) if model == DEFAULT_MODEL),
                "{target} cancellation changed the queued metadata prefix; outcome={outcome:?}"
            );
            next += 1;
        }
        if target == "allowance" {
            assert!(
                matches!(
                    outcome.get(next),
                    Some(Ok(StreamChunk::Usage {
                        input_tokens: 12,
                        output_tokens: 7
                    }))
                ),
                "allowance cancellation changed the queued usage prefix; outcome={outcome:?}"
            );
            next += 1;
        }
        assert_eq!(
            next, STREAM_BUFFER_CAPACITY,
            "{target} fixture did not fill exactly the ordinary budget; outcome={outcome:?}"
        );
        assert!(
                matches!(outcome.last(), Some(Err(error)) if error.contains("cancelled")),
                "{target} cancellation must end in exactly one error with no target/post-terminal chunk; outcome={outcome:?}"
            );
        models.assert_async().await;
        inference.assert_async().await;
    }
}

#[tokio::test]
async fn test_full_queue_drains_valid_stream_to_exact_success() {
    const DELTA_COUNT: usize = 35;
    let mut server = mockito::Server::new_async().await;
    let models = server
        .mock("GET", "/backend-api/codex/models")
        .match_query(mockito::Matcher::UrlEncoded(
            "client_version".into(),
            CHATGPT_CATALOG_CLIENT_VERSION.into(),
        ))
        .with_status(200)
        .with_body(catalog_body())
        .expect(1)
        .create_async()
        .await;
    let inference = server
        .mock("POST", RESPONSES_PATH)
        .with_status(200)
        .with_header("content-type", "text/event-stream")
        .with_header("openai-model", DEFAULT_MODEL)
        .with_header("x-codex-primary-used-percent", "25.5")
        .with_body(completed_sse_after_deltas(DELTA_COUNT))
        .expect(1)
        .create_async()
        .await;
    let provider = ChatGptSubscriptionProvider::for_test(
        Arc::new(StaticSource::new()),
        &format!("{}/backend-api/codex", server.url()),
        DEFAULT_MODEL,
    )
    .expect("successful-drain fixture must construct a subscription provider");
    let mut receiver = provider
        .send_message_stream(&ProviderRequest::new(vec![Message::user("hello")]))
        .await
        .expect("successful-drain fixture must start the production stream");
    tokio::time::timeout(Duration::from_secs(2), async {
        while receiver.capacity() != 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap_or_else(|_| {
        panic!(
            "valid stream never filled its 32 ordinary slots; capacity={}; max_capacity={}",
            receiver.capacity(),
            receiver.max_capacity()
        )
    });

    let mut outcome = Vec::new();
    tokio::time::timeout(Duration::from_secs(2), async {
        while let Some(chunk) = receiver.recv().await {
            outcome.push(chunk.map_err(|error| error.to_string()));
        }
    })
    .await
    .unwrap_or_else(|_| {
        panic!("valid full queue did not resume as capacity drained; outcome={outcome:?}")
    });
    assert_eq!(
            outcome.len(),
            DELTA_COUNT + 4,
            "valid stream must emit every delta and terminal projection exactly once; outcome={outcome:?}"
        );
    assert!(
        outcome.iter().all(Result::is_ok),
        "valid full-queue drain emitted a terminal error; outcome={outcome:?}"
    );
    for (index, chunk) in outcome[..DELTA_COUNT].iter().enumerate() {
        assert!(
            matches!(chunk, Ok(StreamChunk::TextDelta(delta)) if delta == &format!("delta-{index}")),
            "valid drain changed or reordered delta {index}; outcome={outcome:?}"
        );
    }
    assert!(
        matches!(outcome.get(DELTA_COUNT), Some(Ok(StreamChunk::ResponseMetadata { model })) if model == DEFAULT_MODEL),
        "valid drain omitted or reordered response metadata; outcome={outcome:?}"
    );
    assert!(
        matches!(
            outcome.get(DELTA_COUNT + 1),
            Some(Ok(StreamChunk::Usage {
                input_tokens: 12,
                output_tokens: 7
            }))
        ),
        "valid drain omitted or reordered usage; outcome={outcome:?}"
    );
    assert!(
        matches!(outcome.get(DELTA_COUNT + 2), Some(Ok(StreamChunk::Allowance { primary_used_percent: Some(primary), secondary_used_percent: None })) if (*primary - 25.5).abs() < f32::EPSILON),
        "valid drain omitted or reordered allowance; outcome={outcome:?}"
    );
    let expected_text = (0..DELTA_COUNT)
        .map(|index| format!("delta-{index}"))
        .collect::<String>();
    assert!(
        matches!(outcome.last(), Some(Ok(StreamChunk::ContentBlockComplete(ContentBlock::Text { text }))) if text == &expected_text),
        "valid drain omitted or changed completed content; outcome={outcome:?}"
    );
    models.assert_async().await;
    inference.assert_async().await;
}

#[tokio::test]
async fn request_timeout_is_bounded_and_releases_subscription_transport() {
    let (base, closed) = stalling_subscription_server(false).await;
    let mut provider =
        ChatGptSubscriptionProvider::for_test(Arc::new(StaticSource::new()), &base, DEFAULT_MODEL)
            .unwrap();
    provider.client = Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_millis(50))
        .build()
        .unwrap();
    let error = provider
        .send_message(&ProviderRequest::new(vec![Message::user("hello")]))
        .await
        .unwrap_err()
        .to_string();
    assert!(error.contains("Failed to start ChatGPT subscription response"));
    tokio::time::timeout(Duration::from_secs(2), closed)
        .await
        .expect("subscription transport was not released after request timeout")
        .unwrap();
}

#[tokio::test]
async fn test_eof_duplicate_done_and_post_terminal_data_fail_before_completion_effects() {
    let created = format!(
        "event: response.created\ndata: {}\n\n",
        json!({"type":"response.created","sequence_number":1,"response":{"headers":{"openai-model":DEFAULT_MODEL}}})
    );
    let eof = subscription_stream_outcome(created.clone(), DEFAULT_MODEL).await;
    assert_eq!(
        eof.len(),
        1,
        "EOF before completion must emit exactly one terminal error and no completion effects; \
             outcome={eof:?}"
    );
    assert!(
        matches!(&eof[0], Err(error) if error.contains("before response.completed")),
        "EOF before completion must report the missing response.completed event; \
             outcome={eof:?}"
    );

    let terminal = format!(
        "{}event: response.completed\ndata: {}\n\ndata: [DONE]\n\n",
        created,
        json!({"type":"response.completed","sequence_number":2,"response":{"id":"resp"}})
    );
    let duplicate =
        subscription_stream_outcome(format!("{terminal}data: [DONE]\n\n"), DEFAULT_MODEL).await;
    assert_eq!(
        duplicate.len(),
        1,
        "duplicate terminal markers must emit exactly one terminal error and no completion \
             effects; outcome={duplicate:?}"
    );
    assert!(
        matches!(&duplicate[0], Err(error) if error.contains("terminal marker was invalid")),
        "duplicate terminal markers must report an invalid terminal marker; \
             outcome={duplicate:?}"
    );

    let late = subscription_stream_outcome(
        format!(
            "{terminal}event: response.in_progress\ndata: {}\n\n",
            json!({"type":"response.in_progress","sequence_number":3,"response":{}})
        ),
        DEFAULT_MODEL,
    )
    .await;
    assert_eq!(
        late.len(),
        1,
        "post-terminal data must emit exactly one terminal error and no completion effects; \
             outcome={late:?}"
    );
    assert!(
        matches!(&late[0], Err(error) if error.contains("data after its terminal response")),
        "post-terminal data must report data after the terminal response; outcome={late:?}"
    );
}

#[tokio::test]
async fn test_buffered_and_streaming_reject_actual_model_drift_before_completion_effects() {
    let body = format!(
        concat!(
            "event: response.created\ndata: {}\n\n",
            "event: response.metadata\ndata: {}\n\n"
        ),
        json!({"type":"response.created","sequence_number":1,"response":{"headers":{"openai-model":"gpt-4o"}}}),
        json!({"type":"response.metadata","sequence_number":2,"headers":{"openai-model":DEFAULT_MODEL}})
    );
    let mut server = mockito::Server::new_async().await;
    let models = server
        .mock("GET", "/backend-api/codex/models")
        .match_query(mockito::Matcher::UrlEncoded(
            "client_version".into(),
            CHATGPT_CATALOG_CLIENT_VERSION.into(),
        ))
        .with_status(200)
        .with_body(catalog_body())
        .expect(1)
        .create_async()
        .await;
    let inference = server
        .mock("POST", RESPONSES_PATH)
        .with_status(200)
        .with_header("content-type", "text/event-stream")
        .with_header("openai-model", "gpt-4o")
        .with_body(body)
        .expect(2)
        .create_async()
        .await;
    let provider = ChatGptSubscriptionProvider::for_test(
        Arc::new(StaticSource::new()),
        &format!("{}/backend-api/codex", server.url()),
        DEFAULT_MODEL,
    )
    .expect("model-drift fixture must construct a provider");
    let request = ProviderRequest::new(vec![Message::user("hello")]);
    let (buffered, streaming) = tokio::join!(
        provider.send_message(&request),
        provider.send_message_stream(&request)
    );
    models.assert_async().await;
    inference.assert_async().await;

    let buffered_error = buffered
        .err()
        .expect("model drift must fail the buffered response");
    assert!(
        buffered_error
            .to_string()
            .contains("changed during the response"),
        "buffered model drift returned an unhelpful diagnostic: {buffered_error:#}"
    );
    let mut receiver =
        streaming.expect("stream setup must succeed before contradictory provenance is consumed");
    let mut outcome = Vec::new();
    while let Some(chunk) = receiver.recv().await {
        outcome.push(chunk.map_err(|error| error.to_string()));
    }
    assert_eq!(
        outcome.len(),
        1,
        "model drift must emit exactly one terminal error and no completion effects; \
             outcome={outcome:?}"
    );
    assert!(
        matches!(&outcome[0], Err(error) if error.contains("changed during the response")),
        "model drift must emit exactly one terminal error and no completion effects; \
             outcome={outcome:?}"
    );
}

#[tokio::test]
async fn test_buffered_and_streaming_reject_conflicting_outer_model_headers_before_effects() {
    let mut server = mockito::Server::new_async().await;
    let models = server
        .mock("GET", "/backend-api/codex/models")
        .match_query(mockito::Matcher::UrlEncoded(
            "client_version".into(),
            CHATGPT_CATALOG_CLIENT_VERSION.into(),
        ))
        .with_status(200)
        .with_body(catalog_body())
        .expect(1)
        .create_async()
        .await;
    let inference = server
        .mock("POST", RESPONSES_PATH)
        .with_status(200)
        .with_header("content-type", "text/event-stream")
        .with_header("openai-model", "gpt-4o")
        .with_header("openai-model", DEFAULT_MODEL)
        .with_body(completed_sse_with_model_provenance(None))
        .expect(2)
        .create_async()
        .await;
    let provider = ChatGptSubscriptionProvider::for_test(
        Arc::new(StaticSource::new()),
        &format!("{}/backend-api/codex", server.url()),
        DEFAULT_MODEL,
    )
    .expect("conflicting-outer-header fixture must construct a provider");
    let request = ProviderRequest::new(vec![Message::user("hello")]);
    let (buffered, streaming) = tokio::join!(
        provider.send_message(&request),
        provider.send_message_stream(&request)
    );
    models.assert_async().await;
    inference.assert_async().await;

    let buffered_error = buffered
        .err()
        .expect("conflicting outer model headers must fail the buffered response");
    assert!(
        buffered_error
            .to_string()
            .contains("changed during the response"),
        "conflicting buffered outer headers returned an unhelpful diagnostic: \
             {buffered_error:#}"
    );
    let mut receiver = streaming
        .expect("stream setup must succeed before conflicting outer model headers are consumed");
    let mut outcome = Vec::new();
    while let Some(chunk) = receiver.recv().await {
        outcome.push(chunk.map_err(|error| error.to_string()));
    }
    assert_eq!(
        outcome.len(),
        1,
        "conflicting outer model headers must emit exactly one terminal error and no effects; \
             outcome={outcome:?}"
    );
    assert!(
        matches!(&outcome[0], Err(error) if error.contains("changed during the response")),
        "conflicting outer model headers returned the wrong streaming outcome; \
             outcome={outcome:?}"
    );
}

#[tokio::test]
async fn catalog_etag_is_generation_revalidated_but_never_crosses_accounts() {
    let mut server = mockito::Server::new_async().await;
    let first = server
        .mock("GET", "/backend-api/codex/models")
        .match_query(mockito::Matcher::UrlEncoded(
            "client_version".into(),
            CHATGPT_CATALOG_CLIENT_VERSION.into(),
        ))
        .match_header("chatgpt-account-id", "account-1")
        .match_header("if-none-match", mockito::Matcher::Missing)
        .with_status(200)
        .with_header("etag", "account-1-etag")
        .with_body(catalog_body())
        .expect(1)
        .create_async()
        .await;
    let revalidated = server
        .mock("GET", "/backend-api/codex/models")
        .match_query(mockito::Matcher::UrlEncoded(
            "client_version".into(),
            CHATGPT_CATALOG_CLIENT_VERSION.into(),
        ))
        .match_header("chatgpt-account-id", "account-1")
        .match_header("if-none-match", "account-1-etag")
        .with_status(304)
        .expect(1)
        .create_async()
        .await;
    let other_account = server
        .mock("GET", "/backend-api/codex/models")
        .match_query(mockito::Matcher::UrlEncoded(
            "client_version".into(),
            CHATGPT_CATALOG_CLIENT_VERSION.into(),
        ))
        .match_header("chatgpt-account-id", "account-2")
        .match_header("if-none-match", mockito::Matcher::Missing)
        .with_status(200)
        .with_body(catalog_body())
        .expect(1)
        .create_async()
        .await;
    let provider = ChatGptSubscriptionProvider::for_test(
        Arc::new(StaticSource::new()),
        &format!("{}/backend-api/codex", server.url()),
        DEFAULT_MODEL,
    )
    .unwrap();
    let cancel = CancellationToken::new();
    provider
        .account_catalog(
            &ChatGptCredentialLease {
                access_token: "secret-1".into(),
                account: "account-1".into(),
                generation: "generation-1".into(),
            },
            &cancel,
        )
        .await
        .unwrap();
    provider
        .account_catalog(
            &ChatGptCredentialLease {
                access_token: "secret-2".into(),
                account: "account-1".into(),
                generation: "generation-2".into(),
            },
            &cancel,
        )
        .await
        .unwrap();
    provider
        .account_catalog(
            &ChatGptCredentialLease {
                access_token: "secret-3".into(),
                account: "account-2".into(),
                generation: "generation-3".into(),
            },
            &cancel,
        )
        .await
        .unwrap();
    first.assert_async().await;
    revalidated.assert_async().await;
    other_account.assert_async().await;
}

#[tokio::test]
async fn non_success_bodies_are_bounded_and_redacted() {
    let mut server = mockito::Server::new_async().await;
    let models = server
        .mock("GET", "/backend-api/codex/models")
        .match_query(mockito::Matcher::UrlEncoded(
            "client_version".into(),
            CHATGPT_CATALOG_CLIENT_VERSION.into(),
        ))
        .with_status(200)
        .with_body(catalog_body())
        .create_async()
        .await;
    let secret = "attacker-tool-argument-and-reasoning-secret";
    let inference = server
        .mock("POST", RESPONSES_PATH)
        .with_status(429)
        .with_body(format!("{secret}{}", "x".repeat(MAX_ERROR_BYTES)))
        .create_async()
        .await;
    let provider = ChatGptSubscriptionProvider::for_test(
        Arc::new(StaticSource::new()),
        &format!("{}/backend-api/codex", server.url()),
        DEFAULT_MODEL,
    )
    .unwrap();
    let error = provider
        .send_message(&ProviderRequest::new(vec![Message::user("hello")]))
        .await
        .unwrap_err()
        .to_string();
    assert!(!error.contains(secret));
    assert!(!error.contains("subscription-secret"));
    assert!(error.contains("size limit"));
    assert!(error.len() < 256);
    models.assert_async().await;
    inference.assert_async().await;
}

#[tokio::test]
async fn response_rejection_is_typed_clear_and_secret_free() {
    let mut server = mockito::Server::new_async().await;
    let models = server
        .mock("GET", "/backend-api/codex/models")
        .match_query(mockito::Matcher::UrlEncoded(
            "client_version".into(),
            CHATGPT_CATALOG_CLIENT_VERSION.into(),
        ))
        .with_status(200)
        .with_body(catalog_body())
        .create_async()
        .await;
    let attacker_body = "account-1 subscription-secret private-tool-argument private-reasoning";
    let inference = server
        .mock("POST", RESPONSES_PATH)
        .with_status(400)
        .with_body(attacker_body)
        .create_async()
        .await;
    let provider = ChatGptSubscriptionProvider::for_test(
        Arc::new(StaticSource::new()),
        &format!("{}/backend-api/codex", server.url()),
        DEFAULT_MODEL,
    )
    .unwrap();
    let error = provider
        .send_message(&ProviderRequest::new(vec![Message::user("hello")]))
        .await
        .unwrap_err();
    let rejection = error
        .downcast_ref::<SubscriptionResponseRejected>()
        .expect("HTTP rejection must retain its typed provider boundary");
    assert_eq!(rejection.0, StatusCode::BAD_REQUEST);
    let display = error.to_string();
    assert!(display.contains("HTTP 400 Bad Request"));
    assert!(display.contains("pinned protocol contract may have changed"));
    assert!(!display.contains(attacker_body));
    assert!(!display.contains("account-1"));
    assert!(!display.contains("subscription-secret"));
    assert!(display.len() < 256);
    models.assert_async().await;
    inference.assert_async().await;
}

#[test]
fn hostile_origin_route_and_request_preflight_fail_before_credentials() {
    let source = Arc::new(StaticSource::new());
    for endpoint in [
        "https://api.openai.com/backend-api/codex",
        "https://chatgpt.com.evil/backend-api/codex",
        "https://user@chatgpt.com/backend-api/codex",
        "https://chatgpt.com/backend-api/codex?redirect=evil",
    ] {
        assert!(ChatGptSubscriptionProvider::new(
            source.clone(),
            endpoint,
            DEFAULT_MODEL,
            ReasoningEffort::High,
            false,
        )
        .is_err());
    }
    assert_eq!(source.leases.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn malformed_image_and_history_fail_before_credential_or_network_use() {
    let source = Arc::new(StaticSource::new());
    let provider = ChatGptSubscriptionProvider::for_test(
        source.clone(),
        "http://127.0.0.1:9/backend-api/codex",
        DEFAULT_MODEL,
    )
    .unwrap();
    let invalid_image = ProviderRequest::new(vec![Message::with_content(
        "user",
        vec![ContentBlock::image("image/png", "iVBORw0KGgo=")],
    )]);
    assert!(provider.send_message(&invalid_image).await.is_err());
    let invalid_role = ProviderRequest::new(vec![Message::with_content(
        "developer",
        vec![ContentBlock::text("attacker-controlled")],
    )]);
    assert!(provider.send_message(&invalid_role).await.is_err());
    assert_eq!(source.leases.load(Ordering::SeqCst), 0);
}

#[test]
fn tool_argument_reasoning_and_sse_boundaries_are_enforced() {
    let huge = "x".repeat(MAX_TOOL_ARGUMENT_BYTES + 1);
    let request = ProviderRequest::new(vec![Message::with_content(
        "assistant",
        vec![ContentBlock::ToolUse {
            id: "call".to_string(),
            name: "read".to_string(),
            input: json!({"secret":huge}),
        }],
    )])
    .with_model(DEFAULT_MODEL);
    assert!(encode_responses_lite(&request, ReasoningEffort::High).is_err());
    assert!(enforce_sse_remainder_bounds(&vec![b'x'; MAX_SSE_LINE_BYTES + 1]).is_err());
    assert!(sse_data(b"future: attacker-secret").is_err());
}

fn live_chatgpt_subscription_provider() -> Result<ChatGptSubscriptionProvider> {
    if std::env::var("FINCH_LIVE_CHATGPT_ACCEPTANCE").as_deref() != Ok("1") {
        bail!("Set FINCH_LIVE_CHATGPT_ACCEPTANCE=1 after security review");
    }
    let credential_name = std::env::var("FINCH_LIVE_CHATGPT_CREDENTIAL")
        .context("Set FINCH_LIVE_CHATGPT_CREDENTIAL to the named OAuth credential")?;
    let oauth_root_preview = std::env::var_os("FINCH_LIVE_CHATGPT_OAUTH_ROOT")
        .context("Set FINCH_LIVE_CHATGPT_OAUTH_ROOT to Finch's oauth directory")?;
    let store = crate::oauth::FileOAuthCredentialStore::new(oauth_root_preview.into());
    let record = store
        .load_existing(&credential_name)?
        .context("named ChatGPT credential is missing from the oauth store")?;
    let credential = record.provider_credential(&credential_name);
    let configured_model = std::env::var("FINCH_LIVE_CHATGPT_MODEL").ok();
    let configured_reasoning = None;
    let oauth_root = std::env::var_os("FINCH_LIVE_CHATGPT_OAUTH_ROOT")
        .context("Set FINCH_LIVE_CHATGPT_OAUTH_ROOT to Finch's oauth directory")?;
    ChatGptSubscriptionProvider::production_in_oauth_root(
        &credential,
        configured_model.as_deref(),
        configured_reasoning,
        oauth_root,
    )
}

/// Opt-in live acceptance uses Finch's own named device credential. It is
/// ignored by default and intentionally never prints tokens or bodies.
#[tokio::test]
#[ignore = "requires FINCH_LIVE_CHATGPT_ACCEPTANCE=1 and reviewed Finch device login"]
async fn live_chatgpt_subscription_acceptance_is_explicitly_opt_in() -> Result<()> {
    let provider = live_chatgpt_subscription_provider()?;
    const EXPECTED_TEXT: &str = "Finch native subscription transport accepted";
    let response = provider
        .send_message(&ProviderRequest::new(vec![Message::user(format!(
            "Reply with exactly: {EXPECTED_TEXT}"
        ))]))
        .await?;
    if response.model.trim().is_empty() {
        bail!("Live ChatGPT subscription acceptance returned incompatible provenance");
    }
    if response.text().trim() != EXPECTED_TEXT {
        bail!("Live ChatGPT subscription acceptance returned unexpected text");
    }
    Ok::<(), anyhow::Error>(())
}

/// Opt-in live acceptance for Finch's collaboration-tool wire binding.
/// The provider parses the call but intentionally does not execute it.
#[tokio::test]
#[ignore = "requires FINCH_LIVE_CHATGPT_ACCEPTANCE=1 and reviewed Finch device login"]
async fn live_chatgpt_subscription_collaboration_tool_is_explicitly_opt_in() -> Result<()> {
    let provider = live_chatgpt_subscription_provider()?;
    let request = ProviderRequest::new(vec![Message::user(
            "Call the spawn_agent function exactly once with task exactly `say your name`. Do not answer in text.",
        )])
        .with_tools(vec![named_tool("spawn_agent")]);
    let response = provider.send_message(&request).await?;
    if response.model.trim().is_empty() {
        bail!("Live ChatGPT collaboration-tool acceptance returned incompatible provenance");
    }
    let calls = response
        .content
        .iter()
        .filter_map(|block| match block {
            ContentBlock::ToolUse { id, name, input } => Some((id, name, input)),
            _ => None,
        })
        .collect::<Vec<_>>();
    if calls.len() != 1 {
        bail!(
                "Live ChatGPT collaboration-tool acceptance returned an unexpected call count: actual_count={}",
                calls.len()
            );
    }
    let (id, name, input) = calls[0];
    let id_present = !id.trim().is_empty();
    let local_name_matched = name == "spawn_agent";
    let arguments_matched = input == &json!({"task":"say your name"});
    if !id_present || !local_name_matched || !arguments_matched {
        bail!(
            "Live ChatGPT collaboration-tool acceptance returned an incompatible call: \
                 id_present={id_present}, local_name_matched={local_name_matched}, \
                 arguments_matched={arguments_matched}"
        );
    }
    if !response.text().trim().is_empty() {
        bail!("Live ChatGPT collaboration-tool acceptance returned unexpected text");
    }
    Ok::<(), anyhow::Error>(())
}
