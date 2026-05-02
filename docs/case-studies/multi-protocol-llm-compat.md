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
  +-- OpenAI  --> OpenaiAdapter  (uses async-openai crate)
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

**4. Different HTTP clients per adapter.** The OpenAI adapter uses the `async-openai` crate (which provides typed Request/Response structs), while the Anthropic adapter uses a hand-rolled `reqwest` client with custom serde types. This lets us leverage existing ecosystem tooling where it exists and write minimal code where it doesn't.

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
            Protocol::Openai => Arc::new(openai::OpenaiAdapter::new(&binding.base_url, &binding.api_key)),
            Protocol::Anthropic => Arc::new(anthropic::AnthropicAdapter::new(&binding.base_url, &binding.api_key)),
        };
        // store in registry[protocol][provider_name]
    }
}
```

The frontend discovers available options via `GET /api/config`, which returns protocols, providers (with supported protocols and models), and tools. API keys are never exposed to the frontend.

## Key Takeaways

- A single async trait with a uniform return type is enough to fully abstract away protocol differences. Keep the trait narrow -- one method forces you to normalize at the right level.
- Store protocol-native data rather than inventing a canonical format. Translation layers leak; opaque JSON in the database preserves everything.
- New providers that speak an existing protocol should be config-only entries, never require code changes.
- Message builders are the highest-risk code -- they must handle multiple serialization formats and maintain round-trip fidelity. Test them thoroughly with real protocol responses.
- Lock protocol at session creation to avoid cross-contamination. Different sessions can use different protocols, but a single session is internally consistent.

## References

- `fork-chat-backend/src/llm/mod.rs` -- `ChatAdapter` trait, `SendResult`, `ProviderRegistry`
- `fork-chat-backend/src/llm/openai/adapter.rs` -- OpenAI adapter using `async-openai`
- `fork-chat-backend/src/llm/openai/message_builder.rs` -- OpenAI message reconstruction
- `fork-chat-backend/src/llm/anthropic/adapter.rs` -- Anthropic adapter using `reqwest`
- `fork-chat-backend/src/llm/anthropic/message_builder.rs` -- Anthropic message reconstruction
- `fork-chat-backend/src/llm/anthropic/types.rs` -- Hand-written serde types for Anthropic wire format
- `fork-chat-backend/src/config.rs` -- `Protocol` enum, `ProviderConfig`, `AppState`
- `fork-chat-backend/src/handlers/config.rs` -- Public config endpoint (strips credentials)
- `specs/multi-protocol.md` -- Design doc for the multi-protocol architecture
