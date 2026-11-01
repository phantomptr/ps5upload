// @ts-expect-error -- the app tsconfig has no node types; Vitest runs this under Node.
import { readFileSync } from "node:fs";
import { describe, expect, it } from "vitest";

import { NAV_ITEMS } from "../../layout/navItems";
import {
  defaultPayloadTab,
  legacyPayloadsTarget,
  payloadCardLink,
  payloadTabsFor,
} from "./payloadTabs";

const read = (rel: string): string => readFileSync(new URL(rel, import.meta.url), "utf8");
const app = read("../../App.tsx");
const shim = read("../../lib/browserInvoke.ts");

describe("Payloads in the browser build", () => {
  it("keeps every tab on the desktop", () => {
    expect(payloadTabsFor(true)).toEqual(["catalog", "send", "ps5", "running"]);
  });

  it("offers browser payload sending alongside On your PS5 and Running", () => {
    expect(payloadTabsFor(false)).toEqual(["send", "ps5", "running"]);
  });

  it("opens on On your PS5 once a console is connected", () => {
    expect(defaultPayloadTab(true, true)).toBe("ps5");
    expect(defaultPayloadTab(true, false)).toBe("catalog");
    expect(defaultPayloadTab(false, false)).toBe("ps5");
  });

  it("is reachable: in the nav and not behind NativeOnlyRoute", () => {
    expect(NAV_ITEMS.find((i) => i.to === "/payloads")?.hideInBrowser).toBeFalsy();
    expect(app).not.toMatch(/path="\/payloads"\s*element=\{\s*<NativeOnlyRoute>/);
  });

  it("relies only on calls the browser build maps", () => {
    for (const cmd of [
      "smp_status",
      "fs_read_preview",
      "fs_write_bytes_run",
      "process_list_get",
      "process_kill_pid",
      "ps5_list_dir",
    ]) {
      expect(shim, cmd).toContain(`case "${cmd}":`);
    }
  });
});

describe("old Payloads links", () => {
  it("open the integration's card on On your PS5", () => {
    expect(legacyPayloadsTarget("manager")).toBe("/payloads?tab=ps5&open=payload-manager");
    expect(legacyPayloadsTarget("shadowmount")).toBe("/payloads?tab=ps5&open=shadowmountplus");
    expect(legacyPayloadsTarget("nanodns")).toBe("/payloads?tab=ps5&open=nanodns");
  });

  it("open Running for the old Processes and Ports tabs", () => {
    expect(legacyPayloadsTarget("processes")).toBe("/payloads?tab=running");
    expect(legacyPayloadsTarget("ports")).toBe("/payloads?tab=running");
  });

  it("leave current tabs alone", () => {
    for (const t of [null, "catalog", "send", "ps5", "running", "nonsense"]) {
      expect(legacyPayloadsTarget(t)).toBeNull();
    }
  });

  it("are routed in the app", () => {
    for (const [from, card] of [
      ["/nanodns", "nanodns"],
      ["/nano-dns", "nanodns"],
      ["/shadowmount", "shadowmountplus"],
      ["/payloads/manager", "payload-manager"],
      ["/payloads/shadowmount", "shadowmountplus"],
      ["/payloads/nanodns", "nanodns"],
      ["/payloads/hearbridge", "hearbridge"],
    ]) {
      expect(app, from).toContain(`path="${from}" element={<Navigate to={payloadCardLink("${card}")} replace />}`);
    }
    expect(app).toContain(`path="/payloads/ports" element={<Navigate to={RUNNING_LINK} replace />}`);
    expect(app).toContain(`path="/ports" element={<Navigate to={RUNNING_LINK} replace />}`);
    expect(payloadCardLink("payload-manager")).toBe("/payloads?tab=ps5&open=payload-manager");
  });
});
