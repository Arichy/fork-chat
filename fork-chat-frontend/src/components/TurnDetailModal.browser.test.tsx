import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { render, screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { describe, expect, it, vi } from 'vitest';
import type { Turn } from '../api/types';
import { makeTurn } from '../test/fixtures';
import { TurnDetailModal } from './TurnDetailModal';

// MessageInput queries the config endpoint through @/api; mock it out so the
// modal renders deterministically in the browser project.
vi.mock('../api', () => ({
  api: {
    config: {
      get: vi.fn().mockResolvedValue({
        models: [
          { id: 'gpt-4o-mini', name: 'GPT-4o Mini', provider: 'openai' },
        ],
      }),
    },
  },
}));

function renderModal(props: {
  turn: Turn | null;
  open: boolean;
  onOpenChange?: (v: boolean) => void;
  onSend?: (t: string, m: string, pid: string | null) => void;
  onRetry?: (id: string, model: string) => void;
  isSending?: boolean;
}) {
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false } },
  });
  client.setQueryData(['config'], {
    models: [{ id: 'gpt-4o-mini', name: 'GPT-4o Mini', provider: 'openai' }],
  });
  return render(
    <QueryClientProvider client={client}>
      <TurnDetailModal
        turn={props.turn}
        open={props.open}
        onOpenChange={props.onOpenChange ?? vi.fn()}
        onSend={props.onSend ?? vi.fn()}
        onRetry={props.onRetry ?? vi.fn()}
        isSending={props.isSending ?? false}
      />
    </QueryClientProvider>,
  );
}

describe('TurnDetailModal', () => {
  it('renders nothing when turn is null and no previous turn was shown', () => {
    const { container } = renderModal({ turn: null, open: false });
    expect(container.textContent).toBe('');
  });

  it('shows Retry button when displayed turn is failed', async () => {
    const onRetry = vi.fn();
    const failed = makeTurn({
      id: 'failed-1',
      status: 'failed',
      model: 'gpt-4o',
      user_text: 'broke',
      error: { message: 'boom' },
    });
    renderModal({ turn: failed, open: true, onRetry });

    const retry = await screen.findByRole('button', { name: /retry/i });
    await userEvent.setup().click(retry);
    expect(onRetry).toHaveBeenCalledWith('failed-1', 'gpt-4o');
  });

  it('hides Retry and shows MessageInput when status is not failed', async () => {
    const completed = makeTurn({ status: 'completed' });
    renderModal({ turn: completed, open: true });

    expect(
      screen.queryByRole('button', { name: /retry/i }),
    ).not.toBeInTheDocument();
    expect(
      await screen.findByPlaceholderText(/type your message/i),
    ).toBeInTheDocument();
  });

  it('shows "Waiting for AI response..." when isSending is true', () => {
    renderModal({
      turn: makeTurn({ status: 'completed' }),
      open: true,
      isSending: true,
    });
    expect(screen.getByText(/waiting for ai response/i)).toBeInTheDocument();
  });

  it('persists last turn content after it becomes null (lastTurnRef)', () => {
    const turn = makeTurn({
      id: 'persisted',
      user_text: 'remember me',
      assistant_text: 'will do',
    });
    const { rerender } = renderModal({ turn, open: true });
    // The user text appears both in the main MessageView and in the
    // "Replying to: ..." preview inside MessageInput.
    expect(screen.getAllByText('remember me').length).toBeGreaterThan(0);

    // Simulate the parent mutation clearing `turn` while the modal closes.
    rerender(
      <QueryClientProvider
        client={
          new QueryClient({
            defaultOptions: { queries: { retry: false } },
          })
        }
      >
        <TurnDetailModal
          turn={null}
          open={true}
          onOpenChange={vi.fn()}
          onSend={vi.fn()}
          onRetry={vi.fn()}
          isSending={false}
        />
      </QueryClientProvider>,
    );
    // The previously rendered content should still be in the DOM because of
    // the lastTurnRef fallback.
    expect(screen.getAllByText('remember me').length).toBeGreaterThan(0);
  });

  it('displays the turn status badge and model', () => {
    renderModal({
      turn: makeTurn({ status: 'running', model: 'gpt-4o' }),
      open: true,
    });
    expect(screen.getByText('running')).toBeInTheDocument();
    expect(screen.getByText('gpt-4o')).toBeInTheDocument();
  });

  it('renders error JSON for failed turns', () => {
    renderModal({
      turn: makeTurn({
        status: 'failed',
        error: { message: 'network_error' },
      }),
      open: true,
    });
    expect(screen.getByText(/network_error/)).toBeInTheDocument();
  });
});
