import { setupWorker } from 'msw/browser';
import { handlers, MOCK_ROLE_KEY } from './handlers';

export async function startMocks(): Promise<void> {
  const role = new URLSearchParams(window.location.search).get('mockRole');
  try {
    if (role === 'viewer' || role === 'admin') window.localStorage.setItem(MOCK_ROLE_KEY, role);
  } catch {
    /* storage unavailable */
  }
  const worker = setupWorker(...handlers);
  await worker.start({
    onUnhandledFrame: 'bypass',
    quiet: true,
    serviceWorker: { url: '/mockServiceWorker.js' },
  });
}
