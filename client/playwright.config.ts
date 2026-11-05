import { defineConfig, devices } from "@playwright/test";

// The suite starts its own servers on its own ports, so a dev server on 1420 is neither in the way
// nor tested by accident: vite (its live reload takes the next port) and, one up from that, the
// production build. PS5UPLOAD_VITE_PORT moves them when two checkouts run the suite at once.
const vitePort = Number(process.env.PS5UPLOAD_VITE_PORT) || 14470;
const cspPort = vitePort + 2;

export default defineConfig({
  testDir: "./e2e",
  outputDir: "../output/playwright/test-results",
  fullyParallel: true,
  // A committed test.only would run one test and report green.
  forbidOnly: !!process.env.CI,
  timeout: 120_000,
  // One retry tells a flake from a failure, and a flake fails the run too: passing on a retry hid
  // these for weeks. Failed on a real one? gh run rerun <id> --failed, and fix the test.
  retries: process.env.CI ? 1 : 0,
  failOnFlakyTests: !!process.env.CI,
  reporter: process.env.CI
    ? [["line"], ["html", { outputFolder: "../output/playwright/report", open: "never" }]]
    : "line",
  use: {
    baseURL: `http://127.0.0.1:${vitePort}`,
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
      use: { ...devices["Desktop Chrome"], baseURL: `http://127.0.0.1:${cspPort}` },
    },
  ],
  webServer: [
    {
      command: "npm run dev:vite -- --host 127.0.0.1",
      env: { PS5UPLOAD_VITE_PORT: String(vitePort) },
      url: `http://127.0.0.1:${vitePort}`,
      reuseExistingServer: false,
      timeout: 120_000,
    },
    {
      command: "node e2e/fixtures/serve-dist-csp.mjs",
      env: { PORT: String(cspPort) },
      url: `http://127.0.0.1:${cspPort}`,
      reuseExistingServer: false,
      timeout: 30_000,
    },
  ],
});
