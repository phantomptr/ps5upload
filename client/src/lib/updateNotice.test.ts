import { describe, expect, it } from "vitest";

import { bannerId, helperIsNewer, helperPair, updateNotice, type NoticeInput } from "./updateNotice";

const base: NoticeInput = {
  appVersion: "12.6.11",
  helperVersion: "12.6.11",
  waitingVersion: null,
  installFailed: false,
  dismissed: [],
};
const notice = (over: Partial<NoticeInput>) => updateNotice({ ...base, ...over });

describe("a routine update is quiet", () => {
  it("shows a dot and the Settings row, never a banner", () => {
    const n = notice({ waitingVersion: "12.6.13" });
    expect(n.dot).toBe(true);
    expect(n.row).toEqual({ version: "12.6.13", helperNewer: false });
    expect(n.banner).toBeNull();
  });

  it("says nothing when there is no update and the helper matches", () => {
    expect(notice({})).toEqual({ dot: false, row: null, banner: null });
  });

  it("an older helper is not the app's update to announce (the app updates the helper itself)", () => {
    expect(notice({ helperVersion: "12.6.9" })).toEqual({ dot: false, row: null, banner: null });
  });
});

describe("a PS5 that runs a newer helper than this app", () => {
  it("is one line naming the version to update to, merged with the update row", () => {
    const n = notice({ helperVersion: "12.6.13", waitingVersion: "12.6.13" });
    expect(n.banner).toEqual({
      kind: "helper-newer",
      version: "12.6.13",
      id: bannerId("helper-newer", "12.6.13->12.6.11"),
      more: 0,
    });
    expect(n.row).toEqual({ version: "12.6.13", helperNewer: true });
    expect(n.dot).toBe(true);
  });

  it("names the helper's version while no update is found yet, and still lights Settings", () => {
    const n = notice({ helperVersion: "12.6.13" });
    expect(n.banner?.version).toBe("12.6.13");
    expect(n.row).toEqual({ version: "12.6.13", helperNewer: true });
    expect(n.dot).toBe(true);
  });

  it("names the waiting update when it goes past the helper, the helper when it falls short", () => {
    expect(notice({ helperVersion: "12.6.13", waitingVersion: "12.6.15" }).banner?.version).toBe("12.6.15");
    expect(notice({ helperVersion: "12.6.13", waitingVersion: "12.6.12" }).banner?.version).toBe("12.6.13");
  });

  it("stays dismissed for that version pair only", () => {
    const dismissed = [bannerId("helper-newer", helperPair("12.6.13", "12.6.11"))];
    expect(notice({ helperVersion: "12.6.13", dismissed }).banner).toBeNull();
    // The dot and the row stay: dismissing the banner is not skipping the update.
    expect(notice({ helperVersion: "12.6.13", dismissed }).dot).toBe(true);
    // Another device sends a newer one again: a new pair, shown again.
    expect(notice({ helperVersion: "12.6.14", dismissed }).banner?.kind).toBe("helper-newer");
    // This app updated, the helper still newer: a new pair too.
    expect(notice({ helperVersion: "12.6.13", appVersion: "12.6.12", dismissed }).banner?.kind).toBe(
      "helper-newer",
    );
  });

  it("a pre-release helper of the same version is older, not newer", () => {
    expect(helperIsNewer("12.6.15-rc.1", "12.6.15")).toBe(false);
    expect(helperIsNewer("12.6.15", "12.6.15-rc.1")).toBe(true);
    expect(notice({ helperVersion: "12.6.15-rc.1", appVersion: "12.6.15" }).banner).toBeNull();
  });

  it("unknown or unreadable versions never raise it", () => {
    expect(notice({ helperVersion: null }).banner).toBeNull();
    expect(notice({ appVersion: null, helperVersion: "99.0.0" }).banner).toBeNull();
    expect(notice({ helperVersion: "dev-build" }).banner).toBeNull();
  });
});

describe("a failed install", () => {
  it("is a banner for that version", () => {
    const n = notice({ waitingVersion: "12.6.13", installFailed: true });
    expect(n.banner).toEqual({
      kind: "install-failed",
      version: "12.6.13",
      id: bannerId("install-failed", "12.6.13"),
      more: 0,
    });
  });

  it("with a newer helper too, is still one line: the failure first, the rest as a count", () => {
    const n = notice({ waitingVersion: "12.6.13", installFailed: true, helperVersion: "12.6.13" });
    expect(n.banner?.kind).toBe("install-failed");
    expect(n.banner?.more).toBe(1);
  });

  it("dismissed, gives way to the other one", () => {
    const n = notice({
      waitingVersion: "12.6.13",
      installFailed: true,
      helperVersion: "12.6.13",
      dismissed: [bannerId("install-failed", "12.6.13")],
    });
    expect(n.banner?.kind).toBe("helper-newer");
    expect(n.banner?.more).toBe(0);
  });
});
