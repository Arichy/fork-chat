import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { render, screen, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { describe, expect, it, vi } from 'vitest';
import type { ConfigResponse, Protocol, Turn } from '../api/types';
import { makeTurn } from '../test/fixtures';
import { TurnDetailModal } from './TurnDetailModal';

const { CONFIG } = vi.hoisted(() => {
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
      {
        name: 'write',
        description: 'write',
        default_policy: 'require_approval',
      },
      { name: 'bash', description: 'bash', default_policy: 'require_approval' },
    ],
  };
  return { CONFIG };
});

// MessageInput queries the config endpoint through @/api; mock it out so the
// modal renders deterministically in the browser project.
vi.mock('../api', () => ({
  api: {
    config: {
      get: vi.fn().mockResolvedValue(CONFIG),
    },
  },
}));

function renderModal(props: {
  turn: Turn | null;
  open: boolean;
  protocol?: Protocol;
  onOpenChange?: (v: boolean) => void;
  onSend?: (t: string, provider: string, m: string, pid: string | null) => void;
  onRetry?: (id: string, provider: string, model: string) => void;
  onApprove?: (
    turnId: string,
    pendingCallId: string,
    decision: 'allow' | 'allow_always' | 'deny',
  ) => void;
  onCancel?: (turnId: string) => void;
  isSending?: boolean;
}) {
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false } },
  });
  client.setQueryData(['config'], CONFIG);
  return render(
    <QueryClientProvider client={client}>
      <TurnDetailModal
        turn={props.turn}
        protocol={props.protocol ?? 'openai'}
        open={props.open}
        onOpenChange={props.onOpenChange ?? vi.fn()}
        onSend={props.onSend ?? vi.fn()}
        onRetry={props.onRetry ?? vi.fn()}
        onApprove={props.onApprove ?? vi.fn()}
        onCancel={props.onCancel ?? vi.fn()}
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
      provider: 'openai',
      model: 'gpt-5.5',
      user_text: 'broke',
      error: { message: 'boom' },
    });
    renderModal({ turn: failed, open: true, onRetry });

    const retry = await screen.findByRole('button', { name: /retry/i });
    await userEvent.setup().click(retry);
    expect(onRetry).toHaveBeenCalledWith('failed-1', 'openai', 'gpt-5.5');
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
          protocol="openai"
          open={true}
          onOpenChange={vi.fn()}
          onSend={vi.fn()}
          onRetry={vi.fn()}
          onApprove={vi.fn()}
          onCancel={vi.fn()}
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
      turn: makeTurn({ status: 'running', model: 'gpt-5.5' }),
      open: true,
    });
    expect(screen.getByText('running')).toBeInTheDocument();
    expect(screen.getByText('gpt-5.5')).toBeInTheDocument();
  });

  it('renders structured error diagnostics for failed turns', () => {
    renderModal({
      turn: makeTurn({
        status: 'failed',
        error: {
          kind: 'loop_error',
          message: 'network_error',
          chain: ['LLM API error', 'EOF while parsing a value'],
          debug: 'turn_lifecycle.rs:1090',
        },
      }),
      open: true,
    });
    expect(screen.getByText(/network_error/)).toBeInTheDocument();
    expect(screen.getByText('Diagnostics')).toBeInTheDocument();
    expect(screen.getByText(/EOF while parsing a value/)).toBeInTheDocument();
    expect(screen.getByText(/turn_lifecycle.rs:1090/)).toBeInTheDocument();
  });

  it('renders approval actions inside the matching tool call card', async () => {
    const onApprove = vi.fn();
    const turn = makeTurn({
      id: 'await-1',
      status: 'awaiting_approval',
      turn_messages: [
        {
          role: 'assistant',
          content: [
            {
              type: 'function_call',
              id: 'fc_1',
              call_id: 'toolu_1',
              name: 'write',
              arguments: '{"path":"a.txt","content":"x"}',
            },
          ],
        },
      ],
      runtime_state: {
        pending_tool_calls: [
          {
            pending_call_id: 'pcall_1',
            call_id: 'toolu_1',
            name: 'write',
            input: { path: 'a.txt', content: 'x' },
          },
        ],
      },
    });
    renderModal({ turn, open: true, onApprove });

    const toolCard = await screen.findByTestId('tool-call-card');
    const inputDetails = within(toolCard).getByText('Input').closest('details');
    expect(inputDetails).toHaveAttribute('open');
    expect(screen.queryByText('write • pcall_1')).not.toBeInTheDocument();

    const allowBtn = within(toolCard).getByRole('button', { name: 'Allow' });
    await userEvent.setup().click(allowBtn);
    expect(onApprove).toHaveBeenCalledWith('await-1', 'pcall_1', 'allow');
  });

  it('renders trace immediately when transcript is present', () => {
    const turn = makeTurn({
      turn_messages: [
        {
          role: 'user',
          content: [{ type: 'text', text: 'hello from transcript' }],
        },
        {
          role: 'assistant',
          content: [{ type: 'text', text: 'assistant from transcript' }],
        },
      ],
      user_text: 'fallback user text',
      assistant_text: 'fallback assistant text',
    });

    renderModal({ turn, open: true });

    expect(screen.getByText('Trace')).toBeInTheDocument();
    expect(screen.getByText('assistant from transcript')).toBeInTheDocument();
  });

  it('renders OpenAI-style user transcript entries as user text cards', () => {
    const turn = makeTurn({
      turn_messages: [
        {
          role: 'user',
          content: [{ role: 'user', content: 'hello from openai transcript' }],
        },
      ],
      user_text: 'fallback user text',
    });

    renderModal({ turn, open: true });

    expect(screen.getByText('Trace')).toBeInTheDocument();
    expect(
      screen.getByText('hello from openai transcript'),
    ).toBeInTheDocument();
  });

  it('renders large tool output as truncated preview text', async () => {
    const hugeOutput = 'x'.repeat(10_000);
    const turn = makeTurn({
      turn_messages: [
        {
          role: 'user',
          content: [
            {
              type: 'function_call_output',
              call_id: 'call_1',
              output: hugeOutput,
            },
          ],
        },
      ],
    });

    renderModal({ turn, open: true });

    expect(screen.getByText('Tool result')).toBeInTheDocument();
    expect(
      screen.getByText(/\[preview truncated for performance\]/i),
    ).toBeInTheDocument();
  });
});
