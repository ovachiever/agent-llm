use std::{fs, path::PathBuf};

use agent_llm_core::{
    Database, LocalSecretStore, SecretStore,
    settings::default_db_path,
    types::{AuthMode, ProviderKind, SecretRef},
};
use anyhow::{Context, Result, anyhow};
use clap::{Args, Parser, Subcommand};
use reqwest::blocking::Client;
use serde_json::{Map, Value};

#[derive(Parser, Debug)]
#[command(name = "agent-llm")]
#[command(about = "Manage local projects and auth for the agent-llm gateway")]
struct Cli {
    #[arg(long, env = "AGENT_LLM_DB")]
    db_path: Option<PathBuf>,
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand, Debug)]
enum Commands {
    Init,
    Status,
    #[command(subcommand)]
    Auth(AuthCommand),
    #[command(subcommand)]
    Project(ProjectCommand),
    #[command(subcommand)]
    Mode(ModeCommand),
}

#[derive(Subcommand, Debug)]
enum AuthCommand {
    Add(AuthAddArgs),
    Modes,
    Verify(AuthVerifyArgs),
}

#[derive(Args, Debug)]
#[command(
    long_about = "Create or update an auth profile for a provider. Supported auth modes are \
api_key for direct provider billing, openai_session for local OpenAI session-backed billing, \
and anthropic_session for local Anthropic session-backed billing. Secrets can be passed \
directly, read from an environment variable, or read from stdin to avoid shell history."
)]
struct AuthAddArgs {
    #[arg(long)]
    provider: String,
    #[arg(long)]
    name: String,
    #[arg(long, help = "One of: api_key, openai_session, anthropic_session")]
    auth_mode: String,
    #[arg(
        long,
        help = "Pass secret material directly. Avoid this if shell history matters."
    )]
    secret: Option<String>,
    #[arg(long, help = "Read secret material from this environment variable.")]
    secret_env: Option<String>,
    #[arg(
        long,
        default_value_t = false,
        help = "Read secret material from stdin."
    )]
    secret_stdin: bool,
    #[arg(long, default_value_t = false)]
    default: bool,
    #[arg(long, help = "Raw metadata JSON merged into the profile metadata.")]
    metadata_json: Option<String>,
    #[arg(
        long = "header",
        value_name = "NAME=VALUE",
        help = "Add provider-specific extra headers into metadata.headers."
    )]
    headers: Vec<String>,
}

#[derive(Args, Debug)]
#[command(
    long_about = "Verify a stored auth profile by calling the provider's model-list endpoint with the current local secret-store-backed credential."
)]
struct AuthVerifyArgs {
    #[arg(long)]
    provider: String,
    #[arg(long)]
    profile: String,
}

#[derive(Subcommand, Debug)]
enum ProjectCommand {
    Link(ProjectLinkArgs),
    SetDefaults(ProjectSetDefaultsArgs),
}

#[derive(Args, Debug)]
struct ProjectLinkArgs {
    #[arg(long)]
    name: String,
    #[arg(long)]
    env_file: Option<PathBuf>,
}

#[derive(Args, Debug)]
struct ProjectSetDefaultsArgs {
    #[arg(long)]
    project: String,
    #[arg(long)]
    provider: String,
    #[arg(long)]
    auth_profile: Option<String>,
    #[arg(long)]
    default_model: Option<String>,
    #[arg(long)]
    route_mode: Option<String>,
}

#[derive(Subcommand, Debug)]
enum ModeCommand {
    UseLocal(ModeArgs),
    UseDirect(ModeArgs),
}

#[derive(Args, Debug)]
struct ModeArgs {
    #[arg(long)]
    project: String,
    #[arg(long)]
    env_file: Option<PathBuf>,
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Commands::Init => {
            let db = open_db(cli.db_path.clone())?;
            println!("Initialized agent-llm database at {}", db.path.display());
        }
        Commands::Status => {
            let db = open_db(cli.db_path.clone())?;
            print_status(&db)?;
        }
        Commands::Auth(AuthCommand::Add(args)) => {
            let db = open_db(cli.db_path.clone())?;
            add_auth_profile(&db, args)?;
        }
        Commands::Auth(AuthCommand::Modes) => {
            print_auth_modes();
        }
        Commands::Auth(AuthCommand::Verify(args)) => {
            let db = open_db(cli.db_path.clone())?;
            verify_auth_profile(&db, args)?;
        }
        Commands::Project(ProjectCommand::Link(args)) => {
            let db = open_db(cli.db_path.clone())?;
            link_project(&db, args)?;
        }
        Commands::Project(ProjectCommand::SetDefaults(args)) => {
            let db = open_db(cli.db_path.clone())?;
            set_defaults(&db, args)?;
        }
        Commands::Mode(ModeCommand::UseLocal(args)) => {
            let db = open_db(cli.db_path.clone())?;
            write_mode_env(&db, args, true)?;
        }
        Commands::Mode(ModeCommand::UseDirect(args)) => {
            let db = open_db(cli.db_path.clone())?;
            write_mode_env(&db, args, false)?;
        }
    }

    Ok(())
}

fn open_db(db_path: Option<PathBuf>) -> Result<Database> {
    Database::open(db_path.unwrap_or(default_db_path()?))
}

fn print_status(db: &Database) -> Result<()> {
    let providers = db.list_provider_summaries()?;
    let projects = db.list_projects()?;
    let requests = db.recent_requests(10)?;

    println!("Database: {}", db.path.display());
    println!("Providers: {}", providers.len());
    println!("Projects: {}", projects.len());
    println!("Recent requests: {}", requests.len());
    println!();
    println!("Provider auth profiles:");
    for summary in providers {
        let names = summary
            .auth_profiles
            .iter()
            .map(|profile| {
                if profile.is_default {
                    format!("{} ({}) [default]", profile.name, profile.auth_mode.label())
                } else {
                    format!("{} ({})", profile.name, profile.auth_mode.label())
                }
            })
            .collect::<Vec<_>>();
        println!("  {}: {}", summary.provider.provider, names.join(", "));
    }

    Ok(())
}

fn add_auth_profile(db: &Database, args: AuthAddArgs) -> Result<()> {
    let provider = parse_provider(&args.provider)?;
    let auth_mode = parse_auth_mode(provider, &args.auth_mode)?;
    let metadata = merge_metadata(
        args.metadata_json
            .as_deref()
            .map(serde_json::from_str)
            .transpose()
            .context("invalid --metadata-json payload")?
            .unwrap_or(Value::Null),
        &args.headers,
    )?;
    let secret = read_secret_input(&args)?;
    let prepared_secret =
        prepare_secret_material_for_core(provider, auth_mode, &args.name, secret)?;
    let profile = db.add_auth_profile(
        provider,
        &args.name,
        auth_mode,
        &prepared_secret,
        args.default,
        metadata,
    )?;
    println!(
        "Configured auth profile `{}` for {} (mode: {})",
        profile.name,
        profile.provider,
        profile.auth_mode.label()
    );
    println!(
        "Secret input captured from {}.",
        describe_secret_source(&args)
    );
    Ok(())
}

fn link_project(db: &Database, args: ProjectLinkArgs) -> Result<()> {
    let project = db.create_project(&args.name)?;
    println!("Created project `{}`", project.name);
    println!("Gateway key: {}", project.project_key);

    let env_file = args
        .env_file
        .unwrap_or_else(|| PathBuf::from(".agent-llm.env"));
    write_env_file(&env_file, &build_local_env(db, &project.name)?)?;
    println!("Wrote local gateway env template to {}", env_file.display());
    Ok(())
}

fn set_defaults(db: &Database, args: ProjectSetDefaultsArgs) -> Result<()> {
    let provider = parse_provider(&args.provider)?;
    let route_mode = args.route_mode.as_deref();
    if let Some(route_mode) = route_mode {
        if !matches!(route_mode, "local" | "direct") {
            return Err(anyhow!("route mode must be `local` or `direct`"));
        }
    }

    let setting = db.set_project_provider_defaults(
        &args.project,
        provider,
        args.auth_profile.as_deref(),
        args.default_model.as_deref(),
        route_mode,
    )?;
    println!(
        "Updated {} defaults for project {}",
        setting.provider, args.project
    );
    Ok(())
}

fn write_mode_env(db: &Database, args: ModeArgs, use_local: bool) -> Result<()> {
    let env_file = args
        .env_file
        .unwrap_or_else(|| PathBuf::from(".agent-llm.env"));
    let content = if use_local {
        build_local_env(db, &args.project)?
    } else {
        build_direct_env(db, &args.project)?
    };
    write_env_file(&env_file, &content)?;
    if use_local {
        println!("Wrote local gateway env to {}", env_file.display());
    } else {
        println!(
            "Wrote direct provider env template to {}",
            env_file.display()
        );
    }
    Ok(())
}

fn build_local_env(db: &Database, project_name: &str) -> Result<String> {
    let project = db.project_by_name(project_name)?;
    let openai = db.provider(ProviderKind::OpenAi)?;
    let anthropic = db.provider(ProviderKind::Anthropic)?;
    let google = db.provider(ProviderKind::Google)?;
    let openrouter = db.provider(ProviderKind::OpenRouter)?;

    Ok(format!(
        "# Generated by agent-llm\n\
AGENT_LLM_PROJECT={project}\n\
AGENT_LLM_PROJECT_KEY={key}\n\
\n\
# OpenAI-compatible SDKs\n\
OPENAI_BASE_URL={openai_url}\n\
OPENAI_API_KEY={key}\n\
\n\
# Anthropic SDKs\n\
ANTHROPIC_BASE_URL={anthropic_url}\n\
ANTHROPIC_API_KEY={key}\n\
\n\
# Google AI Studio SDKs and raw HTTP\n\
GOOGLE_BASE_URL={google_url}\n\
GOOGLE_API_KEY={key}\n\
GOOGLE_GENERATIVE_AI_BASE_URL={google_url}\n\
\n\
# OpenRouter SDKs\n\
OPENROUTER_BASE_URL={openrouter_url}\n\
OPENROUTER_API_KEY={key}\n\
",
        project = project.name,
        key = project.project_key,
        openai_url = openai.local_base_url,
        anthropic_url = anthropic.local_base_url,
        google_url = google.local_base_url,
        openrouter_url = openrouter.local_base_url,
    ))
}

fn build_direct_env(db: &Database, project_name: &str) -> Result<String> {
    let _project = db.project_by_name(project_name)?;
    let openai = db.provider(ProviderKind::OpenAi)?;
    let anthropic = db.provider(ProviderKind::Anthropic)?;
    let google = db.provider(ProviderKind::Google)?;
    let openrouter = db.provider(ProviderKind::OpenRouter)?;

    Ok(format!(
        "# Generated by agent-llm\n\
# Fill in your direct provider credentials before use.\n\
\n\
OPENAI_BASE_URL={openai_url}/v1\n\
OPENAI_API_KEY=\n\
\n\
ANTHROPIC_BASE_URL={anthropic_url}/v1\n\
ANTHROPIC_API_KEY=\n\
\n\
GOOGLE_BASE_URL={google_url}/v1beta\n\
GOOGLE_API_KEY=\n\
GOOGLE_GENERATIVE_AI_BASE_URL={google_url}/v1beta\n\
\n\
OPENROUTER_BASE_URL={openrouter_url}/v1\n\
OPENROUTER_API_KEY=\n\
",
        openai_url = openai.upstream_base_url,
        anthropic_url = anthropic.upstream_base_url,
        google_url = google.upstream_base_url,
        openrouter_url = openrouter.upstream_base_url,
    ))
}

fn write_env_file(path: &PathBuf, content: &str) -> Result<()> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent)
                .with_context(|| format!("failed to create {}", parent.display()))?;
        }
    }
    fs::write(path, content).with_context(|| format!("failed to write {}", path.display()))
}

fn parse_provider(value: &str) -> Result<ProviderKind> {
    ProviderKind::parse(value.trim()).ok_or_else(|| anyhow!("unknown provider `{value}`"))
}

fn parse_auth_mode(provider: ProviderKind, value: &str) -> Result<AuthMode> {
    AuthMode::parse_for_provider(provider, value.trim()).ok_or_else(|| {
        anyhow!(
            "unknown auth mode `{}` for provider `{}`",
            value,
            provider.as_str()
        )
    })
}

fn print_auth_modes() {
    println!("Supported auth modes:");
    println!("  api_key            Direct lab billing with a provider-issued API key");
    println!("  openai_session     Local OpenAI session-backed billing");
    println!("  anthropic_session  Local Anthropic session-backed billing");
    println!();
    println!("Examples:");
    println!(
        "  agent-llm auth add --provider openai --name default-api --auth-mode api_key --secret-env OPENAI_API_KEY --default"
    );
    println!(
        "  agent-llm auth add --provider openai --name codex --auth-mode openai_session --secret-stdin"
    );
    println!(
        "  agent-llm auth add --provider anthropic --name claude-console --auth-mode anthropic_session --secret-env ANTHROPIC_SESSION_TOKEN --header anthropic-beta=context-1m-2025-08-07"
    );
    println!(
        "  agent-llm auth verify --provider openai --profile codex"
    );
}

fn merge_metadata(metadata: Value, headers: &[String]) -> Result<Value> {
    if headers.is_empty() {
        return Ok(metadata);
    }

    let mut root = match metadata {
        Value::Null => Map::new(),
        Value::Object(map) => map,
        _ => {
            return Err(anyhow!(
                "metadata JSON must be an object when --header is used"
            ));
        }
    };

    let mut metadata_headers = match root.remove("headers") {
        Some(Value::Object(map)) => map,
        Some(_) => return Err(anyhow!("metadata.headers must be an object when present")),
        None => Map::new(),
    };

    for header in headers {
        let (name, value) = header
            .split_once('=')
            .ok_or_else(|| anyhow!("header must be in NAME=VALUE format"))?;
        let name = name.trim();
        let value = value.trim();
        if name.is_empty() || value.is_empty() {
            return Err(anyhow!("header must include both a name and a value"));
        }
        metadata_headers.insert(name.to_string(), Value::String(value.to_string()));
    }

    root.insert("headers".into(), Value::Object(metadata_headers));
    Ok(Value::Object(root))
}

fn read_secret_input(args: &AuthAddArgs) -> Result<String> {
    let mut sources = 0;
    if args.secret.is_some() {
        sources += 1;
    }
    if args.secret_env.is_some() {
        sources += 1;
    }
    if args.secret_stdin {
        sources += 1;
    }

    if sources != 1 {
        return Err(anyhow!(
            "choose exactly one secret input method: --secret, --secret-env, or --secret-stdin"
        ));
    }

    if let Some(secret) = &args.secret {
        let trimmed = secret.trim();
        if trimmed.is_empty() {
            return Err(anyhow!("secret cannot be empty"));
        }
        return Ok(trimmed.to_string());
    }

    if let Some(env_name) = &args.secret_env {
        let value = std::env::var(env_name)
            .with_context(|| format!("environment variable `{env_name}` is not set"))?;
        let trimmed = value.trim();
        if trimmed.is_empty() {
            return Err(anyhow!("secret environment variable `{env_name}` is empty"));
        }
        return Ok(trimmed.to_string());
    }

    let mut buffer = String::new();
    std::io::stdin()
        .read_line(&mut buffer)
        .context("failed to read secret from stdin")?;
    let trimmed = buffer.trim();
    if trimmed.is_empty() {
        return Err(anyhow!("stdin secret cannot be empty"));
    }
    Ok(trimmed.to_string())
}

fn describe_secret_source(args: &AuthAddArgs) -> &'static str {
    if args.secret.is_some() {
        "--secret"
    } else if args.secret_env.is_some() {
        "--secret-env"
    } else {
        "--secret-stdin"
    }
}

fn prepare_secret_material_for_core(
    provider: ProviderKind,
    _auth_mode: AuthMode,
    profile_name: &str,
    secret: String,
) -> Result<SecretRef> {
    let store = LocalSecretStore::detect()?;
    store.store_auth_profile_secret(provider, profile_name, &secret)
}

fn verify_auth_profile(db: &Database, args: AuthVerifyArgs) -> Result<()> {
    let provider = parse_provider(&args.provider)?;
    let provider_record = db.provider(provider)?;
    let profile = db.auth_profile_by_name(provider, &args.profile)?;
    let secrets = LocalSecretStore::detect()?;
    let secret = secrets.read_secret(&profile.secret_ref)?;
    let url = format!(
        "{}{}",
        provider_record.upstream_base_url, provider_record.models_path
    );

    let client = Client::builder()
        .build()
        .context("failed to construct verification HTTP client")?;
    let request = build_probe_request(client.get(url), provider, profile.auth_mode, &secret, &profile.metadata);
    let response = request.send().context("provider verification request failed")?;
    let status = response.status();
    let body = response
        .json::<Value>()
        .unwrap_or_else(|_| Value::Null);

    if !status.is_success() {
        return Err(anyhow!(
            "verification failed for `{}` on {} with status {}: {}",
            profile.name,
            provider.as_str(),
            status,
            body
        ));
    }

    let preview = collect_models_preview(provider, &body);
    println!(
        "Verified `{}` for {} via {}",
        profile.name,
        provider.as_str(),
        provider_record.models_path
    );
    if preview.is_empty() {
        println!("Provider returned success but no model preview was parsed.");
    } else {
        println!("Models: {}", preview.join(", "));
    }
    Ok(())
}

fn build_probe_request(
    mut request: reqwest::blocking::RequestBuilder,
    provider: ProviderKind,
    auth_mode: AuthMode,
    secret: &str,
    metadata: &Value,
) -> reqwest::blocking::RequestBuilder {
    match (provider, auth_mode) {
        (ProviderKind::OpenAi, AuthMode::ApiKey | AuthMode::OpenAiSession)
        | (ProviderKind::OpenRouter, AuthMode::ApiKey) => {
            request = request.header("authorization", format!("Bearer {secret}"));
        }
        (ProviderKind::Anthropic, AuthMode::ApiKey) => {
            request = request.header("x-api-key", secret);
        }
        (ProviderKind::Anthropic, AuthMode::AnthropicSession) => {
            request = request.header("authorization", format!("Bearer {secret}"));
        }
        (ProviderKind::Google, AuthMode::ApiKey) => {
            request = request.header("x-goog-api-key", secret);
        }
        _ => {}
    }

    if provider == ProviderKind::Anthropic {
        request = request.header("anthropic-version", "2023-06-01");
    }

    if let Some(headers) = metadata.get("headers").and_then(Value::as_object) {
        for (name, value) in headers {
            if let Some(value) = value.as_str() {
                request = request.header(name, value);
            }
        }
    }

    request
}

fn collect_models_preview(provider: ProviderKind, body: &Value) -> Vec<String> {
    let items = match provider {
        ProviderKind::OpenAi | ProviderKind::Anthropic | ProviderKind::OpenRouter => {
            body.get("data").and_then(Value::as_array)
        }
        ProviderKind::Google => body.get("models").and_then(Value::as_array),
    };

    items
        .into_iter()
        .flatten()
        .filter_map(|item| {
            item.get("id")
                .and_then(Value::as_str)
                .or_else(|| item.get("name").and_then(Value::as_str))
                .map(ToOwned::to_owned)
        })
        .take(5)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn merges_headers_into_metadata() {
        let merged = merge_metadata(
            serde_json::json!({
                "headers": {
                    "existing": "value"
                }
            }),
            &[String::from("anthropic-beta=context-1m-2025-08-07")],
        )
        .expect("metadata merges");

        assert_eq!(
            merged
                .get("headers")
                .and_then(Value::as_object)
                .and_then(|headers| headers.get("existing"))
                .and_then(Value::as_str),
            Some("value")
        );
        assert_eq!(
            merged
                .get("headers")
                .and_then(Value::as_object)
                .and_then(|headers| headers.get("anthropic-beta"))
                .and_then(Value::as_str),
            Some("context-1m-2025-08-07")
        );
    }

    #[test]
    fn rejects_multiple_secret_sources() {
        let args = AuthAddArgs {
            provider: "openai".into(),
            name: "test".into(),
            auth_mode: "openai_session".into(),
            secret: Some("one".into()),
            secret_env: Some("OPENAI_SESSION_TOKEN".into()),
            secret_stdin: false,
            default: false,
            metadata_json: None,
            headers: vec![],
        };

        let error = read_secret_input(&args).expect_err("should reject");
        assert!(error.to_string().contains("choose exactly one"));
    }

    #[test]
    fn collects_model_previews_from_openai_shape() {
        let preview = collect_models_preview(
            ProviderKind::OpenAi,
            &serde_json::json!({
                "data": [
                    { "id": "gpt-4.1" },
                    { "id": "gpt-4o" }
                ]
            }),
        );

        assert_eq!(preview, vec!["gpt-4.1".to_string(), "gpt-4o".to_string()]);
    }
}
