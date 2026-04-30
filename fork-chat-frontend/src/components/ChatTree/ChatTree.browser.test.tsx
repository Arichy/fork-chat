import { fireEvent, render, screen, waitFor } from '@testing-library/react';
import { ReactFlowProvider } from '@xyflow/react';
import { describe, expect, it, vi } from 'vitest';
import { makeTurn } from '../../test/fixtures';
import { ChatTree } from './ChatTree';

// Wrap in a sized container so React Flow can measure nodes.
function SizedContainer({ children }: { children: React.ReactNode }) {
  return (
    <div style={{ width: 1200, height: 800 }}>
      <ReactFlowProvider>{children}</ReactFlowProvider>
    </div>
  );
}

describe('ChatTree', () => {
  it('shows "No messages yet" when turns is empty', () => {
    render(
      <SizedContainer>
        <ChatTree turns={[]} selectedTurnId={null} onSelectTurn={vi.fn()} />
      </SizedContainer>,
    );
    expect(screen.getByText(/no messages yet/i)).toBeInTheDocument();
  });

  it('renders one node per turn', async () => {
    const root = makeTurn({
      id: 'r',
      user_text: 'hello',
      parent_turn_id: null,
    });
    const child = makeTurn({
      id: 'c',
      user_text: 'follow-up',
      parent_turn_id: 'r',
    });

    render(
      <SizedContainer>
        <ChatTree
          turns={[root, child]}
          selectedTurnId={null}
          onSelectTurn={vi.fn()}
        />
      </SizedContainer>,
    );

    await waitFor(() => {
      expect(screen.getByText('hello')).toBeInTheDocument();
      expect(screen.getByText('follow-up')).toBeInTheDocument();
    });
  });

  it('invokes onSelectTurn when a node is clicked', async () => {
    const onSelect = vi.fn();
    const turn = makeTurn({ id: 'click-me', user_text: 'click target' });

    render(
      <SizedContainer>
        <ChatTree
          turns={[turn]}
          selectedTurnId={null}
          onSelectTurn={onSelect}
        />
      </SizedContainer>,
    );

    await screen.findByText('click target');
    // Use fireEvent.click on the node's outer wrapper to avoid triggering
    // React Flow's mousedown-based drag handler (which schedules async work
    // and can fire after cleanup in jsdom-less browser mode).
    const node = document.getElementById('click-me')!;
    fireEvent.click(node);
    expect(onSelect).toHaveBeenCalledWith('click-me');
  });

  it('applies selected styling (border-blue-500) to the selected turn', async () => {
    const selected = makeTurn({ id: 'sel', user_text: 'picked' });

    render(
      <SizedContainer>
        <ChatTree
          turns={[selected]}
          selectedTurnId="sel"
          onSelectTurn={vi.fn()}
        />
      </SizedContainer>,
    );

    const node = document.getElementById('sel');
    expect(node).not.toBeNull();
    expect(node!.className).toContain('border-blue-500');
  });

  it('applies yellow styling for running status', async () => {
    const running = makeTurn({
      id: 'run-1',
      status: 'running',
      user_text: 'pending',
      assistant_text: null,
    });

    render(
      <SizedContainer>
        <ChatTree
          turns={[running]}
          selectedTurnId={null}
          onSelectTurn={vi.fn()}
        />
      </SizedContainer>,
    );

    await waitFor(() => {
      const node = document.getElementById('run-1');
      expect(node).not.toBeNull();
      expect(node!.className).toContain('border-yellow-500');
    });
  });

  it('applies red styling for failed status', async () => {
    const failed = makeTurn({
      id: 'fail-1',
      status: 'failed',
      user_text: 'oops',
    });

    render(
      <SizedContainer>
        <ChatTree
          turns={[failed]}
          selectedTurnId={null}
          onSelectTurn={vi.fn()}
        />
      </SizedContainer>,
    );

    await waitFor(() => {
      const node = document.getElementById('fail-1');
      expect(node!.className).toContain('border-red-500');
    });
  });

  it('positions nodes after layout runs (x/y differ per node)', async () => {
    const root = makeTurn({
      id: 'p',
      user_text: 'parent',
      parent_turn_id: null,
    });
    const a = makeTurn({ id: 'a', user_text: 'a', parent_turn_id: 'p' });
    const b = makeTurn({ id: 'b', user_text: 'b', parent_turn_id: 'p' });

    render(
      <SizedContainer>
        <ChatTree
          turns={[root, a, b]}
          selectedTurnId={null}
          onSelectTurn={vi.fn()}
        />
      </SizedContainer>,
    );

    // Wait until all three turn DOMs exist and siblings got distinct X.
    await waitFor(() => {
      const nodes = document.querySelectorAll(
        '.react-flow__node[data-id]',
      ) as NodeListOf<HTMLElement>;
      expect(nodes.length).toBe(3);
      const byId = new Map<string, HTMLElement>();
      nodes.forEach((n) => byId.set(n.getAttribute('data-id')!, n));

      const aNode = byId.get('a')!;
      const bNode = byId.get('b')!;
      expect(aNode.style.transform).not.toBe(bNode.style.transform);
    });
  });

  it('updates when turns prop changes', async () => {
    const root = makeTurn({ id: 'only', user_text: 'original' });
    const { rerender } = render(
      <SizedContainer>
        <ChatTree turns={[root]} selectedTurnId={null} onSelectTurn={vi.fn()} />
      </SizedContainer>,
    );
    await screen.findByText('original');

    const added = makeTurn({
      id: 'added',
      user_text: 'added later',
      parent_turn_id: 'only',
    });
    rerender(
      <SizedContainer>
        <ChatTree
          turns={[root, added]}
          selectedTurnId={null}
          onSelectTurn={vi.fn()}
        />
      </SizedContainer>,
    );

    await screen.findByText('added later');
  });
});
