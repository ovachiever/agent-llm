# Gateway API Reference

The gateway listens on `http://127.0.0.1:8787` (configurable via
`AGENT_LLM_HOST` / `AGENT_LLM_PORT`). Three kinds of surface: provider
passthroughs, translation endpoints, and the admin API.

## Authentication

Every non-admin request needs a project key (`agllm_...`, from
`agent-llm project link`). The gateway accepts it in any of the places
provider SDKs naturally put credentials:

- `Authorization: Bearer <key>`
- `x-api-key: <key>` (Anthropic SDKs)
- `x-goog-api-key: <key>` (Google SDKs)
- `x-agent-llm-project-key: <key>`

Optional per-request headers:

- `x-agent-llm-auth-profile: <name>`: use a specific auth profile instead of
  the project default.
- `x-agent-llm-fallback-auth-profile: <name>` (passthrough only): retried on
  401/403.

Responses carry `x-agent-llm-request-id` for correlating with the request log.

## Passthrough routes

`ANY /{provider}/{path...}` forwards the request to the provider's upstream
with real credentials applied, body untouched. The provider namespaces:

| route prefix | upstream |
|---|---|
| `/openai/*` | https://api.openai.com |
| `/anthropic/*` | https://api.anthropic.com |
| `/google/*` | https://generativelanguage.googleapis.com |
| `/openrouter/*` | https://openrouter.ai/api |
| `/kimi/*` | https://api.moonshot.ai/anthropic |
| `/lmstudio/*` | http://127.0.0.1:1234 |

Example: `POST /anthropic/v1/messages` behaves exactly like the Anthropic API
with your stored key. `anthropic-version` is added when absent on Anthropic
and Kimi routes.

## Translation surfaces

Both accept `model` ids of the form `provider/model` (everything after the
first slash is the upstream model id). Streaming is fully translated, and
streamed requests log token usage and estimated cost.

### `POST /v1/responses` (OpenAI Responses API in)

For Codex and other Responses-API clients. Requests translate to the
upstream's native dialect:

- Chat Completions upstreams: `openai`, `openrouter`, `lmstudio`
- Anthropic Messages upstreams: `anthropic`, `kimi`

Translated: instructions/input items, multi-part content, images, function
tools and tool outputs, `tool_choice`, `reasoning.effort` (to
`reasoning_effort` or a `thinking` budget), `max_output_tokens`,
`text.format` JSON schema (Chat Completions targets). `previous_response_id`
is rejected; resend the full conversation. Upstream reasoning comes back as
`reasoning` output items and reasoning-summary stream events.

### `POST /v1/messages` (Anthropic Messages API in)

For Claude Code and Anthropic SDKs. Anthropic-dialect upstreams pass through
with the model id rewritten; Chat-Completions upstreams translate both ways.
Details and session recipes: [`CLAUDE_CODE_SETUP.md`](./CLAUDE_CODE_SETUP.md).

Special behavior:

- Unprefixed `claude-*` model ids route to the `anthropic` passthrough (a
  `[1m]` alias suffix is stripped). This keeps sessions alive when global
  client settings leak Anthropic model pins into a routed session.
- Upstream reasoning text becomes `thinking` blocks (unsigned); thinking
  blocks in requests are dropped before reaching foreign upstreams.

### `POST /v1/messages/count_tokens`

Returns `{"input_tokens": <estimate>}` from serialized request length
(chars/4). Exists so Claude Code's context tracking functions against
upstreams with no counting API. It is an estimate, not a bill.

### `GET /v1/models`

Aggregated catalog as `provider/model` ids from the per-provider model cache.
The LM Studio list live-refreshes whenever the local server is reachable.

## Admin API

Unauthenticated, loopback-only, JSON:

- `GET /health`
- `GET /admin/status`: service, version, counts
- `GET /admin/providers`: provider records + auth profiles + cached models
- `GET|POST /admin/projects`
- `GET /admin/requests?limit=50` (max 500)
- `GET /admin/auth-methods`: catalog the UIs render
- `POST /admin/auth-profiles`: `{provider, name, auth_mode, secret |
  secret_ref, is_default, metadata}`
- `POST /admin/auth/start`, `GET /admin/auth/attempts/{id}`,
  `POST /admin/auth/attempts/{id}/complete`: browser-based account sign-in
  flows (ChatGPT, Claude, Google OAuth)
- `GET /admin/oauth/{openai|google}/callback`: OAuth redirect targets
- `POST /admin/providers/{provider}/models/refresh`: refresh the model cache
  using the provider's default auth profile

## Request log semantics

Each row: request id, project, provider, auth profile, method, path, status,
latency, prompt/completion/total tokens, estimated cost, fallback flag, error
text. Streamed passthrough rows have no token usage (bytes are relayed
untouched); streamed translation rows do, because every event passes through
the translator. Cost comes from `crates/agent-llm-core/src/pricing.rs`;
models without a rule log usage with a null cost.
