import { defineConfig, devices } from "@playwright/test";

export default defineConfig({
  testDir: "./e2e",
  outputDir: "../output/playwright/test-results",
  fullyParallel: true,
  timeout: 120_000,
  retries: process.env.CI ? 2 : 0,
  reporter: process.env.CI
    ? [["line"], ["html", { outputFolder: "../output/playwright/report", open: "never" }]]
    : "line",
  use: {
    baseURL: "http://127.0.0.1:1420",
    trace: "retain-on-failure",
    screenshot: "only-on-failure",
    // Never through the computer's speakers: the Remote Play specs play real sound.
    launchOptions: { args: ["--mute-audio"] },
  },
  projects: [
    {
      name: "chromium-desktop",
      use: { ...devices["Desktop Chrome"] },
    },
    // The production build under the apps' Content-Security-Policy (e2e/fixtures/serve-dist-csp.mjs):
    // what only breaks in the packaged apps. Needs a build (npm run build:vite) first.
    {
      name: "app-csp",
      testMatch: /remote-play\.spec\.ts/,
      grep: /microphone sends|game.s sound plays/,
      use: { ...devices["Desktop Chrome"], baseURL: "http://127.0.0.1:1421" },
    },
  ],
  webServer: [
    {
      command: "npm run dev:vite -- --host 127.0.0.1",
      url: "http://127.0.0.1:1420",
      reuseExistingServer: !process.env.CI,
      timeout: 120_000,
    },
    {
      command: "node e2e/fixtures/serve-dist-csp.mjs",
      url: "http://127.0.0.1:1421",
      reuseExistingServer: !process.env.CI,
      timeout: 30_000,
    },
  ],
});
