import { isValidElement, type ReactElement, type ReactNode } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import { beforeEach, describe, expect, it, vi } from "vitest";

vi.mock("../api", async (importOriginal) => {
  const real = await importOriginal<typeof import("../api")>();
  return { ...real, call: vi.fn() };
});

import { ApiError, call } from "../api";
import type { Volume } from "../home/Home";
import { t } from "../strings";
import { ConfirmDialog } from "../ui/ConfirmDialog";
import { ActionsSheet } from "./ActionsSheet";
import { canEject, DriveList, isImage, isSystemVolume, likelyFitsBytes, placeLabel } from "./DriveList";
import { ejectBlocker, FilesView, PinnedChoices, volumeErrorText, type FilesViewProps } from "./Files";
import { createPrefsApplier } from "./prefs";
import type { Job } from "../home/useJobs";
import { PathBar } from "./PathBar";
import { barItems, checkFolder, pathSegments, typedPath } from "./pathParts";
import { cleanPath, EMPTY_PREFS, isPinned, toPrefs, type FilesPrefs } from "./prefs";
import { TextPrompt } from "./TextPrompt";
import type { DirState, Entry } from "./useDir";

const mockCall = vi.mocked(call);

function elements(node: ReactNode): ReactElement<Record<string, unknown>>[] {
  const out: ReactElement<Record<string, unknown>>[] = [];
  const walk = (n: ReactNode) => {
    if (Array.isArray(n)) return n.forEach(walk);
    if (!isValidElement(n)) return;
    const el = n as ReactElement<Record<string, unknown>>;
    out.push(el);
    walk(el.props.children as ReactNode);
  };
  walk(node);
  return out;
}

const vol = (path: string, mount_from = "/dev/x", over: Partial<Volume> = {}): Volume => ({
  path,
  mount_from,
  fs_type: "ufs",
  total_bytes: 1000,
  free_bytes: 400,
  writable: true,
  ...over,
});

const VOLS = [
  vol("/"),
  vol("/data"),
  vol("/mnt/usb0", "/dev/da0s1"),
  vol("/mnt/ext1", "/dev/nvme0"),
  vol("/mnt/ps5upload/game.exfat", "/dev/lvd2", { source_image: "/mnt/usb0/images/game.exfat" }),
  vol("/system_ex", "/dev/ssd0"),
];

const entry = (name: string, kind: Entry["kind"] = "file"): Entry => ({ name, kind, size: 10, mtime: 0 });
const dir = (entries: Entry[]): DirState => ({ entries, more: false, loading: false, loaded: true, error: null });

const PREFS: FilesPrefs = {
  favorites: ["/mnt/usb0/games", "/data"],
  recent: ["/data/homebrew/中文 game", "/mnt/ext1"],
  last: "/data/homebrew/中文 game",
};

function props(over: Partial<FilesViewProps> = {}): FilesViewProps {
  return {
    path: null,
    volumes: VOLS,
    volumesError: false,
    dir: dir([entry("games", "dir"), entry("a.txt")]),
    selecting: false,
    selected: [],
    ask: null,
    busy: null,
    message: null,
    tasks: [],
    prefs: PREFS,
    onOpen: vi.fn(),
    onUp: vi.fn(),
    onEntry: vi.fn(),
    onLoadMore: vi.fn(),
    onToggleSelecting: vi.fn(),
    onToggleItem: vi.fn(),
    onAsk: vi.fn(),
    onDismiss: vi.fn(),
    onConfirmDelete: vi.fn(),
    onConfirmFixPerm: vi.fn(),
    onRename: vi.fn(),
    onMkdir: vi.fn(),
    onPicked: vi.fn(),
    onPin: vi.fn(),
    onUnpin: vi.fn(),
    onEject: vi.fn(),
    onUnmount: vi.fn(),
    onGoTo: vi.fn(),
    ...over,
  };
}

beforeEach(() => {
  mockCall.mockReset();
});

describe("path bar", () => {
  it("segments start at the drive (by its name) and jump to each folder", () => {
    expect(pathSegments("/data/homebrew/game", VOLS)).toEqual([
      { label: t("cw_home_vol_internal"), path: "/data" },
      { label: "homebrew", path: "/data/homebrew" },
      { label: "game", path: "/data/homebrew/game" },
    ]);
    expect(pathSegments("/mnt/usb0//a/", VOLS).map((s) => s.path)).toEqual(["/mnt/usb0", "/mnt/usb0/a"]);
    // Outside every drive: from "/" (the "/" volume is not a drive to name).
    expect(pathSegments("/user/app", VOLS)).toEqual([
      { label: "/", path: "/" },
      { label: "user", path: "/user" },
      { label: "app", path: "/user/app" },
    ]);
    // A mounted image is its own root, named by its path.
    expect(pathSegments("/mnt/ps5upload/game.exfat/sce_sys", VOLS)[0]).toEqual({
      label: "/mnt/ps5upload/game.exfat",
      path: "/mnt/ps5upload/game.exfat",
    });
  });

  it("long paths collapse the middle behind …, which reveals them", () => {
    const segs = pathSegments("/data/a/b/c/d/e", VOLS);
    expect(segs).toHaveLength(6);
    const collapsed = barItems(segs, false);
    expect(collapsed.map((i) => (i.kind === "seg" ? i.seg.label : "…"))).toEqual([t("cw_home_vol_internal"), "…", "d", "e"]);
    const more = collapsed[1];
    expect(more.kind === "more" && more.hidden.map((s) => s.path)).toEqual(["/data/a", "/data/a/b", "/data/a/b/c"]);
    expect(collapsed[3]).toMatchObject({ kind: "seg", last: true });
    expect(barItems(segs, true)).toHaveLength(6);
    expect(barItems(pathSegments("/data/a/b", VOLS), false)).toHaveLength(3);

    const html = renderToStaticMarkup(<PathBar path="/data/a/b/c/d/e" volumes={VOLS} onOpen={() => {}} onUp={() => {}} />);
    expect(html).toContain('data-action="more-path"');
    expect(html).toContain('data-seg="/data/a/b/c/d/e"');
    expect(html).not.toContain('data-seg="/data/a/b"');
    expect(html).toContain('aria-current="location"');
    expect(html).toContain('data-action="up"');
  });

  it("the path bar is in the folder view, keyed by the folder", () => {
    const p = props({ path: "/data/homebrew" });
    const bar = elements(FilesView(p)).find((e) => e.type === PathBar)!;
    expect(bar.key).toBe("/data/homebrew");
    expect(bar.props.onOpen).toBe(p.onOpen);
    expect(bar.props.onUp).toBe(p.onUp);
  });

  it("typed paths: absolute and clean, else refused", () => {
    expect(typedPath(" /data//games/ ")).toBe("/data/games");
    expect(typedPath("/")).toBe("/");
    expect(typedPath("data")).toBeNull();
    expect(typedPath("/data/../etc")).toBeNull();
    expect(typedPath("")).toBeNull();
  });

  it("go to path checks the folder first and says why not", async () => {
    mockCall.mockResolvedValueOnce({ kind: "dir" } as never);
    expect(await checkFolder("/data/games/")).toBeNull();
    expect(mockCall).toHaveBeenCalledWith("/api/fs/stat", { path: "/data/games" });
    mockCall.mockResolvedValueOnce({ kind: "file" } as never);
    expect(await checkFolder("/data/a.txt")).toBe(t("cw_files_goto_not_folder"));
    mockCall.mockRejectedValueOnce(new ApiError(404, "fs_stat_failed"));
    expect(await checkFolder("/nope")).toBe(t("cw_files_goto_missing"));
    mockCall.mockRejectedValueOnce(new ApiError(403, "path_not_allowed"));
    expect(await checkFolder("/data/ps5upload/ava")).toBe(t("cw_files_err_denied"));
    expect(await checkFolder("relative")).toBe(t("cw_files_goto_bad"));
    mockCall.mockRejectedValueOnce(new ApiError(0, "offline"));
    expect(await checkFolder("/data")).toBe(t("cw_home_err_offline"));
    expect(mockCall).toHaveBeenCalledTimes(5);
  });

  it("go to path is a text prompt with that check; OK navigates", () => {
    const p = props({ path: "/data", ask: { kind: "goto" } });
    const prompt = elements(FilesView(p)).find((e) => e.type === TextPrompt)!;
    expect(prompt.props.check).toBe(checkFolder);
    expect(prompt.props.initial).toBe("/data");
    (prompt.props.onSubmit as (v: string) => void)(" /mnt/usb0/games/ ");
    expect(p.onGoTo).toHaveBeenCalledWith("/mnt/usb0/games");
    // From the drive list too.
    const q = props({ ask: { kind: "goto" } });
    expect(elements(FilesView(q)).find((e) => e.type === TextPrompt)!.props.initial).toBe("/");
  });

  it("go to path lists the pinned folders as buttons; ✕ opens one", () => {
    const p = props({ path: "/data", ask: { kind: "goto" } });
    const prompt = elements(FilesView(p)).find((e) => e.type === TextPrompt)!;
    const extra = prompt.props.extra as ReactElement<{ paths: string[]; onOpen: (x: string) => void }>;
    expect(extra.type).toBe(PinnedChoices);
    expect(extra.props.paths).toEqual(PREFS.favorites);
    const buttons = elements(PinnedChoices(extra.props)).filter((e) => e.props["data-goto-pin"]);
    expect(buttons.map((b) => b.props["data-goto-pin"])).toEqual(PREFS.favorites);
    (buttons[1].props.onClick as () => void)();
    expect(p.onGoTo).toHaveBeenCalledWith("/data");
    // Nothing pinned: no list.
    const none = props({ ask: { kind: "goto" }, prefs: EMPTY_PREFS });
    expect(elements(FilesView(none)).find((e) => e.type === TextPrompt)!.props.extra).toBeNull();
  });
});

describe("pinned and recent", () => {
  it("the drive list shows Pinned first and Recent last, each row focusable and opening", () => {
    const html = renderToStaticMarkup(<FilesView {...props()} />);
    const at = (s: string) => html.indexOf(s);
    expect(at(t("cw_files_pinned"))).toBeGreaterThan(-1);
    expect(at(t("cw_files_pinned"))).toBeLessThan(at('data-drive="/data"'));
    expect(at('data-drive="/data"')).toBeLessThan(at(t("cw_files_recent")));
    expect(at('data-place="/mnt/usb0/games"')).toBeLessThan(at('data-place="/data"'));
    expect(html).toContain("中文 game");
    expect(html.split("data-place-main").length - 1).toBe(4);

    const kinds = Array.from(html.matchAll(/data-place-kind="(\w+)"/g)).map((m) => m[1]);
    expect(kinds).toEqual(["pinned", "pinned", "recent", "recent"]);
    // The lists sit in DriveList, handed what Files does with a row.
    const p = props();
    const list = elements(FilesView(p)).find((e) => e.type === DriveList)!;
    expect(list.props.onOpen).toBe(p.onOpen);
    expect(list.props.prefs).toBe(PREFS);
    (list.props.onPlaceActions as (path: string) => void)("/data");
    expect(p.onAsk).toHaveBeenCalledWith({ kind: "place", path: "/data" });
  });

  it("no prefs (an older helper): no Pinned / Recent lists", () => {
    const html = renderToStaticMarkup(<FilesView {...props({ prefs: null })} />);
    expect(html).not.toContain("data-place");
    expect(html).toContain('data-drive="/data"');
    const empty = renderToStaticMarkup(<FilesView {...props({ prefs: EMPTY_PREFS })} />);
    expect(empty).not.toContain(t("cw_files_recent"));
  });

  it("⋯ on a pinned folder offers Open and Unpin; on a recent one Pin", () => {
    const p = props({ ask: { kind: "place", path: "/mnt/usb0/games" } });
    const sheet = elements(FilesView(p)).find((e) => e.type === ActionsSheet)!;
    expect(sheet.props.actions).toEqual(["open", "unpin"]);
    const choose = sheet.props.onChoose as (a: string) => void;
    choose("unpin");
    expect(p.onUnpin).toHaveBeenCalledWith("/mnt/usb0/games");
    choose("open");
    expect(p.onOpen).toHaveBeenCalledWith("/mnt/usb0/games");

    const r = props({ ask: { kind: "place", path: "/mnt/ext1" } });
    const rs = elements(FilesView(r)).find((e) => e.type === ActionsSheet)!;
    expect(rs.props.actions).toEqual(["open", "pin"]);
    (rs.props.onChoose as (a: string) => void)("pin");
    expect(r.onPin).toHaveBeenCalledWith("/mnt/ext1");
  });

  it("a folder can be pinned from the star and from its △ menu", () => {
    const p = props({ path: "/data", ask: { kind: "sheet", names: ["games"] } });
    const sheet = elements(FilesView(p)).find((e) => e.type === ActionsSheet)!;
    expect(sheet.props.actions).toContain("pin");
    (sheet.props.onChoose as (a: string) => void)("pin");
    expect(p.onPin).toHaveBeenCalledWith("/data/games");
    // A file has no Pin; a pinned folder offers Unpin.
    const f = props({ path: "/data", ask: { kind: "sheet", names: ["a.txt"] } });
    expect(elements(FilesView(f)).find((e) => e.type === ActionsSheet)!.props.actions).not.toContain("pin");
    const u = props({ path: "/mnt/usb0", ask: { kind: "sheet", names: ["games"] } });
    expect(elements(FilesView(u)).find((e) => e.type === ActionsSheet)!.props.actions).toContain("unpin");

    const html = renderToStaticMarkup(<FilesView {...props({ path: "/data" })} />);
    expect(html).toMatch(/data-action="pin"[^>]*aria-pressed="true"|aria-pressed="true"[^>]*data-action="pin"/);
    const off = renderToStaticMarkup(<FilesView {...props({ path: "/data/homebrew" })} />);
    expect(off).toMatch(/data-action="pin"[^>]*aria-pressed="false"|aria-pressed="false"[^>]*data-action="pin"/);
  });

  it("prefs from the helper: anything unexpected reads as empty", () => {
    expect(toPrefs(null)).toEqual(EMPTY_PREFS);
    expect(toPrefs({ favorites: ["/a", 3, "rel"], recent: "x", last: 5 })).toEqual({
      favorites: ["/a"],
      recent: [],
      last: null,
    });
    expect(cleanPath("/a//b/")).toBe("/a/b");
    expect(cleanPath("/")).toBe("/");
    expect(isPinned(PREFS, "/data/")).toBe(true);
    expect(isPinned(PREFS, "/data/x")).toBe(false);
    expect(isPinned(null, "/data")).toBe(false);
    expect(placeLabel("/data", VOLS)).toBe(t("cw_home_vol_internal"));
    expect(placeLabel("/data/homebrew", VOLS)).toBe("homebrew");
  });
});

describe("how much is likely to fit", () => {
  const G = 1024 ** 3;
  it("says it for internal storage only, as the app's Volumes does (the PS5 holds back more as data is written)", () => {
    // engine volumes.rs likely_fits_bytes: (allocatable - 1 GiB) / 6 * 5
    expect(likelyFitsBytes(vol("/data", "/dev/ssd0.user", { free_bytes: 163 * G, allocatable_bytes: 161 * G }))).toBe(Math.floor((160 * G) / 6) * 5);
    expect(likelyFitsBytes(vol("/data", "/dev/ssd0.user", { free_bytes: 7 * G }))).toBe(5 * G);
    expect(likelyFitsBytes(vol("/data", "/dev/ssd0.user", { free_bytes: 0 }))).toBeNull();
    expect(likelyFitsBytes(vol("/mnt/usb0", "/dev/da0s1", { free_bytes: 7 * G }))).toBeNull();
    const html = renderToStaticMarkup(<FilesView {...props({ volumes: [vol("/data", "/dev/ssd0.user", { free_bytes: 7 * G })] })} />);
    expect(html).toContain(t("cw_files_likely_fits", { safe: "5.00 GiB" }));
  });
});

describe("eject and unmount", () => {
  it("USB drives have Eject; mounted images Unmount; system volumes neither", () => {
    expect(canEject(vol("/mnt/usb0"))).toBe(true);
    expect(canEject(vol("/mnt/usb8"))).toBe(false);
    expect(canEject(vol("/mnt/ext1", "/dev/da1"))).toBe(false);
    expect(isImage(VOLS[4])).toBe(true);
    for (const p of ["/", "/data", "/data/x", "/user", "/system_ex", "/system", "/preinst", "/mnt/usb0", "/mnt/ext1"])
      expect(isSystemVolume(p)).toBe(true);
    expect(isSystemVolume("/mnt/ps5upload/game.exfat")).toBe(false);
    expect(VOLS.filter(isImage).map((v) => v.path)).toEqual(["/mnt/ps5upload/game.exfat"]);

    const html = renderToStaticMarkup(<FilesView {...props()} />);
    expect(html.split("data-eject=").length - 1).toBe(1);
    expect(html).toContain('data-eject="/mnt/usb0"');
    expect(html.split("data-unmount=").length - 1).toBe(1);
    expect(html).toContain('data-unmount="/mnt/ps5upload/game.exfat"');
    expect(html).toContain("game.exfat");
    expect(html).not.toContain("/system_ex");
  });

  it("eject asks first, then once more to force when the drive is in use", () => {
    const p = props({ ask: { kind: "eject", path: "/mnt/usb0", force: false } });
    const d = elements(FilesView(p)).find((e) => e.type === ConfirmDialog)!;
    expect(d.props.title).toBe(t("cw_files_eject_title", { path: "/mnt/usb0" }));
    expect(d.props.danger).toBe(false);
    (d.props.onConfirm as () => void)();
    expect(p.onEject).toHaveBeenCalledWith("/mnt/usb0", false);

    const f = props({ ask: { kind: "eject", path: "/mnt/usb0", force: true }, askError: "x", askBusy: true });
    const fd = elements(FilesView(f)).find((e) => e.type === ConfirmDialog)!;
    expect(fd.props.title).toBe(t("cw_files_eject_force_title", { path: "/mnt/usb0" }));
    expect(fd.props.danger).toBe(true);
    expect(fd.props.busy).toBe(true);
    expect(fd.props.error).toBe("x");
    (fd.props.onConfirm as () => void)();
    expect(f.onEject).toHaveBeenCalledWith("/mnt/usb0", true);
  });

  it("unmount warns that a running game stops", () => {
    const p = props({ ask: { kind: "unmount", path: "/mnt/ps5upload/game.exfat" } });
    const d = elements(FilesView(p)).find((e) => e.type === ConfirmDialog)!;
    expect(d.props.body).toBe(t("cw_files_unmount_body"));
    expect(d.props.danger).toBe(true);
    (d.props.onConfirm as () => void)();
    expect(p.onUnmount).toHaveBeenCalledWith("/mnt/ps5upload/game.exfat");
  });

  it("unmount is held while a game runs", () => {
    const html = renderToStaticMarkup(<FilesView {...props({ gameRunning: true })} />);
    expect(html).not.toContain("data-unmount=");
    expect(html).toContain('data-unmount-held="/mnt/ps5upload/game.exfat"');
    expect(html).toContain(t("cw_files_unmount_game"));
    const p = props({ gameRunning: true, ask: { kind: "unmount", path: "/mnt/ps5upload/game.exfat" } });
    const d = elements(FilesView(p)).find((e) => e.type === ConfirmDialog)!;
    expect(d.props.busy).toBe(true); // ✕ on Unmount does nothing
    expect(d.props.error).toBe(t("cw_files_unmount_game"));
    expect(volumeErrorText(new ApiError(409, "game_running"))).toBe(t("cw_files_unmount_game"));
  });

  it("eject is held while the page's batch or any transfer uses the drive", () => {
    const job = (over: Partial<Job>): Job => ({
      job: "a".repeat(32),
      kind: "copy",
      state: "running",
      files_done: 0,
      files_total: 1,
      bytes_done: 0,
      bytes_total: 1,
      mine: true,
      ...over,
    });
    const batch = (paths: string[]) => ({ op: "copy" as const, at: 1, total: 2, stopping: false, paths });
    expect(ejectBlocker("/mnt/usb0", null, [])).toBeNull();
    // The page's batch copies onto the drive, or from it.
    expect(ejectBlocker("/mnt/usb0", batch(["/data/a", "/mnt/usb0/games"]), [job({})])).toBe(
      t("cw_files_eject_blocked_batch"),
    );
    expect(ejectBlocker("/mnt/usb0", batch(["/mnt/usb0/a", "/data"]), [])).toBe(t("cw_files_eject_blocked_batch"));
    // A batch elsewhere: its own job does not hold this drive.
    expect(ejectBlocker("/mnt/usb0", batch(["/data/a", "/mnt/ext1"]), [job({})])).toBeNull();
    // A PC upload (where it writes is not listed), or a job a stopped batch left running, holds it.
    expect(ejectBlocker("/mnt/usb0", null, [job({ kind: "upload", mine: false })])).toBe(t("cw_files_eject_blocked_jobs"));
    expect(ejectBlocker("/mnt/usb0", null, [job({})])).toBe(t("cw_files_eject_blocked_jobs"));
    expect(ejectBlocker("/mnt/usb0", null, [job({ state: "done", mine: false })])).toBeNull();
    // "/mnt/usb0" is not inside "/mnt/usb01".
    expect(ejectBlocker("/mnt/usb0", batch(["/mnt/usb01/x"]), [])).toBeNull();
  });

  it("prefs answers arriving out of order: the older one is dropped", async () => {
    const seen: string[][] = [];
    const apply = createPrefsApplier((p) => seen.push(p.recent));
    let first: (v: unknown) => void = () => {};
    mockCall.mockImplementationOnce(() => new Promise((res) => (first = res)) as never);
    mockCall.mockResolvedValueOnce({ favorites: [], recent: ["/b", "/a"], last: "/b" } as never);
    const a = apply("visit", "/a");
    await apply("visit", "/b");
    first({ favorites: [], recent: ["/a"], last: "/a" });
    await a;
    expect(seen).toEqual([["/b", "/a"]]);
    mockCall.mockRejectedValueOnce(new ApiError(409, "prefs_full"));
    await expect(apply("pin", "/c")).rejects.toThrow();
    // A gone recent folder is forgotten through the same route.
    mockCall.mockResolvedValueOnce({ favorites: [], recent: ["/a"], last: "/a" } as never);
    await apply("forget", "/b");
    expect(mockCall).toHaveBeenLastCalledWith("/api/files/prefs/set", { op: "forget", path: "/b" });
    expect(seen[seen.length - 1]).toEqual(["/a"]);
  });

  it("the helper's refusals read as what to do", () => {
    expect(volumeErrorText(new ApiError(409, "usb_eject_busy"))).toBe(t("cw_files_eject_busy"));
    expect(volumeErrorText(new ApiError(409, "fs_unmount_busy"))).toBe(t("cw_files_unmount_busy"));
    expect(volumeErrorText(new ApiError(500, "fs_unmount_not_our_mount"))).toBe(t("cw_files_unmount_not_ours"));
    expect(volumeErrorText(new ApiError(403, "system_volume"))).toBe(t("cw_files_unmount_system"));
    expect(volumeErrorText(new ApiError(409, "prefs_full"))).toBe(t("cw_files_pin_full"));
    expect(volumeErrorText(new ApiError(401, "token"))).toBeNull();
  });
});
