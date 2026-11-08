import { describe, expect, it, vi } from "vitest";
import { tokenUrl } from "../api";
import {
  compareVersions,
  freshUrl,
  helperIsNewer,
  mayReloadFor,
  onHelperVersion,
  reloadFresh,
  RELOAD_KEY,
  RELOAD_PARAM,
  VERSION_CHECK_MS,
  watchHelperVersion,
} from "./selfUpdate";

/** A Storage stand-in; `broken` throws like a private window or blocked site data. */
function store(broken = false) {
  const m = new Map<string, string>();
  return {
    m,
    getItem(k: string) {
      if (broken) throw new Error("denied");
      return m.get(k) ?? null;
    },
    setItem(k: string, v: string) {
      if (broken) throw new Error("denied");
      m.set(k, v);
    },
  };
}

describe("version compare", () => {
  it("orders release versions by their numbers, not as text", () => {
    expect(compareVersions("11.6.1", "11.6.0")).toBe(1);
    expect(compareVersions("11.6.0", "11.6.1")).toBe(-1);
    expect(compareVersions("11.10.0", "11.9.9")).toBe(1);
    expect(compareVersions("11.6.0", "11.6.0")).toBe(0);
    expect(compareVersions("11.6", "11.6.0")).toBe(0);
    expect(compareVersions("v11.7.0", "11.6.9")).toBe(1);
  });
  it("puts a prerelease below its release", () => {
    expect(compareVersions("11.7.0-beta.1", "11.7.0")).toBe(-1);
    expect(compareVersions("11.7.0", "11.7.0-beta.1")).toBe(1);
    expect(compareVersions("11.7.0-beta.1", "11.6.9")).toBe(1);
  });
  it("orders prerelease numbers as numbers", () => {
    expect(compareVersions("11.7.0-beta.10", "11.7.0-beta.9")).toBe(1);
    expect(compareVersions("11.7.0-beta.9", "11.7.0-beta.10")).toBe(-1);
    expect(compareVersions("11.7.0-beta", "11.7.0-beta.1")).toBe(-1);
    expect(compareVersions("11.7.0-alpha.2", "11.7.0-beta.1")).toBe(-1);
  });
  it("is null for anything that is not a version", () => {
    expect(compareVersions("dev", "11.6.0")).toBeNull();
    expect(compareVersions("11.6.0", "")).toBeNull();
  });
  it("a newer helper only: older, equal, unknown and unversioned pages never count", () => {
    expect(helperIsNewer("11.6.1", "11.6.0")).toBe(true);
    expect(helperIsNewer("11.6.0", "11.6.1")).toBe(false); // an older helper: its page is older too
    expect(helperIsNewer("11.6.0", "11.6.0")).toBe(false);
    expect(helperIsNewer(undefined, "11.6.0")).toBe(false);
    expect(helperIsNewer(42, "11.6.0")).toBe(false);
    expect(helperIsNewer("11.6.1", "")).toBe(false); // tests / dev build
  });
});

describe("reload guard", () => {
  it("allows one fresh load per helper version", () => {
    const s = store();
    expect(mayReloadFor("11.6.1", "", s)).toBe(true);
    s.setItem(RELOAD_KEY, "11.6.1");
    expect(mayReloadFor("11.6.1", "", s)).toBe(false);
    expect(mayReloadFor("11.7.0", "", s)).toBe(true); // the next update gets its own
  });
  it("without storage, the fresh load's own URL stops a second one", () => {
    const broken = store(true);
    expect(mayReloadFor("11.6.1", "", broken)).toBe(true);
    expect(mayReloadFor("11.6.1", `?${RELOAD_PARAM}=11.6.1`, broken)).toBe(false);
    expect(mayReloadFor("11.6.1", `?${RELOAD_PARAM}=11.6.0`, broken)).toBe(true);
    expect(mayReloadFor("11.6.1", `?${RELOAD_PARAM}=11.6.1`, null)).toBe(false);
  });
  it("the fresh URL carries the version the guard reads back", () => {
    const url = freshUrl("11.6.1-beta.1");
    expect(url.startsWith("/?")).toBe(true);
    expect(mayReloadFor("11.6.1-beta.1", url.slice(1), null)).toBe(false);
  });
});

describe("onHelperVersion", () => {
  it("a newer helper: remembered, then the page replaces itself (no banner)", () => {
    const s = store();
    const reload = vi.fn();
    const banner = vi.fn();
    expect(onHelperVersion("11.6.1", { page: "11.6.0", search: "", storage: s, reload, banner })).toBe("reload");
    expect(reload).toHaveBeenCalledWith("11.6.1");
    expect(banner).not.toHaveBeenCalled();
    expect(s.m.get(RELOAD_KEY)).toBe("11.6.1");
  });
  it("never loops: the second time for the same version it falls back to the banner", () => {
    const s = store();
    const reload = vi.fn();
    const banner = vi.fn();
    onHelperVersion("11.6.1", { page: "11.6.0", search: "", storage: s, reload, banner });
    expect(onHelperVersion("11.6.1", { page: "11.6.0", search: "", storage: s, reload, banner })).toBe("banner");
    expect(reload).toHaveBeenCalledTimes(1);
    expect(banner).toHaveBeenCalledTimes(1);
  });
  it("never loops without storage either (the URL guards it)", () => {
    const reload = vi.fn();
    const banner = vi.fn();
    const broken = store(true);
    expect(onHelperVersion("11.6.1", { page: "11.6.0", search: "", storage: broken, reload, banner })).toBe("reload");
    const after = freshUrl("11.6.1").slice(1);
    expect(onHelperVersion("11.6.1", { page: "11.6.0", search: after, storage: broken, reload, banner })).toBe("banner");
    expect(reload).toHaveBeenCalledTimes(1);
  });
  it("an older helper keeps the banner; the same one does nothing", () => {
    const reload = vi.fn();
    const banner = vi.fn();
    expect(onHelperVersion("11.5.0", { page: "11.6.0", search: "", storage: store(), reload, banner })).toBe("banner");
    expect(onHelperVersion("11.6.0", { page: "11.6.0", search: "", storage: store(), reload, banner })).toBe("none");
    expect(onHelperVersion(undefined, { page: "11.6.0", search: "", storage: store(), reload, banner })).toBe("none");
    expect(reload).not.toHaveBeenCalled();
    expect(banner).toHaveBeenCalledTimes(1);
  });
});

describe("reloadFresh", () => {
  it("drops the service worker and its caches, then opens a URL no appcache holds", async () => {
    const order: string[] = [];
    const win = {
      navigator: {
        serviceWorker: {
          getRegistrations: async () => [
            {
              unregister: async () => {
                order.push("unregister");
                return true;
              },
            },
          ],
        },
      },
      caches: {
        keys: async () => ["cw-page-abc", "other"],
        delete: async (k: string) => {
          order.push(`delete ${k}`);
          return true;
        },
      },
      location: { replace: (u: string) => order.push(`open ${u}`) },
    };
    await reloadFresh("11.6.1", win);
    expect(order).toEqual(["unregister", "delete cw-page-abc", `open /?${RELOAD_PARAM}=11.6.1`]);
  });
  it("still opens the page when the browser has neither, or a step fails", async () => {
    const opened: string[] = [];
    await reloadFresh("11.6.1", { navigator: {}, location: { replace: (u) => opened.push(u) } });
    await reloadFresh("11.6.1", {
      navigator: { serviceWorker: { getRegistrations: () => Promise.reject(new Error("no")) } },
      caches: { keys: () => Promise.reject(new Error("no")), delete: async () => true },
      location: { replace: (u) => opened.push(u) },
    });
    expect(opened).toHaveLength(2);
  });
  it("does not wait for ever on a step that never settles", async () => {
    vi.useFakeTimers();
    try {
      const opened: string[] = [];
      const p = reloadFresh("11.6.1", {
        navigator: { serviceWorker: { getRegistrations: () => new Promise(() => undefined) } },
        location: { replace: (u) => opened.push(u) },
      });
      await vi.advanceTimersByTimeAsync(5000);
      await p;
      expect(opened).toHaveLength(1);
    } finally {
      vi.useRealTimers();
    }
  });
});

describe("watchHelperVersion", () => {
  it("checks again on a 401, a page restored from the cache and each poll after the first, and stops after a reload", async () => {
    let tick: () => unknown = () => undefined;
    let expired: () => void = () => undefined;
    const listeners: Record<string, (e: Event) => void> = {};
    const read = vi.fn(async () => "11.6.1");
    const decide = vi.fn((): "reload" | "banner" | "none" => "none");
    const stop = watchHelperVersion({
      read,
      decide,
      poll: (fn, ms) => {
        expect(ms).toBe(VERSION_CHECK_MS);
        tick = fn;
        return () => undefined;
      },
      onExpired: (fn) => {
        expired = fn;
        return () => undefined;
      },
      target: {
        addEventListener: ((t: string, fn: (e: Event) => void) => {
          listeners[t] = fn;
        }) as Window["addEventListener"],
        removeEventListener: (() => undefined) as Window["removeEventListener"],
      },
    });
    tick(); // the poll's immediate first run: the first look already happened
    expect(read).not.toHaveBeenCalled();
    await tick();
    expect(read).toHaveBeenCalledTimes(1);
    expired();
    await Promise.resolve();
    expect(read).toHaveBeenCalledTimes(2);
    listeners.pageshow({ persisted: false } as unknown as Event);
    listeners.pageshow({ persisted: true } as unknown as Event);
    await Promise.resolve();
    expect(read).toHaveBeenCalledTimes(3);
    decide.mockReturnValue("reload");
    await tick();
    await tick();
    expect(read).toHaveBeenCalledTimes(4); // reloading: nothing more
    stop();
  });
});

describe("token read", () => {
  it("marks itself so the helper does not nudge it like an older stored page", () => {
    expect(tokenUrl("11.6.1")).toBe("/api/token?page=11.6.1");
    expect(tokenUrl("")).toBe("/api/token?page=dev");
  });
});
