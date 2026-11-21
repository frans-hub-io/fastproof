import '@mantine/core/styles.css';
import '@mantine/charts/styles.css';
import '@mantine/notifications/styles.css';
import '@franshub/ui-shell/styles.css';
import './styles/app.css';
import { applyInitialColorScheme } from '@franshub/ui-shell';
import { StrictMode } from 'react';
import { createRoot } from 'react-dom/client';
import { App } from './App';

async function boot(): Promise<void> {
  applyInitialColorScheme();
  if (import.meta.env.DEV && import.meta.env.VITE_API === 'mock') {
    const { startMocks } = await import('./mocks/browser');
    await startMocks();
  }
  const root = document.getElementById('root');
  if (!root) return;
  createRoot(root).render(
    <StrictMode>
      <App />
    </StrictMode>,
  );
}

void boot();
