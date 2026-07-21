# agent-llm Engineering Guide

This file is the current as-is operating guide for agents working in this project.

## Scope
- Applies to `agent-llm/`.

## Source Of Truth
1. Running code and checked-in files in this project
2. Local manifests and lockfiles
3. Local README, deployment files, and nearest scoped `AGENTS.md` files
4. Historic notes only when they still match the code

## Current Repo Signals
- Root manifests: `Cargo.toml`.
- Inferred stack signals: Rust.

## Top-Level Layout
- `apps/macos/` - native SwiftUI menu bar app (primary UI)
- `apps/desktop/` - legacy Electron shell
- `docs/` - documentation (gateway API, Codex/Claude Code setup, provider playbook, SDK recipes)
- `bin/` - stable wrapper scripts and opt-in shell session wrappers
- `crates/agent-llm-core/` - types, SQLite, pricing, secret storage
- `crates/agent-llm-gateway/` - Axum gateway: passthrough, /v1/responses, /v1/messages, admin API
- `crates/agent-llm-translate/` - sans-IO protocol translation and SSE stream state machines
- `crates/agent-llm-cli/` - setup and operational CLI
- `target/` - build output (gitignored)
- `Cargo.toml` / `Cargo.lock` - workspace manifests
- `LICENSE`, `README.md` - root files

## Working Rules
- Keep this file factual and current-state. Do not turn it into a roadmap or target architecture document.
- Keep unrelated non-engineering language out of this file.
- Use the nearest scoped `AGENTS.md` before changing a deeper package, app, or subsystem.
- Prefer small, local changes and validate through the manifest that owns the touched code.

## agent-do Tooling

Prefer `agent-do <tool> <command>` over raw CLI/scripts.

Discovery:
- `agent-do suggest "task"`
- `agent-do suggest --project`
- `agent-do find <keyword>`
- `agent-do --list`
- `agent-do <tool> --help`

Readiness:
- `agent-do --health`
- `agent-do bootstrap --recommend`
- `agent-do bootstrap`

Routing:
- `agent-do -n "..."`
- `agent-do --how "..."`
- `agent-do --dry-run "..."`
- `agent-do --offline "..."`

Common commands:
- `agent-do browse open <url>`
- `agent-do browse login <url>`
- `agent-do context fetch-llms <domain>`
- `agent-do context search "<query>"`
- `agent-do zpc learn <ctx> <prob> <sol> <takeaway> --tags "t1"`
- `agent-do zpc patterns`
- `agent-do dpt score <target>`
- `agent-do supabase --help`
- `agent-do render --help`
- `agent-do resend status <domain>`
- `agent-do cloudflare --help`
- `agent-do auth ensure <site>`
- `agent-do creds check --tool <tool>`

## Validation
- `cargo test --workspace`
- `cargo fmt --all -- --check`
- `cargo clippy --workspace --all-targets`
- macOS app: `./apps/macos/build-macos-app.sh`
