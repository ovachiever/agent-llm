# Use agent-llm From Another Project

This guide is for the case where `agent-llm` lives next to another local repo and should act as that repo's LLM gateway during development.

Example layout:

```text
workspace/
  agent-llm/
  another-project/
```

From `another-project`, the gateway repo would usually be referenced as `../agent-llm`.

## Goal

Use `agent-llm` in local/dev so apps can keep their provider-native SDKs and payloads while sharing one local control plane for:

- auth profiles
- project-level gateway keys
- request logging
- usage and cost visibility
- local session-backed vs API-key billing decisions

Production can still call labs directly unless you explicitly choose to route through a hosted gateway.

## Stable commands

Prefer the wrapper scripts:

```bash
../agent-llm/bin/agent-llm
../agent-llm/bin/agent-llm-gateway
../agent-llm/bin/agent-llm-up
../agent-llm/bin/agent-llm-down
../agent-llm/bin/agent-llm-status
```

## One-time local setup

Initialize state:

```bash
../agent-llm/bin/agent-llm init
```

Add an auth profile:

OpenAI API key:

```bash
../agent-llm/bin/agent-llm auth add \
  --provider openai \
  --name default-api \
  --auth-mode api_key \
  --secret-env OPENAI_API_KEY \
  --default
```

Anthropic local session:

```bash
../agent-llm/bin/agent-llm auth add \
  --provider anthropic \
  --name claude-console \
  --auth-mode anthropic_session \
  --secret-env ANTHROPIC_SESSION_TOKEN \
  --default
```

Verify the profile before wiring a project:

```bash
../agent-llm/bin/agent-llm auth verify --provider anthropic --profile claude-console
```

## Project setup

Inside the target project directory:

Generate the project env file:

```bash
../agent-llm/bin/agent-llm project link \
  --name my-project \
  --env-file .env.agent-llm
```

Start the gateway:

```bash
../agent-llm/bin/agent-llm-up
../agent-llm/bin/agent-llm-status
```

Load `.env.agent-llm` into the project's local runtime.

## What the generated env does

Local mode points provider SDKs at:

- `http://127.0.0.1:8787/openai/v1`
- `http://127.0.0.1:8787/anthropic/v1`
- `http://127.0.0.1:8787/google/v1beta`
- `http://127.0.0.1:8787/openrouter/v1`

The generated project key becomes the local API key for those SDKs.

## Switching modes

Switch the target project back to direct provider mode:

```bash
../agent-llm/bin/agent-llm mode use-direct \
  --project my-project \
  --env-file .env.agent-llm
```

Switch it back to local gateway mode:

```bash
../agent-llm/bin/agent-llm mode use-local \
  --project my-project \
  --env-file .env.agent-llm
```

## Recommended handoff text for another AI

Use this instruction in the target repo:

> Use `../agent-llm` as the local LLM gateway for development. Do not add direct provider auth logic if the existing SDK can already target a base URL. Load `.env.agent-llm` for local/dev, keep direct-provider env support for production, and keep the switch controlled by environment variables rather than hardcoding the gateway.
