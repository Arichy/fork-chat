# Tool Use

## Context

Every turn in fork-chat is currently a single request/response exchange with
one upstream call. To support flows like "read `package.json`, then bump the
patch version and write it back", the assistant must be able to request tool
calls, receive their results, and continue reasoning — possibly for several
rounds — **inside a single turn**.

This document defines how OpenAI function calling and Anthropic tool use are
modelled, persisted, and streamed, without breaking the tree invariants from
`init.md` or the protocol invariants from `multi-protocol.md`.

## Core Invariants

- One turn = one tree node = one `turns` row = one tool-use loop.
- The tree shape is decided by the user (forks, retries). Tool rounds do **not**
  create new tree nodes — they extend the transcript of an existing node.
- `turn_messages` stays **protocol-native** and **append-only per round**. After
  every round (assistant reply or tool result batch) it is persisted before the
  next round begins.
- Tool schemas, permissions, and execution are **backend-owned**. The frontend
  is never asked to execute a tool.
- While a turn is non-terminal (`running` or `awaiting_approval`), that turn
  itself is locked in the UI: no fork/retry/new-child from that node until it
  terminates. Other turns in the same session may continue concurrently.

## Why This Shape

### One turn, not one-turn-per-round

Treating each round as a separate tree node would explode the graph with
scaffolding nodes the user never authored and make "fork from here" meaningless
(fork from which internal round?). It would also split token accounting across
rows that nobody will ever edit independently. We keep the user's mental model:
a turn is a thing the user said followed by the final assistant answer, even
when that answer took several internal steps.

### Append-only persistence per round

The upstream call for a single turn can take minutes in the worst case (e.g. a
long `bash` step). A crash during round N must not erase the output of rounds
`1..N-1`. Appending after each round keeps the database the source of truth
and lets a reconnecting client replay the whole transcript so far.

### Backend-native tools, not MCP or client execution

- The first tools we need — `read`, `write`, `bash` — are local to the machine
  running the backend. Shipping them in-process is the shortest path from zero
  to a working coding agent.
- Client-side execution would require a second protocol (request/response for
  tool invocation) and a trusted browser sandbox. Out of scope.
- MCP is a natural follow-up: a third policy that delegates schema and
  execution to an external MCP server. It fits the `Tool` abstraction below
  but is not in v1.

### Round-level streaming, not token streaming

The interesting visible events are "the model called a tool", "the tool
produced output", "the model wrote the final answer". Token streaming can be
added later without schema changes; round streaming can not.

## Built-in Tool Set

v1 ships three tools. Each exposes a JSON Schema consumed by both protocols
(OpenAI function tool definitions and Anthropic `tools[]`):

| Tool    | Inputs                                  | Default policy      |
| ------- | --------------------------------------- | ------------------- |
| `read`  | `path`                                  | auto                |
| `write` | `path`, `content`                       | require_approval    |
| `bash`  | `command`, `cwd?`, `timeout_sec?`       | require_approval    |

Tool output is always a UTF-8 string. Errors are surfaced as tool results with
an `is_error` flag so the model can recover instead of failing the turn.

The set is fixed in code, registered in a single place, and exposed on
`GET /api/config` (name, description, default policy). Filesystem sandboxing
and working-directory isolation are out of scope for v1.

## Turn Model: Multi-Round Loop

A turn starts in `running` and proceeds:

1. The orchestrator calls the adapter with the current `turn_messages` plus the
   tool catalogue.
2. The adapter returns a **round result**: the next assistant entry (native
   content blocks) plus a possibly-empty list of tool calls.
3. The assistant entry is appended to `turn_messages` and persisted.
4. If there are no tool calls, the turn moves to `completed` and the loop
   ends.
5. Otherwise each tool call is checked against the permission policy
   (see below). If any call needs approval, the turn moves to
   `awaiting_approval`, the pending calls are persisted, and the loop
   suspends until the user approves or denies each one.
6. Approved calls are executed (parallel by default). Their results are
   bundled into a single user-role entry with `tool_result` blocks and
   appended to `turn_messages`.
7. Go back to step 1.

The loop has two hard limits: `max_rounds` and a soft cap on cumulative output
tokens. Hitting either fails the turn with a machine-readable error kind.

## Turn Status

Extended from the `multi-protocol.md` set:

- `running` — loop active, no approval needed right now.
- `awaiting_approval` — loop suspended pending user decision on one or more
  tool calls. The pending calls live in a dedicated turn-scoped field so a
  reconnecting client can render the approval prompt without re-parsing the
  transcript.
- `completed` — loop ended cleanly (no more tool calls).
- `failed` — loop aborted (upstream error, cancel, loop-limit exceeded,
  abandoned by a crashed orchestrator).

## `turn_messages` Shape

`turn_messages` is still the protocol-native, ordered transcript defined in
`multi-protocol.md`. The only change is that it can now contain more than two
entries for a single turn.

Anthropic example (two rounds, one tool call each):

```json
[
  { "role": "user", "content": [{ "type": "text", "text": "bump patch version" }] },
  {
    "role": "assistant",
    "content": [
      {
        "type": "tool_use",
        "id": "toolu_1",
        "name": "read",
        "input": { "path": "package.json" }
      }
    ],
    "response_id": "msg_1",
    "stop_reason": "tool_use",
    "usage": { "...": "..." }
  },
  {
    "role": "user",
    "content": [
      {
        "type": "tool_result",
        "tool_use_id": "toolu_1",
        "content": "{\"version\":\"1.2.3\",...}"
      }
    ]
  },
  {
    "role": "assistant",
    "content": [
      {
        "type": "tool_use",
        "id": "toolu_2",
        "name": "write",
        "input": { "path": "package.json", "content": "..." }
      }
    ],
    "response_id": "msg_2",
    "stop_reason": "tool_use"
  },
  {
    "role": "user",
    "content": [
      {
        "type": "tool_result",
        "tool_use_id": "toolu_2",
        "content": "ok"
      }
    ]
  },
  {
    "role": "assistant",
    "content": [{ "type": "text", "text": "Patched to 1.2.4." }],
    "response_id": "msg_3",
    "stop_reason": "end_turn"
  }
]
```

OpenAI is the same shape with `function_call` assistant items and
`function_call_output` input items instead of `tool_use` / `tool_result`.

The message-builder layer already round-trips unknown content blocks as raw
JSON values (verified by the existing Anthropic transcript tests), so replay
through the tool loop needs no translation — each round is reassembled from
the transcript as-is.

## Permissions Model

Inspired by modern coding agents. Allow-list entries are **rules**, not only
bare tool names.

Rule syntax:

- `tool_name` — match all calls of this tool (for example, `read`).
- `tool_name(specifier)` — match only calls whose arguments satisfy the
  tool-specific matcher.

v1 tool-specific matcher:

- `bash(command pattern)` where pattern supports `*` wildcard (for example,
  `bash(cargo check *)`, `bash(* --help)`).
- Matching is applied to the model-provided `command` string.
- `*` matches any character sequence (including spaces).
- Matching is full-string against the pattern (with wildcard expansion), not
  regex.

Resolution order for each tool call:

1. If the tool name is unknown, do not execute it. Generate a synthetic
   `tool_result` with `is_error: true` and `error.kind = "unknown_tool"`.
2. If any session allow-rule matches this call, auto-approve.
3. Otherwise, use the global default policy for that tool:
   `auto` -> run immediately, `require_approval` -> suspend the turn.

Approval responses per pending call:

- **Allow** — run this call only.
- **Allow always** — run it and append a new session allow-rule derived from
  this call.
- **Deny** — do not run it. The orchestrator fabricates a `tool_result` with
  `is_error: true` and a standard deny message so the model can recover inside
  the same turn.

Rule materialization for **Allow always**:

- `read`, `write`: store bare tool rule (`read`, `write`).
- `bash`: store `bash(<command pattern>)`.
  - Default pattern is the exact command string.
  - UI may offer "widen pattern" controls (for example from
    `cargo check --workspace` to `cargo check *`) before submitting
    `allow_always`.

Allow-rules are **per session**, not global, so one session trusting
`bash(cargo check *)` does not silently grant unrelated sessions.

## Streaming Model

Creating a turn no longer blocks on the LLM. The create-turn endpoint returns
as soon as the row is persisted with `status=running`. Clients subscribe to
a per-turn SSE stream for progress.

Event vocabulary:

- `turn_started` — emitted once when a new turn row is initialized and the
  background loop is about to start.
- `round_started` — emitted at the start of each LLM call round; payload
  includes `{ "round": N }`.
- `turn_snapshot` — first event on every subscription (including reconnect);
  carries the current persisted `turn_messages`, `status`, and pending
  approvals.
- `assistant_entry_appended` — emitted after each round's assistant entry is
  persisted; carries the new entry so clients can append it to their in-memory
  transcript.
- `tool_calls` — tool calls the orchestrator is about to execute
  (post-approval, pre-execution).
- `approval_needed` — turn just transitioned to `awaiting_approval`; carries
  the pending calls.
- `tool_result_appended` — a tool-result user entry was persisted.
- `turn_completed` / `turn_failed` — terminal events; clients unsubscribe.

Each SSE event includes a monotonically increasing `seq` per turn. Clients
must de-duplicate by `seq` and apply only unseen events.

On every subscribe (including reconnect) the server first emits
`turn_snapshot` from the database, then attaches the client to the live
broadcast if the turn is still active.

## API Surface

New or changed endpoints:

- `POST /api/sessions/:sid/turns` — returns immediately with the new turn row
  in `status=running`; the tool loop runs in the background.
- `GET  /api/sessions/:sid/turns/:tid/stream` — SSE, events above.
- `POST /api/sessions/:sid/turns/:tid/approve` — body carries a decision per
  pending tool call (`allow` / `allow_always` / `deny`).
  - Each decision targets one `pending_call_id` generated and persisted when
    entering `awaiting_approval`.
  - The endpoint is idempotent: repeated decisions with the same
    `pending_call_id` + decision are no-op success.
  - Deciding a non-pending or unknown id returns `409` with machine-readable
    reason.
- `POST /api/sessions/:sid/turns/:tid/cancel` — marks the turn failed
  (`error.kind = "cancelled"`) and stops the orchestrator.
- `GET  /api/config` — gains `tools[]` (name, description, default policy).

`approve` request body example:

```json
{
  "decisions": [
    { "pending_call_id": "pcall_1", "decision": "allow" },
    { "pending_call_id": "pcall_2", "decision": "allow_always" },
    { "pending_call_id": "pcall_3", "decision": "deny" }
  ]
}
```

Retry (`POST .../turns/:tid/retry`) follows the same async + SSE contract as
create.

## Frontend Contract

- A non-terminal turn is locally locked: no fork/retry/new-child from that
  node until it terminates. Other nodes remain operable, including creating
  concurrent turns elsewhere in the same session tree.
- A small spinner marks `running`; a lock icon marks `awaiting_approval`; a
  wrench icon marks turns whose transcript contains tool blocks.
- The turn detail modal renders the transcript as an ordered list of blocks
  instead of two markdown blobs:
  - user text / tool-result blocks (collapsible, red border when `is_error`)
  - assistant text / tool_use / function_call blocks (collapsible input JSON
    for tool invocations)
- When `status === 'awaiting_approval'`, the modal shows one approval prompt
  per pending call with three buttons: **Allow**, **Always allow this tool**,
  **Deny**. The existing MessageInput is hidden until the turn terminates.
- `MessageInput` remains disabled as long as the parent turn is non-terminal.

There is no global "settings" surface for permissions in v1; the only way to
grow session allow-rules is to click **Always allow this tool** in an
approval prompt.

## Lifecycle & Recovery

- The orchestrator keeps per-turn state in memory (SSE broadcaster, approval
  signal, cancel token). It does **not** try to survive a backend restart.
- On startup, a reaper scans for `running` or `awaiting_approval` turns that
  no live orchestrator owns and marks them failed with
  `error.kind = "abandoned"`. This keeps the UI honest after a crash.
- Cancelling is cooperative: in-flight tool execution is interrupted where
  possible, and the cancel is recorded before the orchestrator exits.

## Test Coverage

Backend:

- Built-in tool happy paths and error surfaces (`read`, `write`, `bash`).
- OpenAI multi-round loop against wiremock scripting `function_call` → final
  text, verifying per-round DB append.
- Anthropic multi-round loop against wiremock scripting
  `tool_use` → `tool_use` → final text, same append verification.
- Approval flow: `write` triggers `awaiting_approval`; `approve`, `deny`, and
  `allow_always` each reach the expected terminal state and DB effects.
- Unknown tool call is converted into `tool_result.is_error=true` with
  `error.kind = "unknown_tool"` and the loop continues.
- SSE `turn_snapshot` + live events on reconnect, with `seq` de-dup behavior.
- Loop-limit exhaustion produces a failed turn with the documented error
  kind.
- `GET /api/config` exposes the new tool catalogue and default policies.

Frontend:

- Turn detail modal renders mixed transcripts (text + tool_use + tool_result,
  including `is_error`).
- Chat tree keeps non-terminal nodes locally locked while allowing concurrent
  work on other nodes.
- Chat store reducer consumes the SSE event vocabulary into the expected
  transcript state.
- MSW fixtures cover the new stream and approve endpoints.

## Out of Scope

- MCP (Model Context Protocol) servers.
- Filesystem / process sandboxing for `read`, `write`, `bash`.
- Token-level streaming inside a round.
- Global (cross-session) permissions UI or persistence.

## Pre-Commit Gate

Unchanged from `multi-protocol.md`:

Frontend:

- `pnpm format`
- `pnpm lint`
- `pnpm typecheck`
- `pnpm test`

Backend:

- `cargo fmt --all --check`
- `cargo clippy --all-targets --all-features -- -D warnings`
- `cargo nextest run` (or `cargo test` fallback)
