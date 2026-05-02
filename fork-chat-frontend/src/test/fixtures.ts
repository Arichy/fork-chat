import type { Session, Turn } from '../api/types';

let turnCounter = 0;
let sessionCounter = 0;

export function makeSession(overrides: Partial<Session> = {}): Session {
  sessionCounter += 1;
  return {
    id: `session-${sessionCounter}`,
    title: null,
    system_prompt: null,
    protocol: 'openai',
    preferences: {},
    created_at: new Date('2026-01-01T00:00:00Z').toISOString(),
    updated_at: new Date('2026-01-01T00:00:00Z').toISOString(),
    ...overrides,
  };
}

export function makeTurn(overrides: Partial<Turn> = {}): Turn {
  turnCounter += 1;
  return {
    id: `turn-${turnCounter}`,
    session_id: 'session-1',
    parent_turn_id: null,
    retry_turn_id: null,
    status: 'completed',
    user_text: 'Hello',
    assistant_text: 'Hi there!',
    turn_messages: [],
    provider: 'openai',
    model: 'gpt-5.4-mini',
    input_tokens: 10,
    output_tokens: 20,
    cached_tokens: 0,
    error: null,
    runtime_state: {},
    created_at: new Date('2026-01-01T00:00:00Z').toISOString(),
    completed_at: new Date('2026-01-01T00:00:01Z').toISOString(),
    ...overrides,
  };
}
