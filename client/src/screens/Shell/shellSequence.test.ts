import { describe, expect, it } from "vitest";
import { splitShellSequence } from "./shellSequence";

describe("splitShellSequence", () => {
  it("splits semicolon and && command lines", () => {
    expect(splitShellSequence("cd /data && ls; pwd")).toEqual([
      { op: "always", cmd: "cd /data" },
      { op: "and", cmd: "ls" },
      { op: "always", cmd: "pwd" },
    ]);
  });

  it("keeps separators inside quotes", () => {
    expect(splitShellSequence("echo 'a && b'; echo \"c;d\"")).toEqual([
      { op: "always", cmd: "echo 'a && b'" },
      { op: "always", cmd: 'echo "c;d"' },
    ]);
  });
});
