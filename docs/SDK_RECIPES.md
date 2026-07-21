# SDK Recipes

This file is the short, copy-paste companion to [`PROVIDER_SETUP_PLAYBOOK.md`](./PROVIDER_SETUP_PLAYBOOK.md).

Use it when you already know which provider you want and just need a working starting point through `agent-llm`.

## Before you run any example

1. Start the gateway.
2. Load the target project's `.env.agent-llm`.
3. Keep the request provider-native. Do not rewrite Anthropic, OpenAI, Gemini, or OpenRouter payloads into one shared schema.

## OpenAI Python

```python
from openai import OpenAI
import os

client = OpenAI(
    api_key=os.environ["OPENAI_API_KEY"],
    base_url=os.environ["OPENAI_BASE_URL"],
)

response = client.responses.create(
    model="gpt-5.1",
    input="Write a concise implementation plan.",
    reasoning={"effort": "high"},
    text={"verbosity": "high"},
    max_output_tokens=64000,
)

print(response.output_text)
```

## OpenAI TypeScript

```ts
import OpenAI from "openai";

const client = new OpenAI({
  apiKey: process.env.OPENAI_API_KEY,
  baseURL: process.env.OPENAI_BASE_URL,
});

const response = await client.responses.create({
  model: "gpt-5.1",
  input: "Write a concise implementation plan.",
  reasoning: { effort: "high" },
  text: { verbosity: "high" },
  max_output_tokens: 64000,
});

console.log(response.output_text);
```

## Anthropic Python

```python
from anthropic import Anthropic
import os

client = Anthropic(
    api_key=os.environ["ANTHROPIC_API_KEY"],
    base_url=os.environ["ANTHROPIC_BASE_URL"],
)

response = client.messages.create(
    model="claude-opus-4-6",
    max_tokens=32000,
    thinking={"type": "adaptive"},
    output_config={"effort": "high"},
    messages=[{"role": "user", "content": "Write a concise implementation plan."}],
)

text_parts = [block.text for block in response.content if getattr(block, "type", None) == "text"]
print("".join(text_parts))
```

## Anthropic Python Streaming

```python
from anthropic import AsyncAnthropic
import os

client = AsyncAnthropic(
    api_key=os.environ["ANTHROPIC_API_KEY"],
    base_url=os.environ["ANTHROPIC_BASE_URL"],
)

async with client.messages.stream(
    model="claude-opus-4-6",
    max_tokens=32000,
    thinking={"type": "adaptive"},
    output_config={"effort": "high"},
    messages=[{"role": "user", "content": "Write a long implementation plan."}],
) as stream:
    final = await stream.get_final_message()

text_parts = [block.text for block in final.content if getattr(block, "type", None) == "text"]
print("".join(text_parts))
```

## Gemini Python

```python
from google import genai
from google.genai import types
import os

client = genai.Client(
    api_key=os.environ["GOOGLE_API_KEY"],
    http_options={"base_url": os.environ["GOOGLE_GENERATIVE_AI_BASE_URL"]},
)

response = client.models.generate_content(
    model="gemini-2.5-flash",
    contents="Summarize the architecture tradeoffs.",
    config=types.GenerateContentConfig(
        thinking_config=types.ThinkingConfig(thinking_budget=-1)
    ),
)

print(response.text)
```

## OpenRouter via OpenAI SDK

```python
from openai import OpenAI
import os

client = OpenAI(
    api_key=os.environ["OPENROUTER_API_KEY"],
    base_url=os.environ["OPENROUTER_BASE_URL"],
)

response = client.chat.completions.create(
    model="anthropic/claude-sonnet-4.6",
    messages=[{"role": "user", "content": "Summarize the architecture tradeoffs."}],
    extra_body={
        "provider": {
            "order": ["anthropic"],
            "allow_fallbacks": False,
            "require_parameters": True,
        }
    },
)

print(response.choices[0].message.content)
```

## Kimi (Moonshot) via Anthropic SDK

The `kimi` provider speaks the Anthropic protocol, so the Anthropic SDK works unchanged:

```python
import anthropic, os

client = anthropic.Anthropic(
    api_key=os.environ["KIMI_API_KEY"],  # the project key
    base_url="http://127.0.0.1:8787/kimi",
)

response = client.messages.create(
    model="kimi-k2.6",
    max_tokens=8000,
    messages=[{"role": "user", "content": "Summarize this repo layout."}],
)
print(response.content[0].text)
```

## LM Studio via OpenAI SDK

Local models, no upstream key, still logged:

```python
from openai import OpenAI
import os

client = OpenAI(
    api_key=os.environ["LMSTUDIO_API_KEY"],  # the project key
    base_url="http://127.0.0.1:8787/lmstudio/v1",
)

response = client.chat.completions.create(
    model="openai/gpt-oss-20b",
    messages=[{"role": "user", "content": "Write a haiku about local inference."}],
)
print(response.choices[0].message.content)
```

## Raw HTTP smoke test

OpenAI-compatible:

```bash
curl -s "$OPENAI_BASE_URL/responses" \
  -H "Authorization: Bearer $OPENAI_API_KEY" \
  -H "Content-Type: application/json" \
  -d '{
    "model": "gpt-5.1",
    "input": "Say hello in one sentence.",
    "reasoning": {"effort": "medium"},
    "text": {"verbosity": "low"},
    "max_output_tokens": 2000
  }'
```

Anthropic:

```bash
curl -s "$ANTHROPIC_BASE_URL/messages" \
  -H "x-api-key: $ANTHROPIC_API_KEY" \
  -H "anthropic-version: 2023-06-01" \
  -H "content-type: application/json" \
  -d '{
    "model": "claude-sonnet-4-6",
    "max_tokens": 8000,
    "thinking": {"type": "adaptive"},
    "output_config": {"effort": "high"},
    "messages": [{"role": "user", "content": "Say hello in one sentence."}]
  }'
```

## When to stop and check the playbook

Go back to [`PROVIDER_SETUP_PLAYBOOK.md`](./PROVIDER_SETUP_PLAYBOOK.md) if:

- you are changing model families
- you need tool calling or structured outputs
- you need to tune token budgets or reasoning depth
- temperature or sampling params are involved
- a wrapper or SDK helper starts rejecting a parameter that the provider docs allow
