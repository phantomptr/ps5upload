// Global test setup.
//
// Tests must never talk to a real engine. `invokeLogged` falls back to
// `browserInvoke` (an HTTP fetch) whenever it is not running inside
// Tauri -- which is always true under vitest. So any API wrapper a test
// forgets to mock silently issues a real request to whatever engine
// happens to be listening on the dev port.
//
// That is not hypothetical: `uploadQueue.test.ts` mocked the transfer
// starters but not `jobCancel`, so every run fired real
// `POST /api/jobs/job/cancel` requests at the developer's running
// engine, which answered 400 because "job" is not a UUID. The tests
// still passed -- the calls are fire-and-forget -- so nothing surfaced
// it except noise in the engine log.
//
// Failing loudly turns that class of leak into a test failure naming
// the exact URL, instead of an invisible dependency on whether an
// engine is running.
import { beforeAll } from "vitest";

beforeAll(() => {
  globalThis.fetch = (async (input: unknown) => {
    const url =
      typeof input === "string"
        ? input
        : input instanceof URL
          ? input.toString()
          : ((input as Request)?.url ?? String(input));
    throw new Error(
      `Test made a real network request to ${url}. ` +
        `Mock the API wrapper it came from -- tests must not depend on a ` +
        `running engine. See client/src/test-setup.ts.`,
    );
  }) as typeof fetch;
});
