import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { fireEvent, render, screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { describe, expect, it, vi } from 'vitest';
import type { ConfigResponse } from '../api/types';
import { makeTurn } from '../test/fixtures';
import { MessageInput } from './MessageInput';

const { CONFIG } = vi.hoisted(() => {
  const CONFIG: ConfigResponse = {
    protocols: ['openai', 'anthropic'],
    providers: [
      {
        name: 'openai',
        supported_protocols: ['openai'],
        models: [
          { id: 'gpt-5.4-mini', name: 'GPT-5.4 Mini' },
          { id: 'gpt-5.5', name: 'GPT-5.5' },
        ],
      },
      {
        name: 'anthropic',
        supported_protocols: ['anthropic'],
        models: [{ id: 'claude-sonnet-4-6', name: 'Claude Sonnet 4.6' }],
      },
    ],
  };
  return { CONFIG };
});

// Mock the api module so useQuery resolves deterministically in the browser project.
vi.mock('../api', () => ({
  api: {
    config: {
      get: vi.fn().mockResolvedValue(CONFIG),
    },
  },
}));

function renderInput(props: Partial<Parameters<typeof MessageInput>[0]> = {}) {
  const onSend = props.onSend ?? vi.fn();
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false } },
  });
  // Pre-seed the config so we don't need to await an async fetch.
  client.setQueryData(['config'], CONFIG);

  const utils = render(
    <QueryClientProvider client={client}>
      <MessageInput
        parentTurn={props.parentTurn ?? null}
        protocol={props.protocol ?? 'openai'}
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
    expect(onSend).toHaveBeenCalledWith(
      'hello world',
      'openai',
      'gpt-5.4-mini',
      null,
    );
  });

  it('submits on Enter and clears the input', async () => {
    const { onSend } = renderInput();
    const user = userEvent.setup();

    const textarea = screen.getByPlaceholderText(/type your message/i);
    await user.type(textarea, 'one{Enter}');

    expect(onSend).toHaveBeenCalledWith('one', 'openai', 'gpt-5.4-mini', null);
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

    expect(onSend).toHaveBeenCalledWith(
      'reply',
      'openai',
      'gpt-5.4-mini',
      'parent-1',
    );
    expect(screen.getByText(/replying to:/i)).toBeInTheDocument();
  });

  it('defaults to the parent turn model when creating a child turn', async () => {
    const parent = makeTurn({
      id: 'parent-2',
      provider: 'openai',
      model: 'gpt-5.5',
      user_text: 'ask follow-up',
    });
    const { onSend } = renderInput({ parentTurn: parent, protocol: 'openai' });
    const user = userEvent.setup();

    await user.type(screen.getByPlaceholderText(/type your message/i), 'child');
    await user.click(screen.getByRole('button', { name: /send/i }));

    expect(onSend).toHaveBeenCalledWith(
      'child',
      'openai',
      'gpt-5.5',
      'parent-2',
    );
  });

  it('trims text before sending', async () => {
    const { onSend } = renderInput();
    const user = userEvent.setup();
    const textarea = screen.getByPlaceholderText(/type your message/i);
    // Use fireEvent to set exact whitespace-padded value.
    fireEvent.change(textarea, { target: { value: '   padded   ' } });
    await user.click(screen.getByRole('button', { name: /send/i }));
    expect(onSend).toHaveBeenCalledWith(
      'padded',
      'openai',
      'gpt-5.4-mini',
      null,
    );
  });

  it('only shows providers that support the session protocol', async () => {
    const { onSend } = renderInput({ protocol: 'anthropic' });
    const user = userEvent.setup();

    await user.type(
      screen.getByPlaceholderText(/type your message/i),
      'hi anthropic',
    );
    await user.click(screen.getByRole('button', { name: /send/i }));

    expect(onSend).toHaveBeenCalledWith(
      'hi anthropic',
      'anthropic',
      'claude-sonnet-4-6',
      null,
    );
  });
});
