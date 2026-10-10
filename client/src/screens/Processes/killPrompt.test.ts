import { describe, expect, it } from "vitest";

import type { ProcessInfo } from "../../api/ps5";
import { killPrompt } from "./killPrompt";

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
