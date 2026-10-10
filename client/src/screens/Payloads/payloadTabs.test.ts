// @ts-expect-error -- the app tsconfig has no node types; Vitest runs this under Node.
import { readFileSync } from "node:fs";
import { describe, expect, it } from "vitest";

import { NAV_ITEMS } from "../../layout/navItems";
import { payloadTabsFor } from "./payloadTabs";

const read = (rel: string): string => readFileSync(new URL(rel, import.meta.url).pathname, "utf8");
const app = read("../../App.tsx");
const shim = read("../../lib/browserInvoke.ts");

describe("Payloads in the browser build", () => {
  it("keeps every tab on the desktop", () => {
    expect(payloadTabsFor(true)).toEqual(["catalog", "send", "shadowmount", "nanodns"]);
  });

  it("offers only the tabs that work through the engine", () => {
    expect(payloadTabsFor(false)).toEqual(["shadowmount", "nanodns"]);
  });

  it("is reachable: in the nav and not behind NativeOnlyRoute", () => {
    expect(NAV_ITEMS.find((i) => i.to === "/payloads")?.hideInBrowser).toBeFalsy();
    expect(app).not.toMatch(/path="\/payloads"\s*element=\{\s*<NativeOnlyRoute>/);
  });

  it("relies only on calls the browser build maps", () => {
    for (const cmd of ["smp_status", "fs_read_preview", "fs_write_bytes_run"]) {
      expect(shim, cmd).toContain(`case "${cmd}":`);
    }
  });
});
