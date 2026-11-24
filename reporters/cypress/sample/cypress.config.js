const { defineConfig } = require('cypress');
const { fastproof } = require('../dist/index.js');

let attempts = 0;

module.exports = defineConfig({
  video: true,
  screenshotOnRunFailure: true,
  retries: { runMode: 2, openMode: 0 },
  e2e: {
    supportFile: 'cypress/support/e2e.js',
    specPattern: 'cypress/e2e/**/*.cy.js',
    setupNodeEvents(on, config) {
      on('task', {
        attempt() {
          attempts += 1;
          return attempts;
        },
      });
      return fastproof(on, config, { launchName: 'cypress sample', flushTimeoutMs: 30000 });
    },
  },
});
