# Streaming SSE Architecture: POST-Return + Snapshot + Live Events

> A two-phase SSE design where POST returns immediately, a spawned tokio task
> runs the turn loop, and SSE clients receive one persisted snapshot followed by
> live events from an in-memory broadcast hub.

## Problem

We needed real-time UI updates for a multi-round agentic turn. A single turn can
run for minutes, pause for tool approval, resume, or fail. The frontend needs:

- an immediate `turn_id` from `POST /turns`
- a complete current view after reconnect
- live incremental updates while the turn is still active

The original design tried to solve this with three layers at once: persisted
turn state, persisted stream-event history, and live broadcast. That made the
contract harder to reason about and introduced a race window between "read the
snapshot" and "subscribe to live events".

## Chosen Model

We simplified the contract to:

1. **Persist the latest turn state in the `turns` row**
2. **Broadcast live events only to currently connected subscribers**
3. **Use a persisted `stream_seq` to stitch snapshot and live together safely**

This keeps the database responsible for durable state and the hub responsible
for ephemeral push delivery.

## Why This Shape

### Why not DB-backed event replay?

Persisting a replay buffer inside `runtime_state` duplicated information that
was already present in the materialized turn row:

- `status`
- `turn_messages`
- `pending_tool_calls`
- `error`
- token usage

Late-connecting clients do not need every historical intermediate event if they
can reconstruct the current UI from one full snapshot. Replaying old events on
top of a current snapshot mostly added deduplication complexity.

### Why keep `stream_seq`?

Even without replay, we still need a boundary between:

- the state already reflected in the snapshot
- live events that happened after that snapshot

Persisting `runtime_state.stream_seq` gives us that boundary. The SSE handler
records the snapshot's `baseline_seq`, then ignores any live event whose
sequence is `<= baseline_seq`.

## Request Flow

```
Frontend                          Backend
   |                                 |
   |--- POST /turns --------------->|  create_turn_handler
   |<-- { turn } -------------------|  (persist turn, spawn task, return)
   |                                 |
   |--- GET /turns/:id/stream ----->|  stream_turn_handler
   |                                 |
   |<-- event: turn_snapshot --------|  full persisted state
   |<-- event: round_started --------|  live events only
   |<-- event: assistant_entry ------|
   |<-- event: tool_calls -----------|
   |<-- event: tool_result ----------|
   |<-- event: approval_needed ------|
   |                                 |
   |--- POST /approve ------------->|  separate endpoint
   |                                 |
   |<-- event: round_started --------|
   |<-- event: turn_completed -------|  terminal; stream closes
```

## Backend Responsibilities

### 1. POST returns immediately

`TurnLifecycleService::create_turn`
(`fork-chat-backend/src/turn_lifecycle.rs`) persists the new turn row, stores
the initial user transcript entry, bumps `stream_seq`, spawns the background
loop, and returns the created turn immediately.

The HTTP request never waits for the model call to finish.

### 2. Background loop persists state before publishing

Every lifecycle step follows the same pattern:

1. Update the turn row in PostgreSQL
2. Bump and persist `runtime_state.stream_seq`
3. Publish the live SSE event through `TurnStreamHub`

That write-before-publish ordering is the key invariant. It means a snapshot is
always at least as new as every sequence number it claims to include.

### 3. SSE handler subscribes before taking the active snapshot

`stream_turn_handler` (`fork-chat-backend/src/handlers/turns.rs`) does:

1. Read the turn once
2. If already terminal, emit a single `turn_snapshot` and finish
3. Otherwise subscribe to the per-turn broadcast channel
4. Read the turn again to obtain the active snapshot and `baseline_seq`
5. Emit `turn_snapshot`
6. Forward only live events where `event.seq > baseline_seq`

This closes the old race:

- if an update happens before the second DB read, the snapshot includes it
- if it happens after the second DB read, the live event has a larger `seq`
- if a stale event is still sitting in the receiver buffer, it is ignored by
  the `seq > baseline_seq` guard

## Runtime State

`TurnRuntimeState` (`fork-chat-backend/src/turn_runtime.rs`) now stores only
durable execution metadata:

- `approval_decisions`
- `pending_tool_calls`
- `stream_seq`

It deliberately does **not** persist historical SSE events anymore.

## Frontend Contract

`useTurnStream` (`fork-chat-frontend/src/hooks/useTurnStream.ts`) already had
the right client-side shape for this model:

- apply `turn_snapshot` as the full source of truth
- keep the highest seen `seq` per turn
- ignore any live event whose `seq` is not newer

This makes reconnect straightforward: fetch one fresh snapshot, then continue
with strictly newer live events.

## Event Types

The live event taxonomy is unchanged:

- `turn_started`
- `round_started`
- `assistant_entry_appended`
- `tool_calls`
- `tool_result_appended`
- `approval_needed`
- `turn_completed`
- `turn_failed`

`turn_snapshot` remains a synthetic SSE event emitted by the handler itself on
subscribe; it is not stored as part of turn runtime state.

## Tradeoffs

### Pros

- Cleaner split: DB stores state, hub streams "now"
- Smaller `runtime_state`
- Fewer synchronization paths than DB replay + broadcast
- Reconnect logic stays simple because snapshot is authoritative

### Cons

- A late-connecting client does not see every historical intermediate event as
  separate SSE frames; it sees their *result* in the snapshot
- Live delivery is still bounded by the broadcast channel's in-memory buffer
  for currently connected subscribers

We accepted those tradeoffs because the UI cares about current state fidelity
more than preserving every past transition as a first-class stream artifact.

## Key Takeaways

- Persist **state**, not redundant stream history, unless you truly need a
  first-class event log.
- `stream_seq` is enough to safely bridge one snapshot to subsequent live
  events.
- Subscribe-first plus write-before-publish is the simple race-free shape for
  SSE over mutable state.
- If historical event replay becomes a product requirement later, it should
  live in a dedicated `turn_events` table rather than inside
  `turns.runtime_state`.

## References

- `fork-chat-backend/src/turn_lifecycle.rs` -- background loop and stream-seq
  persistence
- `fork-chat-backend/src/handlers/turns.rs` -- snapshot + live SSE endpoint
- `fork-chat-backend/src/turn_stream.rs` -- per-turn broadcast hub
- `fork-chat-backend/src/turn_runtime.rs` -- durable runtime state
- `fork-chat-frontend/src/hooks/useTurnStream.ts` -- snapshot application and
  seq deduplication
