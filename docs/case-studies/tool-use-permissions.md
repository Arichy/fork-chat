# Tool-Use Permission Design: Three-Layer Resolution with Allow-Always Rules

> A per-tool-call permission system with three resolution layers (existence check, session allow-rules, default policy) and a user-facing Allow/Always-Allow/Deny approval flow via SSE.

## Problem

The LLM in our agentic loop can invoke tools (`read`, `write`, `bash`) on the host machine. We need a permission system that: (1) auto-approves safe operations, (2) blocks dangerous ones by default, (3) lets users create persistent allow-rules to reduce friction, and (4) integrates seamlessly with the streaming SSE architecture so the UI can render approval prompts in real-time.

## Why It's Hard

- **Per-tool-call granularity.** Permission decisions aren't binary per tool -- a user might allow `bash(cargo check)` but deny `bash(rm -rf)`. The system must evaluate rules against both the tool name and its arguments.
- **Async loop pausing.** The agentic loop runs in a background tokio task. When it needs approval, it must pause (persist state), wait for a separate HTTP request from the user, then resume. The loop can't block on a channel -- it must save its state to the DB and return.
- **Persistent rules vs. one-time decisions.** Users need both "allow just this once" and "always allow this pattern" options. The latter must persist across turn boundaries and survive the loop pausing/resuming.
- **Wildcard matching.** For `bash`, users need patterns like `bash(cargo check *)` to allow any cargo-check variant. The matching must be simple enough to explain but powerful enough to be useful.
- **No user authentication.** The system is single-user, so permissions are about tool safety, not access control. There's no login, no JWT, no RBAC.

## Alternatives Considered

### Option A: Global Allowlist in Config

Define allowed/disallowed tools in `config.json` at startup.

- **Pros:** Simple. No runtime decisions. No UI needed.
- **Cons:** Cannot change rules without restart. No per-call granularity. Users can't approve a specific `bash` command -- it's all or nothing.

### Option B: Permission Middleware on Every Request

Run all tool calls through an authorization middleware before execution.

- **Pros:** Centralized enforcement. Could support multi-user RBAC in the future.
- **Cons:** Over-engineered for a single-user tool. Tool calls happen inside a spawned task, not during HTTP request handling -- middleware doesn't fit the architecture.

### Option C: Three-Layer Per-Call Resolution with Session-Scoped Rules (Chosen)

Each tool call goes through three resolution layers: (1) does this tool exist?, (2) does a session allow-rule match?, (3) what's the default policy? If no layer auto-approves, the loop pauses and the user decides via SSE + POST.

- **Pros:** Fine-grained control. Session-scoped rules are intuitive (rules follow the conversation). Integrates naturally with the SSE streaming architecture. `AllowAlways` creates rules that reduce future friction.
- **Cons:** The loop-pause mechanism (persist state, return, resume on POST) adds complexity to the turn lifecycle. Wildcard matching is limited to simple `*` patterns.

## Solution

### Architecture Overview

```
LLM Response (contains tool calls)
  |
  v
For each tool call:
  1. Tool Existence Check -- unknown tool -> synthetic error, continue loop
  2. Session Allow-Rules -- matching rule -> auto-approve
  3. Default Tool Policy -- Auto -> auto-approve, RequireApproval -> pending
  |
  v
If all auto-approved: execute in parallel, continue loop
If any pending:
  - Persist pending_tool_calls in runtime_state
  - Publish APPROVAL_NEEDED event via SSE
  - Set turn status to "awaiting_approval"
  - Return from loop (pause)
  |
  v
User submits decisions via POST /approve
  - Allow -> execute
  - AllowAlways -> save rule to session preferences, execute
  - Deny -> return synthetic error to LLM
  |
  v
Loop resumes (spawn_turn_loop called again)
```

### Layer 1: Tool Existence Check

If the LLM requests a tool that doesn't exist in the tool definitions (`read`, `write`, `bash`), the call is immediately rejected with a synthetic error result:

```rust
// fork-chat-backend/src/turn_lifecycle.rs (in continue_turn_loop)
if !tool_defs.contains_key(&call.name) {
    // Return error: { is_error: true, error_kind: "unknown_tool" }
    // LLM sees the error and can recover within the same turn
}
```

This prevents hallucinated tool names from crashing the system.

### Layer 2: Session Allow-Rules

Each session stores an array of allow-rules in its `preferences` JSONB under `tool_allow_rules`:

```json
{
  "tool_allow_rules": ["read", "bash(cargo check *)", "bash(cargo nextest run *)"]
}
```

The `match_allow_rule` function (`fork-chat-backend/src/tooling.rs`) supports two patterns:

- **Bare tool name** (e.g., `"read"`, `"write"`): Matches all calls to that tool regardless of arguments.
- **Parameterized rule** (e.g., `"bash(cargo check *)"`): Only for `bash`. Uses `*` wildcard matching on the command string.

```rust
fn match_allow_rule(rule: &str, tool_name: &str, input: &JsonValue) -> bool {
    if rule == tool_name {
        return true;
    }
    if tool_name != "bash" {
        return false;
    }
    let (prefix, suffix) = ("bash(", ")");
    if !rule.starts_with(prefix) || !rule.ends_with(suffix) {
        return false;
    }
    let pattern = &rule[prefix.len()..rule.len() - suffix.len()];
    let command = input
        .get("command")
        .and_then(|v| v.as_str())
        .unwrap_or_default();
    wildcard_match(pattern, command)
}
```

Rules are **per-session** -- they follow the conversation but don't leak across sessions.

### Layer 3: Default Tool Policy

Each tool has a built-in default policy (`fork-chat-backend/src/tooling.rs`):

| Tool | Default Policy | Rationale |
|---|---|---|
| `read` | `Auto` | Reading files is safe -- no side effects |
| `write` | `RequireApproval` | Writing files can be destructive |
| `bash` | `RequireApproval` | Shell commands can be arbitrary and dangerous |

The `GET /api/config` endpoint exposes these policies to the frontend so it can show appropriate UI hints.

### The Approval Flow (Loop Pausing)

When a tool call reaches `RequireApproval` with no matching allow-rule, the loop pauses:

1. The pending tool calls are serialized and stored in `runtime_state.pending_tool_calls`.
2. An `APPROVAL_NEEDED` SSE event is published with details of each pending call (name, input, `pending_call_id`).
3. Turn status transitions to `"awaiting_approval"`.
4. The `continue_turn_loop` function **returns** -- the spawned task ends.

### Cancellation and Race Safety

Turn execution now uses a process-local `TurnTaskManager` plus
`CancellationToken`:

- `cancel_turn_handler` first signals cancellation for the active turn task.
- In-flight loop branches (`adapter.send`, tool execution) listen on the same
  token and exit cooperatively.
- `bash` execution uses `kill_on_drop(true)` so cancelling drops the waiting
  future and terminates the child process promptly.

To guarantee sticky terminal status under races, loop writes are guarded by
`update_turn_if_active` (CAS-style `WHERE status IN ('running','awaiting_approval')`).
If a concurrent cancel already moved the row to `failed`, stale loop writes are
discarded instead of resurrecting the turn.

The frontend renders an approval UI in `TurnDetailModal.tsx`:

```tsx
// Three buttons per pending tool call
<Button onClick={() => approve(call.pending_call_id, "allow")}>Allow</Button>
<Button onClick={() => approve(call.pending_call_id, "allow_always")}>Always allow this tool</Button>
<Button onClick={() => approve(call.pending_call_id, "deny")}>Deny</Button>
```

### User Decisions

The user submits decisions via `POST /api/sessions/:id/turns/:id/approve`:

```json
{
  "decisions": [
    { "pending_call_id": "call_abc123", "decision": "allow_always" },
    { "pending_call_id": "call_def456", "decision": "deny" }
  ]
}
```

The `approve_turn_handler` processes each decision:

- **Allow**: Execute the tool call. No rule saved.
- **AllowAlways**: Derive an allow-rule from the call, persist it to `session.preferences.tool_allow_rules`, then execute the tool call. The `derive_allow_rule` function (`tooling.rs`) generates the rule string: `bash(<exact command>)` for bash, bare tool name for others.
- **Deny**: Return a synthetic error result `{ is_error: true, content: "Denied by user" }` to the LLM. The LLM sees this and can adjust its approach within the same turn.

After processing all decisions, if the turn's status returns to `"running"`, `spawn_turn_loop` is called again to resume the agentic loop.

### Security Considerations

- **API keys never reach the frontend.** The `GET /api/config` endpoint returns `PublicProvider` structs that strip `base_url` and `api_key`.
- **No authentication middleware.** The system is single-user -- `main.rs` only applies `CorsLayer`. No JWT, no login, no RBAC.
- **Synthetic errors for denied/unknown tools.** The LLM receives structured error messages that let it recover rather than crashing the loop.
- **`ON DELETE CASCADE` on sessions.** Deleting a session removes all associated turns and their data. No orphaned data.

### Interaction Flow Diagram

```
User sends message
  |
  v
POST /turns -> turn created, loop starts
  |
  v
LLM responds with tool calls
  |
  v
Permission resolution per call
  |
  +-- Auto-approved -> execute, continue loop
  |
  +-- Needs approval -> SSE: approval_needed
                          |
                          v
                    Frontend shows buttons
                          |
                    User clicks Allow/Always/Deny
                          |
                          v
                    POST /approve
                          |
                          v
                    Process decisions, resume loop
                          |
                          v
                    LLM continues or completes
```

## Key Takeaways

- **Three-layer resolution** (existence -> rules -> default) handles the common cases automatically while still allowing per-call user control. Most safe operations never prompt the user.
- **`AllowAlways` with session-scoped rules** is the key UX feature. It lets users progressively teach the system their trust boundaries without a separate settings page.
- **Loop pausing via DB persistence** (not blocking) is essential for the async architecture. The spawned task saves state and returns; a new task picks up where it left off when the user responds.
- **Wildcard rules for `bash`** strike a balance between security and convenience. `bash(cargo check *)` is specific enough to be safe but flexible enough to avoid repeated prompts.
- **Deny is not terminal.** Returning a synthetic error to the LLM (rather than failing the turn) lets the agent recover -- it can try a different approach. This is critical for good agentic behavior.
- **No auth in a single-user tool.** Permissions are about tool safety, not access control. Keeping the system simple avoids over-engineering.

## References

- `fork-chat-backend/src/tooling.rs` -- Tool definitions, policies, allow-rule matching, tool execution, `derive_allow_rule`
- `fork-chat-backend/src/turn_lifecycle.rs` -- Turn loop with permission resolution, approval/cancel lifecycle logic
- `fork-chat-backend/src/handlers/turns.rs` -- Thin HTTP handlers that map API payloads to lifecycle service calls
- `fork-chat-backend/src/turn_runtime.rs` -- Runtime state keys (pending_tool_calls, approval_decisions)
- `fork-chat-backend/src/turn_task_manager.rs` -- Per-turn cancellation token registry
- `fork-chat-backend/src/config.rs` -- Provider config with API keys and protocol bindings
- `fork-chat-backend/src/handlers/config.rs` -- Public config endpoint (tools + providers, no secrets)
- `fork-chat-frontend/src/components/TurnDetailModal.tsx` -- Approval UI (Allow / Always allow / Deny buttons)
- `fork-chat-frontend/src/api/types.ts` -- `ApproveDecisionKind` type (`allow` / `allow_always` / `deny`)
- `fork-chat-frontend/src/api/turnStream.ts` -- Turn status constants including `AWAITING_APPROVAL`
- `specs/tool-use.md` -- Design doc for the permission model
