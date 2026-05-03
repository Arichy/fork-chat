# Batch Delete Sessions

## Summary

Add checkbox-based batch selection to the session sidebar, allowing users to select multiple sessions and delete them in one action.

## Backend

### New endpoint: `POST /api/sessions/batch-delete`

- **Request body:** `{ "ids": ["uuid1", "uuid2", ...] }`
- **Validation:**
  - `ids` array is non-empty
  - Max 100 IDs (matches page size limit)
  - All elements are valid UUIDs
  - Deduplicated server-side
- **Response:** `{ "deleted": N }` where N is the number of rows actually deleted
- **Implementation:** Single `DELETE FROM sessions WHERE id = ANY($1)` inside a transaction. Cascade on the `turns` table handles related data automatically.
- **Error handling:** Returns 400 for invalid input. No per-ID "not found" errors — the response `deleted` count reflects actual deletions.

### Files to modify

- `fork-chat-backend/src/db/sessions.rs` — add `batch_delete_sessions` function
- `fork-chat-backend/src/handlers/sessions.rs` — add `batch_delete_sessions_handler` with request/response types
- `fork-chat-backend/src/routes.rs` — mount the new route

## Frontend

### Selection mode toggle

- A pencil/edit icon button in the sidebar header (next to the `+` create button) toggles selection mode on/off.
- When selection mode is active, the edit button is visually highlighted (variant="default" instead of "ghost").

### Selection mode UI

- **Checkboxes on rows:** Each session row shows a Checkbox on the left side, replacing the MessageSquare icon area.
- **Select All:** A "Select All" checkbox at the top of the session list area. Checks/unchecks all currently loaded sessions across all pages.
- **Header changes:** The header area shows a "Cancel" text button and a "N selected" count label, replacing the normal sort/filter controls.
- **Bottom action bar:** A floating bar pinned to the bottom of the sidebar showing a "Delete N" button in destructive variant. Only visible when at least 1 session is selected.

### Delete flow

1. User clicks "Delete N" in the bottom action bar.
2. A confirmation Dialog appears: "Delete N sessions? This will permanently remove all selected sessions and their messages. This action cannot be undone."
3. On confirm, call `POST /api/sessions/batch-delete` with the selected IDs.
4. On success: invalidate the sessions query cache, navigate to `/` if the current session was among the deleted, exit selection mode.
5. On error: show the error in the dialog, keep dialog open.

### State management

- `isSelectionMode: boolean` — whether selection mode is active
- `selectedIds: Set<string>` — the set of selected session IDs
- Both live as `useState` inside the `SessionSidebar` component.

### Exit conditions

Selection mode exits when:
- User clicks "Cancel" in the header
- User clicks the edit button again (toggle off)
- After a successful batch delete

### Files to modify

- `fork-chat-frontend/src/routes/__root.tsx` — selection mode state, UI changes to SessionSidebar
- `fork-chat-frontend/src/api/client.ts` — add `batchDelete` method
- `fork-chat-frontend/src/api/types.ts` — add request/response types

## Testing

### Backend tests

- Batch delete multiple sessions — all are removed, turns cascade-deleted
- Batch delete with non-existent IDs — returns `deleted: 0` for those, no error
- Batch delete with empty array — returns 400
- Batch delete with duplicate IDs — deduplicates, no error
- Batch delete exceeds 100 limit — returns 400

### Frontend

### Frontend tests

- Render SessionSidebar with sessions — verify edit button exists
- Click edit button — selection mode activates, checkboxes appear on each row
- Click a checkbox — session ID is added to selectedIds
- Click "Select All" — all loaded sessions become selected
- Uncheck "Select All" — all sessions become deselected
- Click "Delete N" with selections — confirmation dialog appears with correct count
- Confirm delete in dialog — batch delete API is called with correct IDs, cache is invalidated, selection mode exits
- Cancel delete in dialog — dialog closes, no API call, selections remain
- After successful delete of current session — navigates to `/`
- Exit selection mode via cancel button — clears selections, hides checkboxes

## Scope exclusions

- No group-level select-all (flat select-all only)
- No undo/restore functionality
- No progress indicator for batch delete (the operation is fast)
