// The website's screenshots (site-shots/site-shots.spec.ts): not part of the test suite.
import { defineConfig, devices } from "@playwright/test";

// SHOTS_PORT moves the dev server off 1420, so a run never borrows another checkout's server.
const port = Number(process.env.SHOTS_PORT) || 1420;
const url = `http://127.0.0.1:${port}`;

export default defineConfig({
  testDir: "./site-shots",
  outputDir: "../output/playwright/site-shots",
  timeout: 180_000,
  workers: 2,
  reporter: "line",
  // Never a sound from a screenshot run.
  use: { ...devices["Desktop Chrome"], baseURL: url, launchOptions: { args: ["--mute-audio"] } },
  webServer: {
    command: "npm run dev:vite -- --host 127.0.0.1",
    env: { PS5UPLOAD_VITE_PORT: String(port) },
    url,
    reuseExistingServer: true,
    timeout: 120_000,
  },
});
