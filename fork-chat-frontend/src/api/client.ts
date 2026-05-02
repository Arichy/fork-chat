const API_BASE = 'http://localhost:3000/api';

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

export const api = {
  config: {
    get: () => fetchApi<import('./types').ConfigResponse>('/config'),
  },

  sessions: {
    list: async (params?: {
      limit?: number;
      cursor?: import('./types').SessionsPageCursor | null;
      sort?: import('./types').SessionsSort;
      filter?: string;
    }) => {
      const search = new URLSearchParams();
      if (params?.limit != null) {
        search.set('limit', String(params.limit));
      }
      if (params?.cursor) {
        search.set('before_at', params.cursor.before_at);
        search.set('before_id', params.cursor.before_id);
      }
      if (params?.sort) {
        search.set('sort', params.sort);
      }
      if (params?.filter != null && params.filter.trim().length > 0) {
        search.set('filter', params.filter.trim());
      }
      const query = search.size > 0 ? `?${search.toString()}` : '';
      return fetchApi<import('./types').SessionsPageResponse>(
        `/sessions${query}`,
      );
    },

    get: (id: string) =>
      fetchApi<{ session: import('./types').Session }>(`/sessions/${id}`),

    create: (data: import('./types').CreateSessionRequest) =>
      fetchApi<import('./types').CreateSessionResponse>('/sessions', {
        method: 'POST',
        body: JSON.stringify(data),
      }),

    delete: (id: string) =>
      fetchApi<{ deleted: boolean }>(`/sessions/${id}`, { method: 'DELETE' }),

    updateTitle: (id: string, title: string) =>
      fetchApi<{ session: import('./types').Session }>(`/sessions/${id}`, {
        method: 'PATCH',
        body: JSON.stringify({ title }),
      }),
  },

  turns: {
    create: (sessionId: string, data: import('./types').CreateTurnRequest) =>
      fetchApi<import('./types').CreateTurnResponse>(
        `/sessions/${sessionId}/turns`,
        {
          method: 'POST',
          body: JSON.stringify(data),
        },
      ),

    get: (sessionId: string, turnId: string) =>
      fetchApi<{ turn: import('./types').Turn }>(
        `/sessions/${sessionId}/turns/${turnId}`,
      ),

    streamUrl: (sessionId: string, turnId: string) =>
      `${API_BASE}/sessions/${sessionId}/turns/${turnId}/stream`,

    retry: (
      sessionId: string,
      turnId: string,
      data: { provider: string; model: string },
    ) =>
      fetchApi<import('./types').CreateTurnResponse>(
        `/sessions/${sessionId}/turns/${turnId}/retry`,
        { method: 'POST', body: JSON.stringify(data) },
      ),

    approve: (
      sessionId: string,
      turnId: string,
      data: import('./types').ApproveTurnRequest,
    ) =>
      fetchApi<{ turn: import('./types').Turn }>(
        `/sessions/${sessionId}/turns/${turnId}/approve`,
        { method: 'POST', body: JSON.stringify(data) },
      ),

    cancel: (sessionId: string, turnId: string) =>
      fetchApi<{ turn: import('./types').Turn }>(
        `/sessions/${sessionId}/turns/${turnId}/cancel`,
        { method: 'POST' },
      ),

    tree: (sessionId: string) =>
      fetchApi<import('./types').TreeResponse>(`/sessions/${sessionId}/tree`),
  },
};
