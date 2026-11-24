module.exports = {
  rootDir: __dirname,
  testMatch: ['<rootDir>/tests/**/*.test.js'],
  setupFilesAfterEnv: ['<rootDir>/../dist/setup.js'],
  reporters: [
    'default',
    [
      '<rootDir>/../dist/index.js',
      {
        launchName: 'e2e',
        flushTimeoutMs: 20000,
        secrets: { patterns: ['card-\\d{4}'] },
        attributes: { suite: 'e2e' },
      },
    ],
  ],
};
