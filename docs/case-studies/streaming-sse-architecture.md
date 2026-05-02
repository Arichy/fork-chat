# Streaming SSE Architecture: Snapshot + Live with Persisted Sequence Boundaries

> A per-turn SSE design where `POST /turns` returns immediately, the background
> loop persists materialized turn state, and subscribers receive one full
> snapshot followed by strictly newer live events.

## Problem

We needed real-time UI updates for a multi-round turn lifecycle:

- creating a turn should give the frontend a stable handle for that turn
- the backend may keep working for seconds or minutes after creation
- a turn can pause for approval, resume later, complete, or fail
- reconnecting clients must recover the current UI state without ambiguity

That still leaves an API design choice open: we could make `POST /turns`
return quickly with JSON and attach streaming separately, or we could make the
creation request itself a long-lived streaming response.

The system therefore needs two things at once:

1. a durable source of truth for the current turn state
2. a low-latency push channel for in-flight updates

## Why It's Hard

- **A turn is long-lived.** One HTTP request creates the turn, but the actual
  work happens in a spawned task that may outlive the request by a lot.
- **Reconnects are normal.** The UI may refresh, the SSE connection may drop,
  or the user may reopen a finished turn later.
- **Ordering matters.** A subscriber must not miss updates that happen around
  the moment it attaches to the stream.
- **Snapshot/live handoff is subtle.** If one design reads a persisted snapshot
  first and only then subscribes to live delivery, an event can land in the
  middle: too new for the snapshot, but too early for the subscriber's live
  channel. Any replay-based design needs an explicit ordering boundary that
  closes this gap.
- **The UI cares about state, not just events.** The frontend needs the latest
  `status`, transcript, pending approvals, token usage, and errors. A stream of
  deltas without a durable baseline is awkward to recover from.

## Alternatives Considered

### Option A: Long-lived `POST /turns` stream

Have `POST /turns` create the turn and keep the HTTP response open as the live
stream for that same turn.

- **Pros:** Creation and observation happen in one request. The client does not
  need to immediately make a second call just to start receiving progress.
- **Cons:** It couples two different concerns: creating a resource and
  subscribing to updates for that resource. Reconnects still need a separate
  recovery path, because a dropped POST stream cannot be resumed. It also makes
  later subscribers, page refreshes, and "open an existing turn detail view"
  less natural, because the canonical stream endpoint still has to exist
  anyway.

We rejected this because it does not actually remove the need for a standalone
per-turn stream contract; it only makes the initial happy path look shorter
while leaving reconnect and re-subscribe behavior to a second mechanism.

### Option B: Poll the turn row

Keep the turn row authoritative and have the frontend poll `GET /turns/:id`.

- **Pros:** Very simple backend model. No live channel, no stream ordering
  problem, no reconnect protocol.
- **Cons:** Worse UX for multi-step turns. Polling adds latency, redundant load,
  and awkward handling for approval prompts and incremental progress.

### Option C: Dedicated persisted event log plus replay

Persist every stream event into a first-class `turn_events` table, then replay
the event log to late subscribers before switching them to live delivery.

- **Pros:** Subscribers can observe the full historical transition sequence.
  This is the right design if event history itself is a product requirement.
- **Cons:** Higher write amplification and more schema surface area. The system
  must now maintain both materialized turn state and a durable event log, then
  define exactly how replay and live handoff work.

The handoff detail is the critical part. A replay design is only sound when the
replay source itself provides the ordering boundary for live takeover. A weaker
shape, where the server reads a mutable turn snapshot or history blob and then
subscribes to an in-memory broadcast channel, has a race:

1. load snapshot/history
2. an event is persisted and broadcast
3. subscribe to live delivery

That event is now in neither place for this subscriber: it was too new for the
snapshot/history that was already read, and too early for the live
subscription. Because of this, we would only consider replay again via a
dedicated event table with durable sequence ordering, not via ad hoc replay
from turn state plus in-memory broadcast.

### Option D: Snapshot + live SSE with persisted `stream_seq` (Chosen)

Persist the latest turn state in `turns`, keep live delivery in memory via a
per-turn broadcast hub, and persist one monotonic `stream_seq` counter so the
subscriber can distinguish "already in the snapshot" from "happened after the
snapshot".

- **Pros:** Clean split of responsibilities. The database stores durable state;
  the hub only pushes current activity. Reconnect logic stays simple because one
  full snapshot is authoritative.
- **Cons:** A late subscriber sees the current state, not every historical
  intermediate event as separate frames.

We chose this because the product needs accurate current state more than a
first-class historical event ledger.

## Solution

### Architecture Overview

```
Frontend                          Backend
   |                                 |
   |--- POST /turns --------------->|  persist turn, spawn loop, return
   |<-- { turn } -------------------|
   |                                 |
   |--- GET /turns/:id/stream ----->|  subscribe / snapshot / live
   |<-- event: turn_snapshot --------|  full persisted state
   |<-- event: round_started --------|  only seq > baseline_seq
   |<-- event: assistant_entry ------|
   |<-- event: tool_calls -----------|
   |<-- event: tool_result ----------|
   |<-- event: approval_needed ------|
   |<-- event: turn_completed -------|  terminal; stream closes
```

### 1. The turn row is the durable source of truth

`TurnLifecycleService::create_turn`
(`fork-chat-backend/src/turn_lifecycle.rs`) creates the turn row, appends the
initial user transcript entry, bumps `runtime_state.stream_seq`, spawns the
background loop, and returns immediately.

The turn row carries the state the UI actually needs to recover:

- `status`
- `turn_messages`
- `assistant_text`
- token usage
- `runtime_state.pending_tool_calls`
- `error`

### 2. Live events are published only after state is persisted

Each lifecycle step follows the same ordering:

1. update the persisted turn row
2. bump and persist `runtime_state.stream_seq`
3. publish the live event through `TurnStreamHub`

That write-before-publish invariant means every emitted sequence number refers
to state that is already durable.

### 3. SSE subscription bridges snapshot and live with `baseline_seq`

`stream_turn_handler` (`fork-chat-backend/src/handlers/turns.rs`) handles
subscription in two shapes:

- **terminal turn:** load the turn, emit one `turn_snapshot`, finish
- **active turn:** subscribe first, then read the turn snapshot, emit it, and
  forward only live events whose `seq > baseline_seq`

The key logic is:

```rust
let initial_turn = get_turn_in_session(&state.db, session_id, turn_id).await?;
let mut live_rx = None;
let turn = if turn_status::is_terminal(&initial_turn.status) {
    initial_turn
} else {
    live_rx = Some(state.turn_stream_hub.subscribe(turn_id).await);
    get_turn_in_session(&state.db, session_id, turn_id).await?
};
let baseline_seq = turn.runtime_state.stream_seq;
```

This gives the subscriber a clean boundary:

- if an update happens before the snapshot read, the snapshot includes it
- if an update happens after the snapshot read, its `seq` is greater than the
  baseline and it is forwarded live
- if an older event is still buffered in the receiver, the `seq` guard drops it

### 4. `runtime_state` stores runtime control state, not stream history

`TurnRuntimeState` (`fork-chat-backend/src/turn_runtime.rs`) is the per-turn
backend control-state blob persisted inside `turns.runtime_state`.

The simplest way to think about it is:

- **top-level turn columns** store the turn's durable product state
  (`status`, `turn_messages`, `assistant_text`, token usage, `error`)
- **`runtime_state`** stores the extra runtime control state the backend needs in
  order to safely continue, resume, or reconnect while that turn is in flight

It is not a second transcript and not a replay log. It is the small amount of
"state machine baggage" that does not belong in the user-facing transcript but
still must survive process restarts, approval round-trips, and SSE reconnects.

We keep it as one typed JSONB blob because:

- the fields are always loaded together with the turn row
- they evolve with the lifecycle implementation, not with query/reporting needs
- we do not need SQL-level filtering on individual runtime fields

Today it contains exactly three fields:

#### `stream_seq`

`stream_seq` is the monotonic sequence counter for this turn.

- It is bumped whenever the backend makes a new stream-visible transition
  durable.
- The SSE snapshot includes the latest `stream_seq`.
- The live stream only forwards events whose `seq` is newer than the snapshot's
  baseline.

In practice this means `stream_seq` is the handoff boundary between "already
reflected in the snapshot" and "must still be delivered live".

It is important that `stream_seq` does **not** mean "number of transcript
messages". It counts backend lifecycle transitions that matter to the stream.

For example, a turn that:

1. stores the initial user message
2. receives an assistant response containing a tool call
3. pauses for approval
4. executes the approved tool
5. appends the tool result
6. calls the model again
7. appends the final assistant answer
8. completes

can easily end with a `stream_seq` much larger than the 4 transcript entries a
human might count (`user`, `assistant-with-tool-call`, `user-tool-result`,
`assistant-final`).

One concrete sequence for that shape is:

1. `turn_started`
2. `round_started`
3. `assistant_entry_appended`
4. `approval_needed`
5. `tool_calls`
6. `tool_result_appended`
7. `round_started`
8. `assistant_entry_appended`
9. `turn_completed`

So `stream_seq = 9` is perfectly normal for a turn whose transcript only has a
few entries.

#### `pending_tool_calls`

`pending_tool_calls` is the persisted list of tool calls currently waiting for
user approval.

- It is filled when the loop discovers tool calls that require approval and
  transitions the turn to `awaiting_approval`.
- It is cleared when those calls are resolved, or when the turn reaches a
  terminal state.

This field exists because approval is not handled inside one long-running stack
frame. The loop persists the pending calls, returns, and later a separate
`POST /approve` request reloads the row and continues from those exact calls.

#### `approval_decisions`

`approval_decisions` records previously accepted or rejected approval actions,
keyed by `pending_call_id`.

- It is updated when the user submits `allow`, `allow_always`, or `deny`.
- It stays persisted even after the active pending list shrinks.

Its main job is idempotency and conflict detection: if the frontend retries an
approval request, or sends a decision for a call that was already processed, the
backend can tell whether that request is a safe duplicate or a real conflict.

#### Typical shapes

Early in a running turn:

```json
{
  "stream_seq": 3,
  "pending_tool_calls": [],
  "approval_decisions": {}
}
```

Paused for approval:

```json
{
  "stream_seq": 8,
  "pending_tool_calls": [
    {
      "pending_call_id": "pcall_123",
      "call_id": "call_abc",
      "name": "bash",
      "input": { "command": "cargo check" }
    }
  ],
  "approval_decisions": {}
}
```

After one approval decision has been recorded:

```json
{
  "stream_seq": 9,
  "pending_tool_calls": [],
  "approval_decisions": {
    "pcall_123": "allow"
  }
}
```

Notice the pattern:

- transcript and final text stay in `turn_messages` / `assistant_text`
- approval workflow bookkeeping stays in `runtime_state`
- no historical SSE event list is persisted here

### 5. The frontend treats `turn_snapshot` as authoritative

`useTurnStream` (`fork-chat-frontend/src/hooks/useTurnStream.ts`) already fits
this contract:

- apply `turn_snapshot` as the full source of truth
- remember the highest seen `seq`
- ignore any incoming live event whose `seq` is not newer

That makes reconnect behavior straightforward: fetch one fresh snapshot, then
continue from strictly newer live events.

## Event Types

The live event taxonomy remains:

- `turn_snapshot`: emitted by the SSE handler immediately after subscribe. This
  is not part of the background loop; it is the handler's "here is the latest
  persisted truth for this turn" bootstrap event.
- `turn_started`: emitted once when a new turn row (or retry turn row) has been
  initialized, the initial user transcript entry has been persisted, and the
  background loop is about to start.
- `round_started`: emitted once per model-call round. It marks that the loop is
  beginning another LLM round for this turn. In the current implementation it
  is published together with the persisted assistant entry for that round, so
  subscribers usually see it immediately before `assistant_entry_appended`.
- `assistant_entry_appended`: emitted after one assistant transcript entry has
  been appended to `turn_messages` and persisted. That entry may contain plain
  assistant text, reasoning/thinking blocks, tool calls, or any other
  protocol-native assistant content for the round.
- `tool_calls`: emitted after the backend has decided which tool calls will now
  execute, but before their results have been appended. This can happen either
  because the calls were auto-allowed by policy/rules or because the user just
  approved them.
- `tool_result_appended`: emitted after one tool-result transcript entry has
  been appended and persisted. The payload contains the exact user-side
  transcript entry that feeds the next LLM round, including both successful
  tool output and synthetic denied/unknown-tool results.
- `approval_needed`: emitted when the loop finds tool calls that require user
  approval, persists them into `runtime_state.pending_tool_calls`, and moves
  the turn into `awaiting_approval`.
- `turn_completed`: emitted when the turn has reached a terminal successful
  state and `status = completed` has already been persisted.
- `turn_failed`: emitted when the turn has reached a terminal failed state and
  `status = failed` plus structured error payload have already been persisted.
  This covers provider errors, cancellation, loop-limit exhaustion, and similar
  terminal failures.

Two patterns are worth remembering:

1. events are emitted **after** the corresponding durable turn update
2. not every event maps 1:1 to a transcript entry; some events describe
   lifecycle transitions (`turn_started`, `round_started`, `approval_needed`,
   `turn_completed`, `turn_failed`) rather than new transcript content

## Key Takeaways

- Use the database for **durable state** and SSE for **ephemeral push**.
- If the UI fundamentally wants current state, one authoritative snapshot is
  more useful than replaying a pile of historical deltas.
- A persisted monotonic sequence number is enough to bridge snapshot and live
  delivery safely.
- If full historical event replay becomes a real requirement later, model it as
  a dedicated `turn_events` table rather than mixing it into turn runtime state.

## References

- `fork-chat-backend/src/turn_lifecycle.rs` -- background turn loop and
  sequence persistence
- `fork-chat-backend/src/handlers/turns.rs` -- SSE endpoint and
  snapshot/live handoff
- `fork-chat-backend/src/turn_stream.rs` -- per-turn broadcast hub
- `fork-chat-backend/src/turn_runtime.rs` -- persisted runtime control state
- `fork-chat-frontend/src/hooks/useTurnStream.ts` -- snapshot application and
  live event deduplication
