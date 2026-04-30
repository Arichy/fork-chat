import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { fireEvent, render, screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { describe, expect, it, vi } from 'vitest';
import { makeTurn } from '../test/fixtures';
import { MessageInput } from './MessageInput';

// Mock the api module so useQuery resolves deterministically in the browser project.
vi.mock('../api', () => ({
  api: {
    config: {
      get: vi.fn().mockResolvedValue({
        models: [
          { id: 'gpt-4o-mini', name: 'GPT-4o Mini', provider: 'openai' },
          { id: 'gpt-4o', name: 'GPT-4o', provider: 'openai' },
        ],
      }),
    },
  },
}));

function renderInput(props: Partial<Parameters<typeof MessageInput>[0]> = {}) {
  const onSend = props.onSend ?? vi.fn();
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false } },
  });
  // Pre-seed the config so we don't need to await an async fetch.
  client.setQueryData(['config'], {
    models: [
      { id: 'gpt-4o-mini', name: 'GPT-4o Mini', provider: 'openai' },
      { id: 'gpt-4o', name: 'GPT-4o', provider: 'openai' },
    ],
  });

  const utils = render(
    <QueryClientProvider client={client}>
      <MessageInput
        parentTurn={props.parentTurn ?? null}
        onSend={onSend}
        disabled={props.disabled}
      />
    </QueryClientProvider>,
  );
  return { ...utils, onSend };
}

describe('MessageInput', () => {
  it('renders an enabled Send button once text is typed', async () => {
    const { onSend } = renderInput();
    const user = userEvent.setup();

    const send = screen.getByRole('button', { name: /send/i });
    expect(send).toBeDisabled();

    const textarea = screen.getByPlaceholderText(/type your message/i);
    await user.type(textarea, 'hello world');
    expect(send).toBeEnabled();

    await user.click(send);
    expect(onSend).toHaveBeenCalledWith('hello world', 'gpt-4o-mini', null);
  });

  it('submits on Enter and clears the input', async () => {
    const { onSend } = renderInput();
    const user = userEvent.setup();

    const textarea = screen.getByPlaceholderText(/type your message/i);
    await user.type(textarea, 'one{Enter}');

    expect(onSend).toHaveBeenCalledWith('one', 'gpt-4o-mini', null);
    expect(textarea).toHaveValue('');
  });

  it('Shift+Enter inserts a newline without submitting', async () => {
    const { onSend } = renderInput();
    const user = userEvent.setup();

    const textarea = screen.getByPlaceholderText(
      /type your message/i,
    ) as HTMLTextAreaElement;
    await user.type(textarea, 'line1{Shift>}{Enter}{/Shift}line2');

    expect(onSend).not.toHaveBeenCalled();
    expect(textarea.value).toContain('\n');
  });

  it('does not submit when text is only whitespace', async () => {
    const { onSend } = renderInput();
    const user = userEvent.setup();

    const textarea = screen.getByPlaceholderText(/type your message/i);
    await user.type(textarea, '   {Enter}');
    expect(onSend).not.toHaveBeenCalled();
  });

  it('disables textarea and Send button when disabled prop is true', () => {
    renderInput({ disabled: true });
    expect(screen.getByPlaceholderText(/type your message/i)).toBeDisabled();
    expect(screen.getByRole('button', { name: /send/i })).toBeDisabled();
  });

  it('passes parentTurn.id to onSend when provided', async () => {
    const parent = makeTurn({ id: 'parent-1', user_text: 'parent question' });
    const { onSend } = renderInput({ parentTurn: parent });
    const user = userEvent.setup();

    await user.type(screen.getByPlaceholderText(/type your message/i), 'reply');
    await user.click(screen.getByRole('button', { name: /send/i }));

    expect(onSend).toHaveBeenCalledWith('reply', 'gpt-4o-mini', 'parent-1');
    expect(screen.getByText(/replying to:/i)).toBeInTheDocument();
  });

  it('trims text before sending', async () => {
    const { onSend } = renderInput();
    const user = userEvent.setup();
    const textarea = screen.getByPlaceholderText(/type your message/i);
    // Use fireEvent to set exact whitespace-padded value.
    fireEvent.change(textarea, { target: { value: '   padded   ' } });
    await user.click(screen.getByRole('button', { name: /send/i }));
    expect(onSend).toHaveBeenCalledWith('padded', 'gpt-4o-mini', null);
  });
});
