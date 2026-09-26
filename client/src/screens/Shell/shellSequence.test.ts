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

  it("splits || as an 'or' part (run on previous failure)", () => {
    expect(splitShellSequence("test -f x || echo missing")).toEqual([
      { op: "always", cmd: "test -f x" },
      { op: "or", cmd: "echo missing" },
    ]);
    // Mixed chain: ; , && , ||
    expect(splitShellSequence("a; b && c || d")).toEqual([
      { op: "always", cmd: "a" },
      { op: "always", cmd: "b" },
      { op: "and", cmd: "c" },
      { op: "or", cmd: "d" },
    ]);
  });

  it("leaves a single | pipe inside the command", () => {
    expect(splitShellSequence("cat x | grep y")).toEqual([
      { op: "always", cmd: "cat x | grep y" },
    ]);
  });

  it("keeps || inside quotes literal", () => {
    expect(splitShellSequence("echo 'a || b'")).toEqual([
      { op: "always", cmd: "echo 'a || b'" },
    ]);
  });
});
