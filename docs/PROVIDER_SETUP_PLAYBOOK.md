# Provider Setup Playbook

This file is the canonical setup guide for wiring provider-native SDKs through `agent-llm`.

Use it when a new project, script, or agent needs to talk to OpenAI, Anthropic, Google Gemini, OpenRouter, Kimi (Moonshot), or a local LM Studio server without rediscovering model-specific request quirks.

Status date: 2026-07-21

## Core rule

`agent-llm` should proxy provider-native payloads. Do not invent one house schema for all providers, and do not trust wrapper libraries to be more correct than the upstream docs.

## Default integration policy

1. Keep the project's existing provider SDK if it can target a custom base URL.
2. Point that SDK at `agent-llm` in local/dev.
3. Preserve direct-provider env support for production.
4. Use the provider's current docs to choose parameters.
5. Run one live smoke request for each provider/model family you add.

## Fast rules by provider

### Anthropic Claude 4.6

Use these defaults unless the workload proves otherwise:

- Prefer `client.messages.create(...)` or streaming `client.messages.stream(...)`.
- Use `thinking: {"type": "adaptive"}` for Opus 4.6 and Sonnet 4.6.
- Use `output_config: {"effort": "high"}` as the default starting point.
- Use `output_config: {"effort": "max"}` only on Opus 4.6 when you explicitly want the deepest reasoning.
- Keep `max_tokens` large enough for both thinking and visible output. Treat small values like `4000` as a common failure mode for long reports and coding tasks.
- Stream large responses. Anthropic recommends streaming for larger outputs and the SDK path is more robust there.
- Do not assume `budget_tokens` is the right control on 4.6 models. Manual thinking with `thinking: {"type": "enabled", "budget_tokens": N}` is deprecated on 4.6.
- If using structured outputs, prefer `output_config.format` over the older `output_format`.

Anthropic 4.6 notes:

- On Opus 4.6, adaptive thinking is the recommended path and manual `budget_tokens` mode is deprecated.
- On Sonnet 4.6, adaptive thinking also works. Manual extended thinking still exists, including the interleaved-thinking beta header path, but that is not the default recommendation for new setups.
- `max_tokens` is still real. It is not deprecated, and it still matters. On modern thinking-enabled Claude requests, it acts as a hard cap that can starve visible output if set too low.

Recommended starting example:

```python
response = client.messages.create(
    model="claude-opus-4-6",
    max_tokens=32000,
    thinking={"type": "adaptive"},
    output_config={"effort": "high"},
    messages=[{"role": "user", "content": prompt}],
)
```

Use streaming when the task is long-form or tool-heavy:

```python
async with client.messages.stream(
    model="claude-opus-4-6",
    max_tokens=32000,
    thinking={"type": "adaptive"},
    output_config={"effort": "high"},
    messages=[{"role": "user", "content": prompt}],
) as stream:
    response = await stream.get_final_message()
```

### OpenAI GPT-5 family

Use these defaults unless the workload proves otherwise:

- Prefer the Responses API for GPT-5 family models.
- Control reasoning depth with `reasoning: {"effort": ...}`.
- Control visible answer length with `text: {"verbosity": ...}`.
- Use `max_output_tokens` as the total generated-token ceiling, including reasoning and final visible output.
- Raise `max_output_tokens` aggressively for long answers, codegen, or deep reasoning. If the answer is getting truncated, the limit is usually too low.
- Treat `temperature` as off-limits unless the exact model/docs explicitly allow it for the reasoning level you chose.

Recommended starting points:

- General coding or agentic work: `reasoning.effort = "medium"` or `"high"`
- Long-form answers or code diffs: `text.verbosity = "high"`
- Constrained or terse output: `text.verbosity = "low"`

Safe example:

```python
response = client.responses.create(
    model="gpt-5.1",
    input=prompt,
    reasoning={"effort": "high"},
    text={"verbosity": "high"},
    max_output_tokens=64000,
)
```

OpenAI family notes:

- GPT-5.1 and GPT-5.2 docs explicitly say `temperature`, `top_p`, and `logprobs` are only supported when `reasoning.effort` is `none`.
- Earlier GPT-5 models do not support `none`; in practice, treat non-`none` reasoning and temperature sampling as incompatible unless the official model docs say otherwise.
- GPT-5 pro is Responses-only and only supports high reasoning effort.

### Google Gemini 2.5 and 3

Use these defaults unless the workload proves otherwise:

- Keep Gemini requests Gemini-native. Do not force OpenAI-style reasoning params into Gemini.
- For Gemini 2.5 models, use `thinkingBudget` when you need explicit control.
- For Gemini 3 models, prefer `thinkingLevel` over `thinkingBudget`.
- Do not send both `thinkingBudget` and `thinkingLevel` in the same request.
- Use `includeThoughts` only when you actually need thought summaries for debugging or evaluation.
- Preserve thought signatures in multi-turn/function-calling flows when the SDK requires it.

Recommended starting points:

- `gemini-2.5-pro`: leave dynamic thinking on unless latency forces a change
- `gemini-2.5-flash`: `thinkingBudget = -1` for dynamic, `0` for fast no-thinking mode
- `gemini-3-pro` or `gemini-3-flash-preview`: prefer `thinkingLevel`, usually `high` or `low` depending on latency needs

Examples:

```python
response = client.models.generate_content(
    model="gemini-2.5-flash",
    contents=prompt,
    config=types.GenerateContentConfig(
        thinking_config=types.ThinkingConfig(thinking_budget=-1)
    ),
)
```

```python
response = client.models.generate_content(
    model="gemini-3-flash-preview",
    contents=prompt,
    config=types.GenerateContentConfig(
        thinking_config=types.ThinkingConfig(thinking_level="high")
    ),
)
```

Gemini notes:

- Gemini 2.5 Pro uses dynamic thinking by default and cannot disable thinking.
- Gemini 2.5 Flash can disable thinking with `thinkingBudget = 0`.
- Gemini 3 prefers `thinkingLevel`; `thinkingBudget` is still accepted for backward compatibility but is no longer the recommended control surface.

### OpenRouter

Use OpenRouter when you want access to multiple upstream labs, but remember that parameter support is still provider-dependent.

Default rules:

- Treat OpenRouter as OpenAI-compatible transport, not as proof that all providers support all OpenAI-shaped params.
- If you care about exact parameter preservation, set `provider.require_parameters = true`.
- If you care about routing order, set `provider.order`.
- If you want OpenRouter to stay on a specific provider, also set `provider.allow_fallbacks = false`.
- Keep the upstream provider's own model guidance in mind. An OpenRouter Anthropic model still inherits Anthropic-style thinking and output tradeoffs.

Example:

```json
{
  "model": "anthropic/claude-sonnet-4.6",
  "messages": [{"role": "user", "content": "..." }],
  "provider": {
    "order": ["anthropic"],
    "allow_fallbacks": false,
    "require_parameters": true
  }
}
```

### Kimi (Moonshot)

The `kimi` provider targets Moonshot's pay-per-token platform endpoint (`https://api.moonshot.ai/anthropic`, Anthropic Messages protocol). The Kimi Code subscription endpoint (`api.kimi.com/coding`) closed to new signups in July 2026.

- Auth: a platform.moonshot.ai API key stored as a normal `api_key` profile. Both `x-api-key` and bearer auth are accepted upstream; the gateway sends `x-api-key` plus `anthropic-version`.
- The platform tier currently serves `kimi-k2.6` and `kimi-k2.7-code`, **not K3**. Verify with the models list before assuming; the account must be funded or every call returns `exceeded_current_quota_error`.
- **K3 route:** `openrouter/moonshotai/kimi-k3` through `/v1/responses` ($3/M in, $15/M out, 1M context, int4). K3 has exactly one OpenRouter upstream (Moonshot AI), so the OpenRouter account's provider policy must permit it; a restrictive data-policy setting yields `404 No allowed providers are available`.
- Claude Code: `ANTHROPIC_BASE_URL=http://127.0.0.1:8787/kimi` + project key runs a session on the platform models with request logging.

### LM Studio (local)

The `lmstudio` provider targets the local server at `http://127.0.0.1:1234` (OpenAI-compatible).

- No credentials: a default `local` profile (auth mode `none`) ships preconfigured.
- Start the server with `lms server start`; confirm with `curl http://127.0.0.1:1234/v1/models`.
- Model ids often contain slashes (`openai/gpt-oss-20b`, `qwen/qwen3.6-35b-a3b`); through `/v1/responses` prefix them as `lmstudio/openai/gpt-oss-20b`.
- `GET /v1/models` on the gateway live-refreshes the LM Studio catalog when the server is reachable.
- Cost is always zero; token usage still logs.

## Failure patterns to avoid

### Anthropic

- `max_tokens` too low for long reports, codegen, or adaptive thinking
- using deprecated `budget_tokens` on 4.6 because an older example did
- forgetting to stream large responses
- assuming visible output is the only thing competing for token budget

### OpenAI

- using Chat Completions-era defaults when the GPT-5 family works better through Responses
- setting `temperature` with a non-`none` reasoning level
- keeping `max_output_tokens` too low and then misdiagnosing truncation as a model-quality issue
- trying to solve answer length only with prompting instead of `text.verbosity`

### Gemini

- sending OpenAI or Anthropic-style reasoning params
- using `thinkingBudget` for Gemini 3 when `thinkingLevel` is the intended control
- forgetting that thought signatures matter in some multi-turn flows

### OpenRouter

- assuming a model name guarantees every provider backend supports every parameter
- relying on fallback behavior when you actually need strict provider behavior
- debugging provider quirks without pinning provider order first

## Recommended live smoke tests

For every new provider/model family integration:

1. Verify auth with `agent-llm auth verify`.
2. Send one short non-stream request.
3. Send one long or reasoning-heavy request with the real intended params.
4. Confirm the request log captures provider, auth profile, latency, usage, and estimated cost.
5. Only then bake defaults into another project.

## Copy-paste handoff for another AI

Use this in the target repo:

> Use `../agent-llm` as the local LLM gateway for development. Keep provider SDKs provider-native and point them at `agent-llm` base URLs instead of adding a custom abstraction. Before changing model params or request payloads, read `../agent-llm/docs/PROVIDER_SETUP_PLAYBOOK.md` and follow its provider-specific rules. Preserve direct-provider env support for production.

## Sources

Official docs used for this playbook:

- Anthropic Claude 4.6 overview: <https://platform.claude.com/docs/en/about-claude/models/whats-new-claude-4-6>
- Anthropic migration guide: <https://platform.claude.com/docs/en/about-claude/models/migration-guide>
- Anthropic adaptive thinking: <https://platform.claude.com/docs/en/build-with-claude/adaptive-thinking>
- Anthropic effort: <https://platform.claude.com/docs/en/build-with-claude/effort>
- Anthropic extended thinking: <https://platform.claude.com/docs/en/build-with-claude/extended-thinking>
- OpenAI GPT-5 guide: <https://platform.openai.com/docs/guides/gpt-5>
- OpenAI latest model guide: <https://platform.openai.com/docs/guides/latest-model>
- OpenAI reasoning guide: <https://platform.openai.com/docs/guides/reasoning>
- OpenAI GPT-5 and GPT-5 pro model pages: <https://platform.openai.com/docs/models/gpt-5> and <https://platform.openai.com/docs/models/gpt-5-pro>
- Google Gemini thinking guide: <https://ai.google.dev/gemini-api/docs/thinking>
- Google Gemini docs overview: <https://ai.google.dev/docs>
- OpenRouter provider routing: <https://openrouter.ai/docs/features/provider-routing>
