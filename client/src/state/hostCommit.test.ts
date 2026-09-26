import { beforeEach, describe, expect, it } from "vitest";
import { useRosterStore, ensureRosterMigrated } from "./roster";
import { useConnectionStore } from "./connection";

/**
 * #276 follow-up — changing the address on the Connection screen must
 * re-point the ACTIVE ROSTER PROFILE, not just `connection.host`.
 *
 * The roster profile is the source of truth for which console is selected.
 * `ensureRosterMigrated()` runs on every AppShell mount and reconciles
 * `connection.host` back to the active profile's host. Committing a new
 * address by calling `setHost()` alone therefore survives only until the
 * next mount — and since changing the host remounts the whole route tree,
 * that is immediate. The user sees the field snap back to the old IP.
 */
describe("committing a new address from the Connection screen", () => {
  beforeEach(() => {
    useRosterStore.setState({ profiles: [], active_id: null });
  });

  it("setHost alone is reverted by the roster reconcile (the #276 bug)", () => {
    const id = useRosterStore.getState().add({
      name: "PS5",
      host: "192.168.1.10",
    });
    useRosterStore.getState().setActive(id);
    expect(useConnectionStore.getState().host).toBe("192.168.1.10");

    // What the Connection screen used to do on Check.
    useConnectionStore.getState().setHost("192.168.1.99");
    expect(useConnectionStore.getState().host).toBe("192.168.1.99");

    // The remount that the host change itself triggers.
    ensureRosterMigrated();

    // The roster still points at the OLD address, so it wins.
    expect(useConnectionStore.getState().host).toBe("192.168.1.10");
  });

  it("updateHost on the active profile survives the reconcile", () => {
    const id = useRosterStore.getState().add({
      name: "PS5",
      host: "192.168.1.10",
    });
    useRosterStore.getState().setActive(id);

    useRosterStore.getState().updateHost(id, "192.168.1.99");
    expect(useConnectionStore.getState().host).toBe("192.168.1.99");

    ensureRosterMigrated();

    expect(useConnectionStore.getState().host).toBe("192.168.1.99");
    expect(
      useRosterStore.getState().profiles.find((p) => p.id === id)?.host,
    ).toBe("192.168.1.99");
  });

  it("clears the old console's cached firmware when re-pointed at a new IP", () => {
    const id = useRosterStore.getState().add({
      name: "PS5 (192.168.1.10)",
      host: "192.168.1.10",
    });
    useRosterStore.getState().setActive(id);
    useRosterStore.getState().noteSeen(id, {
      kernel: "r188096/releases/05.10",
      payload: "5.4.9",
    });

    useRosterStore.getState().updateHost(id, "192.168.1.99");

    const p = useRosterStore.getState().profiles.find((x) => x.id === id)!;
    // The cached values described the console that WAS at .10. Rendering
    // them next to .99 claimed the new console's firmware without a probe.
    expect(p.last_seen_kernel).toBeNull();
    expect(p.last_seen_payload).toBeNull();
    expect(p.last_seen_at).toBeNull();
    // The auto-generated name must not keep advertising the old address.
    expect(p.name).toBe("PS5 (192.168.1.99)");
  });

  it("leaves a user-chosen name and same-console history alone", () => {
    const id = useRosterStore.getState().add({
      name: "Living-room PS5",
      host: "192.168.1.10",
    });
    useRosterStore.getState().setActive(id);
    useRosterStore.getState().noteSeen(id, { kernel: "k", payload: "p" });

    // Same bare host, port added — not a console change.
    useRosterStore.getState().updateHost(id, "192.168.1.10:9113");
    let p = useRosterStore.getState().profiles.find((x) => x.id === id)!;
    expect(p.last_seen_kernel).toBe("k");

    // Real address change, but the name is the user's — keep it.
    useRosterStore.getState().updateHost(id, "192.168.1.99");
    p = useRosterStore.getState().profiles.find((x) => x.id === id)!;
    expect(p.name).toBe("Living-room PS5");
    expect(p.last_seen_kernel).toBeNull();
  });
});
