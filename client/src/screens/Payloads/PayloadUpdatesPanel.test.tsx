import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";

import type { PayloadUpdateState, PmSyncReport } from "../../api/payloadUpdates";
import { PayloadUpdatesView, pmChangeText, visibleChanges, type PayloadUpdatesViewProps } from "./PayloadUpdatesPanel";

// No DOM here (vitest runs in node): static markup, asserting on what is shown.
const tr = ((_k: string, vars: Record<string, unknown> | undefined, fallback: string) =>
  fallback.replace(/\{(\w+)\}/g, (_, n) => String(vars?.[n] ?? ""))) as never;
const noop = () => undefined;
const NOW = 1_800_000_000;

const state = (over: Partial<PayloadUpdateState> = {}): PayloadUpdateState => ({
  checked_at: NOW - 120,
  rows: [
    {
      payload_id: "kstuff-echostretch",
      display_name: "kstuff-lite (EchoStretch)",
      is_custom: false,
      installed_version: "v1.5",
      latest_version: "v1.6",
      update_available: true,
      previous_version: "v1.4",
      prerelease: false,
      published_at: "",
      html_url: "",
    },
    {
      payload_id: "shadowmountplus",
      display_name: "ShadowMount+",
      is_custom: false,
      installed_version: "v2.0",
      latest_version: "v2.0",
      update_available: false,
      prerelease: false,
      published_at: "",
      html_url: "",
    },
  ],
  rate_limited: false,
  applied: [],
  available_seq: 1,
  applied_seq: 0,
  last_auto_run_at: 0,
  pm_syncs: {},
  ...over,
});

const props = (over: Partial<PayloadUpdatesViewProps> = {}): PayloadUpdatesViewProps => ({
  tr,
  settings: { auto_update: false },
  state: state(),
  host: "",
  pmPresent: null,
  choice: null,
  busy: null,
  nowSecs: NOW,
  onCheck: noop,
  onUpdate: noop,
  onRollback: noop,
  onAuto: noop,
  onChoice: noop,
  onSync: noop,
  ...over,
});

const html = (over: Partial<PayloadUpdatesViewProps> = {}) => renderToStaticMarkup(<PayloadUpdatesView {...props(over)} />);

describe("Payload updates section", () => {
  it("offers the check and the auto-update switch, off by default", () => {
    const out = html({ state: state({ checked_at: 0, rows: [] }) });
    expect(out).toContain("Check for payload updates");
    expect(out).toContain("Keep payloads up to date");
    expect(out).toContain('aria-checked="false"');
    expect(out).toContain("Not checked yet");
  });

  it("shows what is newer, with Update and a one-click rollback", () => {
    const out = html();
    expect(out).toContain("Updates available: 1");
    expect(out).toContain("v1.6 available");
    expect(out).toContain("Update");
    expect(out).toContain("Roll back to v1.4");
    // The current one has no update button of its own.
    const smp = out.split('data-testid="pu-row-shadowmountplus"')[1] ?? "";
    expect(smp).not.toContain("available");
  });

  it("says when everything is current, and when the rate limit was hit", () => {
    const current = state({
      rows: state().rows.map((r) => ({ ...r, update_available: false })),
      rate_limited: true,
    });
    const out = html({ state: current });
    expect(out).toContain("Everything is up to date.");
    expect(out).toContain("limit for checks was reached");
    expect(out).not.toContain("Update all");
  });

  it("without a console, has no Payload Manager controls", () => {
    const out = html();
    expect(out).not.toContain("Keep in Payload Manager");
    expect(out).not.toContain("Sync with Payload Manager");
  });

  it("says so when Payload Manager isn't running, and offers no sync", () => {
    const out = html({ host: "192.0.2.5", pmPresent: false });
    expect(out).toContain("Payload Manager isn&#x27;t running on this PS5");
    expect(out).toContain("this computer&#x27;s payloads only");
    expect(out).not.toContain("Sync with Payload Manager");
  });

  it("lets the user pick what Payload Manager keeps and loads at boot", () => {
    const out = html({
      host: "192.0.2.5",
      pmPresent: true,
      choice: { ids: ["kstuff-echostretch"], autoload_ids: [] },
    });
    expect(out).toContain("Keep in Payload Manager");
    expect(out).toContain("Payload Manager loads it at boot");
    expect(out).toContain("Sync with Payload Manager");
    const kstuff = out.split('data-testid="pu-row-kstuff-echostretch"')[1]?.split("</li>")[0] ?? "";
    // Kept: ticked. Boot: not ticked (only what the user chose).
    expect(kstuff.match(/checked=""/g)?.length).toBe(1);
  });

  it("disables the sync until something is chosen", () => {
    const out = html({ host: "192.0.2.5", pmPresent: true, choice: { ids: [], autoload_ids: [] } });
    const button = out.split("Sync with Payload Manager")[0].split("<button").pop() ?? "";
    expect(button).toContain("disabled");
  });

  it("lists what the last sync changed", () => {
    const report: PmSyncReport = {
      host: "192.0.2.5",
      at: NOW - 60,
      present: true,
      pm_version: "0.6.1",
      changes: [
        { kind: "stored", payload_id: "kstuff-echostretch", filename: "kstuff.elf", from: "v1.5", to: "v1.6" },
        { kind: "unchanged", payload_id: "shadowmountplus", filename: "shadowmountplus.elf", version: "v2.0" },
        { kind: "autoload_added", payload_id: "kstuff-echostretch", entry: "local/kstuff" },
      ],
      autoload_before: [],
      autoload_after: ["local/kstuff"],
    };
    const out = html({
      host: "192.0.2.5",
      pmPresent: true,
      choice: { ids: ["kstuff-echostretch"], autoload_ids: ["kstuff-echostretch"] },
      state: state({ pm_syncs: { "192.0.2.5": report } }),
    });
    expect(out).toContain("Stored kstuff-lite (EchoStretch) v1.6");
    expect(out).toContain("kstuff-lite (EchoStretch) now loads at boot");
    expect(out).not.toContain("already there");
  });
});

describe("Payload Manager change lines", () => {
  const nameOf = (id: string) => ({ hush: "Hush" })[id] ?? id;
  it("words each kind", () => {
    expect(pmChangeText({ kind: "skipped", payload_id: "hush", code: "repo_managed", reason: "x" }, tr, nameOf)).toBe(
      "Skipped Hush: Payload Manager updates it from its own repository",
    );
    expect(pmChangeText({ kind: "skipped", payload_id: "hush", code: "other", reason: "why" }, tr, nameOf)).toBe(
      "Skipped Hush: why",
    );
    expect(pmChangeText({ kind: "removed_old", payload_id: "hush", filename: "hush_v1.elf" }, tr, nameOf)).toBe(
      "Removed the older copy hush_v1.elf",
    );
    expect(pmChangeText({ kind: "autoload_removed", payload_id: "hush", entry: "local/hush" }, tr, nameOf)).toBe(
      "Hush no longer loads at boot",
    );
    expect(pmChangeText({ kind: "failed", payload_id: "", error: "HTTP 500" }, tr, nameOf)).toBe("HTTP 500");
  });

  it("leaves out what did not change", () => {
    const r = {
      changes: [
        { kind: "unchanged", payload_id: "a", filename: "a.elf", version: "1" },
        { kind: "autoload_added", payload_id: "a", entry: "a.elf" },
      ],
    } as PmSyncReport;
    expect(visibleChanges(r).map((c) => c.kind)).toEqual(["autoload_added"]);
  });
});
