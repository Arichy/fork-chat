import { HttpResponse, http } from 'msw';
import type {
  ConfigResponse,
  CreateSessionResponse,
  CreateTurnResponse,
  Session,
  TreeResponse,
  Turn,
} from '../../api/types';
import { makeSession, makeTurn } from '../fixtures';

const API_BASE = 'http://localhost:3000/api';

// Default happy-path handlers. Individual tests override via `server.use(...)`.
export const handlers = [
  http.get(`${API_BASE}/config`, () => {
    const body: ConfigResponse = {
      models: [
        { id: 'gpt-4o-mini', name: 'GPT-4o Mini', provider: 'openai' },
        { id: 'gpt-4o', name: 'GPT-4o', provider: 'openai' },
      ],
    };
    return HttpResponse.json(body);
  }),

  http.get(`${API_BASE}/sessions`, () => {
    const body: Session[] = [makeSession()];
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
      model: string;
      parent_turn_id?: string;
    };
    const turn: Turn = makeTurn({
      session_id: String(params.id),
      user_text: body.user_text,
      assistant_text: 'Hi there!',
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
];
