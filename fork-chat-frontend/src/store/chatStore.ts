import { create } from 'zustand';

interface ChatState {
  selectedTurnId: string | null;
  setSelectedTurn: (turnId: string | null) => void;
}

export const useChatStore = create<ChatState>((set) => ({
  selectedTurnId: null,
  setSelectedTurn: (turnId) => set({ selectedTurnId: turnId }),
}));
