import { renderToStaticMarkup } from "react-dom/server";
import { afterEach, describe, expect, it, vi } from "vitest";

vi.mock("../../state/lang", () => ({
  useTr: () => (key: string, a?: unknown, b?: string) => {
    const vars = typeof a === "object" && a ? (a as Record<string, string | number>) : {};
    let s = (typeof a === "string" ? a : b) ?? key;
    for (const [k, v] of Object.entries(vars)) s = s.replace(`{${k}}`, String(v));
    return s;
  },
}));
const conn = vi.hoisted(() => ({ ps5Kernel: null as string | null }));
vi.mock("../../state/connection", () => ({
  useConnectionStore: (sel: (s: { host: string; ps5Kernel: string | null }) => unknown) =>
    sel({ host: "192.168.1.9", ps5Kernel: conn.ps5Kernel }),
}));
// The store's own hook reads its initial state in a server render, so the snapshot is read here.
vi.mock("../../lib/mounter", async (orig) => {
  const real = await orig<typeof import("../../lib/mounter")>();
  return {
    ...real,
    useMounterFor: (host: string | null | undefined) =>
      host ? (real.useMounterStore.getState().byHost[host] ?? null) : null,
  };
});

import type { SmpStatus } from "../../api/ps5";
import { useMounterStore } from "../../lib/mounter";
import { FpkgNote } from "./FpkgNote";

function snap(mounter: SmpStatus["mounter"]): SmpStatus {
  return {
    installed: false,
    running: false,
    config_ini: null,
    autotune_ini: null,
    debug_log_tail: null,
    mounted_images: [],
    errors: [],
    mounter,
  };
}

afterEach(() => {
  useMounterStore.setState({ byHost: {} });
  conn.ps5Kernel = null;
});

const kernel = (fw: string) => `FreeBSD 11.0-RELEASE-p0 #1 r229358/releases/${fw} Jul 17 2026`;

describe("FpkgNote", () => {
  it("names the two ways side by side, each with what it needs", () => {
    const html = renderToStaticMarkup(<FpkgNote />);
    expect(html).toContain("Two ways to play a fake PS5 package");
    expect(html).toContain("Install it with kstuff loaded, or mount it with Kraken Mount.");
    const install = html.slice(html.indexOf('data-testid="fpkg-way-install"'), html.indexOf('data-testid="fpkg-way-mount"'));
    expect(install).toContain("the latest kstuff (with fake-package support up to 13.60)");
    expect(install).toContain("send the latest one with Send payload.");
    expect(install).toContain("Up to firmware 11.40, if the console refuses the install, also load a53_ppr_install_fast.elf");
    expect(install).toContain("If a game you installed with an older kstuff doesn&#x27;t start, install it again.");
    expect(install).toContain("drakmor/ppr-patch");
    const mount = html.slice(html.indexOf('data-testid="fpkg-way-mount"'));
    expect(mount).toContain("Choose Copy as a file for the package here");
    expect(mount).toContain("not PS4 or retail packages");
    // Neither way is ranked over the other.
    expect(html).not.toMatch(/instead|better|recommended|alternative|fallback/i);
  });

  it("says before installing that 11.61 to 13.60 needs the latest kstuff, outside the fold", () => {
    expect(renderToStaticMarkup(<FpkgNote />)).not.toContain("fpkg-fw-note");
    conn.ps5Kernel = kernel("11.60");
    expect(renderToStaticMarkup(<FpkgNote />)).not.toContain("fpkg-fw-note");
    conn.ps5Kernel = kernel("14.00");
    expect(renderToStaticMarkup(<FpkgNote />)).not.toContain("fpkg-fw-note");
    conn.ps5Kernel = kernel("13.40");
    const html = renderToStaticMarkup(<FpkgNote />);
    const summary = html.slice(html.indexOf("<summary"), html.indexOf("</summary>"));
    expect(summary).toContain('data-testid="fpkg-fw-note"');
    expect(summary).toContain(
      "This PS5 is on firmware 13.40. On firmware 11.61 to 13.60 a fake package needs the latest kstuff (fake-package support up to 13.60); with an older kstuff it installs but won&#x27;t start.",
    );
    expect(html).not.toMatch(/instead|better|recommended|alternative|fallback/i);
  });

  it("says Kraken Mount runs on this PS5 only when the mounter snapshot says so", () => {
    expect(renderToStaticMarkup(<FpkgNote />)).not.toContain("fpkg-way-mount-running");
    useMounterStore.getState().put("192.168.1.9", snap("shadowmount"));
    expect(renderToStaticMarkup(<FpkgNote />)).not.toContain("fpkg-way-mount-running");
    useMounterStore.getState().put("192.168.1.9", snap("kraken"));
    const html = renderToStaticMarkup(<FpkgNote />);
    expect(html).toContain('data-testid="fpkg-way-mount-running"');
    expect(html).toContain("Kraken Mount is running on this PS5");
  });
});
