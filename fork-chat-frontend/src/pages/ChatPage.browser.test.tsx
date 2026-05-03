import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { render, screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import * as React from 'react';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import type { ConfigResponse, Turn } from '../api/types';
import { makeSession, makeTurn } from '../test/fixtures';
import { ChatPage } from './ChatPage';

// Stateful mock for TanStack Router so `useSearch` / `useNavigate` can
// simulate real URL search-param reactivity: ChatPage now stores the modal's
// open state in `?turnId=...`, so the modal only appears when a navigation
// triggers a re-read of `useSearch`.
const { routerState, routerListeners } = vi.hoisted(() => ({
  routerState: { turnId: undefined as string | undefined },
  routerListeners: new Set<() => void>(),
}));

vi.mock('@tanstack/react-router', () => {
  return {
    useParams: () => ({ sessionId: 'session-1' }),
    useSearch: () => {
      // Force a re-render whenever any test-driven navigate() happens so that
      // components reading `useSearch` observe the new value. We reuse the
      // top-level React import to avoid the "two copies of React" issue that
      // appears when `vi.importActual('react')` resolves a separate instance.
      const [, force] = React.useState(0);
      React.useEffect(() => {
        const listener = () => force((x) => x + 1);
        routerListeners.add(listener);
        return () => {
          routerListeners.delete(listener);
        };
      }, []);
      // Return a fresh object each call so ref equality doesn't mislead any
      // downstream memoization.
      return { turnId: routerState.turnId };
    },
    useNavigate:
      () =>
      (opts: {
        search?:
          | { turnId?: string }
          | ((prev: { turnId?: string }) => { turnId?: string });
        replace?: boolean;
      }) => {
        if (!opts || !('search' in opts)) return;
        const prev = { turnId: routerState.turnId };
        const next =
          typeof opts.search === 'function' ? opts.search(prev) : opts.search;
        routerState.turnId = next?.turnId;
        // Notify every mounted useSearch subscriber to re-render.
        for (const listener of routerListeners) listener();
      },
  };
});

// Mock toast so error branches don't blow up without a <Toaster />.
vi.mock('sonner', () => ({ toast: { error: vi.fn() } }));

const CONFIG: ConfigResponse = {
  protocols: ['openai', 'anthropic'],
  providers: [
    {
      name: 'openai',
      supported_protocols: ['openai'],
      models: [{ id: 'gpt-5.4-mini', name: 'GPT-5.4 Mini' }],
    },
  ],
  tools: [
    { name: 'read', description: 'read', default_policy: 'auto' },
    { name: 'write', description: 'write', default_policy: 'require_approval' },
    { name: 'bash', description: 'bash', default_policy: 'require_approval' },
  ],
};

// Use vi.hoisted so these mocks are available when vi.mock's factory runs
// (vi.mock is hoisted to the top of the file).
const { turnsApi, sessionsApi, configGet } = vi.hoisted(() => ({
  turnsApi: {
    create: vi.fn(),
    retry: vi.fn(),
    approve: vi.fn(),
    cancel: vi.fn(),
    tree: vi.fn(),
    get: vi.fn(),
  },
  sessionsApi: {
    get: vi.fn(),
  },
  configGet: vi.fn(),
}));

vi.mock('../api', () => ({
  api: {
    config: { get: configGet },
    sessions: sessionsApi,
    turns: turnsApi,
  },
}));

function renderPage() {
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false } },
  });
  client.setQueryData(['config'], CONFIG);
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
    // Each test starts with a clean URL (no modal open).
    routerState.turnId = undefined;
    turnsApi.create.mockReset();
    turnsApi.retry.mockReset();
    turnsApi.approve.mockReset();
    turnsApi.cancel.mockReset();
    turnsApi.tree.mockReset();
    turnsApi.get.mockReset();
    sessionsApi.get.mockReset();
    sessionsApi.get.mockResolvedValue({
      session: makeSession({ id: 'session-1', protocol: 'openai' }),
    });
    configGet.mockReset();
    configGet.mockResolvedValue(CONFIG);
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
        model: 'gpt-5.4-mini',
      });
    });

    // After the mutation succeeds, the tree query should have been refetched
    // and the new turn text should eventually appear in the tree view.
    await screen.findByText('hi');
  });

  it('auto-opens the failed turn after the first send fails', async () => {
    const failed = makeTurn({
      id: 'failed-1',
      user_text: 'initial prompt',
      status: 'failed',
      retry_turn_id: null,
    });
    turnsApi.tree
      .mockResolvedValueOnce({ turns: [] })
      .mockResolvedValue({ turns: [failed] });
    turnsApi.create.mockRejectedValue(new Error('API error: 502'));

    renderPage();

    const textarea = await screen.findByPlaceholderText(/type your message/i);
    await userEvent.setup().type(textarea, 'initial prompt{Enter}');

    await waitFor(() => {
      expect(turnsApi.tree).toHaveBeenCalledTimes(2);
    });
    expect(
      (await screen.findAllByText('initial prompt')).length,
    ).toBeGreaterThan(0);
    expect(
      await screen.findByRole('button', { name: /retry/i }),
    ).toBeInTheDocument();
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

  it('restores the detail modal when the URL already has a turnId (refresh)', async () => {
    // Simulate landing on `/sessions/session-1?turnId=persisted-1`, i.e. a
    // page refresh after the user had the modal open. The router state is
    // seeded before render so the first paint already shows the modal.
    const persisted = makeTurn({
      id: 'persisted-1',
      user_text: 'hello from the past',
      status: 'completed',
    });
    turnsApi.tree.mockResolvedValue({ turns: [persisted] });
    routerState.turnId = 'persisted-1';

    renderPage();

    // The modal renders the turn's user_text in its DialogTitle (truncated to
    // 80 chars). Finding it confirms the modal opened purely from the URL.
    const dialog = await screen.findByRole('dialog');
    expect(dialog).toHaveTextContent('hello from the past');
  });

  it('clears ?turnId from the URL when the modal is closed', async () => {
    const persisted = makeTurn({
      id: 'persisted-2',
      user_text: 'closing flow',
      status: 'completed',
    });
    turnsApi.tree.mockResolvedValue({ turns: [persisted] });
    routerState.turnId = 'persisted-2';

    renderPage();

    // Wait for the modal to render from the seeded URL state.
    await screen.findByRole('dialog');
    // Close via the radix/base-ui Dialog close control. We press Escape since
    // it's the most robust way to trigger onOpenChange(false) in jsdom/browser
    // without depending on a specific close-button label.
    await userEvent.setup().keyboard('{Escape}');

    await waitFor(() => {
      expect(routerState.turnId).toBeUndefined();
    });
  });
});
