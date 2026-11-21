import { AuthProvider, FransHubProvider } from '@franshub/ui-shell';
import { Badge, createTheme } from '@mantine/core';
import { Notifications } from '@mantine/notifications';
import { QueryClientProvider } from '@tanstack/react-query';
import { lazy, Suspense, useState } from 'react';
import { BrowserRouter, Route, Routes } from 'react-router';
import { makeQueryClient } from './api/queryClient';
import { Loading } from './components/QueryState';
import { cspNonce } from './lib/cspNonce';
import { Shell } from './Shell';
import ProjectsPage from './pages/Projects';

const LaunchesPage = lazy(() => import('./pages/Launches'));
const LaunchPage = lazy(() => import('./pages/Launch'));
const ResultPage = lazy(() => import('./pages/Result'));
const TestCasePage = lazy(() => import('./pages/TestCase'));
const TokensPage = lazy(() => import('./pages/Tokens'));
const AuditPage = lazy(() => import('./pages/Audit'));
const NotFoundPage = lazy(() => import('./pages/NotFound'));
const DashboardPage = lazy(() => import('./pages/Dashboard'));
const FlakyPage = lazy(() => import('./pages/Flaky'));
const ComparePage = lazy(() => import('./pages/Compare'));
const RulesPage = lazy(() => import('./pages/Rules'));
const GatesPage = lazy(() => import('./pages/Gates'));
const NotificationsPage = lazy(() => import('./pages/Notifications'));
const SettingsPage = lazy(() => import('./pages/Settings'));
const JobsPage = lazy(() => import('./pages/Jobs'));

const nonce = cspNonce();
const styleNonce = nonce ? () => nonce : undefined;

const theme = createTheme({
  components: {
    Badge: Badge.extend({ styles: { label: { textTransform: 'none', letterSpacing: 0 } } }),
  },
});

export function AppRoutes() {
  return (
    <Suspense fallback={<Loading />}>
      <Routes>
        <Route index element={<ProjectsPage />} />
        <Route path="p/:key" element={<LaunchesPage />} />
        <Route path="p/:key/launches/:id" element={<LaunchPage />} />
        <Route path="p/:key/results/:id" element={<ResultPage />} />
        <Route path="p/:key/test-cases/:id" element={<TestCasePage />} />
        <Route path="p/:key/dashboard" element={<DashboardPage />} />
        <Route path="p/:key/flaky" element={<FlakyPage />} />
        <Route path="p/:key/compare" element={<ComparePage />} />
        <Route path="p/:key/rules" element={<RulesPage />} />
        <Route path="p/:key/gates" element={<GatesPage />} />
        <Route path="p/:key/notifications" element={<NotificationsPage />} />
        <Route path="p/:key/settings" element={<SettingsPage />} />
        <Route path="admin/jobs" element={<JobsPage />} />
        <Route path="settings/tokens" element={<TokensPage />} />
        <Route path="settings/audit" element={<AuditPage />} />
        <Route path="*" element={<NotFoundPage />} />
      </Routes>
    </Suspense>
  );
}

export function App() {
  const [client] = useState(makeQueryClient);
  return (
    <QueryClientProvider client={client}>
      <FransHubProvider theme={theme} getStyleNonce={styleNonce}>
        <Notifications position="top-right" />
        <AuthProvider>
          <BrowserRouter>
            <Shell>
              <AppRoutes />
            </Shell>
          </BrowserRouter>
        </AuthProvider>
      </FransHubProvider>
    </QueryClientProvider>
  );
}
