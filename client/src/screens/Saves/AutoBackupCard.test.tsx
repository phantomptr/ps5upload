import { afterEach, describe, expect, it, vi } from "vitest";
import { renderToStaticMarkup } from "react-dom/server";

// A static render reads the store's live state, not its initial one.
vi.mock("zustand", async (orig) => {
  const { liveZustand } = await import("../../test-utils/liveZustand");
  return liveZustand(await orig<typeof import("zustand")>());
});

import AutoBackupCard, { ConsoleCopyChoice, IntervalChoice, firstConsoleDrive } from "./AutoBackupCard";
import {
  consoleCopySupported,
  intervalChoices,
  intervalSupported,
  useAutoSaveBackupStore,
} from "../../state/autoSaveBackup";
import type { ConsoleDrive } from "../../api/saveBackup";

describe("automatic backups: a copy on a PS5 drive", () => {
  const m2: ConsoleDrive = { path: "/mnt/ext0", kind: "ext", total_bytes: 1, free_bytes: 1 };
  const usb: ConsoleDrive = { path: "/mnt/usb0", kind: "usb", total_bytes: 1, free_bytes: 1 };
  const render = (p: Partial<Parameters<typeof ConsoleCopyChoice>[0]>) =>
    renderToStaticMarkup(
      <ConsoleCopyChoice
        drive=""
        drives={[m2, usb]}
        connected
        loadError={null}
        status={null}
        onChange={noop}
        {...p}
      />,
    );

  it("is offered only by an engine that can make the copy", () => {
    const base = { enabled: false, dir: "/data/save-backups", keep: 5, hosts: [] };
    expect(consoleCopySupported({ console_drive: "" })).toBe(true);
    expect(consoleCopySupported({})).toBe(false);
    useAutoSaveBackupStore.setState({ settings: { ...base, console_drive: "" } });
    expect(renderToStaticMarkup(<AutoBackupCard />)).toContain("Also keep a copy on a PS5 drive");
    useAutoSaveBackupStore.setState({ settings: base });
    expect(renderToStaticMarkup(<AutoBackupCard />)).not.toContain("auto-save-backup-console");
    useAutoSaveBackupStore.setState({ settings: null, status: null, error: null });
  });

  it("starts on the M.2 drive, else a USB one", () => {
    expect(firstConsoleDrive([usb, m2])).toBe("/mnt/ext0");
    expect(firstConsoleDrive([usb])).toBe("/mnt/usb0");
    expect(firstConsoleDrive([])).toBeNull();
  });

  it("is off with no drive picker until switched on", () => {
    const html = render({});
    expect(html).toContain("Also keep a copy on a PS5 drive");
    expect(html).toContain("ps5upload/save-backups");
    expect(html).not.toContain("auto-save-backup-console-drive");
    expect(html).not.toMatch(/checked=""/);
  });

  it("lists the PS5's M.2 and USB drives with the chosen one selected", () => {
    const html = render({ drive: "/mnt/usb0" });
    expect(html).toMatch(/checked=""/);
    expect(html).toContain(">M.2 drive (/mnt/ext0)</option>");
    expect(html).toMatch(/<option value="\/mnt\/usb0" selected="">USB drive \(\/mnt\/usb0\)<\/option>/);
  });

  it("keeps a chosen drive that is not plugged in, marked so", () => {
    const html = render({ drive: "/mnt/ext1", drives: [usb] });
    expect(html).toMatch(/<option value="\/mnt\/ext1" selected="">\/mnt\/ext1 \(not plugged in\)<\/option>/);
  });

  it("can't be switched on with no drive, and says why", () => {
    const html = render({ drives: [] });
    expect(html).toContain("auto-save-backup-console-none");
    expect(html).toMatch(/<input type="checkbox" disabled=""/);
    expect(render({ connected: false, drives: null })).toContain("Connect to a PS5 to choose its drive.");
  });

  it("shows the last copy result", () => {
    const html = render({
      drive: "/mnt/ext0",
      status: { at: 1_700_000_000_000, drive: "/mnt/ext0", copied: 2, skipped: 1, failed: 0, note: "/mnt/ext0 is not plugged in or not mounted" },
    });
    expect(html).toContain("2 copied, 1 skipped, 0 failed.");
    expect(html).toContain("/mnt/ext0 is not plugged in or not mounted");
  });
});

const noop = () => {};

describe("automatic backups: how often", () => {
  it("offers 5 minutes to a day, with what the engine has in its place", () => {
    expect(intervalChoices(10)).toEqual([5, 10, 30, 60, 360, 1440]);
    // An engine before the choice sends none: it backs up every 10 minutes.
    expect(intervalChoices(undefined)).toEqual([5, 10, 30, 60, 360, 1440]);
    // One set through the API stays listed, in order.
    expect(intervalChoices(45)).toEqual([5, 10, 30, 45, 60, 360, 1440]);
  });

  it("names each choice and selects the engine's", () => {
    const html = renderToStaticMarkup(<IntervalChoice minutes={60} onChange={noop} />);
    expect(html).toContain("Back up every");
    for (const label of ["5 min", "10 min", "30 min", "1 hour", "6 hours", "1 day"]) {
      expect(html).toContain(`>${label}</option>`);
    }
    expect(html).toMatch(/<option value="60" selected="">1 hour<\/option>/);
    // No setting yet: 10 minutes, what the engine does.
    expect(renderToStaticMarkup(<IntervalChoice minutes={undefined} onChange={noop} />)).toMatch(
      /<option value="10" selected="">10 min<\/option>/,
    );
    expect(renderToStaticMarkup(<IntervalChoice minutes={45} onChange={noop} />)).toMatch(
      /<option value="45" selected="">45 min<\/option>/,
    );
  });
});

describe("automatic backups: the card", () => {
  const base = { enabled: false, dir: "/data/save-backups", keep: 5, hosts: [] };
  afterEach(() => useAutoSaveBackupStore.setState({ settings: null, status: null, error: null }));

  it("offers the interval only when the engine has one", () => {
    expect(intervalSupported({ interval_minutes: 30 })).toBe(true);
    expect(intervalSupported({})).toBe(false);
    expect(intervalSupported(null)).toBe(false);
    useAutoSaveBackupStore.setState({ settings: { ...base, interval_minutes: 30 } });
    expect(renderToStaticMarkup(<AutoBackupCard />)).toContain("Back up every");
    // An engine before 12.5.1 ignores the choice: no picker that snaps back.
    useAutoSaveBackupStore.setState({ settings: base });
    const html = renderToStaticMarkup(<AutoBackupCard />);
    expect(html).toContain("Versions to keep");
    expect(html).not.toContain("Back up every");
  });

  it("shows why when the settings could not be read", () => {
    useAutoSaveBackupStore.setState({ settings: null, error: "engine down" });
    const html = renderToStaticMarkup(<AutoBackupCard />);
    expect(html).toContain("auto-save-backup-error");
    expect(html).toContain("engine down");
    useAutoSaveBackupStore.setState({ settings: null, error: null });
    expect(renderToStaticMarkup(<AutoBackupCard />)).toBe("");
  });
});
