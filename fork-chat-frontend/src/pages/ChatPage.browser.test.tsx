import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { render, screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import type { Turn } from '../api/types';
import { makeTurn } from '../test/fixtures';
import { ChatPage } from './ChatPage';

// Mock TanStack Router so ChatPage can call useParams without a real router.
vi.mock('@tanstack/react-router', () => ({
  useParams: () => ({ sessionId: 'session-1' }),
}));

// Mock toast so error branches don't blow up without a <Toaster />.
vi.mock('sonner', () => ({ toast: { error: vi.fn() } }));

// Use vi.hoisted so these mocks are available when vi.mock's factory runs
// (vi.mock is hoisted to the top of the file).
const { turnsApi, configGet } = vi.hoisted(() => ({
  turnsApi: {
    create: vi.fn(),
    retry: vi.fn(),
    tree: vi.fn(),
    get: vi.fn(),
  },
  configGet: vi.fn().mockResolvedValue({
    models: [{ id: 'gpt-4o-mini', name: 'GPT-4o Mini', provider: 'openai' }],
  }),
}));

vi.mock('../api', () => ({
  api: {
    config: { get: configGet },
    turns: turnsApi,
  },
}));

function renderPage() {
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false } },
  });
  client.setQueryData(['config'], {
    models: [{ id: 'gpt-4o-mini', name: 'GPT-4o Mini', provider: 'openai' }],
  });
  return render(
    <QueryClientProvider client={client}>
      <div style={{ width: 1200, height: 800 }}>
        <ChatPage />
      </div>
    </QueryClientProvider>,
  );
}

describe('ChatPage', () => {
  beforeEach(() => {
    turnsApi.create.mockReset();
    turnsApi.retry.mockReset();
    turnsApi.tree.mockReset();
    turnsApi.get.mockReset();
  });

  it('shows the empty state with MessageInput when tree is empty', async () => {
    turnsApi.tree.mockResolvedValue({ turns: [] });
    renderPage();

    expect(
      await screen.findByText(/start a conversation/i),
    ).toBeInTheDocument();
    expect(
      screen.getByPlaceholderText(/type your message/i),
    ).toBeInTheDocument();
  });

  it('shows "Session not found" when the tree query errors', async () => {
    turnsApi.tree.mockRejectedValue(new Error('API error: 404'));
    renderPage();
    expect(await screen.findByText(/session not found/i)).toBeInTheDocument();
  });

  it('sends a message and invalidates the tree on success', async () => {
    const first: Turn[] = [];
    const second: Turn[] = [
      makeTurn({ id: 'new', user_text: 'hi', status: 'completed' }),
    ];
    turnsApi.tree
      .mockResolvedValueOnce({ turns: first })
      .mockResolvedValue({ turns: second });
    turnsApi.create.mockResolvedValue({
      turn: makeTurn({ id: 'new', user_text: 'hi', status: 'completed' }),
    });

    renderPage();

    const textarea = await screen.findByPlaceholderText(/type your message/i);
    await userEvent.setup().type(textarea, 'hi{Enter}');

    await waitFor(() => {
      expect(turnsApi.create).toHaveBeenCalledWith('session-1', {
        user_text: 'hi',
        parent_turn_id: undefined,
        provider: 'openai',
        model: 'gpt-4o-mini',
      });
    });

    // After the mutation succeeds, the tree query should have been refetched
    // and the new turn text should eventually appear in the tree view.
    await screen.findByText('hi');
  });

  it('filters out failed turns that have a retry_turn_id', async () => {
    const replacement = makeTurn({
      id: 'new',
      user_text: 'retry text',
      status: 'completed',
    });
    const superseded = makeTurn({
      id: 'old',
      user_text: 'will be hidden',
      status: 'failed',
      retry_turn_id: 'new',
    });
    turnsApi.tree.mockResolvedValue({ turns: [superseded, replacement] });

    renderPage();

    await screen.findByText('retry text');
    expect(screen.queryByText('will be hidden')).not.toBeInTheDocument();
  });

  it('keeps a failed turn visible while it has no retry_turn_id', async () => {
    const failed = makeTurn({
      id: 'bad',
      user_text: 'failed attempt',
      status: 'failed',
      retry_turn_id: null,
    });
    turnsApi.tree.mockResolvedValue({ turns: [failed] });
    renderPage();
    expect(await screen.findByText('failed attempt')).toBeInTheDocument();
  });
});
