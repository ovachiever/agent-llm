# agent-llm menu bar app

A native SwiftUI menu bar app for the gateway. No Dock icon, no window
clutter: a status lamp in the menu bar, a glanceable popover, and one
dashboard window.

## What it says

The UI is built on a strict message hierarchy:

1. **The verdict.** One computed sentence: "6 of 6 routes open", "5 routes
   open · LM Studio server off", or "Gateway offline" with the command that
   fixes it.
2. **The switchboard.** One row per provider: status lamp, the auth profile
   actually in use (in monospace), and an Add/Change key affordance. LM Studio
   is probed directly, so "server off" is a measurement, not a guess.
3. **The meter.** Today's traffic in one line (requests, tokens, estimated
   cost, errors) above a monospace log where red belongs exclusively to
   failures.

Key entry lives in a per-provider sheet with two paths: paste an API key, or
browser account sign-in (ChatGPT / Claude / Google OAuth) via the gateway's
auth-attempt flow.

## Design system

SF Pro for chrome, SF Mono for data. Four type sizes (11/13/15/22), two
weights, 4pt spacing grid, hairlines over boxes. One amber accent reserved
for interactive elements; green/red/gray strictly for state. The status lamp
pulses only when traffic flowed in the last minute, and respects reduced
motion.

## Build and run

```bash
./build-macos-app.sh
open release/agent-llm.app
```

Requires the gateway (see repo root). The app polls
`http://127.0.0.1:8787/admin` every 10 seconds
(`AGENT_LLM_ADMIN_URL` overrides). App Transport Security allows local
networking only.

## Design-verification harness

SwiftUI has no DOM to screenshot, and external capture needs Screen Recording
permission, so the app can render itself:

```bash
AGENT_LLM_PREVIEW=popover  AGENT_LLM_SNAPSHOT=/tmp/popover.png  ./release/agent-llm.app/Contents/MacOS/AgentLlmMac
AGENT_LLM_PREVIEW=dashboard AGENT_LLM_APPEARANCE=dark AGENT_LLM_SNAPSHOT=/tmp/dash.png ./release/agent-llm.app/Contents/MacOS/AgentLlmMac
```

The app presents the requested surface, renders it via `ImageRenderer` at 2x
into the given path, and exits. Two known renderer artifacts (not app bugs):
ScrollView interiors render through a flattened snapshot mode, and system
button chrome renders as placeholder glyphs. Judge typography, spacing, and
hierarchy from the render; judge control chrome in the live app.
