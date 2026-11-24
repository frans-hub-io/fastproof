import { defineConfig } from 'vitest/config';

export default defineConfig({
  test: {
    include: ['tests/**/*.test.ts'],
    setupFiles: ['../dist/setup.js'],
    reporters: [
      'default',
      [
        '../dist/index.js',
        {
          launchName: 'e2e',
          flushTimeoutMs: 20000,
          secrets: { patterns: ['card-\\d{4}'] },
          attributes: { suite: 'e2e' },
        },
      ],
    ],
  },
});
