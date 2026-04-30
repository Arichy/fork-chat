import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { render, type RenderOptions } from '@testing-library/react';
import type { ReactElement, ReactNode } from 'react';

export function createTestQueryClient(): QueryClient {
  return new QueryClient({
    defaultOptions: {
      queries: { retry: false, gcTime: 0, staleTime: 0 },
      mutations: { retry: false },
    },
  });
}

interface ProviderWrapperProps {
  children: ReactNode;
  client?: QueryClient;
}

function ProviderWrapper({ children, client }: ProviderWrapperProps) {
  const qc = client ?? createTestQueryClient();
  return <QueryClientProvider client={qc}>{children}</QueryClientProvider>;
}

export function renderWithProviders(
  ui: ReactElement,
  {
    client,
    ...options
  }: RenderOptions & { client?: QueryClient } = {},
) {
  const qc = client ?? createTestQueryClient();
  return {
    queryClient: qc,
    ...render(ui, {
      wrapper: ({ children }) => (
        <ProviderWrapper client={qc}>{children}</ProviderWrapper>
      ),
      ...options,
    }),
  };
}
