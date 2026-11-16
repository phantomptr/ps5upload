import { afterEach, describe, expect, it, vi } from "vitest";
import { renderToStaticMarkup } from "react-dom/server";

// A static render reads the store's live state, not its initial one.
vi.mock("zustand", async (orig) => {
  const { liveZustand } = await import("../../test-utils/liveZustand");
  return liveZustand(await orig<typeof import("zustand")>());
});

import AutoBackupCard, { IntervalChoice } from "./AutoBackupCard";
import { intervalChoices, intervalSupported, useAutoSaveBackupStore } from "../../state/autoSaveBackup";

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
