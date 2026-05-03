/**
 * HTTP API client for the fork-chat backend.
 *
 * All endpoints are prefixed with `API_BASE` (currently hardcoded to
 * `http://localhost:3000/api`). Each method returns a typed promise, making
 * the client safe to use with React Query's type inference.
 *
 * The one exception is `streamUrl` which returns a URL string instead of
 * fetching — see its doc comment for details.
 */

const API_BASE = 'http://localhost:3000/api';

/**
 * Generic fetch wrapper that adds JSON headers and throws on non-2xx responses.
 *
 * @param path - API path relative to `API_BASE` (e.g. `/sessions`)
 * @param options - Standard fetch options; `Content-Type: application/json`
 *   is always set.
 */
async function fetchApi<T>(path: string, options?: RequestInit): Promise<T> {
  const response = await fetch(`${API_BASE}${path}`, {
    ...options,
    headers: {
      'Content-Type': 'application/json',
      ...options?.headers,
    },
  });

  if (!response.ok) {
    throw new Error(`API error: ${response.status}`);
  }

  return response.json();
}

/**
 * Typed API surface for all backend endpoints.
 *
 * Organized by resource (config, sessions, turns) matching the backend's
 * route structure.
 */
export const api = {
  /** Server configuration (protocols, providers, tools). */
  config: {
    /** `GET /api/config` — fetch available protocols, providers, and tools. */
    get: () => fetchApi<import('./types').ConfigResponse>('/config'),
  },

  /** Session CRUD operations. */
  sessions: {
    /**
     * `GET /api/sessions` — list sessions with cursor-based pagination.
     *
     * @param params.limit - Max sessions to return (default 20, max 100)
     * @param params.cursor - Pagination cursor from the previous response
     * @param params.sort - Sort field ('updated_at' or 'created_at')
     * @param params.filter - Title search filter (empty strings ignored)
     */
    list: async (params?: {
      limit?: number;
      cursor?: import('./types').SessionsPageCursor | null;
      sort?: import('./types').SessionsSort;
      filter?: string;
    }) => {
      // Build query string from provided parameters, omitting empty values.
      const search = new URLSearchParams();
      if (params?.limit != null) {
        search.set('limit', String(params.limit));
      }
      // Cursor must be sent as a (before_at, before_id) pair.
      if (params?.cursor) {
        search.set('before_at', params.cursor.before_at);
        search.set('before_id', params.cursor.before_id);
      }
      if (params?.sort) {
        search.set('sort', params.sort);
      }
      // Trim whitespace from filter and skip if empty.
      if (params?.filter != null && params.filter.trim().length > 0) {
        search.set('filter', params.filter.trim());
      }
      const query = search.size > 0 ? `?${search.toString()}` : '';
      return fetchApi<import('./types').SessionsPageResponse>(
        `/sessions${query}`,
      );
    },

    /** `GET /api/sessions/{id}` — fetch a single session. */
    get: (id: string) =>
      fetchApi<{ session: import('./types').Session }>(`/sessions/${id}`),

    /** `POST /api/sessions` — create a new session with a locked protocol. */
    create: (data: import('./types').CreateSessionRequest) =>
      fetchApi<import('./types').CreateSessionResponse>('/sessions', {
        method: 'POST',
        body: JSON.stringify(data),
      }),

    /** `DELETE /api/sessions/{id}` — delete a session and all its turns. */
    delete: (id: string) =>
      fetchApi<{ deleted: boolean }>(`/sessions/${id}`, { method: 'DELETE' }),

    /** `POST /api/sessions/batch-delete` — delete multiple sessions. */
    batchDelete: (ids: string[]) =>
      fetchApi<import('./types').BatchDeleteResponse>(
        '/sessions/batch-delete',
        {
          method: 'POST',
          body: JSON.stringify({ ids }),
        },
      ),

    /** `PATCH /api/sessions/{id}` — update a session's title. */
    updateTitle: (id: string, title: string) =>
      fetchApi<{ session: import('./types').Session }>(`/sessions/${id}`, {
        method: 'PATCH',
        body: JSON.stringify({ title }),
      }),
  },

  /** Turn lifecycle operations (create, read, stream, approve, cancel, retry). */
  turns: {
    /**
     * `POST /api/sessions/{id}/turns` — create a new turn.
     *
     * This starts the turn loop on the backend. The turn will begin in
     * `running` state and the client should immediately open an SSE stream
     * to receive incremental updates.
     */
    create: (sessionId: string, data: import('./types').CreateTurnRequest) =>
      fetchApi<import('./types').CreateTurnResponse>(
        `/sessions/${sessionId}/turns`,
        {
          method: 'POST',
          body: JSON.stringify(data),
        },
      ),

    /** `GET /api/sessions/{id}/turns/{turnId}` — fetch a single turn. */
    get: (sessionId: string, turnId: string) =>
      fetchApi<{ turn: import('./types').Turn }>(
        `/sessions/${sessionId}/turns/${turnId}`,
      ),

    /**
     * Returns the SSE stream URL for a turn.
     *
     * This returns a **URL string** rather than fetching because it's used
     * with the browser's `EventSource` API, which requires a URL to
     * construct. `EventSource` manages its own HTTP GET request and handles
     * reconnection logic internally, so we don't use `fetch` here.
     *
     * @example
     * ```ts
     * const source = new EventSource(api.turns.streamUrl(sessionId, turnId));
     * source.addEventListener('turn_snapshot', (evt) => { ... });
     * ```
     */
    streamUrl: (sessionId: string, turnId: string) =>
      `${API_BASE}/sessions/${sessionId}/turns/${turnId}/stream`,

    /**
     * `POST /api/sessions/{id}/turns/{turnId}/retry` — retry a failed turn.
     *
     * Creates a new turn that re-executes from the same parent context as the
     * failed turn, using the specified provider and model.
     */
    retry: (
      sessionId: string,
      turnId: string,
      data: { provider: string; model: string },
    ) =>
      fetchApi<import('./types').CreateTurnResponse>(
        `/sessions/${sessionId}/turns/${turnId}/retry`,
        { method: 'POST', body: JSON.stringify(data) },
      ),

    /**
     * `POST /api/sessions/{id}/turns/{turnId}/approve` — submit approval
     * decisions for pending tool calls.
     *
     * Each pending tool call gets its own decision (allow, allow_always, or deny).
     */
    approve: (
      sessionId: string,
      turnId: string,
      data: import('./types').ApproveTurnRequest,
    ) =>
      fetchApi<{ turn: import('./types').Turn }>(
        `/sessions/${sessionId}/turns/${turnId}/approve`,
        { method: 'POST', body: JSON.stringify(data) },
      ),

    /**
     * `POST /api/sessions/{id}/turns/{turnId}/cancel` — cancel a running
     * or awaiting-approval turn.
     */
    cancel: (sessionId: string, turnId: string) =>
      fetchApi<{ turn: import('./types').Turn }>(
        `/sessions/${sessionId}/turns/${turnId}/cancel`,
        { method: 'POST' },
      ),

    /**
     * `GET /api/sessions/{id}/tree` — fetch the full turn tree for a session.
     *
     * Returns all turns as a flat array. The frontend reconstructs the tree
     * structure using `parent_turn_id` references.
     */
    tree: (sessionId: string) =>
      fetchApi<import('./types').TreeResponse>(`/sessions/${sessionId}/tree`),
  },
};
