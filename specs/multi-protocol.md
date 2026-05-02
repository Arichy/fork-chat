# Multi-Protocol LLM Support

## Context

fork-chat originally only supported OpenAI Responses. We now support two wire
protocols as first-class citizens:

- `openai` (Responses API)
- `anthropic` (Messages API)

A session chooses one protocol at creation time, and that protocol is immutable
for the lifetime of the session tree.

## Core Invariant

- `sessions.protocol` is fixed after session creation.
- Every create/retry turn request validates the triple:
  - `(session.protocol, request.provider, request.model)`
- A provider/model can vary turn-by-turn, but it must remain valid for the
  session protocol.
- Tool-loop semantics (multi-round transcript, approvals, stream events) are
  protocol-agnostic and shared by both OpenAI and Anthropic adapters.

This keeps all replay data in one native wire format and avoids cross-protocol
translation.

## Why This Shape

We intentionally do not build a provider-neutral canonical message schema.
Instead, we keep protocol-native content so reasoning blocks, tool calls,
tool results, and provider-specific metadata are not lost.

## Config Model

`config.json` models providers and protocol bindings:

```json
{
  "providers": [
    {
      "name": "openai",
      "models": [{ "id": "gpt-5.5" }],
      "protocols": {
        "openai": {
          "base_url": "https://api.openai.com/v1",
          "api_key": "sk-..."
        }
      }
    },
    {
      "name": "deepseek",
      "models": [{ "id": "deepseek-v4-pro" }],
      "protocols": {
        "openai": {
          "base_url": "https://api.deepseek.com",
          "api_key": "ds-..."
        },
        "anthropic": {
          "base_url": "https://api.deepseek.com/anthropic",
          "api_key": "ds-..."
        }
      }
    }
  ]
}
```

Validation guarantees:

- provider name is non-empty and unique
- each provider has at least one protocol binding
- each provider has at least one model
- model ids are unique within a provider

`GET /api/config` returns a sanitized view:

- `protocols: ["openai", "anthropic"]`
- `providers[]` with `supported_protocols` + `models`
- no `api_key` / `base_url` leaks

## Storage Model

### sessions

- `protocol TEXT NOT NULL CHECK (protocol IN ('openai', 'anthropic'))`

### turns

- `turn_messages JSONB NOT NULL DEFAULT '[]'`

`turn_messages` stores per-turn ordered transcript entries plus metadata needed
for replay/audit.

Current shape is append-only and may contain many rounds in one turn:

```json
[
  {
    "role": "user",
    "content": "<protocol-native user content array>"
  },
  {
    "role": "assistant",
    "content": "<protocol-native assistant content array>",
    "response_id": "...",
    "stop_reason": "tool_use or end_turn",
    "usage": { "...": "..." }
  },
  {
    "role": "user",
    "content": "<tool_result / function_call_output blocks>"
  }
]
```

Notes:

- A single turn can append multiple assistant/tool-result entries before
  reaching a terminal status.
- Failed turns still preserve already-appended transcript history.
- `assistant_text` remains a display/search convenience field.
- `turn_messages` is the source of truth for wire replay.

## Dispatch and Validation

Turn create/retry rejects invalid triples with `400`:

- unknown provider
- provider not bound to `session.protocol`
- model not exposed by provider

On success, request is dispatched by `(protocol, provider)` to one of two
adapters:

- `OpenaiAdapter`
- `AnthropicAdapter`

New vendors are config-only, as long as they expose one of these protocols.

## Adapter Contract

```rust
pub trait ChatAdapter {
    async fn send(
        &self,
        history: &[Turn],
        new_user_text: &str,
        model: &str,
        instructions: Option<&str>,
    ) -> Result<SendResult, AppError>;
}
```

`SendResult` includes:

- `assistant_text: Option<String>`
- `assistant_content: JsonValue` (native assistant content for replay)
- `raw_response: Option<JsonValue>` (full upstream response)
- `stop_reason: Option<String>`
- `usage: Option<JsonValue>`
- `response_id: Option<String>`
- token stats (`input_tokens`, `output_tokens`, `cached_tokens`)

### Replay Builders

- OpenAI builder reads `turn_messages` message entries and flattens each entry's
  `content` into Responses input items.
- Anthropic builder reads `turn_messages` entries into Messages `messages[]`.
- Both retain legacy fallback paths for older rows that only had text or
  pre-`turn_messages` formats.

## OpenAI vs Anthropic `turn_messages`

OpenAI turn example (assistant `content` is `response.output` items):

```json
[
  {
    "role": "user",
    "content": [{ "role": "user", "content": "Fix test.txt grammar" }]
  },
  {
    "role": "assistant",
    "content": [
      {
        "id": "msg_...",
        "type": "message",
        "role": "assistant",
        "content": [{ "type": "output_text", "text": "..." }]
      }
    ],
    "response_id": "resp_...",
    "stop_reason": null,
    "usage": { "input_tokens": 123, "output_tokens": 45 },
    "raw_response": { "id": "resp_...", "output": ["..."] }
  }
]
```

Anthropic turn example (assistant `content` is `response.content` blocks):

```json
[
  {
    "role": "user",
    "content": [{ "type": "text", "text": "Fix test.txt grammar" }]
  },
  {
    "role": "assistant",
    "content": [
      {
        "type": "tool_use",
        "id": "toolu_...",
        "name": "Read",
        "input": { "file_path": "test.txt" }
      }
    ],
    "response_id": "msg_...",
    "stop_reason": "tool_use",
    "usage": { "input_tokens": 100, "output_tokens": 20 },
    "raw_response": { "id": "msg_...", "content": ["..."] }
  }
]
```

## Frontend Contract

- Session creation chooses only `protocol`.
- Message send/retry chooses `provider` + `model` (validated against session
  protocol).
- Provider dropdown is filtered by session protocol.
- Model dropdown is filtered by selected provider.
- When replying to a parent turn, the default model now inherits from the
  parent turn's model.
- Turn detail rendering must support mixed transcript blocks (text/tool_use/
  tool_result/function_call/function_call_output), not just user/assistant text.
- Turn progress is consumed from per-turn SSE stream (`turn_snapshot` + live
  append events).

## Test Coverage

Backend:

- `tests/config_endpoint.rs`
- `tests/sessions.rs`
- `tests/turns.rs`
- `tests/openai_test.rs`
- `tests/anthropic_test.rs`
- `tests/message_builder.rs`
- `tests/anthropic_message_builder.rs`
- `tests/tree_and_turn.rs`

Frontend:

- `MessageInput.browser.test.tsx`
- `TurnDetailModal.browser.test.tsx`
- `ChatPage.browser.test.tsx`
- API/MSW fixtures updated for protocol-aware config and `turn_messages`

## Out of Scope

- protocol switching in an existing session
- cross-protocol message translation
- token-level streaming parity work

## Pre-Commit Gate

Required before commit:

Frontend:

- `pnpm format`
- `pnpm lint`
- `pnpm typecheck`
- `pnpm test`

Backend:

- `cargo fmt --all --check`
- `cargo clippy --all-targets --all-features -- -D warnings`
- `cargo nextest run` (or `cargo test` fallback)
