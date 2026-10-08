import { beforeEach, describe, expect, it, vi } from "vitest";

const invoke = vi.fn<(cmd: string, args?: Record<string, unknown>) => Promise<unknown>>();
vi.mock("../lib/invokeLogged", () => ({
  invoke: (cmd: string, args?: Record<string, unknown>) => invoke(cmd, args),
}));

import { helperSource } from "./helperSendRuntime";

describe("helperSource", () => {
  beforeEach(() => invoke.mockReset());

  it("in a browser, has the engine send its own (stamped) helper instead of a local file", async () => {
    invoke.mockResolvedValue({ ok: true, bytes: 1234 });
    const src = helperSource(false);
    const elf = await src.bundledPath();
    await src.send("192.168.1.118", elf);
    expect(invoke).toHaveBeenCalledTimes(1);
    expect(invoke).toHaveBeenCalledWith("payload_restore", { ip: "192.168.1.118" });
  });

  it("in a browser, fails the send with the engine's reason when it could not send", async () => {
    invoke.mockResolvedValue({ ok: false, bytes: 0, error: "connect to :9021 refused" });
    await expect(helperSource(false).send("192.168.1.118", "ps5upload.elf")).rejects.toThrow(
      "connect to :9021 refused",
    );
  });

  it("on the desktop, sends the bundled ELF from disk", async () => {
    invoke.mockImplementation(async (cmd) =>
      cmd === "payload_bundled_path"
        ? { ok: true, path: "/app/ps5upload.elf" }
        : { ok: true },
    );
    const src = helperSource(true);
    const elf = await src.bundledPath();
    await src.send("192.168.1.118", elf);
    expect(invoke).toHaveBeenLastCalledWith("payload_send", {
      ip: "192.168.1.118",
      path: "/app/ps5upload.elf",
      port: null,
    });
    expect(invoke).not.toHaveBeenCalledWith("payload_restore", expect.anything());
  });
});
