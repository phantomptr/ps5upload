import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it, vi } from "vitest";
import { ApiError } from "../api";
import { SECTIONS } from "../sections";
import { t } from "../strings";
import { backupPlaces, SavesView, type SavesViewProps } from "./Saves";
import {
  BACKUP_ROOT,
  backUp,
  backupName,
  deleteBackup,
  loadBackups,
  loadSaves,
  loadUsers,
  oneAtATime,
  parseBackupName,
  parseUsers,
  restore,
  saveIconPath,
  usbBackupRoot,
  type OpDeps,
} from "./model";

/** A console's folders as a call() fake: fs.list / mkdir / copy / delete on an in-memory tree, jobs ending at once. */
function fakeConsole(paths: string[]) {
  const files = new Set(paths.filter((p) => !p.endsWith("/")));
  const dirs = new Set<string>();
  const addDirs = (p: string) => {
    for (let at = p; at && at !== "/"; at = at.slice(0, at.lastIndexOf("/")) || "/") dirs.add(at);
  };
  for (const p of paths) addDirs(p.endsWith("/") ? p.slice(0, -1) : p.slice(0, p.lastIndexOf("/")));
  const log: string[] = [];
  const under = (p: string) => (x: string) => x === p || x.startsWith(`${p}/`);
  const call = vi.fn(async (route: string, body?: Record<string, unknown>) => {
    const b = (body ?? {}) as Record<string, string>;
    if (route === "/api/fs/list") {
      if (!dirs.has(b.path)) throw new ApiError(404, "fs_list_not_found");
      const pre = `${b.path}/`;
      const names = new Map<string, string>();
      for (const d of dirs) if (d.startsWith(pre) && !d.slice(pre.length).includes("/")) names.set(d.slice(pre.length), "dir");
      for (const f of files) if (f.startsWith(pre) && !f.slice(pre.length).includes("/")) names.set(f.slice(pre.length), "file");
      return { entries: Array.from(names, ([name, kind]) => ({ name, kind, size: kind === "file" ? 10 : 0 })), more: false };
    }
    log.push(`${route} ${b.path ?? `${b.src} -> ${b.dest}`}${b.mode ? ` ${b.mode}` : ""}`);
    if (route === "/api/fs/mkdir") {
      addDirs(b.path);
      return {};
    }
    if (route === "/api/fs/copy") {
      for (const d of Array.from(dirs).filter(under(b.src))) addDirs(b.dest + d.slice(b.src.length));
      for (const f of Array.from(files).filter(under(b.src))) {
        files.add(b.dest + f.slice(b.src.length));
        addDirs((b.dest + f.slice(b.src.length)).replace(/\/[^/]+$/, ""));
      }
      return { job: "j" };
    }
    if (route === "/api/fs/delete") {
      for (const f of Array.from(files).filter(under(b.path))) files.delete(f);
      for (const d of Array.from(dirs).filter(under(b.path))) dirs.delete(d);
      return { job: "j" };
    }
    if (route === "/api/fs/stat") return { mode: b.path.endsWith("sdimg_SAVEDATA01") ? 0o700 : 0o600 };
    if (route === "/api/fs/chmod") return {};
    if (route === "/api/users") return { foreground: 0x1a2b3c4d, users: [{ id: 0x5e6f7a8b, name: "Sam" }, { id: 0x1a2b3c4d, name: "Alex", foreground: true }] };
    throw new ApiError(404, "not_found");
  });
  return { call: call as unknown as OpDeps["call"], files, dirs, log };
}

const U = "1a2b3c4d";
const HOME = `/user/home/${U}`;
const tree = () =>
  fakeConsole([
    `${HOME}/savedata_prospero/PPSA04609/sdimg_SAVEDATA00`,
    `${HOME}/savedata_prospero/PPSA04609/sdimg_sce_bu_SAVEDATA00`,
    `${HOME}/savedata_prospero/sce_backup/`,
    `${HOME}/savedata_prospero_meta/user/PPSA04609/SAVEDATA00.sfo`,
    `${HOME}/savedata_prospero_meta/user/PPSA99999/SAVEDATA00.sfo`,
    `${HOME}/savedata/CUSA07842/sdimg_SAVE`,
    `${HOME}/savedata/sce_backup4/`,
    `${HOME}/savedata_meta/user/CUSA07842/SAVE.sfo`,
    "/user/home/5e6f7a8b/",
    "/user/home/zzz/",
  ]);
const at = (iso: string): OpDeps["now"] => () => new Date(iso);

describe("save backups: model", () => {
  it("names a backup by its local date and time, and reads the name back", () => {
    const d = new Date(2026, 9, 27, 21, 5, 9);
    expect(backupName(d)).toBe("2026-10-27_21-05-09");
    expect(backupName(d, true)).toBe("2026-10-27_21-05-09_auto");
    expect(parseBackupName("2026-10-27_21-05-09")).toEqual({ name: "2026-10-27_21-05-09", when: d.getTime(), auto: false });
    expect(parseBackupName("2026-10-27_21-05-09_auto")?.auto).toBe(true);
    expect(parseBackupName("../x")).toBeNull();
  });

  it("lists the users with a home folder, named and in the system's order", async () => {
    const c = tree();
    expect(parseUsers({ users: [{ id: 1, name: "A" }] }).get("00000001")?.name).toBe("A");
    const users = await loadUsers(c.call);
    expect(users.map((u) => `${u.id}:${u.name}:${u.foreground}`)).toEqual(["5e6f7a8b:Sam:false", `${U}:Alex:true`]);
  });

  it("lists a user's saved games with their folders, leaving out a meta folder alone and non-title folders", async () => {
    const saves = await loadSaves(U, tree().call);
    expect(saves).toEqual([
      { titleId: "PPSA04609", kinds: ["savedata_prospero", "savedata_prospero_meta"] },
      { titleId: "CUSA07842", kinds: ["savedata", "savedata_meta"] },
    ]);
  });

  it("backs up both PS5 save folders into a dated folder, and lists backups newest first", async () => {
    const c = tree();
    const d: OpDeps = { call: c.call, wait: async () => undefined, now: at("2026-10-27T10:00:00") };
    const name = await backUp(U, "PPSA04609", ["savedata_prospero", "savedata_prospero_meta"], {}, d);
    const dest = `${BACKUP_ROOT}/${U}/PPSA04609/${name}`;
    expect(c.files.has(`${dest}/savedata_prospero/sdimg_SAVEDATA00`)).toBe(true);
    expect(c.files.has(`${dest}/savedata_prospero_meta/SAVEDATA00.sfo`)).toBe(true);
    // a second one in the same second gets the next second's name
    const second = await backUp(U, "PPSA04609", ["savedata_prospero"], {}, d);
    expect(second).not.toBe(name);
    const list = await loadBackups(U, "PPSA04609", c.call);
    expect(list.map((b) => b.name)).toEqual([second, name]);
  });

  it("removes a half-made backup when a copy fails", async () => {
    const c = tree();
    const d: OpDeps = {
      call: c.call,
      wait: async () => {
        throw new ApiError(500, "fs_copy_no_space");
      },
      now: at("2026-10-27T10:00:00"),
    };
    await expect(backUp(U, "PPSA04609", ["savedata_prospero"], {}, d)).rejects.toThrow("fs_copy_no_space");
    expect(await loadBackups(U, "PPSA04609", c.call)).toEqual([]);
  });

  it("restores: backs up the current save first, empties each live folder and copies the backup's files in", async () => {
    const c = tree();
    const d: OpDeps = { call: c.call, wait: async () => undefined, now: at("2026-10-27T10:00:00") };
    const old = await backUp(U, "PPSA04609", ["savedata_prospero", "savedata_prospero_meta"], {}, d);
    // the game saved again since: a new image replaces the old one
    c.files.delete(`${HOME}/savedata_prospero/PPSA04609/sdimg_SAVEDATA00`);
    c.files.add(`${HOME}/savedata_prospero/PPSA04609/sdimg_SAVEDATA01`);
    c.log.length = 0;
    const later: OpDeps = { ...d, now: at("2026-10-27T11:00:00") };
    const safety = await restore(U, "PPSA04609", old, ["savedata_prospero", "savedata_prospero_meta"], later);
    expect(safety).toMatch(/_auto$/);
    expect(c.files.has(`${HOME}/savedata_prospero/PPSA04609/sdimg_SAVEDATA00`)).toBe(true);
    expect(c.files.has(`${HOME}/savedata_prospero/PPSA04609/sdimg_SAVEDATA01`)).toBe(false);
    expect(c.files.has(`${BACKUP_ROOT}/${U}/PPSA04609/${safety}/savedata_prospero/sdimg_SAVEDATA01`)).toBe(true);
    // the restored files get the system's mode back (a copy comes out 0777)
    expect(c.log).toContain(`/api/fs/chmod ${HOME}/savedata_prospero/PPSA04609/sdimg_SAVEDATA00 0600`);
    // the live title folder is never deleted itself, only what is in it
    expect(c.log).not.toContain(`/api/fs/delete ${HOME}/savedata_prospero/PPSA04609`);
    expect(c.dirs.has(`${HOME}/savedata_prospero/PPSA04609`)).toBe(true);
    // the safety copy comes before anything live is touched
    const firstLive = c.log.findIndex((l) => l.includes(`delete ${HOME}`));
    const safetyCopy = c.log.findIndex((l) => l.includes(`${safety}/savedata_prospero`));
    expect(safetyCopy).toBeGreaterThanOrEqual(0);
    expect(safetyCopy).toBeLessThan(firstLive);
  });

  it("restores a backup without the meta folder as it was: the newer meta files go, kept in the safety backup", async () => {
    const c = tree();
    const d: OpDeps = { call: c.call, wait: async () => undefined, now: at("2026-10-27T10:00:00") };
    const old = await backUp(U, "PPSA04609", ["savedata_prospero"], {}, d);
    const later: OpDeps = { ...d, now: at("2026-10-27T11:00:00") };
    const safety = await restore(U, "PPSA04609", old, ["savedata_prospero", "savedata_prospero_meta"], later);
    expect(c.files.has(`${HOME}/savedata_prospero_meta/user/PPSA04609/SAVEDATA00.sfo`)).toBe(false);
    expect(c.dirs.has(`${HOME}/savedata_prospero_meta/user/PPSA04609`)).toBe(true);
    expect(c.files.has(`${BACKUP_ROOT}/${U}/PPSA04609/${safety}/savedata_prospero_meta/SAVEDATA00.sfo`)).toBe(true);
    // another game's meta is untouched
    expect(c.files.has(`${HOME}/savedata_prospero_meta/user/PPSA99999/SAVEDATA00.sfo`)).toBe(true);
  });

  it("finds a save's own icon for a game no longer installed, the system's sce_bu_ copy last", async () => {
    const c = fakeConsole([`${HOME}/savedata_prospero_meta/user/PPSA25411/sce_bu_SYS_icon0.png`, `${HOME}/savedata_prospero_meta/user/PPSA25411/SYS_icon0.png`]);
    expect(await saveIconPath(U, { titleId: "PPSA25411", kinds: ["savedata_prospero", "savedata_prospero_meta"] }, c.call)).toBe(
      `${HOME}/savedata_prospero_meta/user/PPSA25411/SYS_icon0.png`,
    );
    expect(await saveIconPath(U, { titleId: "PPSA25411", kinds: ["savedata_prospero"] }, c.call)).toBeNull();
  });

  it("deletes only a backup folder by its stamp", async () => {
    const c = tree();
    const d: OpDeps = { call: c.call, wait: async () => undefined, now: at("2026-10-27T10:00:00") };
    const name = await backUp(U, "CUSA07842", ["savedata"], {}, d);
    await deleteBackup(U, "CUSA07842", name, d);
    expect(await loadBackups(U, "CUSA07842", c.call)).toEqual([]);
    await expect(deleteBackup(U, "CUSA07842", "..", d)).rejects.toThrow("bad_backup");
  });

  it("backs up to a USB drive and restores from it, keeping the safety backup on the internal drive (#528)", async () => {
    const c = tree();
    const usb = usbBackupRoot("/mnt/usb0");
    expect(usb).toBe("/mnt/usb0/ps5upload/save-backups");
    const d: OpDeps = { call: c.call, wait: async () => undefined, now: at("2026-10-28T09:00:00") };
    const name = await backUp(U, "PPSA04609", ["savedata_prospero", "savedata_prospero_meta"], { root: usb }, d);
    expect(c.files.has(`${usb}/${U}/PPSA04609/${name}/savedata_prospero/sdimg_SAVEDATA00`)).toBe(true);
    expect(await loadBackups(U, "PPSA04609", c.call, usb)).toEqual([expect.objectContaining({ name })]);
    expect(await loadBackups(U, "PPSA04609", c.call)).toEqual([]); // nothing on the internal drive

    d.now = at("2026-10-28T09:30:00");
    const safety = await restore(U, "PPSA04609", name, ["savedata_prospero", "savedata_prospero_meta"], d, usb);
    expect(safety).toBe("2026-10-28_09-30-00_auto");
    expect((await loadBackups(U, "PPSA04609", c.call)).map((b) => b.name)).toEqual([safety]);
    expect(c.files.has(`${HOME}/savedata_prospero/PPSA04609/sdimg_SAVEDATA00`)).toBe(true);

    await deleteBackup(U, "PPSA04609", name, d, usb);
    expect(await loadBackups(U, "PPSA04609", c.call, usb)).toEqual([]);
  });

  it("offers each writable USB drive as a backup place after the internal drive", () => {
    const vol = (path: string, from: string, writable = true) => ({ path, mount_from: from, fs_type: "exfatfs", total_bytes: 1, free_bytes: 1, writable });
    const places = backupPlaces([
      vol("/data", "/dev/ssd0.user"),
      vol("/mnt/usb0", "/dev/da0s1"),
      vol("/mnt/usb1", "/dev/da1s1", false),
      vol("/mnt/ext0", "/dev/nvme0"),
      vol("/mnt/ext1", "/dev/da2"),
      vol("/mnt/ps5upload/game", "/dev/lvd2"),
    ]);
    expect(places.map((p) => p.root)).toEqual([BACKUP_ROOT, "/mnt/usb0/ps5upload/save-backups", "/mnt/ext1/ps5upload/save-backups"]);
    expect(places[1].label).toContain("usb0");
  });
});

const backups = [
  { name: "2026-10-25_09-02-44", when: new Date(2026, 9, 25, 9, 2, 44).getTime(), auto: false },
  { name: "2026-10-20_21-14-03_auto", when: new Date(2026, 9, 20, 21, 14, 3).getTime(), auto: true },
];
const view = (over: Partial<SavesViewProps> = {}): SavesViewProps => ({
  users: [
    { id: U, name: "Alex", foreground: true },
    { id: "5e6f7a8b", name: "Sam", foreground: false },
  ],
  user: U,
  saves: [{ titleId: "PPSA04609", kinds: ["savedata_prospero", "savedata_prospero_meta"] }],
  names: new Map([["PPSA04609", "Hogwarts Legacy"]]),
  icons: new Map(),
  backups: new Map([["PPSA04609", backups]]),
  sizes: new Map(),
  open: null,
  task: null,
  message: null,
  loadError: null,
  running: null,
  ask: null,
  askError: null,
  places: backupPlaces([]),
  place: BACKUP_ROOT,
  onPlace: vi.fn(),
  onUser: vi.fn(),
  onOpen: vi.fn(),
  onRefresh: vi.fn(),
  onBackUp: vi.fn(),
  onAsk: vi.fn(),
  onConfirm: vi.fn(),
  onCloseGame: vi.fn(),
  ...over,
});
const html = (over: Partial<SavesViewProps> = {}) => renderToStaticMarkup(<SavesView {...view(over)} />);

describe("Saves section", () => {
  it("is in the rail after Games, and Games can open it at a game", () => {
    const ids = SECTIONS.map((s) => s.id);
    expect(ids.indexOf("saves")).toBe(ids.indexOf("games") + 1);
    expect(SECTIONS.find((s) => s.id === "saves")?.acceptsParams).toBe(true);
  });

  it("shows each user and their saved games with how many backups each has", () => {
    const h = html();
    expect(h).toContain("Alex");
    expect(h).toContain('aria-pressed="true"');
    expect(h).toContain("Hogwarts Legacy");
    expect(h).toContain(t("cw_saves_backups_n", { n: 2 }));
  });

  it("opens a game with Back up now and each backup's Restore and Delete; marks the one made before a restore", () => {
    const h = html({ open: "PPSA04609" });
    expect(h).toContain(t("cw_saves_back_up"));
    expect(h.match(/data-action="save-restore"/g)?.length).toBe(2);
    expect(h.match(/data-action="save-delete"/g)?.length).toBe(2);
    expect(h).toContain(t("cw_saves_auto"));
  });

  it("will not restore while the game runs, and offers to close it", () => {
    const h = html({ open: "PPSA04609", running: { titleId: "PPSA04609", appId: 7 } });
    expect(h).toContain(t("cw_saves_running", { name: "Hogwarts Legacy" }));
    expect(h).toContain('data-action="save-close-game"');
    expect(h).toMatch(/data-action="save-restore" disabled/);
  });

  it("asks before a restore, naming the game, the user and the backup's time", () => {
    const h = html({ open: "PPSA04609", ask: { kind: "restore", backup: backups[0] } });
    expect(h).toContain(t("cw_saves_restore_title"));
    expect(h).toContain("Hogwarts Legacy");
    expect(h).toContain(new Date(backups[0].when).toLocaleString());
  });

  it("says when a user has no saves", () => {
    expect(html({ saves: [] })).toContain(t("cw_saves_none", { name: "Alex" }));
  });

  it("shows the backup place chooser only when a USB drive is there, and says where backups are", () => {
    expect(html()).not.toContain("data-place=");
    const usb = usbBackupRoot("/mnt/usb0");
    const places = [...backupPlaces([]), { root: usb, label: "USB drive (usb0)" }];
    const out = html({ places, place: usb, open: "PPSA04609" });
    expect(out).toContain(`data-place="${usb}"`);
    expect(out).toMatch(new RegExp(`aria-pressed="true"[^>]*data-place="${usb}"`));
    expect(out).toContain(t("cw_saves_where_usb", { path: `${usb}/${U}/PPSA04609` }));
  });
});

describe("oneAtATime: Confirm pressed twice", () => {
  it("runs the first press and refuses the second until the first has ended", async () => {
    const gate = oneAtATime();
    let finish: (v: string) => void = () => undefined;
    // A restore that is still asking the console whether the game runs.
    const restoreOnce = vi.fn(() => new Promise<string>((r) => (finish = r)));
    const first = gate(restoreOnce);
    const second = gate(restoreOnce);
    expect(await second).toBeUndefined();
    expect(restoreOnce).toHaveBeenCalledTimes(1);
    finish("done");
    expect(await first).toBe("done");
    // Free again afterwards.
    void gate(restoreOnce);
    expect(restoreOnce).toHaveBeenCalledTimes(2);
  });

  it("is free again after a run that failed", async () => {
    const gate = oneAtATime();
    await expect(gate(() => Promise.reject(new Error("no")))).rejects.toThrow("no");
    expect(await gate(() => Promise.resolve(1))).toBe(1);
  });
});
