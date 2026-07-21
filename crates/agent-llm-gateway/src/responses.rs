//! Codex-facing Responses API endpoint.
//!
//! Accepts OpenAI Responses API requests with `provider/model` ids, translates
//! them to the upstream's native dialect via `agent-llm-translate`, and
//! translates the reply (including SSE streams) back. Unlike the passthrough
//! routes, streamed requests get usage and cost logging too, because every
//! stream event passes through the translator.

use std::time::Instant;

use agent_llm_core::{pricing, types::ProviderKind, types::UsageSnapshot};
use agent_llm_translate::{
    Dialect, SseParser, StreamTranslator, TranslateOptions, chat_response_to_responses, format_sse,
    responses_to_anthropic, responses_to_chat, usage_from_responses,
};
use axum::{
    Json,
    body::{Body, Bytes},
    extract::State,
    http::{HeaderMap, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
};
use chrono::Utc;
use futures_util::StreamExt;
use serde_json::{Value, json};
use tokio_stream::wrappers::ReceiverStream;
use tracing::warn;
use uuid::Uuid;

use crate::{ApiError, AppState, apply_upstream_auth, extract_project_key, resolve_profile};

const RESPONSES_LOG_PATH: &str = "/v1/responses";

pub fn dialect_for(provider: ProviderKind) -> Option<Dialect> {
    match provider {
        ProviderKind::OpenAi | ProviderKind::OpenRouter | ProviderKind::LmStudio => {
            Some(Dialect::ChatCompletions)
        }
        ProviderKind::Anthropic | ProviderKind::Kimi => Some(Dialect::AnthropicMessages),
        ProviderKind::Google => None,
    }
}

fn upstream_path(dialect: Dialect) -> &'static str {
    match dialect {
        Dialect::ChatCompletions => "/v1/chat/completions",
        Dialect::AnthropicMessages => "/v1/messages",
    }
}

/// Split `provider/model` into a provider and the bare upstream model id.
/// Model ids may themselves contain slashes (`lmstudio/qwen/qwen3.6-35b-a3b`).
fn split_model(model: &str) -> Result<(ProviderKind, &str), ApiError> {
    let (prefix, bare) = model.split_once('/').ok_or_else(|| {
        ApiError::bad_request(
            "model must be prefixed with a provider, e.g. \"lmstudio/openai/gpt-oss-20b\", \
             \"kimi/k3\", \"anthropic/claude-sonnet-5\", \"openai/gpt-5.5\"",
        )
    })?;
    let provider = ProviderKind::parse(prefix).ok_or_else(|| {
        ApiError::bad_request(format!(
            "unknown provider prefix `{prefix}`; expected one of: openai, anthropic, google, \
             openrouter, kimi, lmstudio"
        ))
    })?;
    if bare.is_empty() {
        return Err(ApiError::bad_request(
            "model id is empty after provider prefix",
        ));
    }
    Ok((provider, bare))
}

pub async fn responses_create(
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

    let request: Value = serde_json::from_slice(&body)
        .map_err(|error| ApiError::bad_request(format!("invalid JSON body: {error}")))?;
    let requested_model = request
        .get("model")
        .and_then(Value::as_str)
        .ok_or_else(|| ApiError::bad_request("missing `model`"))?
        .to_string();
    let (provider_kind, bare_model) = split_model(&requested_model)?;
    let dialect = dialect_for(provider_kind).ok_or_else(|| {
        ApiError::bad_request(
            "google models are not yet supported via /v1/responses; use the /google passthrough",
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

    let upstream_body = match dialect {
        Dialect::ChatCompletions => responses_to_chat(&request, bare_model),
        Dialect::AnthropicMessages => {
            responses_to_anthropic(&request, bare_model, &TranslateOptions::default())
        }
    }
    .map_err(|error| ApiError::bad_request(error.message))?;

    let provider_record = state
        .db
        .provider(provider_kind)
        .map_err(ApiError::internal)?;
    let upstream_url = format!(
        "{}{}",
        provider_record.upstream_base_url.trim_end_matches('/'),
        upstream_path(dialect)
    );

    let request_id = Uuid::new_v4().to_string();
    let response_id = format!("resp_{}", request_id.replace('-', ""));
    let created_at = Utc::now().timestamp();
    let start = Instant::now();

    let mut upstream_request = state.http.post(upstream_url).json(&upstream_body);
    upstream_request = apply_upstream_auth(upstream_request, provider_kind, &profile);
    if dialect == Dialect::AnthropicMessages {
        upstream_request = upstream_request.header("anthropic-version", "2023-06-01");
    }
    if is_stream {
        upstream_request = upstream_request.header("accept", "text/event-stream");
    }

    let upstream_response = upstream_request.send().await.map_err(ApiError::upstream)?;
    let status = upstream_response.status();

    if !status.is_success() {
        let error_body = upstream_response.bytes().await.unwrap_or_default();
        log_responses_request(
            &state,
            &request_id,
            project.id,
            provider_kind,
            &profile_id(&profile),
            status.as_u16() as i64,
            start.elapsed().as_millis() as i64,
            UsageSnapshot {
                prompt_tokens: None,
                completion_tokens: None,
                total_tokens: None,
                estimated_cost_usd: None,
            },
            Some("upstream returned non-success"),
        );
        let payload: Value = serde_json::from_slice(&error_body).unwrap_or_else(
            |_| json!({ "error": { "message": String::from_utf8_lossy(&error_body) } }),
        );
        return Ok((
            StatusCode::from_u16(status.as_u16()).unwrap_or(StatusCode::BAD_GATEWAY),
            Json(payload),
        )
            .into_response());
    }

    if !is_stream {
        let upstream_json = upstream_response
            .json::<Value>()
            .await
            .map_err(ApiError::upstream)?;
        let translated = match dialect {
            Dialect::ChatCompletions => chat_response_to_responses(
                &upstream_json,
                &requested_model,
                &response_id,
                created_at,
            ),
            Dialect::AnthropicMessages => agent_llm_translate::anthropic_response_to_responses(
                &upstream_json,
                &requested_model,
                &response_id,
                created_at,
            ),
        }
        .map_err(|error| ApiError::internal(anyhow::anyhow!(error.message)))?;

        let usage = usage_snapshot(provider_kind, bare_model, usage_from_responses(&translated));
        log_responses_request(
            &state,
            &request_id,
            project.id,
            provider_kind,
            &profile_id(&profile),
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
        return Ok(response);
    }

    // Streaming: pump upstream SSE through the translator into an outgoing SSE body.
    let (tx, rx) = tokio::sync::mpsc::channel::<Result<String, std::io::Error>>(64);
    let db = state.db.clone();
    let stream_project_id = project.id;
    let stream_profile_id = profile_id(&profile);
    let stream_request_id = request_id.clone();
    let bare_model_owned = bare_model.to_string();
    let requested_model_owned = requested_model.clone();
    let response_id_owned = response_id.clone();

    tokio::spawn(async move {
        let mut parser = SseParser::new();
        let mut translator = StreamTranslator::new(
            dialect,
            &requested_model_owned,
            &response_id_owned,
            created_at,
        );
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

        let usage = usage_snapshot(provider_kind, &bare_model_owned, translator.usage());
        if let Err(error) = db.log_request(
            &stream_request_id,
            stream_project_id,
            provider_kind,
            stream_profile_id,
            "POST",
            RESPONSES_LOG_PATH,
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

    let body = Body::from_stream(ReceiverStream::new(rx));
    let mut response = Response::builder()
        .status(StatusCode::OK)
        .header("content-type", "text/event-stream; charset=utf-8")
        .header("cache-control", "no-store")
        .body(body)
        .map_err(|error| ApiError::internal(anyhow::anyhow!(error)))?;
    response.headers_mut().insert(
        "x-agent-llm-request-id",
        HeaderValue::from_str(&request_id).unwrap(),
    );
    Ok(response)
}

/// Aggregate cached models across providers as `provider/model` ids. LM Studio
/// is re-fetched live when reachable so newly loaded local models appear
/// without a manual refresh.
pub async fn models_list(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, ApiError> {
    let project_key = extract_project_key(&headers)
        .ok_or_else(|| ApiError::unauthorized("missing project key"))?;
    state
        .db
        .project_by_key(&project_key)
        .map_err(|_| ApiError::unauthorized("invalid project key"))?;

    refresh_lmstudio_models(&state).await;

    let mut data = Vec::new();
    for record in state.db.list_providers().map_err(ApiError::internal)? {
        for model in state
            .db
            .list_models(&record.provider)
            .map_err(ApiError::internal)?
        {
            data.push(json!({
                "id": format!("{}/{}", record.provider, model.model_id),
                "object": "model",
                "owned_by": record.provider,
            }));
        }
    }
    Ok(Json(json!({ "object": "list", "data": data })))
}

async fn refresh_lmstudio_models(state: &AppState) {
    let Ok(record) = state.db.provider(ProviderKind::LmStudio) else {
        return;
    };
    let url = format!(
        "{}{}",
        record.upstream_base_url.trim_end_matches('/'),
        record.models_path
    );
    let fetch = async {
        state
            .http
            .get(url)
            .send()
            .await
            .ok()?
            .json::<Value>()
            .await
            .ok()
    };
    let Ok(Some(body)) = tokio::time::timeout(std::time::Duration::from_secs(2), fetch).await
    else {
        return;
    };
    let models = crate::parse_models(ProviderKind::LmStudio, body);
    if let Err(error) = state.db.replace_models(ProviderKind::LmStudio, models) {
        warn!("failed to cache LM Studio models: {error:#}");
    }
}

fn usage_snapshot(
    provider: ProviderKind,
    bare_model: &str,
    tokens: (Option<i64>, Option<i64>, Option<i64>),
) -> UsageSnapshot {
    let (prompt_tokens, completion_tokens, total_tokens) = tokens;
    UsageSnapshot {
        prompt_tokens,
        completion_tokens,
        total_tokens,
        estimated_cost_usd: pricing::estimate_cost_usd(
            provider,
            bare_model,
            prompt_tokens,
            completion_tokens,
        ),
    }
}

fn profile_id(profile: &crate::ResolvedAuthProfile) -> Option<i64> {
    Some(profile.id)
}

#[allow(clippy::too_many_arguments)]
fn log_responses_request(
    state: &AppState,
    request_id: &str,
    project_id: i64,
    provider: ProviderKind,
    auth_profile_id: &Option<i64>,
    status_code: i64,
    latency_ms: i64,
    usage: UsageSnapshot,
    error_text: Option<&str>,
) {
    if let Err(error) = state.db.log_request(
        request_id,
        project_id,
        provider,
        *auth_profile_id,
        "POST",
        RESPONSES_LOG_PATH,
        status_code,
        latency_ms,
        usage,
        false,
        error_text,
    ) {
        warn!("failed to log request: {error:#}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn routes_providers_to_their_dialects() {
        assert_eq!(
            dialect_for(ProviderKind::LmStudio),
            Some(Dialect::ChatCompletions)
        );
        assert_eq!(
            dialect_for(ProviderKind::OpenAi),
            Some(Dialect::ChatCompletions)
        );
        assert_eq!(
            dialect_for(ProviderKind::OpenRouter),
            Some(Dialect::ChatCompletions)
        );
        assert_eq!(
            dialect_for(ProviderKind::Kimi),
            Some(Dialect::AnthropicMessages)
        );
        assert_eq!(
            dialect_for(ProviderKind::Anthropic),
            Some(Dialect::AnthropicMessages)
        );
        assert_eq!(dialect_for(ProviderKind::Google), None);
    }

    #[test]
    fn splits_provider_prefix_keeping_slashes_in_model_id() {
        let (provider, model) = split_model("lmstudio/qwen/qwen3.6-35b-a3b").expect("splits");
        assert_eq!(provider, ProviderKind::LmStudio);
        assert_eq!(model, "qwen/qwen3.6-35b-a3b");

        let (provider, model) = split_model("kimi/k3").expect("splits");
        assert_eq!(provider, ProviderKind::Kimi);
        assert_eq!(model, "k3");

        assert!(split_model("no-prefix-model").is_err());
        assert!(split_model("mystery/model").is_err());
        assert!(split_model("kimi/").is_err());
    }
}
