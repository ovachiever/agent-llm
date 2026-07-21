# Using Claude Code through agent-llm

The gateway serves an Anthropic Messages endpoint at `/v1/messages` that
accepts `provider/model` ids. Anthropic-dialect upstreams (anthropic, kimi)
pass straight through; Chat-Completions upstreams (openrouter, openai,
lmstudio) are translated both ways, streams included. That lets Claude Code
run on Kimi K3 via OpenRouter, or on local LM Studio models, with usage and
cost in the request log.

## Sessions

Source `bin/session-wrappers.zsh` (opt-in) or export by hand:

```zsh
ANTHROPIC_BASE_URL="http://127.0.0.1:8787" \
ANTHROPIC_API_KEY="<project key>" \
ANTHROPIC_MODEL="openrouter/moonshotai/kimi-k3" \
claude
```

Wrappers provided: `k3` (Kimi K3 via OpenRouter) and `claude-local`
(LM Studio; default `openai/gpt-oss-20b`, override with `LMSTUDIO_MODEL`).
Both read the project key from the local agent-llm database at call time and
also pin `ANTHROPIC_SMALL_FAST_MODEL` / `CLAUDE_CODE_SUBAGENT_MODEL` so
background tasks stay on the routed model.

## Behavior notes

- Unprefixed `claude-*` model ids (leaked from global settings pins, e.g.
  subagent models) route to the Anthropic passthrough rather than erroring;
  those turns bill your Anthropic API key and appear in the log under
  `anthropic`. The `[1m]` alias suffix is stripped.
- `POST /v1/messages/count_tokens` returns a serialized-length estimate
  (chars/4) so Claude Code's context tracking keeps functioning against
  upstreams that have no counting API.
- Thinking blocks from foreign models are synthesized without signatures and
  dropped on the way back upstream; Anthropic server tools (WebFetch etc.)
  and beta features do not translate.
- The Claude Code harness is tuned for Claude models. K3 drives it well;
  small local models will be noticeably less capable at tool use.
