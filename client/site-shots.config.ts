// The website's screenshots (site-shots/site-shots.spec.ts): not part of the test suite.
import { defineConfig, devices } from "@playwright/test";

export default defineConfig({
  testDir: "./site-shots",
  outputDir: "../output/playwright/site-shots",
  timeout: 180_000,
  workers: 2,
  reporter: "line",
  // Never a sound from a screenshot run.
  use: { ...devices["Desktop Chrome"], baseURL: "http://127.0.0.1:1420", launchOptions: { args: ["--mute-audio"] } },
  webServer: {
    command: "npm run dev:vite -- --host 127.0.0.1",
    url: "http://127.0.0.1:1420",
    reuseExistingServer: true,
    timeout: 120_000,
  },
});
