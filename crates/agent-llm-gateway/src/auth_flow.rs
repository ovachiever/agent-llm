use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};

use agent_llm_core::{
    Database, LocalSecretStore, SecretStore,
    types::{AuthMode, ProviderKind},
};
use anyhow::anyhow;
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use chrono::{DateTime, Duration, Utc};
use rand::RngCore;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::ApiError;

const AUTH_ATTEMPT_TTL_MINUTES: i64 = 10;
const OPENAI_CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";
const OPENAI_ISSUER: &str = "https://auth.openai.com";
const ANTHROPIC_CLIENT_ID: &str = "9d1c250a-e61b-44d9-88ed-5944d1962f5e";
const ANTHROPIC_CLAUDE_AUTHORIZE_URL: &str = "https://claude.ai/oauth/authorize";
const ANTHROPIC_TOKEN_URL: &str = "https://console.anthropic.com/v1/oauth/token";
const GOOGLE_AUTHORIZE_URL: &str = "https://accounts.google.com/o/oauth2/v2/auth";
const GOOGLE_TOKEN_URL: &str = "https://oauth2.googleapis.com/token";

#[derive(Debug, Clone, Serialize)]
pub struct AuthMethodDescriptor {
    pub id: String,
    pub title: String,
    pub summary: String,
    pub button_label: String,
    pub kind: String,
    pub billing_mode: String,
    pub experimental: bool,
    pub supports_completion_code: bool,
    pub auth_mode: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct AuthProviderMethods {
    pub provider: String,
    pub methods: Vec<AuthMethodDescriptor>,
}

pub fn auth_method_catalog() -> Vec<AuthProviderMethods> {
    vec![
        AuthProviderMethods {
            provider: "openai".into(),
            methods: vec![
                AuthMethodDescriptor {
                    id: "api_key".into(),
                    title: "OpenAI API Key".into(),
                    summary: "Direct Platform billing with an OpenAI API key.".into(),
                    button_label: "Add API Key".into(),
                    kind: "api_key".into(),
                    billing_mode: "api".into(),
                    experimental: false,
                    supports_completion_code: false,
                    auth_mode: "api_key".into(),
                },
                AuthMethodDescriptor {
                    id: "openai_account".into(),
                    title: "ChatGPT Account".into(),
                    summary: "Experimental browser-based sign-in for account-backed usage.".into(),
                    button_label: "Connect ChatGPT Account".into(),
                    kind: "browser".into(),
                    billing_mode: "account".into(),
                    experimental: true,
                    supports_completion_code: false,
                    auth_mode: "openai_session".into(),
                },
            ],
        },
        AuthProviderMethods {
            provider: "anthropic".into(),
            methods: vec![
                AuthMethodDescriptor {
                    id: "api_key".into(),
                    title: "Anthropic API Key".into(),
                    summary: "Direct Anthropic Console billing with an API key.".into(),
                    button_label: "Add API Key".into(),
                    kind: "api_key".into(),
                    billing_mode: "api".into(),
                    experimental: false,
                    supports_completion_code: false,
                    auth_mode: "api_key".into(),
                },
                AuthMethodDescriptor {
                    id: "anthropic_account".into(),
                    title: "Claude Account".into(),
                    summary: "Experimental browser-based sign-in for Claude account billing.".into(),
                    button_label: "Connect Claude Account".into(),
                    kind: "browser".into(),
                    billing_mode: "account".into(),
                    experimental: true,
                    supports_completion_code: true,
                    auth_mode: "anthropic_session".into(),
                },
            ],
        },
        AuthProviderMethods {
            provider: "google".into(),
            methods: vec![
                AuthMethodDescriptor {
                    id: "api_key".into(),
                    title: "Google API Key".into(),
                    summary: "Direct Gemini billing with an AI Studio or Gemini API key.".into(),
                    button_label: "Add API Key".into(),
                    kind: "api_key".into(),
                    billing_mode: "api".into(),
                    experimental: false,
                    supports_completion_code: false,
                    auth_mode: "api_key".into(),
                },
                AuthMethodDescriptor {
                    id: "google_oauth".into(),
                    title: "Google OAuth".into(),
                    summary: "Browser-based OAuth flow for a Google account.".into(),
                    button_label: "Connect Google Account".into(),
                    kind: "browser".into(),
                    billing_mode: "account".into(),
                    experimental: false,
                    supports_completion_code: false,
                    auth_mode: "google_oauth".into(),
                },
            ],
        },
        AuthProviderMethods {
            provider: "openrouter".into(),
            methods: vec![AuthMethodDescriptor {
                id: "api_key".into(),
                title: "OpenRouter API Key".into(),
                summary: "Direct OpenRouter billing with an API key.".into(),
                button_label: "Add API Key".into(),
                kind: "api_key".into(),
                billing_mode: "api".into(),
                experimental: false,
                supports_completion_code: false,
                auth_mode: "api_key".into(),
            }],
        },
        AuthProviderMethods {
            provider: "kimi".into(),
            methods: vec![AuthMethodDescriptor {
                id: "api_key".into(),
                title: "Kimi Code API Key".into(),
                summary: "Kimi Code subscription key from the Kimi Code Console (Anthropic-protocol endpoint).".into(),
                button_label: "Add API Key".into(),
                kind: "api_key".into(),
                billing_mode: "api".into(),
                experimental: false,
                supports_completion_code: false,
                auth_mode: "api_key".into(),
            }],
        },
        AuthProviderMethods {
            provider: "lmstudio".into(),
            methods: vec![AuthMethodDescriptor {
                id: "none".into(),
                title: "No Auth (local)".into(),
                summary: "LM Studio's local server needs no credentials; a default profile is preconfigured.".into(),
                button_label: "Use Local Server".into(),
                kind: "none".into(),
                billing_mode: "local".into(),
                experimental: false,
                supports_completion_code: false,
                auth_mode: "none".into(),
            }],
        },
    ]
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum StoredCredential {
    ApiKey {
        secret: String,
    },
    Account {
        access_token: String,
        refresh_token: Option<String>,
        expires_at: Option<DateTime<Utc>>,
        account_id: Option<String>,
        client_id: Option<String>,
        client_secret: Option<String>,
        project_id: Option<String>,
    },
}

impl StoredCredential {
    pub fn parse(auth_mode: AuthMode, raw_secret: &str) -> Result<Self, ApiError> {
        if auth_mode == AuthMode::None {
            return Ok(Self::ApiKey {
                secret: String::new(),
            });
        }
        match serde_json::from_str(raw_secret) {
            Ok(parsed) => Ok(parsed),
            Err(_) => Ok(match auth_mode {
                AuthMode::ApiKey | AuthMode::None => Self::ApiKey {
                    secret: raw_secret.to_string(),
                },
                AuthMode::OpenAiSession | AuthMode::AnthropicSession | AuthMode::GoogleOAuth => {
                    Self::Account {
                        access_token: raw_secret.to_string(),
                        refresh_token: None,
                        expires_at: None,
                        account_id: None,
                        client_id: None,
                        client_secret: None,
                        project_id: None,
                    }
                }
            }),
        }
    }

    pub fn serialize(&self) -> Result<String, ApiError> {
        serde_json::to_string(self).map_err(|error| ApiError::internal(anyhow!(error)))
    }

    pub fn secret_value(&self) -> &str {
        match self {
            Self::ApiKey { secret } => secret,
            Self::Account { access_token, .. } => access_token,
        }
    }

    pub fn refresh_token(&self) -> Option<&str> {
        match self {
            Self::ApiKey { .. } => None,
            Self::Account { refresh_token, .. } => refresh_token.as_deref(),
        }
    }

    pub fn expires_at(&self) -> Option<DateTime<Utc>> {
        match self {
            Self::ApiKey { .. } => None,
            Self::Account { expires_at, .. } => *expires_at,
        }
    }

    pub fn account_id(&self) -> Option<&str> {
        match self {
            Self::ApiKey { .. } => None,
            Self::Account { account_id, .. } => account_id.as_deref(),
        }
    }

    pub fn project_id(&self) -> Option<&str> {
        match self {
            Self::ApiKey { .. } => None,
            Self::Account { project_id, .. } => project_id.as_deref(),
        }
    }

    pub fn needs_refresh(&self) -> bool {
        matches!(
            self.expires_at(),
            Some(expires_at) if expires_at <= Utc::now() + Duration::seconds(60)
        ) && self.refresh_token().is_some()
    }

    pub fn with_account_tokens(
        access_token: String,
        refresh_token: Option<String>,
        expires_at: Option<DateTime<Utc>>,
        account_id: Option<String>,
        client_id: Option<String>,
        client_secret: Option<String>,
        project_id: Option<String>,
    ) -> Self {
        Self::Account {
            access_token,
            refresh_token,
            expires_at,
            account_id,
            client_id,
            client_secret,
            project_id,
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AuthCompletionMode {
    Auto,
    Code,
}

#[derive(Debug, Clone, Serialize)]
pub struct AuthAttemptStatusResponse {
    pub id: String,
    pub provider: String,
    pub method: String,
    pub status: String,
    pub authorize_url: Option<String>,
    pub instructions: Option<String>,
    pub requires_completion_code: bool,
    pub poll_after_ms: u64,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct StartAuthFlowRequest {
    pub provider: String,
    pub name: String,
    pub method: Option<String>,
    pub auth_mode: Option<String>,
    pub is_default: Option<bool>,
    pub metadata: Option<Value>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CompleteAuthFlowRequest {
    pub code: Option<String>,
    pub verification_code: Option<String>,
}

#[derive(Debug, Clone)]
pub struct AuthAttempt {
    pub id: String,
    pub provider: ProviderKind,
    pub method_id: String,
    pub auth_mode: AuthMode,
    pub profile_name: String,
    pub is_default: bool,
    pub metadata: Value,
    pub completion_mode: AuthCompletionMode,
    pub authorize_url: Option<String>,
    pub poll_after_ms: u64,
    pub instructions: String,
    pub flow: ProviderAuthFlow,
    pub status: AuthAttemptStatus,
    pub expires_at: DateTime<Utc>,
}

impl AuthAttempt {
    pub fn status_response(&self) -> AuthAttemptStatusResponse {
        AuthAttemptStatusResponse {
            id: self.id.clone(),
            provider: self.provider.as_str().into(),
            method: self.method_id.clone(),
            status: self.status.label().into(),
            authorize_url: self.authorize_url.clone(),
            instructions: Some(self.instructions.clone()),
            requires_completion_code: self.completion_mode == AuthCompletionMode::Code,
            poll_after_ms: self.poll_after_ms,
            error: match &self.status {
                AuthAttemptStatus::Failed(message) => Some(message.clone()),
                _ => None,
            },
        }
    }
}

#[derive(Debug, Clone)]
pub enum ProviderAuthFlow {
    OpenAi {
        verifier: String,
        state: String,
        redirect_uri: String,
    },
    Anthropic {
        verifier: String,
    },
    Google {
        verifier: String,
        state: String,
        redirect_uri: String,
        client_id: String,
        client_secret: String,
        project_id: Option<String>,
    },
}

#[derive(Debug, Clone)]
pub enum AuthAttemptStatus {
    Pending,
    AwaitingCode,
    Completed,
    Failed(String),
}

impl AuthAttemptStatus {
    pub fn label(&self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::AwaitingCode => "awaiting_code",
            Self::Completed => "completed",
            Self::Failed(_) => "failed",
        }
    }
}

#[derive(Clone, Default)]
pub struct AuthAttemptManager {
    inner: Arc<Mutex<HashMap<String, AuthAttempt>>>,
}

impl AuthAttemptManager {
    pub fn insert(&self, attempt: AuthAttempt) -> Result<(), ApiError> {
        let mut guard = self
            .inner
            .lock()
            .map_err(|_| ApiError::internal(anyhow!("auth attempt mutex poisoned")))?;
        guard.insert(attempt.id.clone(), attempt);
        Ok(())
    }

    pub fn get(&self, id: &str) -> Result<Option<AuthAttempt>, ApiError> {
        let mut guard = self
            .inner
            .lock()
            .map_err(|_| ApiError::internal(anyhow!("auth attempt mutex poisoned")))?;
        prune_expired(&mut guard);
        Ok(guard.get(id).cloned())
    }

    pub fn update(&self, id: &str, update: impl FnOnce(&mut AuthAttempt)) -> Result<(), ApiError> {
        let mut guard = self
            .inner
            .lock()
            .map_err(|_| ApiError::internal(anyhow!("auth attempt mutex poisoned")))?;
        prune_expired(&mut guard);
        let attempt = guard
            .get_mut(id)
            .ok_or_else(|| ApiError::bad_request("auth attempt not found"))?;
        update(attempt);
        Ok(())
    }

    pub fn find_by_state(
        &self,
        provider: ProviderKind,
        state: &str,
    ) -> Result<Option<AuthAttempt>, ApiError> {
        let mut guard = self
            .inner
            .lock()
            .map_err(|_| ApiError::internal(anyhow!("auth attempt mutex poisoned")))?;
        prune_expired(&mut guard);
        Ok(guard
            .values()
            .find(|attempt| {
                attempt.provider == provider
                    && attempt.status.label() == "pending"
                    && match &attempt.flow {
                        ProviderAuthFlow::OpenAi { state: current, .. }
                        | ProviderAuthFlow::Google { state: current, .. } => current == state,
                        ProviderAuthFlow::Anthropic { .. } => false,
                    }
            })
            .cloned())
    }
}

fn prune_expired(store: &mut HashMap<String, AuthAttempt>) {
    let now = Utc::now();
    store.retain(|_, attempt| attempt.expires_at > now);
}

pub fn start_auth_flow(
    admin_base_url: &str,
    request: StartAuthFlowRequest,
) -> Result<AuthAttempt, ApiError> {
    let provider = ProviderKind::parse(request.provider.trim())
        .ok_or_else(|| ApiError::bad_request("unknown provider"))?;
    let (method_id, auth_mode) = resolve_requested_method(provider, &request)?;
    let metadata = request.metadata.unwrap_or(Value::Null);
    let name = request.name.trim();
    if name.is_empty() {
        return Err(ApiError::bad_request("profile name is required"));
    }

    let id = Uuid::new_v4().to_string();
    let expires_at = Utc::now() + Duration::minutes(AUTH_ATTEMPT_TTL_MINUTES);
    let poll_after_ms = 1500;

    let (authorize_url, instructions, completion_mode, flow) = match (provider, auth_mode) {
        (ProviderKind::OpenAi, AuthMode::OpenAiSession) => {
            let redirect_uri = format!("{admin_base_url}/oauth/openai/callback");
            let pkce = generate_pkce();
            let state = generate_random_string(24);
            let mut url = reqwest::Url::parse(&format!("{OPENAI_ISSUER}/oauth/authorize"))
                .map_err(|error| ApiError::internal(anyhow!(error)))?;
            url.query_pairs_mut()
                .append_pair("response_type", "code")
                .append_pair("client_id", OPENAI_CLIENT_ID)
                .append_pair("redirect_uri", &redirect_uri)
                .append_pair("scope", "openid profile email offline_access")
                .append_pair("code_challenge", &pkce.challenge)
                .append_pair("code_challenge_method", "S256")
                .append_pair("state", &state)
                .append_pair("id_token_add_organizations", "true")
                .append_pair("codex_cli_simplified_flow", "true")
                .append_pair("originator", "agent-llm");
            (
                url.to_string(),
                "Complete the ChatGPT account authorization in your browser. agent-llm will store the resulting account credential locally."
                    .to_string(),
                AuthCompletionMode::Auto,
                ProviderAuthFlow::OpenAi {
                    verifier: pkce.verifier,
                    state,
                    redirect_uri,
                },
            )
        }
        (ProviderKind::Anthropic, AuthMode::AnthropicSession) => {
            let pkce = generate_pkce();
            let mut url = reqwest::Url::parse(ANTHROPIC_CLAUDE_AUTHORIZE_URL)
                .map_err(|error| ApiError::internal(anyhow!(error)))?;
            url.query_pairs_mut()
                .append_pair("code", "true")
                .append_pair("client_id", ANTHROPIC_CLIENT_ID)
                .append_pair("response_type", "code")
                .append_pair(
                    "redirect_uri",
                    "https://console.anthropic.com/oauth/code/callback",
                )
                .append_pair("scope", "org:create_api_key user:profile user:inference")
                .append_pair("code_challenge", &pkce.challenge)
                .append_pair("code_challenge_method", "S256")
                .append_pair("state", &pkce.verifier);
            (
                url.to_string(),
                "After login, paste the Claude authorization code here. Anthropic currently returns a copy-paste code flow rather than a localhost callback."
                    .to_string(),
                AuthCompletionMode::Code,
                ProviderAuthFlow::Anthropic {
                    verifier: pkce.verifier,
                },
            )
        }
        (ProviderKind::Google, AuthMode::GoogleOAuth) => {
            let client_id = google_client_id(&metadata)
                .ok_or_else(|| ApiError::bad_request("missing Google OAuth client ID"))?;
            let client_secret = google_client_secret(&metadata)
                .ok_or_else(|| ApiError::bad_request("missing Google OAuth client secret"))?;
            let project_id = google_project_id(&metadata);
            let redirect_uri = format!("{admin_base_url}/oauth/google/callback");
            let pkce = generate_pkce();
            let state = generate_random_string(24);
            let mut url = reqwest::Url::parse(GOOGLE_AUTHORIZE_URL)
                .map_err(|error| ApiError::internal(anyhow!(error)))?;
            url.query_pairs_mut()
                .append_pair("client_id", &client_id)
                .append_pair("response_type", "code")
                .append_pair("redirect_uri", &redirect_uri)
                .append_pair(
                    "scope",
                    "https://www.googleapis.com/auth/cloud-platform openid email profile",
                )
                .append_pair("code_challenge", &pkce.challenge)
                .append_pair("code_challenge_method", "S256")
                .append_pair("state", &state)
                .append_pair("access_type", "offline")
                .append_pair("prompt", "consent");
            (
                url.to_string(),
                "Complete the Google OAuth flow in your browser. If your OAuth app is configured for localhost redirects, agent-llm will finish the connection automatically."
                    .to_string(),
                AuthCompletionMode::Auto,
                ProviderAuthFlow::Google {
                    verifier: pkce.verifier,
                    state,
                    redirect_uri,
                    client_id,
                    client_secret,
                    project_id,
                },
            )
        }
        _ => {
            return Err(ApiError::bad_request(
                "browser auth is not available for this provider/auth mode",
            ));
        }
    };

    let attempt = AuthAttempt {
        id,
        provider,
        method_id,
        auth_mode,
        profile_name: name.into(),
        is_default: request.is_default.unwrap_or(false),
        metadata,
        completion_mode,
        authorize_url: Some(authorize_url),
        poll_after_ms,
        instructions: instructions.clone(),
        flow,
        status: if completion_mode == AuthCompletionMode::Code {
            AuthAttemptStatus::AwaitingCode
        } else {
            AuthAttemptStatus::Pending
        },
        expires_at,
    };
    Ok(attempt)
}

fn resolve_requested_method(
    provider: ProviderKind,
    request: &StartAuthFlowRequest,
) -> Result<(String, AuthMode), ApiError> {
    let requested = request
        .method
        .as_deref()
        .or(request.auth_mode.as_deref())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| ApiError::bad_request("auth method is required"))?;
    let auth_mode = match (provider, requested) {
        (ProviderKind::OpenAi, "openai_account" | "openai_session" | "oauth") => {
            AuthMode::OpenAiSession
        }
        (ProviderKind::Anthropic, "anthropic_account" | "anthropic_session" | "oauth") => {
            AuthMode::AnthropicSession
        }
        (ProviderKind::Google, "google_oauth" | "oauth") => AuthMode::GoogleOAuth,
        (_, "api_key") => AuthMode::ApiKey,
        _ => AuthMode::parse_for_provider(provider, requested)
            .ok_or_else(|| ApiError::bad_request("unknown auth method"))?,
    };
    Ok((method_id_for(provider, auth_mode).into(), auth_mode))
}

fn method_id_for(provider: ProviderKind, auth_mode: AuthMode) -> &'static str {
    match (provider, auth_mode) {
        (_, AuthMode::ApiKey) => "api_key",
        (ProviderKind::OpenAi, AuthMode::OpenAiSession) => "openai_account",
        (ProviderKind::Anthropic, AuthMode::AnthropicSession) => "anthropic_account",
        (ProviderKind::Google, AuthMode::GoogleOAuth) => "google_oauth",
        _ => auth_mode.as_str(),
    }
}

pub async fn complete_auth_attempt_with_code(
    http: &Client,
    db: &Database,
    secrets: &LocalSecretStore,
    attempt: &AuthAttempt,
    code: &str,
) -> Result<(), ApiError> {
    let credential = match (&attempt.provider, &attempt.flow) {
        (ProviderKind::Anthropic, ProviderAuthFlow::Anthropic { verifier }) => {
            exchange_anthropic_code(http, code, verifier).await?
        }
        _ => {
            return Err(ApiError::bad_request(
                "this auth attempt does not accept manual code completion",
            ));
        }
    };

    persist_auth_profile(db, secrets, attempt, &credential)
}

pub async fn complete_auth_attempt_from_callback(
    http: &Client,
    db: &Database,
    secrets: &LocalSecretStore,
    attempt: &AuthAttempt,
    code: &str,
) -> Result<(), ApiError> {
    let credential = match (&attempt.provider, &attempt.flow) {
        (
            ProviderKind::OpenAi,
            ProviderAuthFlow::OpenAi {
                verifier,
                redirect_uri,
                ..
            },
        ) => exchange_openai_code(http, code, verifier, redirect_uri).await?,
        (
            ProviderKind::Google,
            ProviderAuthFlow::Google {
                verifier,
                redirect_uri,
                client_id,
                client_secret,
                project_id,
                ..
            },
        ) => {
            exchange_google_code(
                http,
                code,
                verifier,
                redirect_uri,
                client_id,
                client_secret,
                project_id.clone(),
            )
            .await?
        }
        _ => {
            return Err(ApiError::bad_request(
                "this auth attempt does not use callback completion",
            ));
        }
    };

    persist_auth_profile(db, secrets, attempt, &credential)
}

pub async fn refresh_credential_if_needed(
    http: &Client,
    provider: ProviderKind,
    auth_mode: AuthMode,
    credential: &StoredCredential,
) -> Result<Option<StoredCredential>, ApiError> {
    if !credential.needs_refresh() {
        return Ok(None);
    }

    let refreshed = match (provider, auth_mode, credential) {
        (
            ProviderKind::OpenAi,
            AuthMode::OpenAiSession,
            StoredCredential::Account {
                refresh_token: Some(refresh_token),
                account_id,
                ..
            },
        ) => refresh_openai_credential(http, refresh_token, account_id.clone()).await?,
        (
            ProviderKind::Anthropic,
            AuthMode::AnthropicSession,
            StoredCredential::Account {
                refresh_token: Some(refresh_token),
                ..
            },
        ) => refresh_anthropic_credential(http, refresh_token).await?,
        (
            ProviderKind::Google,
            AuthMode::GoogleOAuth,
            StoredCredential::Account {
                refresh_token: Some(refresh_token),
                client_id,
                client_secret,
                project_id,
                ..
            },
        ) => {
            refresh_google_credential(
                http,
                refresh_token,
                client_id.as_deref(),
                client_secret.as_deref(),
                project_id.clone(),
            )
            .await?
        }
        _ => return Ok(None),
    };

    Ok(Some(refreshed))
}

pub fn persist_auth_profile(
    db: &Database,
    secrets: &LocalSecretStore,
    attempt: &AuthAttempt,
    credential: &StoredCredential,
) -> Result<(), ApiError> {
    let secret_ref = secrets
        .store_auth_profile_secret(
            attempt.provider,
            &attempt.profile_name,
            &credential.serialize()?,
        )
        .map_err(ApiError::internal)?;

    db.add_auth_profile(
        attempt.provider,
        &attempt.profile_name,
        attempt.auth_mode,
        &secret_ref,
        attempt.is_default,
        attempt.metadata.clone(),
    )
    .map_err(ApiError::internal)?;

    Ok(())
}

async fn exchange_openai_code(
    http: &Client,
    code: &str,
    verifier: &str,
    redirect_uri: &str,
) -> Result<StoredCredential, ApiError> {
    #[derive(Deserialize)]
    struct TokenResponse {
        access_token: String,
        refresh_token: String,
        expires_in: Option<i64>,
        id_token: Option<String>,
    }

    let response = http
        .post(format!("{OPENAI_ISSUER}/oauth/token"))
        .header("Content-Type", "application/x-www-form-urlencoded")
        .body(
            [
                ("grant_type", "authorization_code"),
                ("code", code),
                ("redirect_uri", redirect_uri),
                ("client_id", OPENAI_CLIENT_ID),
                ("code_verifier", verifier),
            ]
            .into_iter()
            .map(|(key, value)| format!("{key}={}", urlencoding(value)))
            .collect::<Vec<_>>()
            .join("&"),
        )
        .send()
        .await
        .map_err(ApiError::upstream)?;
    let status = response.status();
    let body = response.text().await.map_err(ApiError::upstream)?;
    if !status.is_success() {
        return Err(ApiError::upstream_status(status, body));
    }

    let token: TokenResponse =
        serde_json::from_str(&body).map_err(|error| ApiError::internal(anyhow!(error)))?;
    let account_id = token
        .id_token
        .as_deref()
        .and_then(extract_openai_account_id)
        .or_else(|| extract_openai_account_id(&token.access_token));
    Ok(StoredCredential::with_account_tokens(
        token.access_token,
        Some(token.refresh_token),
        token
            .expires_in
            .map(|seconds| Utc::now() + Duration::seconds(seconds)),
        account_id,
        Some(OPENAI_CLIENT_ID.into()),
        None,
        None,
    ))
}

async fn exchange_anthropic_code(
    http: &Client,
    code_input: &str,
    verifier: &str,
) -> Result<StoredCredential, ApiError> {
    #[derive(Deserialize)]
    struct TokenResponse {
        access_token: String,
        refresh_token: String,
        expires_in: i64,
    }

    let (code, state) = split_anthropic_code_input(code_input)?;
    let response = http
        .post(ANTHROPIC_TOKEN_URL)
        .json(&json!({
            "code": code,
            "state": state,
            "grant_type": "authorization_code",
            "client_id": ANTHROPIC_CLIENT_ID,
            "redirect_uri": "https://console.anthropic.com/oauth/code/callback",
            "code_verifier": verifier,
        }))
        .send()
        .await
        .map_err(ApiError::upstream)?;
    let status = response.status();
    let body = response.text().await.map_err(ApiError::upstream)?;
    if !status.is_success() {
        return Err(ApiError::upstream_status(status, body));
    }

    let token: TokenResponse =
        serde_json::from_str(&body).map_err(|error| ApiError::internal(anyhow!(error)))?;
    Ok(StoredCredential::with_account_tokens(
        token.access_token,
        Some(token.refresh_token),
        Some(Utc::now() + Duration::seconds(token.expires_in)),
        None,
        Some(ANTHROPIC_CLIENT_ID.into()),
        None,
        None,
    ))
}

async fn exchange_google_code(
    http: &Client,
    code: &str,
    verifier: &str,
    redirect_uri: &str,
    client_id: &str,
    client_secret: &str,
    project_id: Option<String>,
) -> Result<StoredCredential, ApiError> {
    #[derive(Deserialize)]
    struct TokenResponse {
        access_token: String,
        refresh_token: Option<String>,
        expires_in: i64,
    }

    let response = http
        .post(GOOGLE_TOKEN_URL)
        .header("Content-Type", "application/x-www-form-urlencoded")
        .body(
            [
                ("client_id", client_id),
                ("client_secret", client_secret),
                ("code", code),
                ("grant_type", "authorization_code"),
                ("redirect_uri", redirect_uri),
                ("code_verifier", verifier),
            ]
            .into_iter()
            .map(|(key, value)| format!("{key}={}", urlencoding(value)))
            .collect::<Vec<_>>()
            .join("&"),
        )
        .send()
        .await
        .map_err(ApiError::upstream)?;
    let status = response.status();
    let body = response.text().await.map_err(ApiError::upstream)?;
    if !status.is_success() {
        return Err(ApiError::upstream_status(status, body));
    }

    let token: TokenResponse =
        serde_json::from_str(&body).map_err(|error| ApiError::internal(anyhow!(error)))?;
    let refresh_token = token.refresh_token.ok_or_else(|| {
        ApiError::bad_request("Google OAuth response did not include a refresh token")
    })?;
    Ok(StoredCredential::with_account_tokens(
        token.access_token,
        Some(refresh_token),
        Some(Utc::now() + Duration::seconds(token.expires_in)),
        None,
        Some(client_id.into()),
        Some(client_secret.into()),
        project_id,
    ))
}

async fn refresh_openai_credential(
    http: &Client,
    refresh_token: &str,
    account_id: Option<String>,
) -> Result<StoredCredential, ApiError> {
    #[derive(Deserialize)]
    struct TokenResponse {
        access_token: String,
        refresh_token: Option<String>,
        expires_in: Option<i64>,
        id_token: Option<String>,
    }

    let response = http
        .post(format!("{OPENAI_ISSUER}/oauth/token"))
        .header("Content-Type", "application/x-www-form-urlencoded")
        .body(
            [
                ("grant_type", "refresh_token"),
                ("refresh_token", refresh_token),
                ("client_id", OPENAI_CLIENT_ID),
            ]
            .into_iter()
            .map(|(key, value)| format!("{key}={}", urlencoding(value)))
            .collect::<Vec<_>>()
            .join("&"),
        )
        .send()
        .await
        .map_err(ApiError::upstream)?;
    let status = response.status();
    let body = response.text().await.map_err(ApiError::upstream)?;
    if !status.is_success() {
        return Err(ApiError::upstream_status(status, body));
    }

    let token: TokenResponse =
        serde_json::from_str(&body).map_err(|error| ApiError::internal(anyhow!(error)))?;
    Ok(StoredCredential::with_account_tokens(
        token.access_token.clone(),
        Some(
            token
                .refresh_token
                .unwrap_or_else(|| refresh_token.to_string()),
        ),
        token
            .expires_in
            .map(|seconds| Utc::now() + Duration::seconds(seconds)),
        token
            .id_token
            .as_deref()
            .and_then(extract_openai_account_id)
            .or(account_id),
        Some(OPENAI_CLIENT_ID.into()),
        None,
        None,
    ))
}

async fn refresh_anthropic_credential(
    http: &Client,
    refresh_token: &str,
) -> Result<StoredCredential, ApiError> {
    #[derive(Deserialize)]
    struct TokenResponse {
        access_token: String,
        refresh_token: Option<String>,
        expires_in: i64,
    }

    let response = http
        .post(ANTHROPIC_TOKEN_URL)
        .json(&json!({
            "grant_type": "refresh_token",
            "refresh_token": refresh_token,
            "client_id": ANTHROPIC_CLIENT_ID,
        }))
        .send()
        .await
        .map_err(ApiError::upstream)?;
    let status = response.status();
    let body = response.text().await.map_err(ApiError::upstream)?;
    if !status.is_success() {
        return Err(ApiError::upstream_status(status, body));
    }
    let token: TokenResponse =
        serde_json::from_str(&body).map_err(|error| ApiError::internal(anyhow!(error)))?;
    Ok(StoredCredential::with_account_tokens(
        token.access_token,
        Some(
            token
                .refresh_token
                .unwrap_or_else(|| refresh_token.to_string()),
        ),
        Some(Utc::now() + Duration::seconds(token.expires_in)),
        None,
        Some(ANTHROPIC_CLIENT_ID.into()),
        None,
        None,
    ))
}

async fn refresh_google_credential(
    http: &Client,
    refresh_token: &str,
    client_id: Option<&str>,
    client_secret: Option<&str>,
    project_id: Option<String>,
) -> Result<StoredCredential, ApiError> {
    #[derive(Deserialize)]
    struct TokenResponse {
        access_token: String,
        expires_in: i64,
        refresh_token: Option<String>,
    }

    let client_id =
        client_id.ok_or_else(|| ApiError::bad_request("Google OAuth client ID is missing"))?;
    let client_secret = client_secret
        .ok_or_else(|| ApiError::bad_request("Google OAuth client secret is missing"))?;
    let response = http
        .post(GOOGLE_TOKEN_URL)
        .header("Content-Type", "application/x-www-form-urlencoded")
        .body(
            [
                ("client_id", client_id),
                ("client_secret", client_secret),
                ("grant_type", "refresh_token"),
                ("refresh_token", refresh_token),
            ]
            .into_iter()
            .map(|(key, value)| format!("{key}={}", urlencoding(value)))
            .collect::<Vec<_>>()
            .join("&"),
        )
        .send()
        .await
        .map_err(ApiError::upstream)?;
    let status = response.status();
    let body = response.text().await.map_err(ApiError::upstream)?;
    if !status.is_success() {
        return Err(ApiError::upstream_status(status, body));
    }
    let token: TokenResponse =
        serde_json::from_str(&body).map_err(|error| ApiError::internal(anyhow!(error)))?;
    Ok(StoredCredential::with_account_tokens(
        token.access_token,
        Some(
            token
                .refresh_token
                .unwrap_or_else(|| refresh_token.to_string()),
        ),
        Some(Utc::now() + Duration::seconds(token.expires_in)),
        None,
        Some(client_id.into()),
        Some(client_secret.into()),
        project_id,
    ))
}

pub fn oauth_success_html() -> String {
    r#"<!doctype html>
<html>
  <head>
    <meta charset="utf-8" />
    <title>agent-llm Authorization Complete</title>
    <style>
      body { font-family: -apple-system, BlinkMacSystemFont, sans-serif; background: #f6f4ef; color: #1d1d1f; display: flex; min-height: 100vh; align-items: center; justify-content: center; margin: 0; }
      main { width: min(480px, calc(100% - 48px)); background: white; border-radius: 24px; padding: 32px; box-shadow: 0 16px 40px rgba(0,0,0,0.08); }
      h1 { margin-top: 0; }
      p { line-height: 1.5; color: #4a4a4a; }
    </style>
  </head>
  <body>
    <main>
      <h1>Authorization complete</h1>
      <p>The credential was stored locally for agent-llm. You can return to the menu bar app.</p>
    </main>
  </body>
</html>"#
        .into()
}

pub fn oauth_error_html(message: &str) -> String {
    format!(
        r#"<!doctype html>
<html>
  <head>
    <meta charset="utf-8" />
    <title>agent-llm Authorization Failed</title>
    <style>
      body {{ font-family: -apple-system, BlinkMacSystemFont, sans-serif; background: #f8efed; color: #1d1d1f; display: flex; min-height: 100vh; align-items: center; justify-content: center; margin: 0; }}
      main {{ width: min(520px, calc(100% - 48px)); background: white; border-radius: 24px; padding: 32px; box-shadow: 0 16px 40px rgba(0,0,0,0.08); }}
      h1 {{ margin-top: 0; color: #a32020; }}
      p, pre {{ line-height: 1.5; color: #4a4a4a; }}
      pre {{ white-space: pre-wrap; background: #f8f6f2; padding: 12px; border-radius: 12px; }}
    </style>
  </head>
  <body>
    <main>
      <h1>Authorization failed</h1>
      <p>agent-llm could not complete the browser sign-in.</p>
      <pre>{}</pre>
    </main>
  </body>
</html>"#,
        html_escape(message)
    )
}

fn split_anthropic_code_input(input: &str) -> Result<(&str, &str), ApiError> {
    input.trim().split_once('#').ok_or_else(|| {
        ApiError::bad_request(
            "Anthropic authorization code must include the copied `code#state` value",
        )
    })
}

fn extract_openai_account_id(token: &str) -> Option<String> {
    let payload = token.split('.').nth(1)?;
    let decoded = URL_SAFE_NO_PAD.decode(payload).ok()?;
    let json: Value = serde_json::from_slice(&decoded).ok()?;
    json.get("chatgpt_account_id")
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
        .or_else(|| {
            json.pointer("/https://api.openai.com/auth/chatgpt_account_id")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned)
        })
        .or_else(|| {
            json.get("organizations")
                .and_then(Value::as_array)
                .and_then(|items| items.first())
                .and_then(|item| item.get("id"))
                .and_then(Value::as_str)
                .map(ToOwned::to_owned)
        })
}

fn google_client_id(metadata: &Value) -> Option<String> {
    std::env::var("AGENT_LLM_GOOGLE_OAUTH_CLIENT_ID")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .or_else(|| {
            metadata
                .pointer("/oauth_client_id")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned)
        })
        .or_else(|| {
            metadata
                .pointer("/oauth/client_id")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned)
        })
}

fn google_client_secret(metadata: &Value) -> Option<String> {
    std::env::var("AGENT_LLM_GOOGLE_OAUTH_CLIENT_SECRET")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .or_else(|| {
            metadata
                .pointer("/oauth_client_secret")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned)
        })
        .or_else(|| {
            metadata
                .pointer("/oauth/client_secret")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned)
        })
}

fn google_project_id(metadata: &Value) -> Option<String> {
    std::env::var("AGENT_LLM_GOOGLE_PROJECT_ID")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .or_else(|| std::env::var("GOOGLE_CLOUD_PROJECT").ok())
        .or_else(|| {
            metadata
                .pointer("/google_project_id")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned)
        })
        .or_else(|| {
            metadata
                .pointer("/oauth/project_id")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned)
        })
}

struct PkceCodes {
    verifier: String,
    challenge: String,
}

fn generate_pkce() -> PkceCodes {
    let verifier = generate_random_string(64);
    let digest = Sha256::digest(verifier.as_bytes());
    let challenge = URL_SAFE_NO_PAD.encode(digest);
    PkceCodes {
        verifier,
        challenge,
    }
}

fn generate_random_string(len: usize) -> String {
    let mut bytes = vec![0u8; len];
    rand::rng().fill_bytes(&mut bytes);
    URL_SAFE_NO_PAD.encode(bytes)
}

fn urlencoding(value: &str) -> String {
    let mut encoded = String::with_capacity(value.len());
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                encoded.push(byte as char)
            }
            b' ' => encoded.push_str("%20"),
            _ => encoded.push_str(&format!("%{:02X}", byte)),
        }
    }
    encoded
}

fn html_escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

pub fn write_refreshed_credential(
    secrets: &LocalSecretStore,
    secret_ref: &agent_llm_core::types::SecretRef,
    credential: &StoredCredential,
) -> Result<(), ApiError> {
    secrets
        .overwrite_secret(secret_ref, &credential.serialize()?)
        .map_err(ApiError::internal)
}
