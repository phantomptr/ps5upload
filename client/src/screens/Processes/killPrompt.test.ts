import { describe, expect, it } from "vitest";

import type { ProcessInfo } from "../../api/ps5";
import { killChecked, killPrompt } from "./killPrompt";

const proc = (p: Partial<ProcessInfo>): ProcessInfo => ({
  pid: 1,
  name: "",
  comm: "",
  title_id: "",
  app_id: 0,
  memory_mib: 0,
  threads: 1,
  kind: "app",
  ...p,
});

describe("killPrompt", () => {
  it("asks before killing a payload such as kstuff", () => {
    expect(killPrompt(proc({ kind: "payload", comm: "kstuff.elf" }))).toBe("payload");
  });

  it("asks before killing a game or a system process", () => {
    expect(killPrompt(proc({ kind: "app" }))).toBe("app");
    expect(killPrompt(proc({ kind: "system" }))).toBe("system");
  });

  it("never offers to kill the helper itself", () => {
    expect(killPrompt(proc({ kind: "payload", is_self: true }))).toBeNull();
  });
});

describe("killChecked", () => {
  // The helper answers a refused or failed kill with ok:false (HTTP 200): the Running tab
  // used to log that as "killed" and show nothing.
  it("throws the helper's reason when the kill didn't happen", async () => {
    const kill = async (_a: string, pid: number) => ({ ok: false, pid, err: "ESRCH" });
    await expect(killChecked("ps5", 102, kill)).rejects.toThrow("ESRCH");
  });

  it("throws without a reason too, naming the pid", async () => {
    const kill = async (_a: string, pid: number) => ({ ok: false, pid });
    await expect(killChecked("ps5", 102, kill)).rejects.toThrow("102");
  });

  it("resolves when the helper killed it, asking for that pid on that console", async () => {
    const asked: Array<[string, number]> = [];
    const kill = async (a: string, pid: number) => {
      asked.push([a, pid]);
      return { ok: true, pid };
    };
    await expect(killChecked("192.168.0.5:9114", 102, kill)).resolves.toBeUndefined();
    expect(asked).toEqual([["192.168.0.5:9114", 102]]);
  });
});
