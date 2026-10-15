// Playwright configuration for the web UI's end-to-end tests (README.md).
// Each test gets its own mock daemon (fixtures.js), so tests share no state and run in
// parallel. The browser is Playwright's Chromium, or the one JSTREAM_E2E_CHROMIUM names.
import { defineConfig, devices } from '@playwright/test';

const executablePath = process.env.JSTREAM_E2E_CHROMIUM || undefined;

export default defineConfig({
  testDir: './specs',
  globalSetup: './global-setup.js',
  forbidOnly: !!process.env.CI,
  retries: 0,
  reporter: process.env.CI ? [['list'], ['github']] : 'list',
  use: {
    trace: 'retain-on-failure',
  },
  projects: [
    {
      name: 'chromium',
      use: { ...devices['Desktop Chrome'], launchOptions: { executablePath } },
    },
  ],
});
