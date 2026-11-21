import { AuthProvider, FransHubProvider } from '@franshub/ui-shell';
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { render, type RenderResult } from '@testing-library/react';
import type { ReactElement, ReactNode } from 'react';
import { MemoryRouter } from 'react-router';
import { AppRoutes } from '../App';
import { Shell } from '../Shell';

export function testClient(): QueryClient {
  return new QueryClient({ defaultOptions: { queries: { retry: false, staleTime: Infinity }, mutations: { retry: false } } });
}

export function Providers({ children, route = '/', client = testClient() }: { children: ReactNode; route?: string; client?: QueryClient }) {
  return (
    <QueryClientProvider client={client}>
      <FransHubProvider env="test">
        <AuthProvider redirect={() => undefined}>
          <MemoryRouter initialEntries={[route]}>{children}</MemoryRouter>
        </AuthProvider>
      </FransHubProvider>
    </QueryClientProvider>
  );
}

export function renderWith(ui: ReactElement, route = '/'): RenderResult {
  return render(<Providers route={route}>{ui}</Providers>);
}

export function renderApp(route: string): RenderResult {
  return render(
    <Providers route={route}>
      <Shell>
        <AppRoutes />
      </Shell>
    </Providers>,
  );
}
