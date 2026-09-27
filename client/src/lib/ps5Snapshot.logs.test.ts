import { beforeEach, describe, expect, it, vi } from "vitest";

vi.mock("./invokeLogged", () => ({ invoke: vi.fn() }));

import { invoke } from "./invokeLogged";
import { useConnectionStore } from "../state/connection";
import { clearBlackBoxes } from "./payloadBlackBox";
import { buildPs5Snapshot } from "./ps5Snapshot";

type Args = { path?: string };

/** A console whose log directories hold only the files named here. */
function consoleWith(files: Record<string, string[]>, unlistable: string[] = []) {
  const reads: string[] = [];
  vi.mocked(invoke).mockImplementation(async (cmd: string, args?: unknown) => {
    const path = (args as Args | undefined)?.path ?? "";
    if (cmd === "ps5_list_dir") {
      if (unlistable.includes(path)) throw new Error("fs_list_dir failed");
      return { entries: (files[path] ?? []).map((name) => ({ name, kind: "file" })) };
    }
    if (cmd === "fs_read_preview") {
      reads.push(path);
      return { base64: btoa("log line") };
    }
    return {};
  });
  return reads;
}

describe("helper log collection", () => {
  beforeEach(() => {
    clearBlackBoxes();
    vi.mocked(invoke).mockReset();
    useConnectionStore.setState({ host: "192.168.1.50", payloadStatus: "up" });
  });

  it("reads only the log files that exist, so a fresh console logs no failed reads", async () => {
    const reads = consoleWith({ "/data/ps5upload": ["stderr.log", "startup.log"] });
    await buildPs5Snapshot({ redact: true });
    expect(reads.sort()).toEqual(["/data/ps5upload/startup.log", "/data/ps5upload/stderr.log"]);
  });

  it("still tries a file when its directory cannot be listed", async () => {
    const reads = consoleWith({}, ["/data/shadowmount"]);
    await buildPs5Snapshot({ redact: true });
    expect(reads).toContain("/data/shadowmount/debug.log");
    expect(reads).toContain("/data/shadowmount/config.ini");
    expect(reads).not.toContain("/data/ps5upload/stderr.log");
  });
});
