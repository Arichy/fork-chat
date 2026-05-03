# Batch Delete Sessions Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add checkbox-based batch selection and deletion to the session sidebar, backed by a new batch-delete API endpoint.

**Architecture:** New `POST /api/sessions/batch-delete` endpoint on the backend accepts an array of UUIDs and deletes them in one query. The frontend adds a selection mode toggle (pencil icon in sidebar header) that shows checkboxes on each row, a select-all control, and a bottom action bar for triggering batch delete with a confirmation dialog.

**Tech Stack:** Rust/axum/sqlx (backend), React/TanStack Query/shadcn (frontend), vitest + MSW (frontend tests), cargo nextest (backend tests)

---

## File Structure

| Action | File | Responsibility |
|--------|------|---------------|
| Modify | `fork-chat-backend/src/db/sessions.rs` | Add `batch_delete_sessions` DB function |
| Modify | `fork-chat-backend/src/db/mod.rs` | Re-export `batch_delete_sessions` |
| Modify | `fork-chat-backend/src/handlers/sessions.rs` | Add `BatchDeleteRequest`, `BatchDeleteResponse`, `batch_delete_sessions_handler` |
| Modify | `fork-chat-backend/src/handlers/mod.rs` | Re-export `batch_delete_sessions_handler` |
| Modify | `fork-chat-backend/src/routes.rs` | Mount `POST /api/sessions/batch-delete` |
| Create | `fork-chat-backend/tests/batch_delete.rs` | Backend integration tests |
| Add | `fork-chat-frontend/src/components/ui/checkbox.tsx` | shadcn Checkbox component (needed for row selection) |
| Modify | `fork-chat-frontend/src/api/types.ts` | Add `BatchDeleteRequest`, `BatchDeleteResponse` types |
| Modify | `fork-chat-frontend/src/api/client.ts` | Add `api.sessions.batchDelete` method |
| Modify | `fork-chat-frontend/src/test/msw/handlers.ts` | Add MSW handler for batch delete |
| Modify | `fork-chat-frontend/src/routes/__root.tsx` | Selection mode state + UI changes in SessionSidebar |
| Create | `fork-chat-frontend/src/routes/__root.test.tsx` | Frontend tests for selection mode |

---

### Task 1: Backend — `batch_delete_sessions` DB function

**Files:**
- Modify: `fork-chat-backend/src/db/sessions.rs:148` (after `delete_session`)
- Modify: `fork-chat-backend/src/db/mod.rs:38` (re-export)

- [ ] **Step 1: Write the `batch_delete_sessions` function**

Add after `delete_session` in `fork-chat-backend/src/db/sessions.rs`:

```rust
/// Delete multiple sessions in a single query.
///
/// Uses `ANY($1)` to delete all sessions whose id is in the provided set.
/// The `ON DELETE CASCADE` foreign key on `turns.session_id` ensures all
/// associated turns are removed automatically. Returns the number of rows
/// actually deleted — this may be less than `ids.len()` if some ids don't
/// exist in the database.
///
/// Duplicate ids in the input are deduplicated by Postgres (`= ANY` treats
/// duplicates as a single match), so the caller doesn't need to dedup.
pub async fn batch_delete_sessions(db: &PgPool, ids: &[Uuid]) -> Result<u64> {
    let result = sqlx::query("DELETE FROM sessions WHERE id = ANY($1)")
        .bind(ids)
        .execute(db)
        .await
        .map_err(|e| AppError::DatabaseError(format!("Failed to batch delete sessions: {}", e)))?;
    Ok(result.rows_affected())
}
```

- [ ] **Step 2: Re-export from `db/mod.rs`**

Add `batch_delete_sessions` to the re-export list in `fork-chat-backend/src/db/mod.rs`:

```rust
pub use sessions::{
    SessionSort, batch_delete_sessions, create_session, delete_session, get_session, list_sessions,
    touch_session_updated_at,
};
```

- [ ] **Step 3: Verify compilation**

Run: `cd fork-chat-backend && cargo check`
Expected: compiles without errors

- [ ] **Step 4: Commit**

```
feat(backend): add batch_delete_sessions DB function
```

---

### Task 2: Backend — Batch delete handler + route

**Files:**
- Modify: `fork-chat-backend/src/handlers/sessions.rs` (add handler + types)
- Modify: `fork-chat-backend/src/handlers/mod.rs` (re-export)
- Modify: `fork-chat-backend/src/routes.rs` (mount route)

- [ ] **Step 1: Add request/response types and handler**

Add to `fork-chat-backend/src/handlers/sessions.rs` (after the `UpdateSessionRequest` struct, before `update_session_handler`):

```rust
/// Request body for `POST /api/sessions/batch-delete`.
#[derive(Debug, Deserialize)]
pub struct BatchDeleteRequest {
    /// Session ids to delete. Must be non-empty and at most 100.
    pub ids: Vec<Uuid>,
}

/// Response body for `POST /api/sessions/batch-delete`.
#[derive(Debug, Serialize)]
pub struct BatchDeleteResponse {
    /// Number of sessions actually deleted (may be less than requested
    /// if some ids don't exist).
    pub deleted: u64,
}

/// `POST /api/sessions/batch-delete` — delete multiple sessions.
///
/// Validates the `ids` array (non-empty, max 100), then delegates to
/// `batch_delete_sessions` which runs a single `DELETE ... WHERE id = ANY($1)`.
/// The ON DELETE CASCADE on turns handles related data automatically.
pub async fn batch_delete_sessions_handler(
    State(state): State<AppState>,
    Json(req): Json<BatchDeleteRequest>,
) -> Result<Json<BatchDeleteResponse>, AppError> {
    if req.ids.is_empty() {
        return Err(AppError::BadRequest(
            "ids array must not be empty".to_string(),
        ));
    }
    if req.ids.len() > 100 {
        return Err(AppError::BadRequest(
            "ids array must contain at most 100 items".to_string(),
        ));
    }
    let deleted = batch_delete_sessions(&state.db, &req.ids).await?;
    Ok(Json(BatchDeleteResponse { deleted }))
}
```

Add the import at the top of `sessions.rs`:

```rust
use crate::db::{SessionSort, batch_delete_sessions, create_session, delete_session, get_session, list_sessions};
```

(Remove the old `delete_session`-only import line and replace with the one that includes `batch_delete_sessions`.)

- [ ] **Step 2: Re-export from handlers/mod.rs**

Update `fork-chat-backend/src/handlers/mod.rs`:

```rust
pub use sessions::{
    batch_delete_sessions_handler, create_session_handler, delete_session_handler,
    get_session_handler, list_sessions_handler, update_session_handler,
};
```

- [ ] **Step 3: Mount the route**

In `fork-chat-backend/src/routes.rs`, add the import:

```rust
use crate::handlers::{
    approve_turn_handler, batch_delete_sessions_handler, cancel_turn_handler,
    create_session_handler, create_turn_handler, delete_session_handler, get_config_handler,
    get_session_handler, get_session_tree_handler, get_turn_handler, list_sessions_handler,
    retry_turn_handler, stream_turn_handler, update_session_handler,
};
```

Add the route **before** the `/{id}` routes (axum matches top-down, so `/batch-delete` must come before `/{id}` to avoid being captured by the path param):

```rust
// Batch delete — must be mounted before /{id} to avoid path-param collision.
.route("/api/sessions/batch-delete", post(batch_delete_sessions_handler))
```

- [ ] **Step 4: Verify compilation**

Run: `cd fork-chat-backend && cargo check`
Expected: compiles without errors

- [ ] **Step 5: Commit**

```
feat(backend): add POST /api/sessions/batch-delete endpoint
```

---

### Task 3: Backend tests for batch delete

**Files:**
- Create: `fork-chat-backend/tests/batch_delete.rs`

- [ ] **Step 1: Write the tests**

Create `fork-chat-backend/tests/batch_delete.rs`:

```rust
mod common;

use common::spawn_app;
use serde_json::{Value, json};
use uuid::Uuid;

#[tokio::test]
async fn batch_delete_removes_multiple_sessions() {
    let app = spawn_app().await;
    let s1 = app.create_session(None).await;
    let s2 = app.create_session(None).await;
    let s3 = app.create_session(None).await;

    let resp = app
        .http
        .post(app.url("/api/sessions/batch-delete"))
        .json(&json!({ "ids": [s1, s2] }))
        .send()
        .await
        .unwrap();
    assert!(resp.status().is_success(), "status={}", resp.status());
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["deleted"], 2);

    // Verify the two deleted sessions are gone.
    for id in [s1, s2] {
        let get = app
            .http
            .get(app.url(&format!("/api/sessions/{id}")))
            .send()
            .await
            .unwrap();
        assert_eq!(get.status(), reqwest::StatusCode::NOT_FOUND);
    }
    // The third session should still exist.
    let get3 = app
        .http
        .get(app.url(&format!("/api/sessions/{s3}")))
        .send()
        .await
        .unwrap();
    assert!(get3.status().is_success());

    app.cleanup().await;
}

#[tokio::test]
async fn batch_delete_cascades_to_turns() {
    let app = spawn_app().await;
    let id = app.create_session(None).await;
    app.mock_openai_success("hi", "resp_batch_cascade").await;

    let turn_resp = app
        .http
        .post(app.url(&format!("/api/sessions/{id}/turns")))
        .json(&json!({
            "user_text": "hello",
            "provider": "openai",
            "model": "gpt-5.4-mini",
        }))
        .send()
        .await
        .unwrap();
    assert!(turn_resp.status().is_success());
    let turn_body: Value = turn_resp.json().await.unwrap();
    let turn_id = Uuid::parse_str(turn_body["turn"]["id"].as_str().unwrap()).unwrap();
    let _ = app.wait_turn_status(id, turn_id, &["completed"]).await;

    // Verify the turn exists.
    let count: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM turns WHERE session_id = $1")
        .bind(id)
        .fetch_one(&app.db)
        .await
        .unwrap();
    assert_eq!(count.0, 1);

    // Batch delete the session.
    let resp = app
        .http
        .post(app.url("/api/sessions/batch-delete"))
        .json(&json!({ "ids": [id] }))
        .send()
        .await
        .unwrap();
    assert!(resp.status().is_success());

    // Turns should be cascade-deleted.
    let count_after: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM turns WHERE session_id = $1")
        .bind(id)
        .fetch_one(&app.db)
        .await
        .unwrap();
    assert_eq!(count_after.0, 0, "ON DELETE CASCADE should wipe turns");

    app.cleanup().await;
}

#[tokio::test]
async fn batch_delete_with_nonexistent_ids_returns_deleted_count() {
    let app = spawn_app().await;
    let fake1 = Uuid::new_v4();
    let fake2 = Uuid::new_v4();

    let resp = app
        .http
        .post(app.url("/api/sessions/batch-delete"))
        .json(&json!({ "ids": [fake1, fake2] }))
        .send()
        .await
        .unwrap();
    assert!(resp.status().is_success());
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["deleted"], 0);

    app.cleanup().await;
}

#[tokio::test]
async fn batch_delete_rejects_empty_array() {
    let app = spawn_app().await;

    let resp = app
        .http
        .post(app.url("/api/sessions/batch-delete"))
        .json(&json!({ "ids": [] }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::BAD_REQUEST);

    app.cleanup().await;
}

#[tokio::test]
async fn batch_delete_deduplicates_ids() {
    let app = spawn_app().await;
    let id = app.create_session(None).await;

    let resp = app
        .http
        .post(app.url("/api/sessions/batch-delete"))
        .json(&json!({ "ids": [id, id, id] }))
        .send()
        .await
        .unwrap();
    assert!(resp.status().is_success());
    let body: Value = resp.json().await.unwrap();
    // Postgres deduplicates in the ANY match — only 1 row deleted.
    assert_eq!(body["deleted"], 1);

    app.cleanup().await;
}

#[tokio::test]
async fn batch_delete_rejects_over_100_ids() {
    let app = spawn_app().await;
    let ids: Vec<Uuid> = (0..101).map(|_| Uuid::new_v4()).collect();

    let resp = app
        .http
        .post(app.url("/api/sessions/batch-delete"))
        .json(&json!({ "ids": ids }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::BAD_REQUEST);

    app.cleanup().await;
}
```

- [ ] **Step 2: Run the tests**

Run: `cd fork-chat-backend && cargo nextest run --test batch_delete`
Expected: all 6 tests pass

- [ ] **Step 3: Commit**

```
test(backend): add integration tests for batch delete sessions
```

---

### Task 4: Frontend — API types + client method + MSW handler

**Files:**
- Modify: `fork-chat-frontend/src/api/types.ts` (add types)
- Modify: `fork-chat-frontend/src/api/client.ts` (add method)
- Modify: `fork-chat-frontend/src/test/msw/handlers.ts` (add MSW handler)

- [ ] **Step 1: Add types to `api/types.ts`**

Add after the `SessionsPageResponse` interface (around line 160):

```typescript
/** Request body for `POST /api/sessions/batch-delete`. */
export interface BatchDeleteRequest {
  /** Session ids to delete. Non-empty, max 100. */
  ids: string[];
}

/** Response from `POST /api/sessions/batch-delete`. */
export interface BatchDeleteResponse {
  /** Number of sessions actually deleted. */
  deleted: number;
}
```

- [ ] **Step 2: Add client method**

In `fork-chat-frontend/src/api/client.ts`, add inside the `sessions` object (after the `delete` method, around line 102):

```typescript
/** `POST /api/sessions/batch-delete` — delete multiple sessions. */
batchDelete: (ids: string[]) =>
  fetchApi<import('./types').BatchDeleteResponse>('/sessions/batch-delete', {
    method: 'POST',
    body: JSON.stringify({ ids }),
  }),
```

- [ ] **Step 3: Add MSW handler**

In `fork-chat-frontend/src/test/msw/handlers.ts`, add before the closing `];` at the end of the handlers array:

```typescript
http.post(`${API_BASE}/sessions/batch-delete`, async ({ request }) => {
  const body = (await request.json()) as { ids: string[] };
  return HttpResponse.json({ deleted: body.ids.length });
}),
```

- [ ] **Step 4: Run existing tests to verify nothing broke**

Run: `cd fork-chat-frontend && pnpm test`
Expected: all existing tests pass

- [ ] **Step 5: Commit**

```
feat(frontend): add batchDelete API method and types
```

---

### Task 5: Frontend — Add shadcn Checkbox component

**Files:**
- Create: `fork-chat-frontend/src/components/ui/checkbox.tsx`

- [ ] **Step 1: Generate the Checkbox component**

Run: `cd fork-chat-frontend && npx shadcn@latest add checkbox`

This creates `src/components/ui/checkbox.tsx` using the project's existing shadcn config.

- [ ] **Step 2: Verify the component was created**

Run: `ls fork-chat-frontend/src/components/ui/checkbox.tsx`
Expected: file exists

- [ ] **Step 3: Commit**

```
feat(frontend): add shadcn Checkbox component
```

---

### Task 6: Frontend — Selection mode UI in SessionSidebar

**Files:**
- Modify: `fork-chat-frontend/src/routes/__root.tsx`

This is the core UI task. Changes to the `SessionSidebar` component:

1. Add `isSelectionMode` and `selectedIds` state
2. Add a pencil/edit button (`Pencil` icon from lucide) in the sidebar header next to the collapse button
3. In selection mode: replace sort/filter area with "Cancel" + "N selected" header
4. Show Checkbox on each row (left side) instead of MessageSquare icon
5. Add "Select All" checkbox at top of session list
6. Add bottom action bar with "Delete N" button
7. Add batch delete confirmation dialog
8. Add batch delete mutation

- [ ] **Step 1: Add imports**

Add to the imports at the top of `__root.tsx`:

```typescript
import { Pencil } from 'lucide-react';
// Add Checkbox import:
import { Checkbox } from '../components/ui/checkbox';
```

Update the lucide import line to include `Pencil`:

```typescript
import {
  Ellipsis,
  MessageSquare,
  PanelLeftClose,
  PanelLeftOpen,
  Pencil,
  Plus,
} from 'lucide-react';
```

- [ ] **Step 2: Add selection mode state**

After the existing `lastDeleteTitleRef` line (around line 143), add:

```typescript
// Selection mode for batch operations.
const [isSelectionMode, setIsSelectionMode] = useState(false);
const [selectedIds, setSelectedIds] = useState<Set<string>>(new Set());
```

- [ ] **Step 3: Add batch delete mutation**

After the `deleteMutation` definition (around line 225), add:

```typescript
const batchDeleteMutation = useMutation({
  mutationFn: (ids: string[]) => api.sessions.batchDelete(ids),
  onSuccess: (_result, deletedIds) => {
    qc.invalidateQueries({ queryKey: ['sessions'] });
    setSelectedTurn(null);
    // If the current session was deleted, navigate to home.
    const currentSessionId = window.location.pathname.split('/sessions/')[1];
    if (currentSessionId && deletedIds.includes(currentSessionId)) {
      navigate({ to: '/' });
    }
    // Exit selection mode after successful delete.
    setIsSelectionMode(false);
    setSelectedIds(new Set());
    setPendingBatchDeleteCount(null);
  },
});
```

- [ ] **Step 4: Add batch delete confirmation dialog state + handler**

After the batch delete mutation, add:

```typescript
const [pendingBatchDeleteCount, setPendingBatchDeleteCount] = useState<
  number | null
>(null);

const handleBatchDelete = () => {
  if (selectedIds.size > 0) {
    setPendingBatchDeleteCount(selectedIds.size);
  }
};

const confirmBatchDelete = () => {
  if (pendingBatchDeleteCount !== null) {
    batchDeleteMutation.mutate(Array.from(selectedIds));
  }
};

const exitSelectionMode = () => {
  setIsSelectionMode(false);
  setSelectedIds(new Set());
};

const toggleSelectAll = () => {
  if (selectedIds.size === allSessions.length) {
    setSelectedIds(new Set());
  } else {
    setSelectedIds(new Set(allSessions.map((s) => s.id)));
  }
};

const toggleSelectSession = (id: string) => {
  setSelectedIds((prev) => {
    const next = new Set(prev);
    if (next.has(id)) {
      next.delete(id);
    } else {
      next.add(id);
    }
    return next;
  });
};
```

- [ ] **Step 5: Modify the collapsed sidebar to include the edit button**

In the collapsed view (around line 253-273), add an edit button after the expand button:

```tsx
<button
  type="button"
  onClick={() => {
    setIsSelectionMode(!isSelectionMode);
    if (isSelectionMode) {
      setSelectedIds(new Set());
    }
  }}
  className={`p-1.5 rounded-md mb-2 transition-colors cursor-pointer ${
    isSelectionMode
      ? 'text-zinc-100 bg-zinc-700'
      : 'text-zinc-400 hover:text-zinc-100 hover:bg-zinc-800'
  }`}
  title={isSelectionMode ? 'Exit selection' : 'Batch select'}
>
  <Pencil className="size-4" />
</button>
```

- [ ] **Step 6: Add edit button to the expanded header**

In the header area (around line 280-289), change the header to include an edit button:

```tsx
<div className="flex items-center justify-between">
  <h1 className="font-semibold tracking-tight">Fork Chat</h1>
  <div className="flex items-center gap-1">
    <button
      type="button"
      onClick={() => {
        setIsSelectionMode(!isSelectionMode);
        if (isSelectionMode) {
          setSelectedIds(new Set());
        }
      }}
      className={`p-1 rounded-md transition-colors cursor-pointer ${
        isSelectionMode
          ? 'text-zinc-100 bg-zinc-700'
          : 'text-zinc-400 hover:text-zinc-100 hover:bg-zinc-800'
      }`}
      title={isSelectionMode ? 'Exit selection' : 'Batch select'}
    >
      <Pencil className="size-4" />
    </button>
    <button
      type="button"
      onClick={() => setCollapsed(true)}
      className="p-1 text-zinc-400 hover:text-zinc-100 hover:bg-zinc-800 rounded-md transition-colors cursor-pointer"
      title="Collapse sidebar"
    >
      <PanelLeftClose className="size-4" />
    </button>
  </div>
</div>
```

- [ ] **Step 7: Conditionally show sort/filter or selection header**

Replace the sort and filter sections (the two `<div className="mt-2 space-y-1.5">` blocks around lines 335-378) with:

```tsx
{isSelectionMode ? (
  <div className="mt-2 flex items-center justify-between">
    <Button
      variant="ghost"
      size="sm"
      onClick={exitSelectionMode}
      className="text-xs text-zinc-400 hover:text-zinc-100 px-1"
    >
      Cancel
    </Button>
    <span className="text-xs text-zinc-400">
      {selectedIds.size} selected
    </span>
  </div>
) : (
  <>
    <div className="mt-2 space-y-1.5">
      <p className="px-0.5 text-[10px] font-semibold uppercase tracking-wider text-zinc-500">
        Sort
      </p>
      {/* ... existing sort Select ... */}
    </div>
    <div className="mt-2 space-y-1.5">
      <p className="px-0.5 text-[10px] font-semibold uppercase tracking-wider text-zinc-500">
        Filter (title)
      </p>
      {/* ... existing filter Input ... */}
    </div>
  </>
)}
```

Note: Keep the existing protocol selector above this conditional — it should always be visible.

- [ ] **Step 8: Add "Select All" checkbox at top of session list**

Inside the scrollable list area (after the loading/error states, before the session groups), add:

```tsx
{isSelectionMode && allSessions.length > 0 && (
  <div className="flex items-center gap-2 px-2.5 py-1 mb-1">
    <Checkbox
      checked={
        allSessions.length > 0 &&
        selectedIds.size === allSessions.length
      }
      onCheckedChange={toggleSelectAll}
      className="border-zinc-600 data-[state=checked]:bg-zinc-100 data-[state=checked]:border-zinc-100"
    />
    <span className="text-[11px] text-zinc-500">Select all</span>
  </div>
)}
```

- [ ] **Step 9: Add checkbox to each session row**

Inside the session row rendering (the `groups[group].map` callback), replace the `MessageSquare` icon with a conditional:

```tsx
{isSelectionMode ? (
  <Checkbox
    checked={selectedIds.has(session.id)}
    onCheckedChange={() => toggleSelectSession(session.id)}
    onClick={(e) => e.preventDefault()}
    className="border-zinc-600 data-[state=checked]:bg-zinc-100 data-[state=checked]:border-zinc-100 shrink-0"
  />
) : (
  <MessageSquare className="size-3.5 shrink-0 text-zinc-500 group-hover/row:text-zinc-400" />
)}
```

Also hide the dropdown menu in selection mode:

```tsx
{!isSelectionMode && (
  <div className="absolute right-1 top-1/2 -translate-y-1/2 opacity-0 group-hover/row:opacity-100 focus-within:opacity-100 transition-opacity">
    {/* ... existing DropdownMenu ... */}
  </div>
)}
```

- [ ] **Step 10: Add bottom action bar**

After the scrollable list div (before the existing delete dialog), add:

```tsx
{isSelectionMode && selectedIds.size > 0 && (
  <div className="p-3 border-t border-zinc-800 flex items-center justify-between bg-zinc-900">
    <span className="text-xs text-zinc-400">
      {selectedIds.size} selected
    </span>
    <Button
      variant="destructive"
      size="sm"
      onClick={handleBatchDelete}
      className="text-xs"
    >
      Delete {selectedIds.size}
    </Button>
  </div>
)}
```

- [ ] **Step 11: Add batch delete confirmation dialog**

After the existing single-delete dialog, add:

```tsx
<Dialog
  open={pendingBatchDeleteCount !== null}
  onOpenChange={(open) => {
    if (!open) setPendingBatchDeleteCount(null);
  }}
>
  <DialogContent className="max-w-md">
    <DialogHeader>
      <DialogTitle>Delete {pendingBatchDeleteCount} sessions?</DialogTitle>
      <DialogDescription>
        {pendingBatchDeleteCount} sessions and all of their messages will be
        permanently removed. This action cannot be undone.
      </DialogDescription>
    </DialogHeader>
    <DialogFooter>
      <Button
        variant="secondary"
        onClick={() => setPendingBatchDeleteCount(null)}
      >
        Cancel
      </Button>
      <Button
        variant="destructive"
        onClick={confirmBatchDelete}
        disabled={batchDeleteMutation.isPending}
      >
        {batchDeleteMutation.isPending ? 'Deleting...' : 'Delete'}
      </Button>
    </DialogFooter>
  </DialogContent>
</Dialog>
```

- [ ] **Step 12: Run frontend quality checks**

Run:
```bash
cd fork-chat-frontend
pnpm format
pnpm lint
pnpm typecheck
```

Expected: all pass

- [ ] **Step 13: Commit**

```
feat(frontend): add batch selection mode to session sidebar
```

---

### Task 7: Frontend tests for selection mode

**Files:**
- Create: `fork-chat-frontend/src/routes/__root.test.tsx`

- [ ] **Step 1: Write the tests**

Create `fork-chat-frontend/src/routes/__root.test.tsx`:

```tsx
import { HttpResponse, http } from 'msw';
import { describe, expect, it, vi } from 'vitest';
import { server } from '../test/server-from-setup';
import { renderWithProviders } from '../test/render';
import { makeSession } from '../test/fixtures';
import { screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { SessionSidebar } from './__root';

// __root.tsx exports SessionSidebar as a non-exported component,
// so we re-import the module's default component. Since SessionSidebar
// is used inside RootComponent, we test it directly by rendering it.

const API_BASE = 'http://localhost:3000/api';

// Helper: configure MSW to return specific sessions for the list endpoint.
function givenSessions(count: number) {
  const sessions = Array.from({ length: count }, (_, i) =>
    makeSession({
      id: `session-${i + 1}`,
      title: `Session ${i + 1}`,
      updated_at: new Date(Date.now() - i * 3600_000).toISOString(),
    }),
  );
  server.use(
    http.get(`${API_BASE}/sessions`, () =>
      HttpResponse.json({ sessions, next_cursor: null }),
    ),
  );
}

describe('SessionSidebar batch selection', () => {
  it('renders the edit/pencil button', async () => {
    givenSessions(2);
    renderWithProviders(<SessionSidebar />);
    expect(await screen.findByTitle('Batch select')).toBeInTheDocument();
  });

  it('enters selection mode when edit button is clicked', async () => {
    const user = userEvent.setup();
    givenSessions(2);
    renderWithProviders(<SessionSidebar />);

    const editBtn = await screen.findByTitle('Batch select');
    await user.click(editBtn);

    // "Select all" checkbox should appear.
    expect(screen.getByText('Select all')).toBeInTheDocument();
    // "Cancel" button should appear.
    expect(screen.getByText('Cancel')).toBeInTheDocument();
    // Each session row should now have a checkbox.
    const checkboxes = screen.getAllByRole('checkbox');
    // +1 for the "Select all" checkbox itself.
    expect(checkboxes.length).toBe(3);
  });

  it('selects a session when its checkbox is clicked', async () => {
    const user = userEvent.setup();
    givenSessions(2);
    renderWithProviders(<SessionSidebar />);

    const editBtn = await screen.findByTitle('Batch select');
    await user.click(editBtn);

    // Click the checkbox for the first session row (second checkbox overall,
    // since the first is "Select all").
    const checkboxes = screen.getAllByRole('checkbox');
    await user.click(checkboxes[1]);

    expect(screen.getByText('1 selected')).toBeInTheDocument();
  });

  it('selects all sessions when "Select all" is clicked', async () => {
    const user = userEvent.setup();
    givenSessions(3);
    renderWithProviders(<SessionSidebar />);

    const editBtn = await screen.findByTitle('Batch select');
    await user.click(editBtn);

    // Click "Select all" (first checkbox).
    const selectAllCheckbox = screen.getAllByRole('checkbox')[0];
    await user.click(selectAllCheckbox);

    expect(screen.getByText('3 selected')).toBeInTheDocument();
  });

  it('deselects all when "Select all" is clicked twice', async () => {
    const user = userEvent.setup();
    givenSessions(2);
    renderWithProviders(<SessionSidebar />);

    const editBtn = await screen.findByTitle('Batch select');
    await user.click(editBtn);

    const selectAllCheckbox = screen.getAllByRole('checkbox')[0];
    await user.click(selectAllCheckbox);
    expect(screen.getByText('2 selected')).toBeInTheDocument();

    await user.click(selectAllCheckbox);
    // "0 selected" in the header — the bottom action bar is gone since 0 selected.
    expect(screen.getByText('0 selected')).toBeInTheDocument();
  });

  it('shows "Delete N" button in bottom bar when sessions are selected', async () => {
    const user = userEvent.setup();
    givenSessions(2);
    renderWithProviders(<SessionSidebar />);

    const editBtn = await screen.findByTitle('Batch select');
    await user.click(editBtn);

    // Select all.
    const selectAllCheckbox = screen.getAllByRole('checkbox')[0];
    await user.click(selectAllCheckbox);

    expect(screen.getByText('Delete 2')).toBeInTheDocument();
  });

  it('opens confirmation dialog when "Delete N" is clicked', async () => {
    const user = userEvent.setup();
    givenSessions(2);
    renderWithProviders(<SessionSidebar />);

    const editBtn = await screen.findByTitle('Batch select');
    await user.click(editBtn);

    const selectAllCheckbox = screen.getAllByRole('checkbox')[0];
    await user.click(selectAllCheckbox);

    const deleteBtn = screen.getByText('Delete 2');
    await user.click(deleteBtn);

    expect(screen.getByText('Delete 2 sessions?')).toBeInTheDocument();
  });

  it('calls batch delete API on confirm', async () => {
    const user = userEvent.setup();
    givenSessions(2);

    let capturedBody: unknown = null;
    server.use(
      http.post(`${API_BASE}/sessions/batch-delete`, async ({ request }) => {
        capturedBody = await request.json();
        return HttpResponse.json({ deleted: 2 });
      }),
    );

    renderWithProviders(<SessionSidebar />);

    const editBtn = await screen.findByTitle('Batch select');
    await user.click(editBtn);

    const selectAllCheckbox = screen.getAllByRole('checkbox')[0];
    await user.click(selectAllCheckbox);

    const deleteBtn = screen.getByText('Delete 2');
    await user.click(deleteBtn);

    // Confirm in dialog.
    const confirmBtn = screen.getAllByText('Delete').find(
      (el) => el.tagName === 'BUTTON',
    )!;
    await user.click(confirmBtn);

    await waitFor(() => {
      expect(capturedBody).toEqual({
        ids: ['session-1', 'session-2'],
      });
    });
  });

  it('closes dialog on cancel without calling API', async () => {
    const user = userEvent.setup();
    givenSessions(2);

    const batchDeleteSpy = vi.fn();
    server.use(
      http.post(`${API_BASE}/sessions/batch-delete`, () => {
        batchDeleteSpy();
        return HttpResponse.json({ deleted: 2 });
      }),
    );

    renderWithProviders(<SessionSidebar />);

    const editBtn = await screen.findByTitle('Batch select');
    await user.click(editBtn);

    const selectAllCheckbox = screen.getAllByRole('checkbox')[0];
    await user.click(selectAllCheckbox);

    const deleteBtn = screen.getByText('Delete 2');
    await user.click(deleteBtn);

    // Cancel in dialog.
    const cancelBtn = screen.getByText('Cancel');
    await user.click(cancelBtn);

    // Dialog should be closed.
    expect(screen.queryByText('Delete 2 sessions?')).not.toBeInTheDocument();
    expect(batchDeleteSpy).not.toHaveBeenCalled();
    // Selections should remain.
    expect(screen.getByText('2 selected')).toBeInTheDocument();
  });

  it('exits selection mode when cancel button is clicked', async () => {
    const user = userEvent.setup();
    givenSessions(2);
    renderWithProviders(<SessionSidebar />);

    const editBtn = await screen.findByTitle('Batch select');
    await user.click(editBtn);

    // Cancel in the selection header.
    const cancelBtn = screen.getByText('Cancel');
    await user.click(cancelBtn);

    // Selection mode UI should be gone.
    expect(screen.queryByText('Select all')).not.toBeInTheDocument();
    // Sort controls should be back.
    expect(screen.getByText('Sort')).toBeInTheDocument();
  });
});
```

Note: Since `SessionSidebar` is a non-exported function component inside `__root.tsx`, the test file needs the component to be exported. Add a named export to `__root.tsx`:

```tsx
export function SessionSidebar() {
```

(The component is currently declared as `function SessionSidebar()` without `export`.)

- [ ] **Step 2: Export SessionSidebar for testing**

In `__root.tsx`, change line 124 from:

```tsx
function SessionSidebar() {
```

to:

```tsx
export function SessionSidebar() {
```

- [ ] **Step 3: Run the tests**

Run: `cd fork-chat-frontend && pnpm test`
Expected: all tests pass (including new ones)

- [ ] **Step 4: Fix any test failures**

If `SessionSidebar` has routing dependencies that fail in test, wrap with TanStack Router's test utilities or mock `useNavigate` / `Link`. Check the error output and adjust accordingly.

- [ ] **Step 5: Commit**

```
test(frontend): add tests for session sidebar batch selection
```

---

### Task 8: Run full quality gate

- [ ] **Step 1: Backend checks**

```bash
cd fork-chat-backend
cargo fmt --all --check
cargo clippy --all-targets --all-features -- -D warnings
cargo nextest run
```

Expected: all pass

- [ ] **Step 2: Frontend checks**

```bash
cd fork-chat-frontend
pnpm format
pnpm lint
pnpm typecheck
pnpm test
```

Expected: all pass

- [ ] **Step 3: Commit if any formatting fixes were needed**
