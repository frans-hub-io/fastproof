import { readFileSync } from 'node:fs';
import { createRequire } from 'node:module';
import { defineConfig } from 'vitest/config';
import type { Plugin } from 'vite';
import react from '@vitejs/plugin-react';

const require = createRequire(import.meta.url);
const backend = process.env.FASTPROOF_BACKEND_URL ?? 'http://127.0.0.1:8080';
const apiMode = process.env.VITE_API ?? 'mock';

function mockServiceWorker(): Plugin {
  return {
    name: 'fastproof-mock-service-worker',
    apply: 'serve',
    configureServer(server) {
      server.middlewares.use('/mockServiceWorker.js', (_req, res) => {
        res.setHeader('Content-Type', 'text/javascript');
        res.setHeader('Cache-Control', 'no-store');
        res.end(readFileSync(require.resolve('msw/mockServiceWorker.js')));
      });
    },
  };
}

const proxy =
  apiMode === 'proxy'
    ? {
        '/api': { target: backend, changeOrigin: false },
        '/auth': { target: backend, changeOrigin: false },
        '/trace': { target: backend, changeOrigin: false },
      }
    : undefined;

export default defineConfig({
  base: '/',
  plugins: [react(), mockServiceWorker()],
  define: {
    'import.meta.env.VITE_API': JSON.stringify(apiMode),
  },
  resolve: {
    dedupe: ['react', 'react-dom', '@mantine/core', '@mantine/hooks', '@tanstack/react-query'],
  },
  build: {
    outDir: 'dist',
    emptyOutDir: true,
    sourcemap: false,
    assetsInlineLimit: 0,
    chunkSizeWarningLimit: 1200,
    rolldownOptions: {
      output: {
        manualChunks(id: string) {
          if (id.endsWith('.css')) return undefined;
          if (id.includes('node_modules/@mantine/charts')) return undefined;
          if (id.includes('node_modules/@mantine') || id.includes('node_modules/@floating-ui')) return 'mantine';
          if (id.includes('node_modules/react') || id.includes('node_modules/scheduler')) return 'react';
          return undefined;
        },
      },
    },
  },
  server: {
    host: '127.0.0.1',
    port: 5180,
    strictPort: true,
    proxy,
  },
  test: {
    environment: 'jsdom',
    globals: false,
    setupFiles: ['./src/test/setup.ts'],
    include: ['src/**/*.test.{ts,tsx}'],
    css: false,
    testTimeout: 20000,
  },
});
