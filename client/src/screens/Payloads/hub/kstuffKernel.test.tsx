import { describe, expect, it } from "vitest";
import { renderToStaticMarkup } from "react-dom/server";
import { isKstuffName, kstuffProbablyInKernel, KstuffKernelNote } from "./kstuffKernel";

const p = (comm: string) => ({ comm, name: comm, is_self: false });

describe("kstuff in the kernel", () => {
  it("knows kstuff file names", () => {
    expect(isKstuffName("/Users/me/kstuff-1.15-fpkg.elf")).toBe(true);
    expect(isKstuffName("C:\\x\\KSTUFF.elf")).toBe(true);
    expect(isKstuffName("/kstuff/etaHEN.bin")).toBe(false);
    expect(isKstuffName(null)).toBe(false);
  });

  it("is a probably only with kernel access and no kstuff process", () => {
    expect(kstuffProbablyInKernel([p("elfldr.elf")], true)).toBe(true);
    expect(kstuffProbablyInKernel([p("kstuff.elf")], true)).toBe(false);
    expect(kstuffProbablyInKernel([], false)).toBe(false);
    expect(kstuffProbablyInKernel([], null)).toBe(false);
  });

  it("says it is not certain and how to change kstuff, for both places", () => {
    for (const v of ["hub", "send"] as const) {
      const html = renderToStaticMarkup(<KstuffKernelNote variant={v} />);
      expect(html).toContain(`kstuff-kernel-${v}`);
      expect(html).toContain("isn&#x27;t certain");
      expect(html).toContain("restart the console");
    }
  });
});
