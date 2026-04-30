import { beforeEach, describe, expect, it } from 'vitest';
import { useChatStore } from './chatStore';

describe('useChatStore', () => {
  beforeEach(() => {
    useChatStore.setState({ selectedTurnId: null });
  });

  it('has null selectedTurnId by default', () => {
    expect(useChatStore.getState().selectedTurnId).toBeNull();
  });

  it('setSelectedTurn updates the value', () => {
    useChatStore.getState().setSelectedTurn('turn-42');
    expect(useChatStore.getState().selectedTurnId).toBe('turn-42');
  });

  it('setSelectedTurn clears the value when passed null', () => {
    useChatStore.getState().setSelectedTurn('turn-1');
    useChatStore.getState().setSelectedTurn(null);
    expect(useChatStore.getState().selectedTurnId).toBeNull();
  });

  it('notifies subscribers on change', () => {
    const snapshots: (string | null)[] = [];
    const unsubscribe = useChatStore.subscribe((state) => {
      snapshots.push(state.selectedTurnId);
    });
    useChatStore.getState().setSelectedTurn('turn-a');
    useChatStore.getState().setSelectedTurn('turn-b');
    unsubscribe();
    expect(snapshots).toEqual(['turn-a', 'turn-b']);
  });
});
