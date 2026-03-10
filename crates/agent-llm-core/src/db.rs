use std::{
    fs,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

use anyhow::{Context, Result, anyhow};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use chrono::{DateTime, Utc};
use rand::RngCore;
use rusqlite::{Connection, OptionalExtension, Row, params};
use serde_json::Value;

use crate::{
    settings,
    types::{
        AdminStatus, AuthMode, AuthProfile, ModelCacheEntry, ProjectProviderSetting, ProjectRecord,
        ProviderKind, ProviderRecord, ProviderSummary, RequestLog, SecretRef, UsageSnapshot,
    },
};

#[derive(Clone)]
pub struct Database {
    conn: Arc<Mutex<Connection>>,
    pub path: PathBuf,
}

impl Database {
    pub fn open_default() -> Result<Self> {
        Self::open(settings::default_db_path()?)
    }

    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref().to_path_buf();
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("failed to create {}", parent.display()))?;
        }

        let conn = Connection::open(&path)
            .with_context(|| format!("failed to open {}", path.display()))?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.execute_batch(
            "
            CREATE TABLE IF NOT EXISTS providers (
              provider TEXT PRIMARY KEY,
              display_name TEXT NOT NULL,
              upstream_base_url TEXT NOT NULL,
              local_base_url TEXT NOT NULL,
              models_path TEXT NOT NULL
            );

            CREATE TABLE IF NOT EXISTS auth_profiles (
              id INTEGER PRIMARY KEY AUTOINCREMENT,
              provider TEXT NOT NULL,
              name TEXT NOT NULL,
              auth_mode TEXT NOT NULL,
              secret TEXT,
              secret_ref TEXT NOT NULL DEFAULT '',
              is_default INTEGER NOT NULL DEFAULT 0,
              metadata_json TEXT NOT NULL DEFAULT '{}',
              created_at TEXT NOT NULL,
              updated_at TEXT NOT NULL,
              UNIQUE(provider, name)
            );

            CREATE TABLE IF NOT EXISTS projects (
              id INTEGER PRIMARY KEY AUTOINCREMENT,
              name TEXT NOT NULL UNIQUE,
              project_key TEXT NOT NULL UNIQUE,
              active INTEGER NOT NULL DEFAULT 1,
              created_at TEXT NOT NULL,
              updated_at TEXT NOT NULL
            );

            CREATE TABLE IF NOT EXISTS project_provider_settings (
              project_id INTEGER NOT NULL,
              provider TEXT NOT NULL,
              auth_profile_id INTEGER,
              default_model TEXT,
              route_mode TEXT NOT NULL DEFAULT 'local',
              PRIMARY KEY(project_id, provider)
            );

            CREATE TABLE IF NOT EXISTS models_cache (
              id INTEGER PRIMARY KEY AUTOINCREMENT,
              provider TEXT NOT NULL,
              model_id TEXT NOT NULL,
              display_name TEXT NOT NULL,
              raw_json TEXT NOT NULL,
              refreshed_at TEXT NOT NULL,
              UNIQUE(provider, model_id)
            );

            CREATE TABLE IF NOT EXISTS request_logs (
              id INTEGER PRIMARY KEY AUTOINCREMENT,
              request_id TEXT NOT NULL,
              project_id INTEGER NOT NULL,
              provider TEXT NOT NULL,
              auth_profile_id INTEGER,
              method TEXT NOT NULL,
              path TEXT NOT NULL,
              status_code INTEGER NOT NULL,
              latency_ms INTEGER NOT NULL,
              prompt_tokens INTEGER,
              completion_tokens INTEGER,
              total_tokens INTEGER,
              estimated_cost_usd REAL,
              used_fallback INTEGER NOT NULL DEFAULT 0,
              error_text TEXT,
              created_at TEXT NOT NULL
            );
        ",
        )?;
        ensure_auth_profile_secret_ref_column(&conn)?;

        let db = Self {
            conn: Arc::new(Mutex::new(conn)),
            path,
        };
        db.seed_providers()?;
        Ok(db)
    }

    fn with_conn<T>(&self, f: impl FnOnce(&Connection) -> Result<T>) -> Result<T> {
        let guard = self
            .conn
            .lock()
            .map_err(|_| anyhow!("database mutex poisoned"))?;
        f(&guard)
    }

    fn seed_providers(&self) -> Result<()> {
        let defaults = [
            ProviderRecord {
                provider: "openai".into(),
                display_name: "OpenAI".into(),
                upstream_base_url: "https://api.openai.com".into(),
                local_base_url: format!("{}/openai/v1", settings::default_admin_base_url()),
                models_path: "/v1/models".into(),
            },
            ProviderRecord {
                provider: "anthropic".into(),
                display_name: "Anthropic".into(),
                upstream_base_url: "https://api.anthropic.com".into(),
                local_base_url: format!("{}/anthropic/v1", settings::default_admin_base_url()),
                models_path: "/v1/models".into(),
            },
            ProviderRecord {
                provider: "google".into(),
                display_name: "Google AI Studio".into(),
                upstream_base_url: "https://generativelanguage.googleapis.com".into(),
                local_base_url: format!("{}/google/v1beta", settings::default_admin_base_url()),
                models_path: "/v1beta/models".into(),
            },
            ProviderRecord {
                provider: "openrouter".into(),
                display_name: "OpenRouter".into(),
                upstream_base_url: "https://openrouter.ai/api".into(),
                local_base_url: format!("{}/openrouter/v1", settings::default_admin_base_url()),
                models_path: "/v1/models".into(),
            },
        ];

        self.with_conn(|conn| {
            for provider in defaults {
                conn.execute(
                    "
                    INSERT INTO providers (provider, display_name, upstream_base_url, local_base_url, models_path)
                    VALUES (?1, ?2, ?3, ?4, ?5)
                    ON CONFLICT(provider) DO UPDATE SET
                      display_name = excluded.display_name,
                      upstream_base_url = excluded.upstream_base_url,
                      local_base_url = excluded.local_base_url,
                      models_path = excluded.models_path
                    ",
                    params![
                        provider.provider,
                        provider.display_name,
                        provider.upstream_base_url,
                        provider.local_base_url,
                        provider.models_path
                    ],
                )?;
            }

            Ok(())
        })
    }

    pub fn provider(&self, provider: ProviderKind) -> Result<ProviderRecord> {
        self.provider_by_name(provider.as_str())
    }

    pub fn provider_by_name(&self, provider: &str) -> Result<ProviderRecord> {
        self.with_conn(|conn| {
            conn.query_row(
                "SELECT provider, display_name, upstream_base_url, local_base_url, models_path FROM providers WHERE provider = ?1",
                params![provider],
                map_provider,
            )
            .with_context(|| format!("provider `{provider}` is not configured"))
        })
    }

    pub fn list_provider_summaries(&self) -> Result<Vec<ProviderSummary>> {
        let providers = self.list_providers()?;
        providers
            .into_iter()
            .map(|provider| {
                Ok(ProviderSummary {
                    auth_profiles: self.list_auth_profiles(&provider.provider)?,
                    models: self.list_models(&provider.provider)?,
                    provider,
                })
            })
            .collect()
    }

    pub fn list_providers(&self) -> Result<Vec<ProviderRecord>> {
        self.with_conn(|conn| {
            let mut stmt = conn.prepare(
                "SELECT provider, display_name, upstream_base_url, local_base_url, models_path FROM providers ORDER BY provider",
            )?;
            let rows = stmt.query_map([], map_provider)?;
            rows.collect::<rusqlite::Result<Vec<_>>>().map_err(Into::into)
        })
    }

    pub fn add_auth_profile(
        &self,
        provider: ProviderKind,
        name: &str,
        auth_mode: AuthMode,
        secret_ref: &SecretRef,
        is_default: bool,
        metadata: Value,
    ) -> Result<AuthProfile> {
        if !auth_mode.is_allowed_for_provider(provider) {
            return Err(anyhow!(
                "auth mode `{}` is not valid for provider `{}`",
                auth_mode.as_str(),
                provider.as_str()
            ));
        }

        let now = Utc::now();
        self.with_conn(|conn| {
            if is_default {
                conn.execute(
                    "UPDATE auth_profiles SET is_default = 0, updated_at = ?2 WHERE provider = ?1",
                    params![provider.as_str(), now.to_rfc3339()],
                )?;
            }

            conn.execute(
                "
                INSERT INTO auth_profiles (provider, name, auth_mode, secret, secret_ref, is_default, metadata_json, created_at, updated_at)
                VALUES (?1, ?2, ?3, '', ?4, ?5, ?6, ?7, ?8)
                ON CONFLICT(provider, name) DO UPDATE SET
                  auth_mode = excluded.auth_mode,
                  secret = excluded.secret,
                  secret_ref = excluded.secret_ref,
                  is_default = excluded.is_default,
                  metadata_json = excluded.metadata_json,
                  updated_at = excluded.updated_at
                ",
                params![
                    provider.as_str(),
                    name,
                    auth_mode.as_str(),
                    secret_ref.as_storage_value(),
                    if is_default { 1 } else { 0 },
                    metadata.to_string(),
                    now.to_rfc3339(),
                    now.to_rfc3339(),
                ],
            )?;

            conn.query_row(
                "SELECT id, provider, name, auth_mode, secret_ref, is_default, metadata_json, created_at, updated_at FROM auth_profiles WHERE provider = ?1 AND name = ?2",
                params![provider.as_str(), name],
                map_auth_profile,
            )
            .map_err(Into::into)
        })
    }

    pub fn list_auth_profiles(&self, provider: &str) -> Result<Vec<AuthProfile>> {
        self.with_conn(|conn| {
            let mut stmt = conn.prepare(
                "SELECT id, provider, name, auth_mode, secret_ref, is_default, metadata_json, created_at, updated_at FROM auth_profiles WHERE provider = ?1 ORDER BY is_default DESC, name ASC",
            )?;
            let rows = stmt.query_map(params![provider], map_auth_profile)?;
            rows.collect::<rusqlite::Result<Vec<_>>>().map_err(Into::into)
        })
    }

    pub fn default_auth_profile(&self, provider: ProviderKind) -> Result<Option<AuthProfile>> {
        self.with_conn(|conn| {
            conn.query_row(
                "SELECT id, provider, name, auth_mode, secret_ref, is_default, metadata_json, created_at, updated_at FROM auth_profiles WHERE provider = ?1 AND is_default = 1 LIMIT 1",
                params![provider.as_str()],
                map_auth_profile,
            )
            .optional()
            .map_err(Into::into)
        })
    }

    pub fn create_project(&self, name: &str) -> Result<ProjectRecord> {
        let now = Utc::now();
        let key = generate_project_key();

        self.with_conn(|conn| {
            conn.execute(
                "INSERT INTO projects (name, project_key, active, created_at, updated_at) VALUES (?1, ?2, 1, ?3, ?4)",
                params![name, key, now.to_rfc3339(), now.to_rfc3339()],
            )?;

            let project_id = conn.last_insert_rowid();

            for provider in [
                ProviderKind::OpenAi,
                ProviderKind::Anthropic,
                ProviderKind::Google,
                ProviderKind::OpenRouter,
            ] {
                conn.execute(
                    "INSERT INTO project_provider_settings (project_id, provider, auth_profile_id, default_model, route_mode) VALUES (?1, ?2, NULL, NULL, 'local')",
                    params![project_id, provider.as_str()],
                )?;
            }

            conn.query_row(
                "SELECT id, name, project_key, active, created_at, updated_at FROM projects WHERE id = ?1",
                params![project_id],
                map_project,
            )
            .map_err(Into::into)
        })
    }

    pub fn list_projects(&self) -> Result<Vec<ProjectRecord>> {
        self.with_conn(|conn| {
            let mut stmt = conn.prepare(
                "SELECT id, name, project_key, active, created_at, updated_at FROM projects ORDER BY name",
            )?;
            let rows = stmt.query_map([], map_project)?;
            rows.collect::<rusqlite::Result<Vec<_>>>().map_err(Into::into)
        })
    }

    pub fn project_by_name(&self, name: &str) -> Result<ProjectRecord> {
        self.with_conn(|conn| {
            conn.query_row(
                "SELECT id, name, project_key, active, created_at, updated_at FROM projects WHERE name = ?1",
                params![name],
                map_project,
            )
            .with_context(|| format!("project `{name}` not found"))
        })
    }

    pub fn project_by_key(&self, project_key: &str) -> Result<ProjectRecord> {
        self.with_conn(|conn| {
            conn.query_row(
                "SELECT id, name, project_key, active, created_at, updated_at FROM projects WHERE project_key = ?1 AND active = 1",
                params![project_key],
                map_project,
            )
            .with_context(|| "project key is invalid or inactive".to_string())
        })
    }

    pub fn project_provider_setting(
        &self,
        project_id: i64,
        provider: ProviderKind,
    ) -> Result<ProjectProviderSetting> {
        self.with_conn(|conn| {
            conn.query_row(
                "SELECT project_id, provider, auth_profile_id, default_model, route_mode FROM project_provider_settings WHERE project_id = ?1 AND provider = ?2",
                params![project_id, provider.as_str()],
                map_project_provider_setting,
            )
            .with_context(|| format!("provider settings missing for project {project_id}"))
        })
    }

    pub fn set_project_provider_defaults(
        &self,
        project_name: &str,
        provider: ProviderKind,
        auth_profile_name: Option<&str>,
        default_model: Option<&str>,
        route_mode: Option<&str>,
    ) -> Result<ProjectProviderSetting> {
        let project = self.project_by_name(project_name)?;
        let auth_profile_id = if let Some(name) = auth_profile_name {
            Some(self.auth_profile_by_name(provider, name)?.id)
        } else {
            None
        };

        self.with_conn(|conn| {
            conn.execute(
                "
                UPDATE project_provider_settings
                SET auth_profile_id = COALESCE(?3, auth_profile_id),
                    default_model = COALESCE(?4, default_model),
                    route_mode = COALESCE(?5, route_mode)
                WHERE project_id = ?1 AND provider = ?2
                ",
                params![
                    project.id,
                    provider.as_str(),
                    auth_profile_id,
                    default_model,
                    route_mode
                ],
            )?;

            conn.query_row(
                "SELECT project_id, provider, auth_profile_id, default_model, route_mode FROM project_provider_settings WHERE project_id = ?1 AND provider = ?2",
                params![project.id, provider.as_str()],
                map_project_provider_setting,
            )
            .map_err(Into::into)
        })
    }

    pub fn auth_profile_by_name(&self, provider: ProviderKind, name: &str) -> Result<AuthProfile> {
        self.with_conn(|conn| {
            conn.query_row(
                "SELECT id, provider, name, auth_mode, secret_ref, is_default, metadata_json, created_at, updated_at FROM auth_profiles WHERE provider = ?1 AND name = ?2",
                params![provider.as_str(), name],
                map_auth_profile,
            )
            .with_context(|| format!("auth profile `{name}` not found for {}", provider.as_str()))
        })
    }

    pub fn resolve_auth_profile(
        &self,
        project_id: i64,
        provider: ProviderKind,
        requested_profile: Option<&str>,
    ) -> Result<Option<AuthProfile>> {
        if let Some(name) = requested_profile {
            return Ok(Some(self.auth_profile_by_name(provider, name)?));
        }

        let setting = self.project_provider_setting(project_id, provider)?;
        if let Some(auth_profile_id) = setting.auth_profile_id {
            return self.with_conn(|conn| {
                conn.query_row(
                    "SELECT id, provider, name, auth_mode, secret_ref, is_default, metadata_json, created_at, updated_at FROM auth_profiles WHERE id = ?1",
                    params![auth_profile_id],
                    map_auth_profile,
                )
                .optional()
                .map_err(Into::into)
            });
        }

        self.default_auth_profile(provider)
    }

    pub fn replace_models(
        &self,
        provider: ProviderKind,
        models: Vec<(String, String, Value)>,
    ) -> Result<usize> {
        let now = Utc::now().to_rfc3339();
        self.with_conn(|conn| {
            let tx = conn.unchecked_transaction()?;
            tx.execute("DELETE FROM models_cache WHERE provider = ?1", params![provider.as_str()])?;
            for (model_id, display_name, raw_json) in &models {
                tx.execute(
                    "INSERT INTO models_cache (provider, model_id, display_name, raw_json, refreshed_at) VALUES (?1, ?2, ?3, ?4, ?5)",
                    params![provider.as_str(), model_id, display_name, raw_json.to_string(), now],
                )?;
            }
            tx.commit()?;
            Ok(models.len())
        })
    }

    pub fn list_models(&self, provider: &str) -> Result<Vec<ModelCacheEntry>> {
        self.with_conn(|conn| {
            let mut stmt = conn.prepare(
                "SELECT id, provider, model_id, display_name, raw_json, refreshed_at FROM models_cache WHERE provider = ?1 ORDER BY model_id",
            )?;
            let rows = stmt.query_map(params![provider], map_model_cache_entry)?;
            rows.collect::<rusqlite::Result<Vec<_>>>().map_err(Into::into)
        })
    }

    pub fn log_request(
        &self,
        request_id: &str,
        project_id: i64,
        provider: ProviderKind,
        auth_profile_id: Option<i64>,
        method: &str,
        path: &str,
        status_code: i64,
        latency_ms: i64,
        usage: UsageSnapshot,
        used_fallback: bool,
        error_text: Option<&str>,
    ) -> Result<()> {
        self.with_conn(|conn| {
            conn.execute(
                "
                INSERT INTO request_logs (
                  request_id, project_id, provider, auth_profile_id, method, path, status_code, latency_ms,
                  prompt_tokens, completion_tokens, total_tokens, estimated_cost_usd, used_fallback, error_text, created_at
                ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)
                ",
                params![
                    request_id,
                    project_id,
                    provider.as_str(),
                    auth_profile_id,
                    method,
                    path,
                    status_code,
                    latency_ms,
                    usage.prompt_tokens,
                    usage.completion_tokens,
                    usage.total_tokens,
                    usage.estimated_cost_usd,
                    if used_fallback { 1 } else { 0 },
                    error_text,
                    Utc::now().to_rfc3339()
                ],
            )?;
            Ok(())
        })
    }

    pub fn recent_requests(&self, limit: usize) -> Result<Vec<RequestLog>> {
        self.with_conn(|conn| {
            let mut stmt = conn.prepare(
                "
                SELECT
                  rl.id, rl.request_id, p.name, rl.provider, ap.name, rl.method, rl.path, rl.status_code,
                  rl.latency_ms, rl.prompt_tokens, rl.completion_tokens, rl.total_tokens, rl.estimated_cost_usd,
                  rl.used_fallback, rl.error_text, rl.created_at
                FROM request_logs rl
                JOIN projects p ON p.id = rl.project_id
                LEFT JOIN auth_profiles ap ON ap.id = rl.auth_profile_id
                ORDER BY rl.id DESC
                LIMIT ?1
                ",
            )?;
            let rows = stmt.query_map(params![limit as i64], map_request_log)?;
            rows.collect::<rusqlite::Result<Vec<_>>>().map_err(Into::into)
        })
    }

    pub fn admin_status(&self, host: String, port: u16) -> Result<AdminStatus> {
        let project_count = self.list_projects()?.len();
        let request_count = self.recent_requests(1000)?.len();
        let provider_count = self.list_providers()?.len();
        Ok(AdminStatus {
            service: "agent-llm",
            version: env!("CARGO_PKG_VERSION"),
            host,
            port,
            project_count,
            request_count,
            provider_count,
        })
    }
}

fn map_provider(row: &Row<'_>) -> rusqlite::Result<ProviderRecord> {
    Ok(ProviderRecord {
        provider: row.get(0)?,
        display_name: row.get(1)?,
        upstream_base_url: row.get(2)?,
        local_base_url: row.get(3)?,
        models_path: row.get(4)?,
    })
}

fn map_auth_profile(row: &Row<'_>) -> rusqlite::Result<AuthProfile> {
    let provider = row.get::<_, String>(1)?;
    let auth_mode_raw = row.get::<_, String>(3)?;
    let secret_ref_raw = row.get::<_, String>(4)?;
    let provider_kind = ProviderKind::parse(&provider).ok_or_else(|| {
        rusqlite::Error::InvalidColumnType(1, "provider".into(), rusqlite::types::Type::Text)
    })?;

    Ok(AuthProfile {
        id: row.get(0)?,
        provider,
        name: row.get(2)?,
        auth_mode: AuthMode::parse_for_provider(provider_kind, &auth_mode_raw).ok_or_else(
            || {
                rusqlite::Error::InvalidColumnType(
                    3,
                    "auth_mode".into(),
                    rusqlite::types::Type::Text,
                )
            },
        )?,
        secret_ref: SecretRef::parse(&secret_ref_raw).ok_or_else(|| {
            rusqlite::Error::InvalidColumnType(4, "secret_ref".into(), rusqlite::types::Type::Text)
        })?,
        is_default: row.get::<_, i64>(5)? == 1,
        metadata: serde_json::from_str::<Value>(&row.get::<_, String>(6)?).unwrap_or(Value::Null),
        created_at: parse_ts(row.get(7)?)?,
        updated_at: parse_ts(row.get(8)?)?,
    })
}

fn map_project(row: &Row<'_>) -> rusqlite::Result<ProjectRecord> {
    Ok(ProjectRecord {
        id: row.get(0)?,
        name: row.get(1)?,
        project_key: row.get(2)?,
        active: row.get::<_, i64>(3)? == 1,
        created_at: parse_ts(row.get(4)?)?,
        updated_at: parse_ts(row.get(5)?)?,
    })
}

fn map_project_provider_setting(row: &Row<'_>) -> rusqlite::Result<ProjectProviderSetting> {
    Ok(ProjectProviderSetting {
        project_id: row.get(0)?,
        provider: row.get(1)?,
        auth_profile_id: row.get(2)?,
        default_model: row.get(3)?,
        route_mode: row.get(4)?,
    })
}

fn map_model_cache_entry(row: &Row<'_>) -> rusqlite::Result<ModelCacheEntry> {
    Ok(ModelCacheEntry {
        id: row.get(0)?,
        provider: row.get(1)?,
        model_id: row.get(2)?,
        display_name: row.get(3)?,
        raw_json: serde_json::from_str::<Value>(&row.get::<_, String>(4)?).unwrap_or(Value::Null),
        refreshed_at: parse_ts(row.get(5)?)?,
    })
}

fn map_request_log(row: &Row<'_>) -> rusqlite::Result<RequestLog> {
    Ok(RequestLog {
        id: row.get(0)?,
        request_id: row.get(1)?,
        project_name: row.get(2)?,
        provider: row.get(3)?,
        auth_profile_name: row.get(4)?,
        method: row.get(5)?,
        path: row.get(6)?,
        status_code: row.get(7)?,
        latency_ms: row.get(8)?,
        prompt_tokens: row.get(9)?,
        completion_tokens: row.get(10)?,
        total_tokens: row.get(11)?,
        estimated_cost_usd: row.get(12)?,
        used_fallback: row.get::<_, i64>(13)? == 1,
        error_text: row.get(14)?,
        created_at: parse_ts(row.get(15)?)?,
    })
}

fn parse_ts(value: String) -> rusqlite::Result<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(&value)
        .map(|dt| dt.with_timezone(&Utc))
        .map_err(|error| {
            rusqlite::Error::FromSqlConversionFailure(
                0,
                rusqlite::types::Type::Text,
                Box::new(error),
            )
        })
}

fn generate_project_key() -> String {
    let mut bytes = [0u8; 24];
    rand::rng().fill_bytes(&mut bytes);
    format!("agllm_{}", URL_SAFE_NO_PAD.encode(bytes))
}

fn ensure_auth_profile_secret_ref_column(conn: &Connection) -> Result<()> {
    let mut stmt = conn.prepare("PRAGMA table_info(auth_profiles)")?;
    let columns = stmt.query_map([], |row| row.get::<_, String>(1))?;
    let column_names = columns.collect::<rusqlite::Result<Vec<_>>>()?;

    if !column_names.iter().any(|name| name == "secret_ref") {
        conn.execute(
            "ALTER TABLE auth_profiles ADD COLUMN secret_ref TEXT NOT NULL DEFAULT ''",
            [],
        )?;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::*;
    use crate::types::SecretBackend;

    fn test_db_path() -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock is valid")
            .as_nanos();
        std::env::temp_dir().join(format!("agent-llm-test-{nonce}.db"))
    }

    #[test]
    fn creates_project_and_provider_settings() {
        let path = test_db_path();
        let db = Database::open(&path).expect("db opens");
        let project = db.create_project("demo-project").expect("project created");

        let openai = db
            .project_provider_setting(project.id, ProviderKind::OpenAi)
            .expect("openai defaults exist");
        assert_eq!(openai.route_mode, "local");
        assert!(openai.default_model.is_none());

        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn resolves_default_auth_profile() {
        let path = test_db_path();
        let db = Database::open(&path).expect("db opens");
        let project = db.create_project("with-auth").expect("project created");
        db.add_auth_profile(
            ProviderKind::OpenAi,
            "default-api",
            AuthMode::ApiKey,
            &SecretRef::new(SecretBackend::File, "openai/default-api"),
            true,
            Value::Null,
        )
        .expect("auth added");

        let resolved = db
            .resolve_auth_profile(project.id, ProviderKind::OpenAi, None)
            .expect("resolved")
            .expect("profile exists");
        assert_eq!(resolved.name, "default-api");
        assert_eq!(
            resolved.secret_ref,
            SecretRef::new(SecretBackend::File, "openai/default-api")
        );

        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn rejects_invalid_provider_auth_modes() {
        let path = test_db_path();
        let db = Database::open(&path).expect("db opens");
        let error = db
            .add_auth_profile(
                ProviderKind::OpenRouter,
                "bad-session",
                AuthMode::OpenAiSession,
                &SecretRef::new(SecretBackend::File, "bad-session"),
                false,
                Value::Null,
            )
            .expect_err("should reject mismatched auth mode");
        assert!(error.to_string().contains("not valid for provider"));

        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn persists_secret_refs_instead_of_raw_secret_values() {
        let path = test_db_path();
        let db = Database::open(&path).expect("db opens");
        let secret_ref = SecretRef::new(SecretBackend::File, "anthropic/claude-console");

        let profile = db
            .add_auth_profile(
                ProviderKind::Anthropic,
                "claude-console",
                AuthMode::AnthropicSession,
                &secret_ref,
                true,
                Value::Null,
            )
            .expect("profile stored");

        assert_eq!(profile.secret_ref, secret_ref);

        db.with_conn(|conn| {
            let persisted_secret_ref = conn.query_row(
                "SELECT secret_ref FROM auth_profiles WHERE provider = 'anthropic' AND name = 'claude-console'",
                [],
                |row| row.get::<_, String>(0),
            )?;
            let raw_secret = conn.query_row(
                "SELECT COALESCE(secret, '') FROM auth_profiles WHERE provider = 'anthropic' AND name = 'claude-console'",
                [],
                |row| row.get::<_, String>(0),
            )?;
            assert_eq!(persisted_secret_ref, "file:anthropic/claude-console");
            assert!(raw_secret.is_empty());
            Ok(())
        })
        .expect("secret ref persisted");

        let _ = std::fs::remove_file(path);
    }
}
