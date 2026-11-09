import { describe, expect, it, vi } from "vitest";

import { onStatusRecheck, requestStatusRecheck } from "./statusRecheck";

describe("statusRecheck", () => {
  it("tells every listener, and stops telling one that left", () => {
    const a = vi.fn();
    const b = vi.fn();
    const offA = onStatusRecheck(a);
    const offB = onStatusRecheck(b);
    requestStatusRecheck({ host: "192.168.1.5", wake: true });
    expect(a).toHaveBeenCalledWith({ host: "192.168.1.5", wake: true });
    expect(b).toHaveBeenCalledTimes(1);
    offA();
    requestStatusRecheck();
    expect(a).toHaveBeenCalledTimes(1);
    expect(b).toHaveBeenLastCalledWith({});
    offB();
  });

  it("a listener that throws does not stop the others", () => {
    const warn = vi.spyOn(console, "warn").mockImplementation(() => {});
    const ok = vi.fn();
    const off1 = onStatusRecheck(() => {
      throw new Error("boom");
    });
    const off2 = onStatusRecheck(ok);
    requestStatusRecheck();
    expect(ok).toHaveBeenCalledTimes(1);
    off1();
    off2();
    warn.mockRestore();
  });
});
