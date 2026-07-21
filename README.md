# agent-llm

A local-first LLM gateway that speaks every dialect your tools do.

`agent-llm` sits between the coding agents on your machine (Claude Code, Codex,
plain SDKs) and the model providers behind them (Anthropic, OpenAI, Google,
OpenRouter, Moonshot/Kimi, a local LM Studio server). Everything flows through
one stable endpoint on `127.0.0.1:8787`, with API keys in the macOS Keychain,
every request logged with tokens and estimated cost, and a native menu bar app
showing the state of your routes at a glance.

It is two things at once:

- **A passthrough.** SDKs keep speaking their provider's native protocol; only
  the base URL changes. No house schema, no parameter gatekeeping.
- **A translator.** Where a client and an upstream disagree on protocol, the
  gateway converts between them, streaming included. Codex (Responses API) can
  drive Anthropic-protocol models; Claude Code (Anthropic Messages) can drive
  OpenAI-protocol models. Any harness, any provider, one URL.

The practical result: run Claude Code on Kimi K3 or on a free local model, run
Codex against anything on OpenRouter, keep your normal provider setups
untouched, and see all of it in one request log.

## The translation triangle

```
   Codex, Responses-API clients        Claude Code, Anthropic SDKs
              |                                   |
        POST /v1/responses                 POST /v1/messages
              \                                   /
               +---------- agent-llm gateway ----+
               |     model = "provider/model"    |
               |  auth, logging, cost, caching   |
               +---+----------+----------+------+
                   |          |          |
          Chat Completions  Anthropic   (passthrough,
          dialect upstreams  Messages    per-provider
          openai, openrouter dialect     routes under
          lmstudio           anthropic,  /{provider}/*)
                             kimi
```

Model ids carry a provider prefix, and everything after the first slash is the
upstream model id, so ids containing slashes work unchanged:

| model id | upstream | wire dialect |
|---|---|---|
| `openrouter/moonshotai/kimi-k3` | openrouter.ai | Chat Completions |
| `lmstudio/openai/gpt-oss-20b` | 127.0.0.1:1234 | Chat Completions |
| `anthropic/claude-sonnet-5` | api.anthropic.com | Anthropic Messages |
| `kimi/kimi-k2.6` | api.moonshot.ai/anthropic | Anthropic Messages |
| `openai/gpt-5.5` | api.openai.com | Chat Completions |

Google models are currently passthrough-only (`/google/*`).

## Quick start

```bash
# 1. Initialize local state (SQLite in ~/.agent-llm, secrets in Keychain)
./bin/agent-llm init

# 2. Add provider keys (LM Studio needs none; a default profile ships ready)
./bin/agent-llm auth add --provider openrouter --name default-api \
  --auth-mode api_key --secret-env OPENROUTER_API_KEY --default

# 3. Create a project and its gateway key
./bin/agent-llm project link --name my-project

# 4. Start the gateway
./bin/agent-llm-up
```

Then point things at it:

- **Claude Code** on K3 or local models: [`docs/CLAUDE_CODE_SETUP.md`](./docs/CLAUDE_CODE_SETUP.md),
  or source [`bin/session-wrappers.zsh`](./bin/session-wrappers.zsh) and run `k3`.
- **Codex**: [`docs/CODEX_SETUP.md`](./docs/CODEX_SETUP.md).
- **Provider SDKs** (passthrough): [`docs/USE_FROM_ANOTHER_PROJECT.md`](./docs/USE_FROM_ANOTHER_PROJECT.md)
  and [`docs/SDK_RECIPES.md`](./docs/SDK_RECIPES.md).
- **Everything else**: the full endpoint reference is [`docs/GATEWAY_API.md`](./docs/GATEWAY_API.md).

## The menu bar app

A native SwiftUI app (`apps/macos`) answers the three questions you actually
have: is my routing healthy, what is wired to what, and what flowed through
today. A status lamp and verdict line in the popover, a per-provider
switchboard showing the auth actually in use, and a monospace traffic log with
tokens and cost, errors in the only red on screen. Key entry (API key or
browser account connect) happens in a per-provider sheet.

```bash
./apps/macos/build-macos-app.sh
open apps/macos/release/agent-llm.app
```

See [`apps/macos/README.md`](./apps/macos/README.md) for the design brief and
the screenshot-verification harness. An older Electron shell lives in
`apps/desktop`; the Swift app supersedes it.

## What gets logged

Every request row records provider, path, status, latency, token usage, and
estimated cost. Because the translation surfaces parse every stream event,
streamed requests get full usage and cost too, something a raw passthrough
cannot see. Ask the gateway (`GET /admin/requests`), the CLI
(`./bin/agent-llm status`), or the menu bar app.

## Providers

| provider | upstream | auth | notes |
|---|---|---|---|
| `anthropic` | api.anthropic.com | API key or Claude session | |
| `openai` | api.openai.com | API key or ChatGPT session | |
| `google` | generativelanguage.googleapis.com | API key or OAuth | passthrough only |
| `openrouter` | openrouter.ai | API key | K3 lives here; account provider policy must permit Moonshot AI |
| `kimi` | api.moonshot.ai/anthropic | API key | pay-per-token platform; K2 models, no K3 on this tier |
| `lmstudio` | 127.0.0.1:1234 | none | local server; model list live-refreshes |

Model-specific request guidance (thinking budgets, reasoning effort, token
ceilings) is canonical in
[`docs/PROVIDER_SETUP_PLAYBOOK.md`](./docs/PROVIDER_SETUP_PLAYBOOK.md).

## Repo layout

- `crates/agent-llm-gateway`: Axum gateway, passthrough routes, translation
  surfaces, admin API, browser-based auth flows
- `crates/agent-llm-translate`: pure sans-IO protocol translation (request and
  response mapping, SSE stream state machines, 78 unit tests)
- `crates/agent-llm-core`: types, SQLite layer, pricing, Keychain-backed
  secret storage
- `crates/agent-llm-cli`: setup and operational CLI
- `apps/macos`: native menu bar app
- `apps/desktop`: legacy Electron shell
- `bin/`: stable wrapper scripts (`agent-llm`, `agent-llm-up`, `agent-llm-down`,
  `agent-llm-status`, `session-wrappers.zsh`)

## Security and storage

- New secrets go to the macOS Keychain by default; SQLite stores references,
  never raw values. Non-macOS and test environments fall back to a file-backed
  store under the local data directory.
- Resolved credentials are cached in-process, so the Keychain is read once per
  profile per gateway run, not once per request.
- The gateway binds to loopback. The menu bar app's transport policy allows
  local networking only.
- Session-backed auth profiles (ChatGPT, Claude, Google OAuth) refresh their
  tokens automatically.

## Limitations

- Streamed passthrough responses do not record token usage (the translated
  surfaces at `/v1/responses` and `/v1/messages` do).
- Google models are not yet available through the translation surfaces.
- Pricing rules cover common models only; unknown models log usage without a
  cost estimate.
- Foreign models inside the Claude Code harness lose Anthropic-specific
  features (server tools, beta headers), and thinking blocks are synthesized
  without signatures.

## License

[MIT](./LICENSE)
