use std::{net::SocketAddr, path::PathBuf, time::Instant};

use agent_llm_core::{
    Database, LocalSecretStore, SecretStore, pricing,
    settings::{DEFAULT_HOST, DEFAULT_PORT},
    types::{AuthMode, ProviderKind, SecretRef, UsageSnapshot},
};
use anyhow::{Context, Result, anyhow};
use axum::{
    Json, Router,
    body::{Body, Bytes},
    extract::{Path, Query, State},
    http::{HeaderMap, HeaderName, HeaderValue, Method, StatusCode, Uri},
    response::{IntoResponse, Response},
    routing::{any, get, post},
};
use clap::Parser;
use reqwest::header::{
    AUTHORIZATION, CONNECTION, CONTENT_LENGTH, HOST, HeaderMap as ReqwestHeaderMap,
    HeaderName as ReqwestHeaderName, HeaderValue as ReqwestHeaderValue, PROXY_AUTHORIZATION, TE,
    TRAILER, TRANSFER_ENCODING, UPGRADE,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tower_http::{cors::CorsLayer, trace::TraceLayer};
use tracing::{error, info};
use uuid::Uuid;

#[derive(Parser, Debug)]
#[command(name = "agent-llm-gateway")]
struct Args {
    #[arg(long, env = "AGENT_LLM_DB")]
    db_path: Option<PathBuf>,
    #[arg(long, env = "AGENT_LLM_HOST", default_value = DEFAULT_HOST)]
    host: String,
    #[arg(long, env = "AGENT_LLM_PORT", default_value_t = DEFAULT_PORT)]
    port: u16,
}

#[derive(Clone)]
struct AppState {
    db: Database,
    http: reqwest::Client,
    secrets: LocalSecretStore,
    host: String,
    port: u16,
}

#[derive(Debug, Serialize)]
struct ErrorBody {
    error: String,
}

#[derive(Debug, Deserialize)]
struct LimitQuery {
    limit: Option<usize>,
}

#[derive(Debug, Deserialize)]
struct CreateProjectRequest {
    name: String,
}

#[derive(Debug, Deserialize)]
struct CreateAuthProfileRequest {
    provider: String,
    name: String,
    auth_mode: String,
    secret: Option<String>,
    secret_ref: Option<String>,
    is_default: Option<bool>,
    metadata: Option<Value>,
}

#[derive(Debug, Clone)]
struct ResolvedAuthProfile {
    id: i64,
    name: String,
    auth_mode: AuthMode,
    metadata: Value,
    secret_value: String,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "agent_llm_gateway=info,tower_http=info".into()),
        )
        .init();

    let args = Args::parse();
    let db = Database::open(
        args.db_path
            .unwrap_or(agent_llm_core::settings::default_db_path()?),
    )?;
    let http = reqwest::Client::builder()
        .build()
        .context("failed to construct upstream HTTP client")?;
    let secrets = LocalSecretStore::detect()?;

    let state = AppState {
        db,
        http,
        secrets,
        host: args.host.clone(),
        port: args.port,
    };

    let app = Router::new()
        .route("/health", get(health))
        .route("/admin/status", get(admin_status))
        .route("/admin/providers", get(list_providers))
        .route("/admin/projects", get(list_projects).post(create_project))
        .route("/admin/requests", get(list_requests))
        .route("/admin/auth-profiles", post(create_auth_profile))
        .route(
            "/admin/providers/{provider}/models/refresh",
            post(refresh_models),
        )
        .route("/{provider}/{*path}", any(proxy_request))
        .layer(CorsLayer::permissive())
        .layer(TraceLayer::new_for_http())
        .with_state(state);

    let address = SocketAddr::new(args.host.parse()?, args.port);
    info!("agent-llm gateway listening on http://{address}");

    let listener = tokio::net::TcpListener::bind(address).await?;
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await?;

    Ok(())
}

async fn health() -> impl IntoResponse {
    Json(json!({ "ok": true }))
}

async fn admin_status(State(state): State<AppState>) -> Result<Json<Value>, ApiError> {
    let status = state
        .db
        .admin_status(state.host.clone(), state.port)
        .map_err(ApiError::internal)?;
    Ok(Json(
        serde_json::to_value(status).unwrap_or_else(|_| json!({ "ok": false })),
    ))
}

async fn list_providers(State(state): State<AppState>) -> Result<Json<Value>, ApiError> {
    let providers = state
        .db
        .list_provider_summaries()
        .map_err(ApiError::internal)?;
    Ok(Json(json!({ "providers": providers })))
}

async fn list_projects(State(state): State<AppState>) -> Result<Json<Value>, ApiError> {
    let projects = state.db.list_projects().map_err(ApiError::internal)?;
    Ok(Json(json!({ "projects": projects })))
}

async fn create_project(
    State(state): State<AppState>,
    Json(payload): Json<CreateProjectRequest>,
) -> Result<Json<Value>, ApiError> {
    let project = state
        .db
        .create_project(payload.name.trim())
        .map_err(ApiError::internal)?;
    Ok(Json(json!({ "project": project })))
}

async fn list_requests(
    State(state): State<AppState>,
    Query(query): Query<LimitQuery>,
) -> Result<Json<Value>, ApiError> {
    let limit = query.limit.unwrap_or(50).clamp(1, 500);
    let requests = state
        .db
        .recent_requests(limit)
        .map_err(ApiError::internal)?;
    Ok(Json(json!({ "requests": requests })))
}

async fn create_auth_profile(
    State(state): State<AppState>,
    Json(payload): Json<CreateAuthProfileRequest>,
) -> Result<Json<Value>, ApiError> {
    let provider = ProviderKind::parse(payload.provider.trim())
        .ok_or_else(|| ApiError::bad_request("unknown provider"))?;
    let auth_mode = AuthMode::parse_for_provider(provider, payload.auth_mode.trim())
        .ok_or_else(|| ApiError::bad_request("unknown auth mode"))?;
    let secret_ref = normalize_secret_ref(&state.secrets, provider, &payload)?;
    let profile = state
        .db
        .add_auth_profile(
            provider,
            payload.name.trim(),
            auth_mode,
            &secret_ref,
            payload.is_default.unwrap_or(false),
            payload.metadata.unwrap_or(Value::Null),
        )
        .map_err(ApiError::internal)?;
    Ok(Json(json!({ "auth_profile": profile })))
}

async fn refresh_models(
    State(state): State<AppState>,
    Path(provider): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let provider = ProviderKind::parse(provider.as_str())
        .ok_or_else(|| ApiError::bad_request("unknown provider"))?;
    let profile = state
        .db
        .default_auth_profile(provider)
        .map_err(ApiError::internal)?
        .ok_or_else(|| ApiError::bad_request("no default auth profile configured for provider"))?;
    let resolved_profile = resolve_profile(&state.secrets, &profile)?;
    let provider_record = state.db.provider(provider).map_err(ApiError::internal)?;

    let url = format!(
        "{}{}",
        provider_record.upstream_base_url, provider_record.models_path
    );
    let mut request = state.http.get(url);
    request = apply_upstream_auth(request, provider, &resolved_profile);

    if provider == ProviderKind::Anthropic {
        request = request.header("anthropic-version", "2023-06-01");
    }

    let response = request.send().await.map_err(ApiError::upstream)?;
    let status = response.status();
    if !status.is_success() {
        return Err(ApiError::upstream_status(
            status,
            "failed to refresh models",
        ));
    }

    let body = response.json::<Value>().await.map_err(ApiError::upstream)?;
    let models = parse_models(provider, body);
    let count = state
        .db
        .replace_models(provider, models)
        .map_err(ApiError::internal)?;

    Ok(Json(
        json!({ "refreshed": count, "provider": provider.as_str() }),
    ))
}

async fn proxy_request(
    State(state): State<AppState>,
    Path((provider, path)): Path<(String, String)>,
    method: Method,
    headers: HeaderMap,
    uri: Uri,
    body: Bytes,
) -> Result<Response, ApiError> {
    let provider_kind = ProviderKind::parse(&provider)
        .ok_or_else(|| ApiError::bad_request("unsupported provider namespace"))?;
    let provider_record = state
        .db
        .provider(provider_kind)
        .map_err(ApiError::internal)?;
    let project_key = extract_project_key(&headers)
        .ok_or_else(|| ApiError::unauthorized("missing project key"))?;
    let project = state
        .db
        .project_by_key(&project_key)
        .map_err(|_| ApiError::unauthorized("invalid project key"))?;
    let requested_profile = headers
        .get("x-agent-llm-auth-profile")
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let fallback_profile = headers
        .get("x-agent-llm-fallback-auth-profile")
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|value| !value.is_empty());

    let primary_profile = state
        .db
        .resolve_auth_profile(project.id, provider_kind, requested_profile)
        .map_err(ApiError::internal)?
        .ok_or_else(|| ApiError::bad_request("no auth profile configured"))?;
    let fallback_profile = fallback_profile
        .map(|name| state.db.auth_profile_by_name(provider_kind, name))
        .transpose()
        .map_err(ApiError::internal)?;
    let primary_profile = resolve_profile(&state.secrets, &primary_profile)?;
    let fallback_profile = fallback_profile
        .as_ref()
        .map(|profile| resolve_profile(&state.secrets, profile))
        .transpose()?;

    let request_id = Uuid::new_v4().to_string();
    let upstream_url = build_upstream_url(&provider_record.upstream_base_url, &path, uri.query())?;
    let start = Instant::now();
    let request_model = extract_request_model(&body);

    let mut upstream_request = state
        .http
        .request(method.clone(), upstream_url.clone())
        .body(body.clone());
    upstream_request = apply_headers(upstream_request, &headers)?;
    upstream_request = apply_upstream_auth(upstream_request, provider_kind, &primary_profile);
    if provider_kind == ProviderKind::Anthropic && !headers.contains_key("anthropic-version") {
        upstream_request = upstream_request.header("anthropic-version", "2023-06-01");
    }

    let upstream_response = upstream_request.send().await.map_err(ApiError::upstream)?;
    let mut used_fallback = false;
    let response =
        if should_retry_with_fallback(upstream_response.status(), fallback_profile.as_ref()) {
            used_fallback = true;
            let mut retry_request = state.http.request(method.clone(), upstream_url).body(body);
            retry_request = apply_headers(retry_request, &headers)?;
            retry_request = apply_upstream_auth(
                retry_request,
                provider_kind,
                fallback_profile
                    .as_ref()
                    .expect("guarded by should_retry_with_fallback"),
            );
            if provider_kind == ProviderKind::Anthropic
                && !headers.contains_key("anthropic-version")
            {
                retry_request = retry_request.header("anthropic-version", "2023-06-01");
            }
            retry_request.send().await.map_err(ApiError::upstream)?
        } else {
            upstream_response
        };

    let latency_ms = start.elapsed().as_millis() as i64;
    let auth_profile_id = if used_fallback {
        fallback_profile.as_ref().map(|profile| profile.id)
    } else {
        Some(primary_profile.id)
    };
    let auth_profile_ref = if used_fallback {
        fallback_profile.as_ref()
    } else {
        Some(&primary_profile)
    };
    let response = build_proxy_response(
        &state,
        &request_id,
        &project.name,
        project.id,
        provider_kind,
        path,
        method,
        response,
        auth_profile_id,
        auth_profile_ref.and_then(|profile| Some(profile.name.as_str())),
        request_model,
        latency_ms,
        used_fallback,
    )
    .await?;

    Ok(response)
}

async fn build_proxy_response(
    state: &AppState,
    request_id: &str,
    project_name: &str,
    project_id: i64,
    provider: ProviderKind,
    path: String,
    method: Method,
    upstream_response: reqwest::Response,
    auth_profile_id: Option<i64>,
    _auth_profile_name: Option<&str>,
    request_model: Option<String>,
    latency_ms: i64,
    used_fallback: bool,
) -> Result<Response, ApiError> {
    let status = upstream_response.status();
    let response_headers = upstream_response.headers().clone();
    let is_stream = response_headers
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .map(|value| value.contains("text/event-stream"))
        .unwrap_or(false);

    let usage = if is_stream {
        UsageSnapshot {
            prompt_tokens: None,
            completion_tokens: None,
            total_tokens: None,
            estimated_cost_usd: None,
        }
    } else {
        let bytes = upstream_response
            .bytes()
            .await
            .map_err(ApiError::upstream)?;
        let usage = extract_usage(provider, &bytes, request_model.as_deref());
        state
            .db
            .log_request(
                request_id,
                project_id,
                provider,
                auth_profile_id,
                method.as_str(),
                &path,
                status.as_u16() as i64,
                latency_ms,
                usage.clone(),
                used_fallback,
                if status.is_success() {
                    None
                } else {
                    Some("upstream returned non-success")
                },
            )
            .map_err(ApiError::internal)?;

        let mut response = Response::builder().status(status);
        copy_response_headers(
            &response_headers,
            response.headers_mut().expect("response headers"),
        )?;
        response.headers_mut().expect("response headers").insert(
            "x-agent-llm-request-id",
            HeaderValue::from_str(request_id).unwrap(),
        );
        response.headers_mut().expect("response headers").insert(
            "x-agent-llm-project",
            HeaderValue::from_str(project_name).unwrap(),
        );
        return response
            .body(Body::from(bytes))
            .map_err(|error| ApiError::internal(anyhow!(error)));
    };

    state
        .db
        .log_request(
            request_id,
            project_id,
            provider,
            auth_profile_id,
            method.as_str(),
            &path,
            status.as_u16() as i64,
            latency_ms,
            usage,
            used_fallback,
            if status.is_success() {
                None
            } else {
                Some("upstream returned non-success")
            },
        )
        .map_err(ApiError::internal)?;

    let mut response = Response::builder().status(status);
    copy_response_headers(
        &response_headers,
        response.headers_mut().expect("response headers"),
    )?;
    response.headers_mut().expect("response headers").insert(
        "x-agent-llm-request-id",
        HeaderValue::from_str(request_id).unwrap(),
    );
    response.headers_mut().expect("response headers").insert(
        "x-agent-llm-project",
        HeaderValue::from_str(project_name).unwrap(),
    );
    response
        .body(Body::from_stream(upstream_response.bytes_stream()))
        .map_err(|error| ApiError::internal(anyhow!(error)))
}

fn extract_project_key(headers: &HeaderMap) -> Option<String> {
    if let Some(value) = headers
        .get("x-agent-llm-project-key")
        .and_then(|value| value.to_str().ok())
    {
        return Some(value.trim().to_string());
    }
    if let Some(value) = headers
        .get("x-api-key")
        .and_then(|value| value.to_str().ok())
    {
        return Some(value.trim().to_string());
    }
    if let Some(value) = headers
        .get("x-goog-api-key")
        .and_then(|value| value.to_str().ok())
    {
        return Some(value.trim().to_string());
    }
    headers
        .get(AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .map(|value| value.trim().to_string())
}

fn build_upstream_url(base: &str, path: &str, query: Option<&str>) -> Result<String, ApiError> {
    let mut url = format!(
        "{}/{}",
        base.trim_end_matches('/'),
        path.trim_start_matches('/')
    );
    if let Some(query) = query {
        url.push('?');
        url.push_str(query);
    }
    Ok(url)
}

fn apply_headers(
    mut builder: reqwest::RequestBuilder,
    headers: &HeaderMap,
) -> Result<reqwest::RequestBuilder, ApiError> {
    let mut upstream_headers = ReqwestHeaderMap::new();
    for (name, value) in headers {
        if should_strip_request_header(name) {
            continue;
        }
        let reqwest_name = ReqwestHeaderName::from_bytes(name.as_str().as_bytes())
            .map_err(|error| ApiError::internal(anyhow!(error)))?;
        let reqwest_value = ReqwestHeaderValue::from_bytes(value.as_bytes())
            .map_err(|error| ApiError::internal(anyhow!(error)))?;
        upstream_headers.insert(reqwest_name, reqwest_value);
    }
    builder = builder.headers(upstream_headers);
    Ok(builder)
}

fn apply_upstream_auth(
    mut builder: reqwest::RequestBuilder,
    provider: ProviderKind,
    profile: &ResolvedAuthProfile,
) -> reqwest::RequestBuilder {
    match (provider, profile.auth_mode) {
        (ProviderKind::OpenAi, AuthMode::ApiKey | AuthMode::OpenAiSession)
        | (ProviderKind::OpenRouter, AuthMode::ApiKey) => {
            builder = builder.header(AUTHORIZATION, format!("Bearer {}", profile.secret_value));
        }
        (ProviderKind::Anthropic, AuthMode::ApiKey) => {
            builder = builder.header("x-api-key", &profile.secret_value);
        }
        (ProviderKind::Anthropic, AuthMode::AnthropicSession) => {
            builder = builder.header(AUTHORIZATION, format!("Bearer {}", profile.secret_value));
        }
        (ProviderKind::Google, AuthMode::ApiKey) => {
            builder = builder.header("x-goog-api-key", &profile.secret_value);
        }
        _ => {}
    }

    if let Some(extra_headers) = profile.metadata.get("headers").and_then(Value::as_object) {
        for (name, value) in extra_headers {
            if let Some(value) = value.as_str() {
                builder = builder.header(name, value);
            }
        }
    }

    builder
}

fn should_strip_request_header(name: &HeaderName) -> bool {
    matches!(
        name,
        &HOST
            | &CONTENT_LENGTH
            | &AUTHORIZATION
            | &PROXY_AUTHORIZATION
            | &CONNECTION
            | &TRANSFER_ENCODING
            | &UPGRADE
            | &TE
            | &TRAILER
    ) || name.as_str().starts_with("x-agent-llm-")
        || matches!(name.as_str(), "x-api-key" | "x-goog-api-key")
}

fn copy_response_headers(
    headers: &ReqwestHeaderMap,
    target: &mut HeaderMap,
) -> Result<(), ApiError> {
    for (name, value) in headers {
        if should_strip_response_header(name) {
            continue;
        }
        let header_name = HeaderName::from_bytes(name.as_str().as_bytes())
            .map_err(|error| ApiError::internal(anyhow!(error)))?;
        let header_value = HeaderValue::from_bytes(value.as_bytes())
            .map_err(|error| ApiError::internal(anyhow!(error)))?;
        target.insert(header_name, header_value);
    }
    Ok(())
}

fn should_strip_response_header(name: &ReqwestHeaderName) -> bool {
    matches!(
        name,
        &CONNECTION | &TRANSFER_ENCODING | &UPGRADE | &TE | &TRAILER | &CONTENT_LENGTH
    )
}

fn should_retry_with_fallback(
    status: reqwest::StatusCode,
    fallback_profile: Option<&ResolvedAuthProfile>,
) -> bool {
    fallback_profile.is_some()
        && matches!(
            status,
            reqwest::StatusCode::UNAUTHORIZED | reqwest::StatusCode::FORBIDDEN
        )
}

fn extract_request_model(body: &[u8]) -> Option<String> {
    let json = serde_json::from_slice::<Value>(body).ok()?;
    json.get("model")
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
}

fn extract_usage(
    provider: ProviderKind,
    bytes: &[u8],
    request_model: Option<&str>,
) -> UsageSnapshot {
    let json = match serde_json::from_slice::<Value>(bytes) {
        Ok(value) => value,
        Err(_) => {
            return UsageSnapshot {
                prompt_tokens: None,
                completion_tokens: None,
                total_tokens: None,
                estimated_cost_usd: None,
            };
        }
    };

    let (prompt_tokens, completion_tokens, total_tokens) = match provider {
        ProviderKind::OpenAi | ProviderKind::OpenRouter => (
            json.pointer("/usage/prompt_tokens").and_then(Value::as_i64),
            json.pointer("/usage/completion_tokens")
                .and_then(Value::as_i64),
            json.pointer("/usage/total_tokens").and_then(Value::as_i64),
        ),
        ProviderKind::Anthropic => (
            json.pointer("/usage/input_tokens").and_then(Value::as_i64),
            json.pointer("/usage/output_tokens").and_then(Value::as_i64),
            json.pointer("/usage/input_tokens")
                .and_then(Value::as_i64)
                .zip(json.pointer("/usage/output_tokens").and_then(Value::as_i64))
                .map(|(a, b)| a + b),
        ),
        ProviderKind::Google => (
            json.pointer("/usageMetadata/promptTokenCount")
                .and_then(Value::as_i64),
            json.pointer("/usageMetadata/candidatesTokenCount")
                .and_then(Value::as_i64),
            json.pointer("/usageMetadata/totalTokenCount")
                .and_then(Value::as_i64),
        ),
    };

    let model = json
        .get("model")
        .and_then(Value::as_str)
        .or(request_model)
        .or_else(|| json.pointer("/modelVersion").and_then(Value::as_str));
    let estimated_cost_usd = model.and_then(|model| {
        pricing::estimate_cost_usd(provider, model, prompt_tokens, completion_tokens)
    });

    UsageSnapshot {
        prompt_tokens,
        completion_tokens,
        total_tokens,
        estimated_cost_usd,
    }
}

fn parse_models(provider: ProviderKind, json: Value) -> Vec<(String, String, Value)> {
    let items = match provider {
        ProviderKind::OpenAi | ProviderKind::Anthropic | ProviderKind::OpenRouter => json
            .get("data")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default(),
        ProviderKind::Google => json
            .get("models")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default(),
    };

    items
        .into_iter()
        .filter_map(|item| {
            let model_id = item
                .get("id")
                .and_then(Value::as_str)
                .or_else(|| item.get("name").and_then(Value::as_str))?
                .to_string();
            let display_name = item
                .get("display_name")
                .and_then(Value::as_str)
                .or_else(|| item.get("displayName").and_then(Value::as_str))
                .unwrap_or(&model_id)
                .to_string();
            Some((model_id, display_name, item))
        })
        .collect()
}

async fn shutdown_signal() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };

    #[cfg(unix)]
    let terminate = async {
        let mut signal =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()).unwrap();
        signal.recv().await;
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {},
        _ = terminate => {},
    }
}

#[derive(Debug)]
struct ApiError {
    status: StatusCode,
    message: String,
}

impl ApiError {
    fn bad_request(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            message: message.into(),
        }
    }

    fn unauthorized(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::UNAUTHORIZED,
            message: message.into(),
        }
    }

    fn internal(error: anyhow::Error) -> Self {
        error!("{error:#}");
        Self {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            message: "internal server error".into(),
        }
    }

    fn upstream(error: reqwest::Error) -> Self {
        error!("{error:#}");
        Self {
            status: StatusCode::BAD_GATEWAY,
            message: "upstream request failed".into(),
        }
    }

    fn upstream_status(status: reqwest::StatusCode, message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::from_u16(status.as_u16()).unwrap_or(StatusCode::BAD_GATEWAY),
            message: message.into(),
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (
            self.status,
            Json(ErrorBody {
                error: self.message,
            }),
        )
            .into_response()
    }
}

fn resolve_profile(
    secrets: &LocalSecretStore,
    profile: &agent_llm_core::types::AuthProfile,
) -> Result<ResolvedAuthProfile, ApiError> {
    let secret_value = secrets
        .read_secret(&profile.secret_ref)
        .map_err(ApiError::internal)?;

    Ok(ResolvedAuthProfile {
        id: profile.id,
        name: profile.name.clone(),
        auth_mode: profile.auth_mode,
        metadata: profile.metadata.clone(),
        secret_value,
    })
}

fn normalize_secret_ref(
    secrets: &LocalSecretStore,
    provider: ProviderKind,
    payload: &CreateAuthProfileRequest,
) -> Result<SecretRef, ApiError> {
    let inline_secret = payload
        .secret
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let explicit_secret_ref = payload
        .secret_ref
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty());

    match (inline_secret, explicit_secret_ref) {
        (Some(secret), None) => secrets
            .store_auth_profile_secret(provider, payload.name.trim(), secret)
            .map_err(ApiError::internal),
        (None, Some(secret_ref)) => SecretRef::parse(secret_ref)
            .ok_or_else(|| ApiError::bad_request("invalid secret_ref format")),
        (Some(_), Some(_)) => Err(ApiError::bad_request(
            "provide either `secret` or `secret_ref`, not both",
        )),
        (None, None) => Err(ApiError::bad_request(
            "missing auth secret: provide `secret` or `secret_ref`",
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    use agent_llm_core::types::{SecretBackend, SecretRef};

    fn test_secret_dir() -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock is valid")
            .as_nanos();
        std::env::temp_dir().join(format!("agent-llm-gateway-secrets-{nonce}"))
    }

    #[test]
    fn extracts_project_key_from_bearer() {
        let mut headers = HeaderMap::new();
        headers.insert(AUTHORIZATION, HeaderValue::from_static("Bearer agllm_test"));
        assert_eq!(extract_project_key(&headers).as_deref(), Some("agllm_test"));
    }

    #[test]
    fn builds_openai_usage_snapshot() {
        let payload = br#"{
          "model": "gpt-4.1",
          "usage": {
            "prompt_tokens": 100,
            "completion_tokens": 50,
            "total_tokens": 150
          }
        }"#;

        let usage = extract_usage(ProviderKind::OpenAi, payload, None);
        assert_eq!(usage.prompt_tokens, Some(100));
        assert_eq!(usage.completion_tokens, Some(50));
        assert_eq!(usage.total_tokens, Some(150));
        assert!(usage.estimated_cost_usd.is_some());
    }

    #[tokio::test]
    async fn anthropic_session_uses_bearer_auth_and_metadata_headers() {
        let profile = resolved_profile(
            AuthMode::AnthropicSession,
            "claude-console",
            "session-token",
            json!({
                "headers": {
                    "anthropic-beta": "context-1m-2025-08-07"
                }
            }),
        );

        let request = apply_upstream_auth(
            reqwest::Client::new().get("https://example.com"),
            ProviderKind::Anthropic,
            &profile,
        )
        .build()
        .expect("request builds");

        assert_eq!(
            request
                .headers()
                .get(AUTHORIZATION)
                .and_then(|value| value.to_str().ok()),
            Some("Bearer session-token")
        );
        assert_eq!(
            request
                .headers()
                .get("anthropic-beta")
                .and_then(|value| value.to_str().ok()),
            Some("context-1m-2025-08-07")
        );
        assert!(request.headers().get("x-api-key").is_none());
    }

    #[test]
    fn secret_ref_payload_is_normalized_for_storage() {
        let secrets = LocalSecretStore::file_backed(test_secret_dir()).expect("store creates");
        let payload = CreateAuthProfileRequest {
            provider: "openai".into(),
            name: "codex".into(),
            auth_mode: "openai_session".into(),
            secret: None,
            secret_ref: Some("keychain:openai/codex".into()),
            is_default: Some(true),
            metadata: None,
        };

        assert_eq!(
            normalize_secret_ref(&secrets, ProviderKind::OpenAi, &payload).expect("secret ref"),
            SecretRef::new(SecretBackend::Keychain, "openai/codex")
        );
    }

    #[test]
    fn inline_secret_is_stored_in_local_secret_store() {
        let secrets = LocalSecretStore::file_backed(test_secret_dir()).expect("store creates");
        let payload = CreateAuthProfileRequest {
            provider: "anthropic".into(),
            name: "claude-console".into(),
            auth_mode: "anthropic_session".into(),
            secret: Some("session-token".into()),
            secret_ref: None,
            is_default: Some(true),
            metadata: None,
        };

        let secret_ref =
            normalize_secret_ref(&secrets, ProviderKind::Anthropic, &payload).expect("stored");
        let round_trip = secrets.read_secret(&secret_ref).expect("secret read");
        assert_eq!(round_trip, "session-token");
    }

    #[test]
    fn openai_session_uses_bearer_auth() {
        let profile = resolved_profile(
            AuthMode::OpenAiSession,
            "codex-session",
            "openai-session-token",
            Value::Null,
        );

        let request = apply_upstream_auth(
            reqwest::Client::new().get("https://example.com"),
            ProviderKind::OpenAi,
            &profile,
        )
        .build()
        .expect("request builds");

        assert_eq!(
            request
                .headers()
                .get(AUTHORIZATION)
                .and_then(|value| value.to_str().ok()),
            Some("Bearer openai-session-token")
        );
    }

    fn resolved_profile(
        auth_mode: AuthMode,
        name: &str,
        secret_value: &str,
        metadata: Value,
    ) -> ResolvedAuthProfile {
        ResolvedAuthProfile {
            id: 1,
            name: name.into(),
            auth_mode,
            metadata,
            secret_value: secret_value.into(),
        }
    }
}
