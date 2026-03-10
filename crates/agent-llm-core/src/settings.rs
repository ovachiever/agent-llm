use std::path::PathBuf;

use anyhow::{Context, Result};

pub const DEFAULT_HOST: &str = "127.0.0.1";
pub const DEFAULT_PORT: u16 = 8787;

pub fn default_data_dir() -> Result<PathBuf> {
    if let Ok(value) = std::env::var("AGENT_LLM_HOME") {
        return Ok(PathBuf::from(value));
    }

    let home = dirs::home_dir().context("failed to resolve the current user's home directory")?;
    Ok(home.join(".agent-llm"))
}

pub fn default_db_path() -> Result<PathBuf> {
    Ok(default_data_dir()?.join("agent-llm.db"))
}

pub fn default_admin_base_url() -> String {
    format!("http://{DEFAULT_HOST}:{DEFAULT_PORT}")
}
