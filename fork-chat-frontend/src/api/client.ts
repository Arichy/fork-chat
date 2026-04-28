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
    list: () => fetchApi<import('./types').Session[]>('/sessions'),

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

    retry: (
      sessionId: string,
      turnId: string,
      data: { provider: string; model: string },
    ) =>
      fetchApi<import('./types').CreateTurnResponse>(
        `/sessions/${sessionId}/turns/${turnId}/retry`,
        { method: 'POST', body: JSON.stringify(data) },
      ),

    tree: (sessionId: string) =>
      fetchApi<import('./types').TreeResponse>(`/sessions/${sessionId}/tree`),
  },
};
