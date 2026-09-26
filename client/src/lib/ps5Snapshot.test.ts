import { describe, expect, it, beforeEach } from "vitest";
import { useConnectionStore } from "../state/connection";
import { buildPs5Snapshot } from "./ps5Snapshot";

/**
 * When no PS5 is reachable the snapshot must short-circuit to a host-only
 * record WITHOUT firing any PS5 RPC (those would reject against an empty
 * address and could surface as spurious errors). This is the common case —
 * a user filing a bug while disconnected — so it has to be clean.
 */
describe("buildPs5Snapshot (disconnected)", () => {
  beforeEach(() => {
    useConnectionStore.setState({ host: "", payloadStatus: "down" });
  });

  it("returns connected:false and no PS5 data, never throwing", async () => {
    const { snapshot, klog, syslog } = await buildPs5Snapshot({ redact: true });
    expect(snapshot.connected).toBe(false);
    expect(snapshot.redacted).toBe(true);
    expect(snapshot.hw_info).toBeNull();
    expect(snapshot.running_apps).toBeNull();
    expect(snapshot.processes).toBeNull();
    expect(klog).toBeNull();
    expect(syslog).toBeNull();
    expect(Object.keys(snapshot.errors)).toHaveLength(0);
  });

  it("treats payload 'up' with an empty host as disconnected", async () => {
    useConnectionStore.setState({ host: "", payloadStatus: "up" });
    const { snapshot } = await buildPs5Snapshot({ redact: false });
    expect(snapshot.connected).toBe(false);
  });
});
