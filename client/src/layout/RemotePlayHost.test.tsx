import { renderToStaticMarkup } from "react-dom/server";
import { MemoryRouter } from "react-router";
import { afterEach, describe, expect, it, vi } from "vitest";

import type { LiveRun } from "../lib/goLive";
import type { RemotePlayStream } from "../lib/remotePlayStream";
import { justFailed } from "../screens/Play/goLiveText";
import { useRemotePlayStore } from "../state/remotePlay";
import { RemotePlayHost } from "./RemotePlayHost";

vi.mock("../lib/pipPopups", () => ({ closeLeftoverPipPopups: async () => {} }));

const at = (path: string) =>
  renderToStaticMarkup(
    <MemoryRouter initialEntries={[path]}>
      <RemotePlayHost />
    </MemoryRouter>,
  );

afterEach(() => {
  useRemotePlayStore.setState({ stream: null, passcode: null, error: null, pip: null });
});

describe("the passcode a start waits for", () => {
  const asking = () =>
    useRemotePlayStore.setState({
      stream: {} as RemotePlayStream,
      target: { host: "10.0.0.2", name: "PS5", registKey: "", rpKey: "" },
      passcode: { retry: false, waitS: 180, seq: 1 },
    });

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
