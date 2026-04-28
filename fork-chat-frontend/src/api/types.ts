export interface Model {
  id: string;
  name: string;
  provider: string;
}

export interface ConfigResponse {
  models: Model[];
}

export interface Session {
  id: string;
  title: string | null;
  system_prompt: string | null;
  metadata: Record<string, unknown>;
  created_at: string;
  updated_at: string;
}

export interface Turn {
  id: string;
  session_id: string;
  parent_turn_id: string | null;
  retry_turn_id: string | null;
  status: 'running' | 'completed' | 'failed';
  user_text: string | null;
  assistant_text: string | null;
  raw_items: unknown[];
  provider: string | null;
  model: string | null;
  input_tokens: number | null;
  output_tokens: number | null;
  cached_tokens: number | null;
  error: Record<string, unknown> | null;
  metadata: Record<string, unknown>;
  created_at: string;
  completed_at: string | null;
}

export interface CreateSessionRequest {
  system_prompt?: string;
}

export interface CreateSessionResponse {
  session: Session;
}

export interface CreateTurnRequest {
  parent_turn_id?: string;
  user_text: string;
  provider: string;
  model: string;
}

export interface CreateTurnResponse {
  turn: Turn;
}

export interface TreeResponse {
  turns: Turn[];
}
