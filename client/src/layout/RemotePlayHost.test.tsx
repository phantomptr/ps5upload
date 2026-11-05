import { renderToStaticMarkup } from "react-dom/server";
import { MemoryRouter } from "react-router";
import { afterEach, describe, expect, it, vi } from "vitest";

import type { LiveRun } from "../lib/goLive";
import { justFailed } from "../screens/Play/goLiveText";
import { RemotePlayHost } from "./RemotePlayHost";

// Server rendering reads a zustand store's first state, so a plain stand-in that can change.
const h = vi.hoisted(() => ({
  state: { stream: null, target: null, pip: null, passcode: null, error: null } as Record<string, unknown>,
}));
vi.mock("../state/remotePlay", () => ({
  useRemotePlayStore: (sel: (s: unknown) => unknown) => sel(h.state),
  pauseRpInput: () => {},
  rpInput: { held: new Set() },
}));
vi.mock("../lib/pipPopups", () => ({ closeLeftoverPipPopups: async () => {} }));
// The real box puts itself on the document, which this runner doesn't have.
vi.mock("../components/PasscodeDialog", () => ({
  PasscodeDialog: ({ testId }: { testId: string }) => <div data-testid={testId} />,
}));

const at = (path: string) =>
  renderToStaticMarkup(
    <MemoryRouter initialEntries={[path]}>
      <RemotePlayHost />
    </MemoryRouter>,
  );

afterEach(() => {
  h.state = { stream: null, target: null, pip: null, passcode: null, error: null };
});

describe("the passcode a start waits for", () => {
  const asking = () => {
    h.state = {
      ...h.state,
      stream: {},
      target: { host: "10.0.0.2", name: "PS5", registKey: "", rpKey: "" },
      passcode: { retry: false, waitS: 180, seq: 1 },
    };
  };

  it("is asked over the app on any other screen", () => {
    asking();
    expect(at("/library")).toContain('data-testid="rp-passcode"');
  });

  it("is left to the stage on Play", () => {
    asking();
    expect(at("/play")).not.toContain('data-testid="rp-passcode"');
  });

  it("is nothing without an ask", () => {
    expect(at("/library")).not.toContain('data-testid="rp-passcode"');
  });
});

describe("a broadcast that stopped", () => {
  const run = (phase: string) => ({ phase, error: { code: "rejected", message: "x" }, destinations: [] }) as unknown as LiveRun;

  it("is told when it was live a moment ago", () => {
    expect(justFailed(true, run("failed"))?.code).toBe("rejected");
  });

  it("is not told twice, or when it failed before this page saw it", () => {
    expect(justFailed(false, run("failed"))).toBeNull();
    expect(justFailed(true, run("live"))).toBeNull();
    expect(justFailed(true, null)).toBeNull();
  });
});
