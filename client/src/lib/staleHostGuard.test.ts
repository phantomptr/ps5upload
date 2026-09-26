import { beforeEach, describe, expect, it } from "vitest";

import { useConnectionStore } from "../state/connection";
import { captureHostProbe } from "./staleHostGuard";

// We exercise the non-hook `captureHostProbe` because it's the
// pure contract; the hook is a thin useCallback wrapper around
// the same code and adding @testing-library/react just for that
// would be overkill.

describe("captureHostProbe", () => {
  beforeEach(() => {
    useConnectionStore.setState({ host: "192.168.1.50" });
  });

  it("captures the host as it was at probe-creation time", () => {
    const probe = captureHostProbe();
    expect(probe.host).toBe("192.168.1.50");
  });

  it("isStale() returns false when host hasn't changed", () => {
    const probe = captureHostProbe();
    expect(probe.isStale()).toBe(false);
  });

  it("isStale() returns true once host changes after capture", () => {
    const probe = captureHostProbe();
    useConnectionStore.setState({ host: "10.0.0.1" });
    expect(probe.isStale()).toBe(true);
  });

  it("isStale() correctly handles host changing back to the original", () => {
    // Edge case: host A → host B → host A. The probe captured A;
    // we're back at A. Technically state mutations DURING B may
    // have been wrong, but for the post-await read-only check, A
    // == A is "not stale" and correct — the data we're about to
    // write IS for A.
    const probe = captureHostProbe();
    useConnectionStore.setState({ host: "10.0.0.1" });
    expect(probe.isStale()).toBe(true);
    useConnectionStore.setState({ host: "192.168.1.50" });
    expect(probe.isStale()).toBe(false);
  });

  it("independent probes don't interfere", () => {
    const a = captureHostProbe();
    useConnectionStore.setState({ host: "10.0.0.1" });
    const b = captureHostProbe();
    // a was captured on .50, host is now .1 → stale.
    expect(a.isStale()).toBe(true);
    // b was captured on .1, host is still .1 → not stale.
    expect(b.isStale()).toBe(false);
  });
});
