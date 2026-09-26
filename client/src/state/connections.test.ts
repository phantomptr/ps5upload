import { beforeEach, describe, expect, it, vi } from "vitest";

const list = vi.fn();
const add = vi.fn();
const update = vi.fn();
const remove = vi.fn();
const test = vi.fn();
vi.mock("../api/remote", () => ({
  remoteApi: {
    list: (...a: unknown[]) => list(...a),
    add: (...a: unknown[]) => add(...a),
    update: (...a: unknown[]) => update(...a),
    remove: (...a: unknown[]) => remove(...a),
    test: (...a: unknown[]) => test(...a),
  },
}));

import type { Connection } from "../api/remote";
import { useConnectionsStore } from "./connections";

const nas: Connection = {
  id: "nas-1",
  name: "NAS",
  protocol: "smb",
  host: "10.0.0.5",
  port: 445,
  share: "games",
  user: "me",
  start_path: "",
  host_key: null,
  has_secret: true,
};
const input = { name: "NAS", protocol: "smb" as const, host: "10.0.0.5", port: 445, share: "games" };

describe("connections store", () => {
  beforeEach(() => {
    useConnectionsStore.setState({ connections: [], loaded: false, status: {} });
    for (const f of [list, add, update, remove, test]) f.mockReset();
  });

  it("loads, names, and maps check results to a status", async () => {
    list.mockResolvedValue([nas]);
    await useConnectionsStore.getState().load();
    expect(useConnectionsStore.getState().loaded).toBe(true);
    expect(useConnectionsStore.getState().nameOf("nas-1")).toBe("NAS");
    test.mockResolvedValueOnce({ ok: false, error: "Sign-in failed: bad password" });
    expect(await useConnectionsStore.getState().check("nas-1")).toBe("auth-failed");
    test.mockResolvedValueOnce({ ok: false, error: "Can't reach 10.0.0.5:445: refused" });
    expect(await useConnectionsStore.getState().check("nas-1")).toBe("offline");
    test.mockResolvedValueOnce({ ok: true });
    expect(await useConnectionsStore.getState().check("nas-1")).toBe("reachable");
    expect(useConnectionsStore.getState().status["nas-1"]).toBe("reachable");
  });

  it("adds, edits and removes, keeping the list current", async () => {
    add.mockResolvedValue(nas);
    await useConnectionsStore.getState().save(null, input, { password: "pw" });
    expect(add).toHaveBeenCalledWith(input, { password: "pw" });
    update.mockResolvedValue({ ...nas, name: "Home NAS" });
    await useConnectionsStore.getState().save("nas-1", { ...input, name: "Home NAS" });
    expect(useConnectionsStore.getState().connections[0].name).toBe("Home NAS");
    remove.mockResolvedValue(undefined);
    await useConnectionsStore.getState().remove("nas-1");
    expect(useConnectionsStore.getState().connections).toEqual([]);
  });

  it("never keeps a typed password in the store", async () => {
    add.mockResolvedValue({ ...nas, id: "n2" });
    await useConnectionsStore.getState().save(null, input, { password: "hunter2" });
    expect(JSON.stringify(useConnectionsStore.getState())).not.toContain("hunter2");
  });
});
