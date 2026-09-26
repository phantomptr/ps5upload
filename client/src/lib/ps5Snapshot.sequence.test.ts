import { beforeEach, describe, expect, it, vi } from "vitest";

vi.mock("./invokeLogged", () => ({ invoke: vi.fn() }));

import { invoke } from "./invokeLogged";
import { useConnectionStore } from "../state/connection";
import { clearBlackBoxes } from "./payloadBlackBox";
import { buildPs5Snapshot } from "./ps5Snapshot";

const RESET = "read frame header: Connection reset by peer (os error 104)";

/** Records every command in order. The helper "dies" on `killOn`: that call
 *  and every later one fails the way a dead helper's do. `benign` fails on its
 *  own without taking the helper down. */
function fakeHelper(opts: { killOn?: string; benign?: string } = {}) {
  const calls: string[] = [];
  let dead = false;
  vi.mocked(invoke).mockImplementation(async (cmd: string) => {
    calls.push(cmd);
    if (dead || cmd === opts.killOn) {
      dead = true;
      throw new Error(RESET);
    }
    if (cmd === opts.benign) throw new Error("unknown frame type 0x42");
    if (cmd === "fs_read_preview") return { base64: "" };
    return {};
  });
  return calls;
}

const HW = ["ps5_hw_storage", "ps5_hw_power", "ps5_hw_temps", "ps5_hw_info", "ps5_focus"];

describe("bug report collection order", () => {
  beforeEach(() => {
    clearBlackBoxes();
    vi.mocked(invoke).mockReset();
    useConnectionStore.setState({ host: "192.168.1.50", payloadStatus: "up" });
  });

  /* The helper's own logs are the one thing a crash report cannot do without,
   * so they are read before anything that might take the helper down. */
  it("reads the helper's logs before asking it anything else", async () => {
    const calls = fakeHelper();
    await buildPs5Snapshot({ redact: true });
    // Reading the logs is a directory listing (for per-transaction journals)
    // plus one read per file.
    const isLogRead = (c: string) => c === "ps5_list_dir" || c === "fs_read_preview";
    const firstOther = calls.findIndex((c) => !isLogRead(c));
    const lastLogRead = calls.map(isLogRead).lastIndexOf(true);
    expect(calls[0]).toBe("ps5_list_dir");
    expect(calls).toContain("fs_read_preview");
    expect(lastLogRead).toBeLessThan(firstOther);
  });

  /* Hardware and Sony-API reads are the least proven, so they come after
   * every ordinary state read. */
  it("asks for hardware reads only after every ordinary read", async () => {
    const calls = fakeHelper();
    await buildPs5Snapshot({ redact: true });
    const firstHw = Math.min(...HW.map((c) => calls.indexOf(c)).filter((i) => i >= 0));
    for (const ordinary of ["app_list_running", "proc_list_get", "ps5_volumes", "smp_status"]) {
      expect(calls.indexOf(ordinary)).toBeGreaterThanOrEqual(0);
      expect(calls.indexOf(ordinary)).toBeLessThan(firstHw);
    }
  });

  /* One probe at a time is what makes this possible: when the helper goes,
   * the report names the request in flight, and stops asking. */
  it("names the probe the helper stopped answering on, and stops there", async () => {
    const calls = fakeHelper({ killOn: "ps5_hw_info" });
    const { snapshot } = await buildPs5Snapshot({ redact: true });

    expect(snapshot.helper_lost_during).toBe("hw_info");
    expect(snapshot.helper_last_answered).toBe("hw_temps");
    expect(calls).not.toContain("ps5_focus");
    expect(snapshot.errors.focus).toMatch(/^skipped: .*"hw_info"/);
  });

  /* An older helper rejects frames it does not know. That is a per-probe
   * error, not a dead helper, and must not cut the report short. */
  it("carries on past a probe that fails without killing the helper", async () => {
    const calls = fakeHelper({ benign: "smp_status" });
    const { snapshot } = await buildPs5Snapshot({ redact: true });

    expect(snapshot.helper_lost_during).toBeNull();
    expect(snapshot.errors.smp_status).toMatch(/unknown frame type/);
    for (const hw of HW) expect(calls).toContain(hw);
  });

  /* A healthy console answers everything and nothing is flagged. */
  it("flags nothing when the helper answers every probe", async () => {
    fakeHelper();
    const { snapshot } = await buildPs5Snapshot({ redact: true });
    expect(snapshot.helper_lost_during).toBeNull();
    expect(snapshot.helper_last_answered).toBe("focus");
  });
});
