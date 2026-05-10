import { render, screen } from '@testing-library/react';
import { describe, expect, it } from 'vitest';
import { makeTurn } from '../test/fixtures';
import { MessageView } from './MessageView';

describe('MessageView', () => {
  it('renders user and assistant text', () => {
    render(
      <MessageView
        turn={makeTurn({ user_text: 'Question?', assistant_text: 'Answer.' })}
      />,
    );
    expect(screen.getByText('Question?')).toBeInTheDocument();
    expect(screen.getByText('Answer.')).toBeInTheDocument();
  });

  it('applies green badge classes for completed status', () => {
    render(<MessageView turn={makeTurn({ status: 'completed' })} />);
    const badge = screen.getByText('completed');
    expect(badge).toHaveClass('bg-green-100', 'text-green-800');
  });

  it('applies yellow badge classes for running status', () => {
    render(<MessageView turn={makeTurn({ status: 'running' })} />);
    const badge = screen.getByText('running');
    expect(badge).toHaveClass('bg-yellow-100', 'text-yellow-800');
  });

  it('applies red badge classes for failed status', () => {
    render(<MessageView turn={makeTurn({ status: 'failed' })} />);
    const badge = screen.getByText('failed');
    expect(badge).toHaveClass('bg-red-100', 'text-red-800');
  });

  it('shows token counts when input_tokens is present', () => {
    render(
      <MessageView turn={makeTurn({ input_tokens: 15, output_tokens: 42 })} />,
    );
    expect(screen.getByText(/15 in \/ 42 out/)).toBeInTheDocument();
  });

  it('hides token counts when input_tokens is null', () => {
    render(<MessageView turn={makeTurn({ input_tokens: null })} />);
    expect(screen.queryByText(/in \/ .* out/)).not.toBeInTheDocument();
  });

  it('renders the model string', () => {
    render(<MessageView turn={makeTurn({ model: 'gpt-5.5' })} />);
    expect(screen.getByText('gpt-5.5')).toBeInTheDocument();
  });

  it('renders structured error diagnostics when error is present', () => {
    render(
      <MessageView
        turn={makeTurn({
          error: {
            kind: 'loop_error',
            message: 'rate_limit',
            chain: ['LLM API error', 'provider said no'],
            debug: 'src/llm/openai/adapter.rs:120',
          },
          status: 'failed',
        })}
      />,
    );
    expect(screen.getByText(/rate_limit/)).toBeInTheDocument();
    expect(screen.getByText('Diagnostics')).toBeInTheDocument();
    expect(screen.getByText(/provider said no/)).toBeInTheDocument();
    expect(
      screen.getByText(/src\/llm\/openai\/adapter.rs:120/),
    ).toBeInTheDocument();
  });

  it('renders markdown bold as <strong>', () => {
    render(
      <MessageView
        turn={makeTurn({ user_text: 'say **hi**', assistant_text: null })}
      />,
    );
    const strong = screen.getByText('hi');
    expect(strong.tagName.toLowerCase()).toBe('strong');
  });
});
