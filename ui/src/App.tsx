import { AuthProvider, FransHubProvider, useMe } from '@franshub/ui-shell';
import { Badge, createTheme } from '@mantine/core';
import { Notifications } from '@mantine/notifications';
import { QueryClientProvider } from '@tanstack/react-query';
import { lazy, Suspense, useState, type ReactNode } from 'react';
import { BrowserRouter, Route, Routes } from 'react-router';
import { setLoginRedirect } from './api/client';
import { makeQueryClient } from './api/queryClient';
import { navigateForAuth, SIGN_IN_PATH } from './lib/authToken';
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
const SignInPage = lazy(() => import('./pages/SignIn'));
const CasesPage = lazy(() => import('./pages/Cases'));
const CasePage = lazy(() => import('./pages/Case'));
const CaseImportPage = lazy(() => import('./pages/CaseImport'));
const PlansPage = lazy(() => import('./pages/Plans'));
const PlanPage = lazy(() => import('./pages/Plan'));
const RunnerPage = lazy(() => import('./pages/Runner'));
const CaseFieldsPage = lazy(() => import('./pages/CaseFields'));
const MyRunsPage = lazy(() => import('./pages/MyRuns'));
const ManualPrintPage = lazy(() => import('./pages/ManualPrint'));

setLoginRedirect(navigateForAuth);

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
        <Route path="p/:key/settings/case-fields" element={<CaseFieldsPage />} />
        <Route path="p/:key/my-runs" element={<MyRunsPage />} />
        <Route path="p/:key/launches/:id/manual/print" element={<ManualPrintPage />} />
        <Route path="p/:key/cases" element={<CasesPage />} />
        <Route path="p/:key/cases/import" element={<CaseImportPage />} />
        <Route path="p/:key/cases/:n" element={<CasePage />} />
        <Route path="p/:key/plans" element={<PlansPage />} />
        <Route path="p/:key/plans/:id" element={<PlanPage />} />
        <Route path="p/:key/launches/:id/run" element={<RunnerPage />} />
        <Route path="p/:key/launches/:id/run/:itemId" element={<RunnerPage />} />
        <Route path="admin/jobs" element={<JobsPage />} />
        <Route path="settings/tokens" element={<TokensPage />} />
        <Route path="settings/audit" element={<AuditPage />} />
        <Route path="*" element={<NotFoundPage />} />
      </Routes>
    </Suspense>
  );
}

/** Renders the app once the principal is known, so no data request goes out signed out. */
function SignedIn({ children }: { children: ReactNode }) {
  const { me, error } = useMe();
  if (!me && !error) return <Loading />;
  return children;
}

export function App() {
  const [client] = useState(makeQueryClient);
  return (
    <QueryClientProvider client={client}>
      <FransHubProvider theme={theme} getStyleNonce={styleNonce}>
        <Notifications position="top-right" />
        {window.location.pathname === SIGN_IN_PATH ? (
          <Suspense fallback={<Loading />}>
            <SignInPage />
          </Suspense>
        ) : (
          <AuthProvider redirect={navigateForAuth}>
            <SignedIn>
              <BrowserRouter>
                <Shell>
                  <AppRoutes />
                </Shell>
              </BrowserRouter>
            </SignedIn>
          </AuthProvider>
        )}
      </FransHubProvider>
    </QueryClientProvider>
  );
}
