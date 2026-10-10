import { describe, expect, it } from "vitest";
import { profileIdForHost, profileNameForTask, type PS5Profile } from "./roster";
import { useTaskStore } from "./tasks";

const A: PS5Profile = { id: "a", name: "Living room", host: "10.0.0.5" };
const B: PS5Profile = { id: "b", name: "Bedroom", host: "10.0.0.6" };

describe("U14: tasks are attributed by console identity", () => {
  it("resolves the identity from the roster by host", () => {
    expect(profileIdForHost("10.0.0.6:9113", [A, B])).toBe("b");
    expect(profileIdForHost("10.9.9.9", [A, B])).toBeUndefined();
  });

  it("shows a task under its own console, not the one that now has its address", () => {
    const task = { consoleId: "10.0.0.5", consoleKey: "a" };
    // Console A moved to a new IP and B took over 10.0.0.5.
    const moved = [{ ...A, host: "10.0.0.9" }, { ...B, host: "10.0.0.5" }];
    expect(profileNameForTask(task, moved)).toBe("Living room");
    // Without identity the address would wrongly pick B.
    expect(profileNameForTask({ consoleId: "10.0.0.5" }, moved)).toBe("Bedroom");
  });

  it("two consoles, a job on each, each under its own", () => {
    const s = useTaskStore.getState();
    const t1 = s.registerTask({ kind: "upload-file", origin: "t", label: "1", consoleId: "10.0.0.5", consoleKey: "a" });
    const t2 = s.registerTask({ kind: "upload-file", origin: "t", label: "2", consoleId: "10.0.0.5", consoleKey: "b" });
    expect(s.getTask(t1)?.consoleKey).toBe("a");
    expect(s.getTask(t2)?.consoleKey).toBe("b");
  });
});
