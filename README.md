# agent-llm

`agent-llm` is a local-first LLM gateway for projects that need one stable endpoint while preserving provider-native APIs. It is both a passthrough (SDKs keep speaking their provider's dialect) and, where a client and upstream disagree, a translator: Codex's Responses API is converted to each provider's native protocol at `/v1/responses`.

It sits between your apps and upstream labs such as OpenAI, Anthropic, Google, and OpenRouter. It forwards requests without the model/parameter gatekeeping that wrappers like LiteLLM can introduce, while adding:

- provider-native passthrough routing
- project-scoped gateway keys
- local API-key and session-backed auth profiles
- macOS Keychain-backed secret storage
- request logging with latency, usage, and estimated cost
- a small CLI and Electron tray app for local operations

## Why this exists

If you have multiple apps, scripts, and agents talking to different model providers, the repetitive parts pile up quickly:

- provider auth wiring
- base URL switching between local/dev and direct production calls
- per-project defaults
- request logging
- cost visibility
- session-backed vs API-key billing choices

`agent-llm` centralizes that local control plane without forcing a custom SDK contract. Existing SDKs keep talking to provider-shaped endpoints; they just point at `agent-llm` instead of the lab directly during local development.

## Current capabilities

- OpenAI passthrough under `/openai/*`
- Anthropic passthrough under `/anthropic/*`
- Google passthrough under `/google/*`
- OpenRouter passthrough under `/openrouter/*`
- Kimi (Moonshot) passthrough under `/kimi/*` — Anthropic protocol, works as a
  Claude Code base URL
- LM Studio passthrough under `/lmstudio/*` — local server, no auth required
- **Responses API translation at `/v1/responses`** — Codex CLI (and any
  Responses-API client) can drive every provider above through one endpoint
  with `provider/model` ids; streaming, tool calls, and reasoning effort
  translate to each upstream's native dialect (see `docs/CODEX_SETUP.md`)
- `/v1/models` — aggregated `provider/model` catalog, with live LM Studio
  refresh
- admin API under `/admin/*`
- project linking and env generation
- auth profiles for:
  - `api_key`
  - `openai_session`
  - `anthropic_session`
  - `none` (local servers like LM Studio)
- macOS Keychain storage for new secrets by default
- SQLite for metadata, projects, logs, cached models, and cost records —
  including usage on streamed `/v1/responses` calls

## Repo layout

- `crates/agent-llm-core`: shared types, SQLite layer, pricing, secret storage
- `crates/agent-llm-gateway`: Axum gateway and admin API
- `crates/agent-llm-cli`: setup and operational CLI
- `apps/desktop`: Electron tray/admin shell
- `bin/`: stable wrapper scripts
- `docs/USE_FROM_ANOTHER_PROJECT.md`: handoff guide for using this repo from another project
- `docs/PROVIDER_SETUP_PLAYBOOK.md`: canonical provider and model setup guidance
- `docs/SDK_RECIPES.md`: copy-paste SDK examples through the gateway

## Preferred entrypoints

Use the wrappers instead of remembering `cargo run` commands:

```bash
./bin/agent-llm
./bin/agent-llm-gateway
./bin/agent-llm-up
./bin/agent-llm-down
./bin/agent-llm-status
```

## Quick start

### 1. Initialize local state

```bash
./bin/agent-llm init
```

### 2. Add an auth profile

OpenAI API key:

```bash
./bin/agent-llm auth add \
  --provider openai \
  --name default-api \
  --auth-mode api_key \
  --secret-env OPENAI_API_KEY \
  --default
```

Anthropic local session:

```bash
./bin/agent-llm auth add \
  --provider anthropic \
  --name claude-console \
  --auth-mode anthropic_session \
  --secret-env ANTHROPIC_SESSION_TOKEN \
  --default
```

OpenAI local session from stdin:

```bash
./bin/agent-llm auth add \
  --provider openai \
  --name codex-session \
  --auth-mode openai_session \
  --secret-stdin \
  --default
```

Provider-specific extra headers:

```bash
./bin/agent-llm auth add \
  --provider anthropic \
  --name claude-1m-session \
  --auth-mode anthropic_session \
  --secret-env ANTHROPIC_SESSION_TOKEN \
  --header anthropic-beta=context-1m-2025-08-07
```

List supported auth modes:

```bash
./bin/agent-llm auth modes
```

### 3. Verify the profile live

```bash
./bin/agent-llm auth verify --provider anthropic --profile claude-console
```

This calls the provider's model-list endpoint using the stored secret/session material from the local secret store. Use it before routing a real project through the gateway.

### 4. Link a project

```bash
./bin/agent-llm project link --name my-project
```

This creates a gateway project, prints its `agllm_...` project key, and writes `.env.agent-llm` in the current directory.

An example generated env file lives at [`.env.agent-llm.example`](./.env.agent-llm.example).

### 5. Start the gateway

Foreground:

```bash
./bin/agent-llm-gateway
```

Background:

```bash
./bin/agent-llm-up
./bin/agent-llm-status
```

The gateway listens on `http://127.0.0.1:8787` by default.

### 6. Point SDKs at the gateway

In local mode, keep using normal provider SDKs and load `.env.agent-llm`.

Generated env values point clients at:

- `http://127.0.0.1:8787/openai/v1`
- `http://127.0.0.1:8787/anthropic/v1`
- `http://127.0.0.1:8787/google/v1beta`
- `http://127.0.0.1:8787/openrouter/v1`

The gateway-issued project key becomes the local API key for those SDKs.

### 7. Switch back to direct provider mode

```bash
./bin/agent-llm mode use-direct --project my-project
```

That rewrites `.env.agent-llm` with direct provider base URLs and empty credential slots.

## Desktop app

The Electron tray shell provides a lightweight local operator UI for:

- gateway status
- auth-profile creation
- provider summaries
- project visibility
- recent request visibility

Install and run:

```bash
cd apps/desktop
npm install
npm start
```

By default it polls `http://127.0.0.1:8787/admin`.

Build a local macOS app bundle:

```bash
cd apps/desktop
npm run pack:mac
open release/mac-arm64/agent-llm.app
```

This produces an unsigned local `.app` bundle. Packaging works fully offline because the build is configured to reuse the Electron runtime already installed in `node_modules`.

## Admin API

- `GET /health`
- `GET /admin/status`
- `GET /admin/providers`
- `GET /admin/projects`
- `POST /admin/projects`
- `GET /admin/requests?limit=50`
- `POST /admin/auth-profiles`
- `POST /admin/providers/{provider}/models/refresh`

`POST /admin/auth-profiles` accepts either:

- `secret`: raw token/session material to store in the local secret store
- `secret_ref`: an existing reference such as `keychain:openai/codex`

## Use from another project

Start with [`docs/USE_FROM_ANOTHER_PROJECT.md`](./docs/USE_FROM_ANOTHER_PROJECT.md).

For model-specific request setup, defaults, and provider gotchas, read [`docs/PROVIDER_SETUP_PLAYBOOK.md`](./docs/PROVIDER_SETUP_PLAYBOOK.md) before wiring a new lab or model family.

For copy-paste starting examples, use [`docs/SDK_RECIPES.md`](./docs/SDK_RECIPES.md).

If another agent should adopt this repo as a local dependency, the short instruction is:

> Use `../agent-llm` as the local LLM gateway for development. Do not add direct provider auth logic if the existing SDK can already target a base URL. Before changing model params or payloads, read `../agent-llm/docs/PROVIDER_SETUP_PLAYBOOK.md`. Load `.env.agent-llm` for local/dev, keep direct provider env support for production, and make the integration switchable through environment variables.

## Security and storage

- On macOS, new secrets are stored in Keychain by default.
- SQLite stores `secret_ref` metadata for new auth profiles instead of raw secret values.
- On non-macOS systems and in tests, secret storage falls back to a file-backed backend under the local data directory.

## Limitations

- Streaming responses are proxied through, but usage extraction currently only works for non-stream JSON responses.
- The gateway is intentionally a passthrough, so it does not try to normalize every provider into one house schema.
- The Electron app installs cleanly here, but launching the GUI itself was not verified inside this headless terminal environment.

## License

[MIT](./LICENSE)
