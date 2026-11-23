import { AuthProvider, FransHubProvider } from '@franshub/ui-shell';
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { render, type RenderResult } from '@testing-library/react';
import type { ReactElement, ReactNode } from 'react';
import { MemoryRouter } from 'react-router';
import { beforeAll } from 'vitest';
import { AppRoutes } from '../App';
import { Shell } from '../Shell';

// The routes load their pages lazily. Under machine load, transforming a page module on its first render can take
// longer than a `findBy` waits, so a test would time out on the import rather than on its data. Every page module
// is loaded before the tests of a file start (React.lazy then resolves from the module cache), so the waits in tests
// depend on rendered data only.
const pages = import.meta.glob(['../pages/*.tsx', '!../pages/*.test.tsx']);
beforeAll(async () => {
  await Promise.all(Object.values(pages).map((load) => load()));
}, 120_000);

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
