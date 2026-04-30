import { HttpResponse, http } from 'msw';
import { describe, expect, it } from 'vitest';
import { server } from '../test/server-from-setup';
import { api } from './client';

// Re-export the MSW server instance used by the setup file so tests can patch it.
// (setup.ts already wires beforeAll/afterEach/afterAll.)

const API_BASE = 'http://localhost:3000/api';

describe('api.config', () => {
  it('GET /config returns models', async () => {
    const result = await api.config.get();
    expect(result.models.length).toBeGreaterThan(0);
    expect(result.models[0]).toMatchObject({
      id: expect.any(String),
      provider: expect.any(String),
    });
  });
});

describe('api.sessions', () => {
  it('list() returns Session[]', async () => {
    const sessions = await api.sessions.list();
    expect(Array.isArray(sessions)).toBe(true);
  });

  it('get(id) fetches /sessions/:id', async () => {
    const { session } = await api.sessions.get('abc');
    expect(session.id).toBe('abc');
  });

  it('create() POSTs JSON body', async () => {
    let captured: unknown = null;
    server.use(
      http.post(`${API_BASE}/sessions`, async ({ request }) => {
        captured = await request.json();
        return HttpResponse.json({
          session: {
            id: 's',
            title: null,
            system_prompt: null,
            metadata: {},
            created_at: new Date().toISOString(),
            updated_at: new Date().toISOString(),
          },
        });
      }),
    );

    await api.sessions.create({ system_prompt: 'be nice' });
    expect(captured).toEqual({ system_prompt: 'be nice' });
  });

  it('delete() uses DELETE method', async () => {
    const result = await api.sessions.delete('s1');
    expect(result.deleted).toBe(true);
  });

  it('updateTitle() sends PATCH with title', async () => {
    const { session } = await api.sessions.updateTitle('abc', 'New Title');
    expect(session.title).toBe('New Title');
  });
});

describe('api.turns', () => {
  it('create() POSTs to /sessions/:id/turns', async () => {
    const { turn } = await api.turns.create('session-x', {
      user_text: 'hi',
      provider: 'openai',
      model: 'gpt-4o-mini',
    });
    expect(turn.user_text).toBe('hi');
    expect(turn.session_id).toBe('session-x');
  });

  it('retry() POSTs to /turns/:id/retry', async () => {
    const { turn } = await api.turns.retry('s1', 't1', {
      provider: 'openai',
      model: 'gpt-4o-mini',
    });
    expect(turn.status).toBe('completed');
  });

  it('tree() fetches the session tree', async () => {
    const { turns } = await api.turns.tree('session-y');
    expect(Array.isArray(turns)).toBe(true);
  });

  it('get() fetches a single turn', async () => {
    const { turn } = await api.turns.get('s', 't');
    expect(turn.id).toBe('t');
  });
});

describe('fetchApi error handling', () => {
  it('throws "API error: <status>" on non-2xx', async () => {
    server.use(
      http.get(`${API_BASE}/config`, () =>
        HttpResponse.json({ error: 'boom' }, { status: 500 }),
      ),
    );
    await expect(api.config.get()).rejects.toThrow('API error: 500');
  });

  it('sets Content-Type: application/json on mutation requests', async () => {
    let contentType: string | null = null;
    server.use(
      http.post(`${API_BASE}/sessions`, ({ request }) => {
        contentType = request.headers.get('content-type');
        return HttpResponse.json({
          session: {
            id: 's',
            title: null,
            system_prompt: null,
            metadata: {},
            created_at: new Date().toISOString(),
            updated_at: new Date().toISOString(),
          },
        });
      }),
    );
    await api.sessions.create({});
    expect(contentType).toBe('application/json');
  });
});
