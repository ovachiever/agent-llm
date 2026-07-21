# agent-llm session wrappers for Claude Code.
# Opt in by adding to ~/.zshrc:
#   source ~/Custom-Coding/agent-llm/bin/session-wrappers.zsh

# Claude Code on Kimi K3 via the local gateway (OpenRouter upstream).
k3() {
  local key=$(sqlite3 ~/.agent-llm/agent-llm.db "SELECT project_key FROM projects LIMIT 1")
  ANTHROPIC_BASE_URL="http://127.0.0.1:8787" \
  ANTHROPIC_API_KEY="$key" \
  ANTHROPIC_MODEL="openrouter/moonshotai/kimi-k3" \
  ANTHROPIC_SMALL_FAST_MODEL="openrouter/moonshotai/kimi-k3" \
  CLAUDE_CODE_SUBAGENT_MODEL="openrouter/moonshotai/kimi-k3" \
  claude "$@"
}

# Claude Code on a local LM Studio model (default gpt-oss-20b; override with LMSTUDIO_MODEL).
claude-local() {
  local model="lmstudio/${LMSTUDIO_MODEL:-openai/gpt-oss-20b}"
  local key=$(sqlite3 ~/.agent-llm/agent-llm.db "SELECT project_key FROM projects LIMIT 1")
  ANTHROPIC_BASE_URL="http://127.0.0.1:8787" \
  ANTHROPIC_API_KEY="$key" \
  ANTHROPIC_MODEL="$model" \
  ANTHROPIC_SMALL_FAST_MODEL="$model" \
  CLAUDE_CODE_SUBAGENT_MODEL="$model" \
  claude "$@"
}
