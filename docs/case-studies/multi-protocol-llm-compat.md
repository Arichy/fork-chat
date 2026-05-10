# Multi-Protocol LLM Compatibility: Supporting OpenAI and Anthropic Wire Formats

> A trait-based adapter layer that normalizes OpenAI and Anthropic APIs behind a single `ChatAdapter` interface, so the rest of the application is protocol-agnostic.

## Problem

We needed to support multiple LLM providers (OpenAI, Anthropic, and compatible providers like DeepSeek, GLM, Kimi) through two distinct wire formats. The challenge: each protocol has different message schemas, API structures, and response formats, but the application logic (tree-based conversation, tool calling, turn management) should work identically regardless of which protocol a session uses.

## Why It's Hard

- **Fundamentally different message schemas.** OpenAI uses `InputItem` enums (either `Item` or `EasyInputMessage`) with the Responses API, while Anthropic uses flat `{ role, content: Vec<ContentBlock> }` objects.
- **Round-trip fidelity.** Messages from previous turns must be replayed verbatim. Each protocol's `turn_messages` must survive serialization and deserialization without losing tool-use blocks, reasoning content, or metadata.
- **Single protocol per session.** A session locks its protocol at creation time. Every turn within that session uses the same protocol, but different sessions in the same server can use different protocols.
- **New providers without code changes.** DeepSeek, GLM, and others speak OpenAI-compatible or Anthropic-compatible APIs. Adding them should be a config entry, not a new adapter.

## Alternatives Considered

### Option A: Unified Internal Message Format

Define a canonical message schema and translate to/from each protocol at the boundary.

- **Pros:** Internal code never sees protocol-specific types; single message model to test.
- **Cons:** Lossy translations -- OpenAI reasoning blocks and Anthropic tool-use blocks don't map cleanly. Round-trip fidelity suffers. Each new protocol feature requires schema migration.

### Option B: Protocol-Specific Code Scattered in Handlers

Let each handler branch on `protocol` and handle OpenAI/Anthropic inline.

- **Pros:** Simple to start; no abstraction overhead.
- **Cons:** `match protocol` spreads everywhere. Adding a third protocol means touching dozens of files. Testing requires testing every branch.

### Option C: Trait-Based Adapter with Protocol-Native Storage (Chosen)

Define a `ChatAdapter` trait with a single `send()` method. Each protocol gets its own adapter implementation. Store `turn_messages` in protocol-native JSON format. Translate only at the adapter boundary.

- **Pros:** Clean separation -- handlers never see protocol-specific types. New providers are config-only if they speak an existing protocol. Protocol-native storage preserves full fidelity.
- **Cons:** Message builders are protocol-specific and carry different compatibility burdens: OpenAI still supports legacy replay/fallback shapes, while Anthropic is intentionally strict about the canonical transcript. The `ProviderRegistry` adds a layer of indirection.

## Solution

### Architecture Overview

```
Handler (protocol-agnostic)
  |
  v
ProviderRegistry: Map<Protocol, Map<provider_name, Arc<dyn ChatAdapter>>>
  |
  +-- OpenAI  --> OpenaiAdapter  (reqwest transport + async-openai wire types)
  +-- Anthropic --> AnthropicAdapter (hand-rolled reqwest client)
```

The `ChatAdapter` trait (`fork-chat-backend/src/llm/mod.rs`) has one method:

```rust
#[async_trait]
pub trait ChatAdapter: Send + Sync {
    async fn send(
        &self,
        history: &[Turn],
        new_user_text: Option<&str>,
        model: &str,
        instructions: Option<&str>,
    ) -> Result<SendResult, AppError>;
}
```

Every adapter receives the same four inputs and returns a uniform `SendResult`:

```rust
pub struct SendResult {
    pub assistant_text: Option<String>,
    pub assistant_content: JsonValue,       // protocol-native output (preserved as-is)
    pub raw_response: Option<JsonValue>,
    pub stop_reason: Option<String>,
    pub usage: Option<JsonValue>,
    pub response_id: Option<String>,
    pub input_tokens: Option<i32>,
    pub output_tokens: Option<i32>,
    pub cached_tokens: Option<i32>,
}
```

Callers never see protocol-specific types -- the `assistant_content` field is opaque JSON stored directly in `turn_messages`.

### Key Design Decisions

**1. Protocol is locked at session creation.** The `Session` model stores a `protocol` field (`openai` or `anthropic`). At turn creation, `validate_dispatch()` checks that the provider has a binding for the session's protocol and that the requested model exists.

**2. Protocol-native `turn_messages`.** Each turn stores its transcript as protocol-specific JSON. The OpenAI adapter stores `InputItem` and `OutputItem` objects; the Anthropic adapter stores `{ role, content: Vec<ContentBlock> }`. This preserves full fidelity -- no information is lost in translation.

**3. Provider-level abstraction.** A `ProviderConfig` can declare bindings for multiple protocols:

```rust
pub struct ProviderConfig {
    pub name: String,
    pub models: Vec<ModelConfig>,
    pub protocols: HashMap<Protocol, ProtocolBinding>,  // { base_url, api_key } per protocol
}
```

This means DeepSeek (OpenAI-compatible) only needs a config entry with `protocol: "openai"` -- no new adapter code.

**4. Typed protocol structs, explicit HTTP diagnostics.** The OpenAI adapter uses `async-openai`'s Responses API structs for request/response typing, but sends requests with `reqwest` directly. That split is intentional: SDK-level transport errors can hide the upstream status/body when an OpenAI-compatible provider returns an empty or non-Responses response. Reading the raw body first lets failed turns preserve `status`, `request_id`, `body_len`, and a bounded body preview. The Anthropic adapter already uses a hand-rolled `reqwest` client with custom serde types for the same reason.

**5. LLM errors keep construction location and source chains.** `AppError::llm_api()` and `AppError::llm_api_with_source()` record the Rust call site that created the error. When a turn loop fails, `turn_lifecycle.rs` stores a structured `chain` and `debug` field in the turn error JSON, and logs the same chain through `tracing`. This makes provider compatibility failures diagnosable from the UI and server logs instead of collapsing into a single string like `EOF while parsing a value`.

### Message Builder Differences

Each adapter has its own message builder that converts `Turn` history into protocol-native input:

- **OpenAI** (`fork-chat-backend/src/llm/openai/message_builder.rs`): Builds `Vec<InputItem>` for the Responses API. Handles three formats: new transcript entries, legacy `OutputItem` JSON, and fallback `user_text`/`assistant_text` strings. Explicitly filters out `reasoning` blocks (some providers emit them but reject them on replay).

- **Anthropic** (`fork-chat-backend/src/llm/anthropic/message_builder.rs`): Builds `Vec<AnthropicMessage>` (`{ role, content: Vec<JsonValue> }`) from the canonical transcript shape only. Non-transcript legacy payloads are intentionally ignored in the current early-stage implementation.

The builders now diverge intentionally: OpenAI keeps legacy replay support
because older rows still exist in that wire format, while Anthropic is strict
about the canonical transcript shape. That asymmetry is acceptable in this
early-stage codebase because protocol correctness matters more than preserving
old intermediate storage experiments.

### Config and Dispatch

The `ProviderRegistry` is built at startup from `config.json`:

```rust
for provider in &config.providers {
    for (&protocol, binding) in &provider.protocols {
        let adapter: Arc<dyn ChatAdapter> = match protocol {
            Protocol::Openai => Arc::new(openai::OpenaiAdapter::new(&binding.base_url, &binding.api_key, opts.openai_no_retry)),
            Protocol::Anthropic => Arc::new(anthropic::AnthropicAdapter::new(&binding.base_url, &binding.api_key)),
        };
        // store in registry[protocol][provider_name]
    }
}
```

Provider secrets can stay outside the JSON file. During `Config::load`, string
placeholders like `"${DEEPSEEK_API_KEY}"` are expanded from the process
environment before the config crate deserializes the final structure. That
keeps adding an OpenAI-compatible or Anthropic-compatible provider config-only
while avoiding committed API keys.

The frontend discovers available options via `GET /api/config`, which returns protocols, providers (with supported protocols and models), and tools. API keys are never exposed to the frontend.

## Key Takeaways

- A single async trait with a uniform return type is enough to fully abstract away protocol differences. Keep the trait narrow -- one method forces you to normalize at the right level.
- Store protocol-native data rather than inventing a canonical format. Translation layers leak; opaque JSON in the database preserves everything.
- New providers that speak an existing protocol should be config-only entries, never require code changes.
- Provider API keys can be referenced as `${ENV_VAR}` placeholders in `config.json` so secrets stay out of the file.
- Message builders are the highest-risk code -- they must handle multiple serialization formats and maintain round-trip fidelity. Test them thoroughly with real protocol responses.
- For OpenAI-compatible providers, treat the wire protocol and the endpoint surface as separate compatibility questions. If a provider does not actually implement `/responses`, diagnostics must expose the raw status/body so the mismatch is obvious.
- Failed turn errors should carry a source chain and construction location. A terse frontend error is acceptable only when the server logs and stored turn JSON retain the deeper provider/parser context.
- Lock protocol at session creation to avoid cross-contamination. Different sessions can use different protocols, but a single session is internally consistent.

## References

- `fork-chat-backend/src/llm/mod.rs` -- `ChatAdapter` trait, `SendResult`, `ProviderRegistry`
- `fork-chat-backend/src/error.rs` -- `AppError` helpers that attach LLM error locations and source chains
- `fork-chat-backend/src/turn_lifecycle.rs` -- Failed-turn persistence for structured error diagnostics
- `fork-chat-backend/src/llm/openai/adapter.rs` -- OpenAI-compatible adapter using `reqwest` transport with `async-openai` wire types
- `fork-chat-backend/src/llm/openai/message_builder.rs` -- OpenAI message reconstruction
- `fork-chat-backend/src/llm/anthropic/adapter.rs` -- Anthropic adapter using `reqwest`
- `fork-chat-backend/src/llm/anthropic/message_builder.rs` -- Anthropic message reconstruction
- `fork-chat-backend/src/llm/anthropic/types.rs` -- Hand-written serde types for Anthropic wire format
- `fork-chat-backend/src/config.rs` -- `Protocol` enum, `ProviderConfig`, `AppState`
- `fork-chat-backend/src/handlers/config.rs` -- Public config endpoint (strips credentials)
- `specs/multi-protocol.md` -- Design doc for the multi-protocol architecture
