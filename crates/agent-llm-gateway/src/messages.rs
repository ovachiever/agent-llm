//! Claude-Code-facing Anthropic Messages endpoint.
//!
//! Accepts Anthropic Messages requests with `provider/model` ids. Requests for
//! Anthropic-dialect upstreams (anthropic, kimi) pass through with the model id
//! rewritten; requests for Chat-Completions upstreams (openai, openrouter,
//! lmstudio) are translated both ways, including SSE streams, so Claude Code
//! can run on K3 via OpenRouter or on local LM Studio models.

use std::time::Instant;

use agent_llm_core::types::{ProviderKind, UsageSnapshot};
use agent_llm_translate::{
    Dialect, ReverseStreamTranslator, SseParser, anthropic_to_chat, chat_response_to_anthropic,
    format_sse,
};
use axum::{
    Json,
    body::{Body, Bytes},
    extract::State,
    http::{HeaderMap, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
};
use futures_util::StreamExt;
use serde_json::{Value, json};
use tokio_stream::wrappers::ReceiverStream;
use tracing::warn;
use uuid::Uuid;

use crate::{
    ApiError, AppState, apply_upstream_auth, extract_project_key, extract_usage, resolve_profile,
    responses,
};

const MESSAGES_LOG_PATH: &str = "/v1/messages";

pub async fn messages_create(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Response, ApiError> {
    let project_key = extract_project_key(&headers)
        .ok_or_else(|| ApiError::unauthorized("missing project key"))?;
    let project = state
        .db
        .project_by_key(&project_key)
        .map_err(|_| ApiError::unauthorized("invalid project key"))?;

    let mut request: Value = serde_json::from_slice(&body)
        .map_err(|error| ApiError::bad_request(format!("invalid JSON body: {error}")))?;
    let requested_model = request
        .get("model")
        .and_then(Value::as_str)
        .ok_or_else(|| ApiError::bad_request("missing `model`"))?
        .to_string();
    // Claude Code sessions can leak unprefixed `claude-*` ids from global
    // settings (subagent/small-fast pins). Route those to the Anthropic
    // passthrough instead of failing the session; `[1m]` is a harness alias
    // suffix, not part of the API model id.
    let (provider_kind, bare_model) = match responses::split_model(&requested_model) {
        Ok(pair) => pair,
        Err(error) => {
            if requested_model.starts_with("claude") {
                (
                    ProviderKind::Anthropic,
                    requested_model.trim_end_matches("[1m]"),
                )
            } else {
                return Err(error);
            }
        }
    };
    let dialect = responses::dialect_for(provider_kind).ok_or_else(|| {
        ApiError::bad_request(
            "google models are not yet supported via /v1/messages; use the /google passthrough",
        )
    })?;
    let is_stream = request
        .get("stream")
        .and_then(Value::as_bool)
        .unwrap_or(false);

    let requested_profile = headers
        .get("x-agent-llm-auth-profile")
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let profile = state
        .db
        .resolve_auth_profile(project.id, provider_kind, requested_profile)
        .map_err(ApiError::internal)?
        .ok_or_else(|| ApiError::bad_request("no auth profile configured"))?;
    let profile = resolve_profile(&state, provider_kind, &profile).await?;

    let bare_model_owned = bare_model.to_string();
    let upstream_body = match dialect {
        // Same protocol on both sides: forward as-is with the model id rewritten.
        Dialect::AnthropicMessages => {
            request["model"] = Value::String(bare_model_owned.clone());
            request
        }
        Dialect::ChatCompletions => anthropic_to_chat(&request, &bare_model_owned)
            .map_err(|error| ApiError::bad_request(error.message))?,
    };

    let provider_record = state
        .db
        .provider(provider_kind)
        .map_err(ApiError::internal)?;
    let upstream_path = match dialect {
        Dialect::AnthropicMessages => "/v1/messages",
        Dialect::ChatCompletions => "/v1/chat/completions",
    };
    let upstream_url = format!(
        "{}{}",
        provider_record.upstream_base_url.trim_end_matches('/'),
        upstream_path
    );

    let request_id = Uuid::new_v4().to_string();
    let response_id = format!("msg_{}", request_id.replace('-', ""));
    let start = Instant::now();

    let mut upstream_request = state.http.post(upstream_url).json(&upstream_body);
    upstream_request = apply_upstream_auth(upstream_request, provider_kind, &profile);
    if dialect == Dialect::AnthropicMessages {
        let version = headers
            .get("anthropic-version")
            .cloned()
            .unwrap_or_else(|| HeaderValue::from_static("2023-06-01"));
        upstream_request = upstream_request.header("anthropic-version", version);
    }
    if is_stream {
        upstream_request = upstream_request.header("accept", "text/event-stream");
    }

    let upstream_response = upstream_request.send().await.map_err(ApiError::upstream)?;
    let status = upstream_response.status();

    if !status.is_success() {
        let error_body = upstream_response.bytes().await.unwrap_or_default();
        log_messages_request(
            &state,
            &request_id,
            project.id,
            provider_kind,
            Some(profile.id),
            status.as_u16() as i64,
            start.elapsed().as_millis() as i64,
            empty_usage(),
            Some("upstream returned non-success"),
        );
        // Claude Code expects Anthropic-shaped errors.
        let payload: Value = serde_json::from_slice::<Value>(&error_body)
            .ok()
            .filter(|v| v.get("type").is_some())
            .unwrap_or_else(|| {
                json!({
                    "type": "error",
                    "error": {
                        "type": "api_error",
                        "message": String::from_utf8_lossy(&error_body),
                    }
                })
            });
        return Ok((
            StatusCode::from_u16(status.as_u16()).unwrap_or(StatusCode::BAD_GATEWAY),
            Json(payload),
        )
            .into_response());
    }

    match (dialect, is_stream) {
        (Dialect::AnthropicMessages, false) => {
            let bytes = upstream_response
                .bytes()
                .await
                .map_err(ApiError::upstream)?;
            let usage = extract_usage(provider_kind, &bytes, Some(&bare_model_owned));
            log_messages_request(
                &state,
                &request_id,
                project.id,
                provider_kind,
                Some(profile.id),
                status.as_u16() as i64,
                start.elapsed().as_millis() as i64,
                usage,
                None,
            );
            respond_json_bytes(bytes, &request_id)
        }
        (Dialect::AnthropicMessages, true) => {
            // Native Anthropic SSE both sides: relay bytes untouched.
            log_messages_request(
                &state,
                &request_id,
                project.id,
                provider_kind,
                Some(profile.id),
                status.as_u16() as i64,
                start.elapsed().as_millis() as i64,
                empty_usage(),
                None,
            );
            let mut response = Response::builder()
                .status(StatusCode::OK)
                .header("content-type", "text/event-stream; charset=utf-8")
                .header("cache-control", "no-store")
                .body(Body::from_stream(upstream_response.bytes_stream()))
                .map_err(|error| ApiError::internal(anyhow::anyhow!(error)))?;
            response.headers_mut().insert(
                "x-agent-llm-request-id",
                HeaderValue::from_str(&request_id).unwrap(),
            );
            Ok(response)
        }
        (Dialect::ChatCompletions, false) => {
            let upstream_json = upstream_response
                .json::<Value>()
                .await
                .map_err(ApiError::upstream)?;
            let translated =
                chat_response_to_anthropic(&upstream_json, &requested_model, &response_id)
                    .map_err(|error| ApiError::internal(anyhow::anyhow!(error.message)))?;
            let usage =
                anthropic_usage_snapshot(&state, provider_kind, &bare_model_owned, &translated);
            log_messages_request(
                &state,
                &request_id,
                project.id,
                provider_kind,
                Some(profile.id),
                status.as_u16() as i64,
                start.elapsed().as_millis() as i64,
                usage,
                None,
            );
            let mut response = Json(translated).into_response();
            response.headers_mut().insert(
                "x-agent-llm-request-id",
                HeaderValue::from_str(&request_id).unwrap(),
            );
            Ok(response)
        }
        (Dialect::ChatCompletions, true) => {
            let (tx, rx) = tokio::sync::mpsc::channel::<Result<String, std::io::Error>>(64);
            let db = state.db.clone();
            let stream_project_id = project.id;
            let stream_profile_id = profile.id;
            let stream_request_id = request_id.clone();
            let stream_model = bare_model_owned.clone();

            tokio::spawn(async move {
                let mut parser = SseParser::new();
                let mut translator = ReverseStreamTranslator::new(&requested_model, &response_id);
                let mut byte_stream = upstream_response.bytes_stream();
                let mut client_gone = false;

                'pump: while let Some(chunk) = byte_stream.next().await {
                    let bytes = match chunk {
                        Ok(bytes) => bytes,
                        Err(error) => {
                            warn!("upstream stream error: {error:#}");
                            break 'pump;
                        }
                    };
                    for event in parser.push(&bytes) {
                        for out in translator.push_event(&event) {
                            if tx.send(Ok(format_sse(&out))).await.is_err() {
                                client_gone = true;
                                break 'pump;
                            }
                        }
                    }
                }

                if !client_gone {
                    for out in translator.finish() {
                        let _ = tx.send(Ok(format_sse(&out))).await;
                    }
                }

                let (prompt_tokens, completion_tokens, total_tokens) = translator.usage();
                let usage = UsageSnapshot {
                    prompt_tokens,
                    completion_tokens,
                    total_tokens,
                    estimated_cost_usd: agent_llm_core::pricing::estimate_cost_usd(
                        provider_kind,
                        &stream_model,
                        prompt_tokens,
                        completion_tokens,
                    ),
                };
                if let Err(error) = db.log_request(
                    &stream_request_id,
                    stream_project_id,
                    provider_kind,
                    Some(stream_profile_id),
                    "POST",
                    MESSAGES_LOG_PATH,
                    status.as_u16() as i64,
                    start.elapsed().as_millis() as i64,
                    usage,
                    false,
                    if client_gone {
                        Some("client disconnected mid-stream")
                    } else {
                        None
                    },
                ) {
                    warn!("failed to log streamed request: {error:#}");
                }
            });

            let mut response = Response::builder()
                .status(StatusCode::OK)
                .header("content-type", "text/event-stream; charset=utf-8")
                .header("cache-control", "no-store")
                .body(Body::from_stream(ReceiverStream::new(rx)))
                .map_err(|error| ApiError::internal(anyhow::anyhow!(error)))?;
            response.headers_mut().insert(
                "x-agent-llm-request-id",
                HeaderValue::from_str(&request_id).unwrap(),
            );
            Ok(response)
        }
    }
}

/// Claude Code polls this for context tracking. A rough serialized-length
/// estimate keeps it functional across upstreams that have no counting API.
pub async fn count_tokens(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Json<Value>, ApiError> {
    let project_key = extract_project_key(&headers)
        .ok_or_else(|| ApiError::unauthorized("missing project key"))?;
    state
        .db
        .project_by_key(&project_key)
        .map_err(|_| ApiError::unauthorized("invalid project key"))?;

    let request: Value = serde_json::from_slice(&body)
        .map_err(|error| ApiError::bad_request(format!("invalid JSON body: {error}")))?;
    let mut chars = 0usize;
    for key in ["system", "messages", "tools"] {
        if let Some(value) = request.get(key) {
            chars += serde_json::to_string(value).map(|s| s.len()).unwrap_or(0);
        }
    }
    Ok(Json(json!({ "input_tokens": (chars / 4).max(1) })))
}

fn respond_json_bytes(bytes: Bytes, request_id: &str) -> Result<Response, ApiError> {
    let mut response = Response::builder()
        .status(StatusCode::OK)
        .header("content-type", "application/json")
        .body(Body::from(bytes))
        .map_err(|error| ApiError::internal(anyhow::anyhow!(error)))?;
    response.headers_mut().insert(
        "x-agent-llm-request-id",
        HeaderValue::from_str(request_id).unwrap(),
    );
    Ok(response)
}

fn empty_usage() -> UsageSnapshot {
    UsageSnapshot {
        prompt_tokens: None,
        completion_tokens: None,
        total_tokens: None,
        estimated_cost_usd: None,
    }
}

fn anthropic_usage_snapshot(
    _state: &AppState,
    provider: ProviderKind,
    bare_model: &str,
    translated: &Value,
) -> UsageSnapshot {
    let prompt_tokens = translated
        .pointer("/usage/input_tokens")
        .and_then(Value::as_i64);
    let completion_tokens = translated
        .pointer("/usage/output_tokens")
        .and_then(Value::as_i64);
    UsageSnapshot {
        prompt_tokens,
        completion_tokens,
        total_tokens: prompt_tokens.zip(completion_tokens).map(|(a, b)| a + b),
        estimated_cost_usd: agent_llm_core::pricing::estimate_cost_usd(
            provider,
            bare_model,
            prompt_tokens,
            completion_tokens,
        ),
    }
}

#[allow(clippy::too_many_arguments)]
fn log_messages_request(
    state: &AppState,
    request_id: &str,
    project_id: i64,
    provider: ProviderKind,
    auth_profile_id: Option<i64>,
    status_code: i64,
    latency_ms: i64,
    usage: UsageSnapshot,
    error_text: Option<&str>,
) {
    if let Err(error) = state.db.log_request(
        request_id,
        project_id,
        provider,
        auth_profile_id,
        "POST",
        MESSAGES_LOG_PATH,
        status_code,
        latency_ms,
        usage,
        false,
        error_text,
    ) {
        warn!("failed to log request: {error:#}");
    }
}
