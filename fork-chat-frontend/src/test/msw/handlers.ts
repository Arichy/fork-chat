import { HttpResponse, http } from 'msw';
import type {
  ConfigResponse,
  CreateSessionResponse,
  CreateTurnResponse,
  SessionsPageResponse,
  TreeResponse,
  Turn,
} from '../../api/types';
import { makeSession, makeTurn } from '../fixtures';

const API_BASE = 'http://localhost:3000/api';

// Default happy-path handlers. Individual tests override via `server.use(...)`.
export const handlers = [
  http.get(`${API_BASE}/config`, () => {
    const body: ConfigResponse = {
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
      tools: [
        {
          name: 'read',
          description: 'Read a UTF-8 text file from disk by path.',
          default_policy: 'auto',
        },
        {
          name: 'write',
          description: 'Write UTF-8 text content to a file path.',
          default_policy: 'require_approval',
        },
        {
          name: 'bash',
          description: 'Run a shell command via `bash -lc`.',
          default_policy: 'require_approval',
        },
      ],
    };
    return HttpResponse.json(body);
  }),

  http.get(`${API_BASE}/sessions`, () => {
    const body: SessionsPageResponse = {
      sessions: [makeSession()],
      next_cursor: null,
    };
    return HttpResponse.json(body);
  }),

  http.post(`${API_BASE}/sessions`, () => {
    const body: CreateSessionResponse = { session: makeSession() };
    return HttpResponse.json(body);
  }),

  http.get(`${API_BASE}/sessions/:id`, ({ params }) => {
    return HttpResponse.json({
      session: makeSession({ id: String(params.id) }),
    });
  }),

  http.delete(`${API_BASE}/sessions/:id`, () => {
    return HttpResponse.json({ deleted: true });
  }),

  http.patch(`${API_BASE}/sessions/:id`, async ({ request, params }) => {
    const body = (await request.json()) as { title: string };
    return HttpResponse.json({
      session: makeSession({ id: String(params.id), title: body.title }),
    });
  }),

  http.post(`${API_BASE}/sessions/:id/turns`, async ({ request, params }) => {
    const body = (await request.json()) as {
      user_text: string;
      provider: string;
      model: string;
      parent_turn_id?: string;
    };
    const turn: Turn = makeTurn({
      session_id: String(params.id),
      user_text: body.user_text,
      assistant_text: 'Hi there!',
      provider: body.provider,
      model: body.model,
      parent_turn_id: body.parent_turn_id ?? null,
      status: 'completed',
    });
    const resp: CreateTurnResponse = { turn };
    return HttpResponse.json(resp);
  }),

  http.post(`${API_BASE}/sessions/:id/turns/:turnId/retry`, ({ params }) => {
    const turn: Turn = makeTurn({
      session_id: String(params.id),
      status: 'completed',
    });
    return HttpResponse.json({ turn } satisfies CreateTurnResponse);
  }),

  http.post(`${API_BASE}/sessions/:id/turns/:turnId/approve`, ({ params }) => {
    const turn: Turn = makeTurn({
      id: String(params.turnId),
      session_id: String(params.id),
      status: 'running',
    });
    return HttpResponse.json({ turn });
  }),

  http.post(`${API_BASE}/sessions/:id/turns/:turnId/cancel`, ({ params }) => {
    const turn: Turn = makeTurn({
      id: String(params.turnId),
      session_id: String(params.id),
      status: 'failed',
    });
    return HttpResponse.json({ turn });
  }),

  http.get(`${API_BASE}/sessions/:id/tree`, ({ params }) => {
    const body: TreeResponse = {
      turns: [makeTurn({ session_id: String(params.id) })],
    };
    return HttpResponse.json(body);
  }),

  http.get(`${API_BASE}/sessions/:id/turns/:turnId`, ({ params }) => {
    return HttpResponse.json({
      turn: makeTurn({
        id: String(params.turnId),
        session_id: String(params.id),
      }),
    });
  }),

  http.post(`${API_BASE}/sessions/batch-delete`, async ({ request }) => {
    const body = (await request.json()) as { ids: string[] };
    return HttpResponse.json({ deleted: body.ids.length });
  }),
];
