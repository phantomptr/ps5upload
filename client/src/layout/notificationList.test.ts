import { describe, expect, it } from "vitest";

import { addSeen, ps5Level } from "./notificationList";
import { notifTabOf } from "../screens/Notifications";
import { PANEL_SHEET } from "./NotificationInbox";

describe("addSeen", () => {
  it("keeps what was new and adds what arrived while the list is open", () => {
    const first = addSeen(new Set<string>(), ["a", "b"]);
    expect([...first]).toEqual(["a", "b"]);
    // a and b are read now; c arrives while the list is still open.
    const second = addSeen(first, ["c"]);
    expect([...second].sort()).toEqual(["a", "b", "c"]);
  });

  it("returns the same set when nothing is added, so state does not churn", () => {
    const s = addSeen(new Set([1, 2]), [2]);
    expect(addSeen(s, [])).toBe(s);
    expect(addSeen(s, [1, 2])).toBe(s);
  });
});

describe("ps5Level", () => {
  it("maps the helper's free-form levels onto the app's four", () => {
    expect(ps5Level("ERROR")).toBe("error");
    expect(ps5Level("critical")).toBe("error");
    expect(ps5Level("warn")).toBe("warning");
    expect(ps5Level("Warning")).toBe("warning");
    expect(ps5Level("info")).toBe("info");
    expect(ps5Level("debug")).toBe("info");
  });
});

describe("notifTabOf", () => {
  it("opens This app unless the link names another tab", () => {
    expect(notifTabOf(null)).toBe("app");
    expect(notifTabOf("nonsense")).toBe("app");
    expect(notifTabOf("ps5")).toBe("ps5");
    expect(notifTabOf("settings")).toBe("settings");
  });
});

describe("PANEL_SHEET", () => {
  it("makes the bell panel a fixed full-width sheet below md, whatever its anchor", () => {
    for (const cls of ["max-md:fixed", "max-md:inset-x-2", "max-md:w-auto", "max-md:max-w-none"]) {
      expect(PANEL_SHEET.split(" ")).toContain(cls);
    }
    expect(PANEL_SHEET).toMatch(/max-md:max-h-\[/);
    expect(PANEL_SHEET).toContain("--safe-top");
  });
});
