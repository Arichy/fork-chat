/**
 * Minimal Zustand store for chat UI state.
 *
 * This store intentionally holds **only** the currently selected turn ID.
 * All other state (sessions, turns, trees) is managed by React Query, which
 * handles caching, refetching, and background updates from the SSE stream.
 *
 * The selected turn ID lives here rather than in component state because it
 * needs to be accessible across multiple components (the tree view, the detail
 * modal, the message input) without prop drilling.
 */
import { create } from 'zustand';

interface ChatState {
  /** The turn currently selected in the tree view and displayed in the detail
   *  modal. Null when no turn is selected. */
  selectedTurnId: string | null;
  /** Select a turn for viewing (opens the detail modal) or deselect (closes it). */
  setSelectedTurn: (turnId: string | null) => void;
}

export const useChatStore = create<ChatState>((set) => ({
  selectedTurnId: null,
  setSelectedTurn: (turnId) => set({ selectedTurnId: turnId }),
}));
