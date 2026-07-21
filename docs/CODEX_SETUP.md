# Using Codex CLI through agent-llm

The gateway serves an OpenAI Responses API endpoint at `/v1/responses` and
translates requests to each provider's native dialect. Codex therefore gets any
configured upstream — Kimi K3, LM Studio local models, Anthropic, OpenRouter —
from one base URL, with request logging and cost estimates landing in the usual
`agent-llm` request log.

## How routing works

The `model` field carries a provider prefix:

| model | upstream | dialect on the wire |
|---|---|---|
| `kimi/k3` | api.kimi.com/coding | Anthropic Messages |
| `lmstudio/openai/gpt-oss-20b` | 127.0.0.1:1234 | Chat Completions |
| `anthropic/claude-sonnet-5` | api.anthropic.com | Anthropic Messages |
| `openrouter/minimax/minimax-m2.7` | openrouter.ai | Chat Completions |
| `openai/gpt-5.5` | api.openai.com | Chat Completions |

Everything after the first slash is the upstream model id, so ids that contain
slashes (LM Studio, OpenRouter) work unchanged. `google/...` is not yet
supported here; use the `/google/*` passthrough.

Streaming, tool calls, images, and `reasoning.effort` all translate. Reasoning
effort maps to `reasoning_effort` on Chat Completions and to a
`thinking.budget_tokens` tier on Anthropic Messages.

## Codex config

Add to `~/.codex/config.toml`:

```toml
[model_providers.agent-llm]
name = "agent-llm gateway"
base_url = "http://127.0.0.1:8787/v1"
env_key = "AGENT_LLM_PROJECT_KEY"
wire_api = "responses"

[profiles.k3]
model_provider = "agent-llm"
model = "kimi/k3"
model_reasoning_effort = "high"

[profiles.local-oss]
model_provider = "agent-llm"
model = "lmstudio/openai/gpt-oss-20b"
```

Then export the project key (from `agent-llm project link`) and run:

```bash
export AGENT_LLM_PROJECT_KEY=agllm_...
codex --profile k3 "explain this repo"
codex --profile local-oss "write a commit message for the staged diff"
```

## Provider prerequisites

- **kimi**: add a Kimi Code Console API key once:
  `agent-llm auth add --provider kimi --name kimi-code --auth-mode api_key --secret-env KIMI_CODE_KEY --default`
- **lmstudio**: nothing — a default no-auth profile ships preconfigured. Start
  the server with `lms server start` (or the LM Studio UI). `GET /v1/models`
  live-refreshes the local model list whenever the server is reachable.
- **anthropic / openai / openrouter**: same auth profiles the passthrough
  routes use.

## Verifying

```bash
curl -s http://127.0.0.1:8787/v1/models \
  -H "Authorization: Bearer $AGENT_LLM_PROJECT_KEY" | jq '.data[].id'

curl -s http://127.0.0.1:8787/v1/responses \
  -H "Authorization: Bearer $AGENT_LLM_PROJECT_KEY" \
  -H "content-type: application/json" \
  -d '{"model":"lmstudio/openai/gpt-oss-20b","input":"Say hi in five words."}' | jq .
```

Request rows appear in `agent-llm status` / `/admin/requests` with token usage
even for streamed calls — the translator reads usage off the stream, which the
raw passthrough cannot do.
