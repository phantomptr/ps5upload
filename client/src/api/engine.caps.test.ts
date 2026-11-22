import { afterEach, expect, it, vi } from "vitest";

vi.mock("../state/engine", () => ({ getEngineUrl: () => "http://127.0.0.1:19113" }));

import { engineCaps } from "./engine";

afterEach(() => {
  vi.unstubAllGlobals();
  vi.restoreAllMocks();
});

// The Playlists panel asks for the caps in an effect: a throw there takes the screen down. A
// WebKit before Safari 16 (macOS 11 and 12 as shipped, iOS 15) has no AbortSignal.timeout.
it("answers no caps, never throws, where AbortSignal.timeout is missing", async () => {
  const timeout = vi.spyOn(AbortSignal, "timeout").mockImplementation(() => {
    throw new TypeError("AbortSignal.timeout is not a function");
  });
  const fetchMock = vi.fn();
  vi.stubGlobal("fetch", fetchMock);
  let caps: ReturnType<typeof engineCaps> | undefined;
  expect(() => {
    caps = engineCaps();
  }).not.toThrow();
  await expect(caps).resolves.toEqual({ payloadConsolePath: false });
  expect(timeout).toHaveBeenCalled();
  expect(fetchMock).not.toHaveBeenCalled();
});

it("reads payload_console_path from the engine's version answer", async () => {
  vi.stubGlobal(
    "fetch",
    vi.fn(async () => new Response(JSON.stringify({ caps: { payload_console_path: true } }), { status: 200 })),
  );
  await expect(engineCaps()).resolves.toEqual({ payloadConsolePath: true });
});
