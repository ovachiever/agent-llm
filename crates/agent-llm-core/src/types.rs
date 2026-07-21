use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum ProviderKind {
    OpenAi,
    Anthropic,
    Google,
    OpenRouter,
    Kimi,
    LmStudio,
}

impl ProviderKind {
    pub const ALL: [Self; 6] = [
        Self::OpenAi,
        Self::Anthropic,
        Self::Google,
        Self::OpenRouter,
        Self::Kimi,
        Self::LmStudio,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::OpenAi => "openai",
            Self::Anthropic => "anthropic",
            Self::Google => "google",
            Self::OpenRouter => "openrouter",
            Self::Kimi => "kimi",
            Self::LmStudio => "lmstudio",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "openai" => Some(Self::OpenAi),
            "anthropic" => Some(Self::Anthropic),
            "google" => Some(Self::Google),
            "openrouter" => Some(Self::OpenRouter),
            "kimi" => Some(Self::Kimi),
            "lmstudio" => Some(Self::LmStudio),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AuthMode {
    ApiKey,
    OpenAiSession,
    AnthropicSession,
    GoogleOAuth,
    None,
}

impl AuthMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ApiKey => "api_key",
            Self::OpenAiSession => "openai_session",
            Self::AnthropicSession => "anthropic_session",
            Self::GoogleOAuth => "google_oauth",
            Self::None => "none",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::ApiKey => "API key",
            Self::OpenAiSession => "OpenAI session",
            Self::AnthropicSession => "Anthropic session",
            Self::GoogleOAuth => "Google OAuth",
            Self::None => "No auth (local)",
        }
    }

    pub fn is_api_key(self) -> bool {
        matches!(self, Self::ApiKey)
    }

    pub fn is_account_auth(self) -> bool {
        !matches!(self, Self::ApiKey | Self::None)
    }

    pub fn parse_for_provider(provider: ProviderKind, value: &str) -> Option<Self> {
        match value {
            "api_key" => Some(Self::ApiKey),
            "openai_session" if provider == ProviderKind::OpenAi => Some(Self::OpenAiSession),
            "anthropic_session" if provider == ProviderKind::Anthropic => {
                Some(Self::AnthropicSession)
            }
            "google_oauth" if provider == ProviderKind::Google => Some(Self::GoogleOAuth),
            "none" if provider == ProviderKind::LmStudio => Some(Self::None),
            // Backward-compatible alias for the earlier generic auth mode.
            "oauth" if provider == ProviderKind::OpenAi => Some(Self::OpenAiSession),
            "oauth" if provider == ProviderKind::Anthropic => Some(Self::AnthropicSession),
            "oauth" if provider == ProviderKind::Google => Some(Self::GoogleOAuth),
            _ => None,
        }
    }

    pub fn is_allowed_for_provider(self, provider: ProviderKind) -> bool {
        matches!(
            (self, provider),
            (Self::ApiKey, _)
                | (Self::OpenAiSession, ProviderKind::OpenAi)
                | (Self::AnthropicSession, ProviderKind::Anthropic)
                | (Self::GoogleOAuth, ProviderKind::Google)
                | (Self::None, ProviderKind::LmStudio)
        )
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SecretBackend {
    Keychain,
    File,
}

impl SecretBackend {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Keychain => "keychain",
            Self::File => "file",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "keychain" => Some(Self::Keychain),
            "file" => Some(Self::File),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SecretRef {
    pub backend: SecretBackend,
    pub key: String,
}

impl SecretRef {
    pub fn new(backend: SecretBackend, key: impl Into<String>) -> Self {
        Self {
            backend,
            key: key.into(),
        }
    }

    pub fn as_storage_value(&self) -> String {
        format!("{}:{}", self.backend.as_str(), self.key)
    }

    pub fn parse(value: &str) -> Option<Self> {
        let (backend, key) = value.split_once(':')?;
        Some(Self {
            backend: SecretBackend::parse(backend)?,
            key: key.to_string(),
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderRecord {
    pub provider: String,
    pub display_name: String,
    pub upstream_base_url: String,
    pub local_base_url: String,
    pub models_path: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuthProfile {
    pub id: i64,
    pub provider: String,
    pub name: String,
    pub auth_mode: AuthMode,
    pub secret_ref: SecretRef,
    pub is_default: bool,
    pub metadata: serde_json::Value,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectRecord {
    pub id: i64,
    pub name: String,
    pub project_key: String,
    pub active: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectProviderSetting {
    pub project_id: i64,
    pub provider: String,
    pub auth_profile_id: Option<i64>,
    pub default_model: Option<String>,
    pub route_mode: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelCacheEntry {
    pub id: i64,
    pub provider: String,
    pub model_id: String,
    pub display_name: String,
    pub raw_json: serde_json::Value,
    pub refreshed_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RequestLog {
    pub id: i64,
    pub request_id: String,
    pub project_name: String,
    pub provider: String,
    pub auth_profile_name: Option<String>,
    pub method: String,
    pub path: String,
    pub status_code: i64,
    pub latency_ms: i64,
    pub prompt_tokens: Option<i64>,
    pub completion_tokens: Option<i64>,
    pub total_tokens: Option<i64>,
    pub estimated_cost_usd: Option<f64>,
    pub used_fallback: bool,
    pub error_text: Option<String>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderSummary {
    pub provider: ProviderRecord,
    pub auth_profiles: Vec<AuthProfile>,
    pub models: Vec<ModelCacheEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AdminStatus {
    pub service: &'static str,
    pub version: &'static str,
    pub host: String,
    pub port: u16,
    pub project_count: usize,
    pub request_count: usize,
    pub provider_count: usize,
}

#[derive(Debug, Clone)]
pub struct UsageSnapshot {
    pub prompt_tokens: Option<i64>,
    pub completion_tokens: Option<i64>,
    pub total_tokens: Option<i64>,
    pub estimated_cost_usd: Option<f64>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_legacy_oauth_alias_to_provider_sessions() {
        assert_eq!(
            AuthMode::parse_for_provider(ProviderKind::OpenAi, "oauth"),
            Some(AuthMode::OpenAiSession)
        );
        assert_eq!(
            AuthMode::parse_for_provider(ProviderKind::Anthropic, "oauth"),
            Some(AuthMode::AnthropicSession)
        );
        assert_eq!(
            AuthMode::parse_for_provider(ProviderKind::Google, "oauth"),
            Some(AuthMode::GoogleOAuth)
        );
    }

    #[test]
    fn rejects_invalid_provider_session_combinations() {
        assert!(!AuthMode::OpenAiSession.is_allowed_for_provider(ProviderKind::OpenRouter));
        assert!(!AuthMode::AnthropicSession.is_allowed_for_provider(ProviderKind::OpenAi));
        assert!(!AuthMode::GoogleOAuth.is_allowed_for_provider(ProviderKind::OpenAi));
        assert!(AuthMode::ApiKey.is_allowed_for_provider(ProviderKind::Google));
        assert!(AuthMode::GoogleOAuth.is_allowed_for_provider(ProviderKind::Google));
    }

    #[test]
    fn distinguishes_api_and_account_auth_modes() {
        assert!(AuthMode::ApiKey.is_api_key());
        assert!(!AuthMode::OpenAiSession.is_api_key());
        assert!(AuthMode::GoogleOAuth.is_account_auth());
        assert!(!AuthMode::None.is_account_auth());
        assert!(!AuthMode::None.is_api_key());
    }

    #[test]
    fn supports_kimi_and_lmstudio_providers() {
        assert_eq!(ProviderKind::parse("kimi"), Some(ProviderKind::Kimi));
        assert_eq!(
            ProviderKind::parse("lmstudio"),
            Some(ProviderKind::LmStudio)
        );
        assert!(AuthMode::ApiKey.is_allowed_for_provider(ProviderKind::Kimi));
        assert!(AuthMode::None.is_allowed_for_provider(ProviderKind::LmStudio));
        assert!(!AuthMode::None.is_allowed_for_provider(ProviderKind::Kimi));
        assert!(!AuthMode::AnthropicSession.is_allowed_for_provider(ProviderKind::Kimi));
        assert_eq!(
            AuthMode::parse_for_provider(ProviderKind::LmStudio, "none"),
            Some(AuthMode::None)
        );
        assert_eq!(
            AuthMode::parse_for_provider(ProviderKind::Kimi, "none"),
            None
        );
    }

    #[test]
    fn parses_explicit_google_oauth_mode() {
        assert_eq!(
            AuthMode::parse_for_provider(ProviderKind::Google, "google_oauth"),
            Some(AuthMode::GoogleOAuth)
        );
    }

    #[test]
    fn round_trips_secret_ref_storage_values() {
        let secret_ref =
            SecretRef::new(SecretBackend::Keychain, "agent-llm/profiles/openai/default");
        let stored = secret_ref.as_storage_value();
        assert_eq!(
            SecretRef::parse(&stored),
            Some(SecretRef::new(
                SecretBackend::Keychain,
                "agent-llm/profiles/openai/default"
            ))
        );
    }
}
