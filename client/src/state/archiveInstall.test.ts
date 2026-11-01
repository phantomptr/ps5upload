import { beforeEach, describe, expect, it, vi } from "vitest";

const mod = vi.hoisted(() => ({
  listRarPackages: vi.fn(),
  installRarPackages: vi.fn(),
  planArchiveTarget: vi.fn(),
  findArchiveLeftovers: vi.fn(),
  removeConsoleFolder: vi.fn(),
}));
vi.mock("./rarPackages", async (orig) => ({
  ...(await orig<typeof import("./rarPackages")>()),
  listRarPackages: mod.listRarPackages,
  installRarPackages: mod.installRarPackages,
  planArchiveTarget: mod.planArchiveTarget,
  findArchiveLeftovers: mod.findArchiveLeftovers,
  removeConsoleFolder: mod.removeConsoleFolder,
}));
vi.mock("../api/links", () => ({
  rarPackages: vi.fn(),
  pkgConsoleProbe: vi.fn(),
}));
vi.mock("../api/ps5", () => ({}));
vi.mock("./tasks", () => ({
  useTaskStore: {
    getState: () => ({
      registerTask: () => "t1",
      updateTask: () => {},
      finishTask: () => {},
    }),
  },
}));
vi.mock("./pkgLibrary", () => ({
  pkgLibraryStore: () => ({ getState: () => ({}) }),
}));

import {
  archiveInstallFor,
  archiveLeftoversFor,
  leftoverInUse,
  queuedInstallPaths,
  removeArchiveLeftovers,
  scanArchiveLeftovers,
  downloadArchiveParts,
  parseArchiveLinks,
  stopArchiveDownload,
  type ArchiveLinkDeps,
  chooseArchive,
  clearArchiveInstall,
  runArchiveInstall,
  submitArchivePassword,
  useArchiveInstallStore,
} from "./archiveInstall";

const A = "10.0.0.5";
const B = "10.0.0.6";
const at = (h: string) =>
  archiveInstallFor(useArchiveInstallStore.getState(), h);

beforeEach(() => {
  vi.clearAllMocks();
  useArchiveInstallStore.setState({ byHost: {}, leftoversByHost: {} });
  mod.planArchiveTarget.mockResolvedValue(null);
});

describe("archive install, kept outside the screen", () => {
  it("lists what a chosen archive holds, per console", async () => {
    mod.listRarPackages.mockResolvedValue({
      packages: [{ path: "a.pkg", size: 5 }],
    });
    await chooseArchive(A, "/x/Bundle.zip");
    expect(at(A)).toMatchObject({
      archive: "/x/Bundle.zip",
      packages: [{ path: "a.pkg", size: 5 }],
    });
    expect(at(B)).toBeNull();
  });

  it("refuses a later RAR part and a file that is no archive, before reading anything", async () => {
    await chooseArchive(A, "/x/a.part2.rar");
    expect(at(A)?.error).toBe("not_first");
    await chooseArchive(A, "/x/a.pkg");
    expect(at(A)?.error).toBe("not_archive");
    expect(mod.listRarPackages).not.toHaveBeenCalled();
  });

  it("asks for a password, then lists with it", async () => {
    mod.listRarPackages.mockResolvedValueOnce({
      packages: [],
      password: "required",
      error: "x",
    });
    await chooseArchive(A, "/x/a.rar");
    expect(at(A)).toMatchObject({
      passwordProblem: "required",
      packages: null,
    });
    mod.listRarPackages.mockResolvedValueOnce({
      packages: [{ path: "a.pkg", size: 1 }],
    });
    await submitArchivePassword(A, "secret");
    expect(mod.listRarPackages).toHaveBeenLastCalledWith("/x/a.rar", "secret");
    expect(at(A)).toMatchObject({ passwordProblem: null, password: "secret" });
    expect(at(A)?.packages).toHaveLength(1);
  });

  it("runs with its phase in the store, and keeps the outcome when it ends", async () => {
    mod.listRarPackages.mockResolvedValue({
      packages: [{ path: "a.pkg", size: 1 }],
    });
    await chooseArchive(A, "/x/a.zip");
    let release: (v: unknown) => void = () => {};
    mod.installRarPackages.mockImplementation(
      async (opts: { onPhase?: (p: unknown) => void }) => {
        opts.onPhase?.({ phase: "unpacking", sent: 10, total: 100 });
        await new Promise((r) => (release = r));
        return {
          ok: true,
          dest: "/d",
          outcomes: [],
          message: "Installed 1 package from the archive.",
        };
      },
    );
    const run = runArchiveInstall(A);
    await Promise.resolve();
    await Promise.resolve();
    expect(at(A)).toMatchObject({
      busy: true,
      phase: { phase: "unpacking", sent: 10, total: 100 },
    });
    // A second start while it runs does nothing.
    await runArchiveInstall(A);
    expect(mod.installRarPackages).toHaveBeenCalledTimes(1);
    release(null);
    await run;
    expect(at(A)).toMatchObject({
      busy: false,
      phase: null,
      result: { ok: true },
    });
  });

  it("clear forgets a finished run and leaves a running one alone", async () => {
    mod.listRarPackages.mockResolvedValue({ packages: [] });
    await chooseArchive(A, "/x/a.zip");
    clearArchiveInstall(A);
    expect(at(A)).toBeNull();
    useArchiveInstallStore.setState({
      byHost: { [A]: { ...blank(), busy: true } },
    });
    clearArchiveInstall(A);
    expect(at(A)?.busy).toBe(true);
  });
});

function blank() {
  return {
    archive: "/x/a.zip",
    packages: null,
    target: null,
    password: null,
    passwordProblem: null,
    inspecting: false,
    downloading: null,
    downloaded: [],
    busy: false,
    phase: null,
    result: null,
    error: null,
  };
}

describe("archive parts from download links", () => {
  it("reads one link per line, ignoring blanks, duplicates and things that are not links", () => {
    expect(
      parseArchiveLinks(
        " https://x/a.part1.rar \n\nhttps://x/a.part2.rar\nhttps://x/a.part1.rar\nnot a link\nftp://x/y.rar\n",
      ),
    ).toEqual(["https://x/a.part1.rar", "https://x/a.part2.rar"]);
  });

  function linkWorld(over: Partial<ArchiveLinkDeps> = {}) {
    const log: string[] = [];
    const deps: ArchiveLinkDeps = {
      start: async (url) => {
        log.push(`start ${url}`);
        const name = url.split("/").pop() ?? "f";
        return { download_id: name, path: `/dl/${name}`, total: 100 };
      },
      status: async (id) => ({
        written: 100,
        total: 100,
        done: true,
        cancelled: false,
        error: null,
        id,
      }),
      cancel: async (id) => void log.push(`cancel ${id}`),
      sleep: async () => {},
      ...over,
    };
    return { deps, log };
  }

  it("downloads every part in order, then opens the first part as the archive", async () => {
    mod.listRarPackages.mockResolvedValue({
      packages: [{ path: "a.pkg", size: 5 }],
    });
    const w = linkWorld();
    await downloadArchiveParts(
      A,
      ["https://x/a.part2.rar", "https://x/a.part1.rar"],
      false,
      w.deps,
    );
    expect(w.log).toEqual([
      "start https://x/a.part2.rar",
      "start https://x/a.part1.rar",
    ]);
    expect(at(A)).toMatchObject({
      archive: "/dl/a.part1.rar",
      downloading: null,
    });
    expect(at(A)?.packages).toHaveLength(1);
    expect(at(A)?.downloaded).toEqual(["/dl/a.part2.rar", "/dl/a.part1.rar"]);
  });

  it("shows which part is downloading and how far it is", async () => {
    const seen: string[] = [];
    let polls = 0;
    const w = linkWorld({
      status: async () => {
        polls += 1;
        return polls < 2
          ? {
              written: 40,
              total: 100,
              done: false,
              cancelled: false,
              error: null,
            }
          : {
              written: 100,
              total: 100,
              done: true,
              cancelled: false,
              error: null,
            };
      },
      sleep: async () => {
        const d = at(A)?.downloading;
        if (d) seen.push(`${d.index}/${d.count} ${d.written}/${d.total}`);
      },
    });
    mod.listRarPackages.mockResolvedValue({ packages: [] });
    await downloadArchiveParts(A, ["https://x/a.zip"], false, w.deps);
    expect(seen).toContain("1/1 40/100");
  });

  it("stops at the part that fails and says which, keeping what it has", async () => {
    const w = linkWorld({
      status: async (id) =>
        id === "a.part2.rar"
          ? {
              written: 10,
              total: 100,
              done: false,
              cancelled: false,
              error: "404 Not Found",
            }
          : {
              written: 100,
              total: 100,
              done: true,
              cancelled: false,
              error: null,
            },
    });
    await downloadArchiveParts(
      A,
      [
        "https://x/a.part1.rar",
        "https://x/a.part2.rar",
        "https://x/a.part3.rar",
      ],
      false,
      w.deps,
    );
    expect(at(A)?.error).toContain("404 Not Found");
    expect(at(A)?.error).toContain("part 2 of 3");
    expect(w.log).not.toContain("start https://x/a.part3.rar");
    expect(mod.listRarPackages).not.toHaveBeenCalled();
  });

  it("can be stopped: the part being fetched is cancelled and nothing is opened", async () => {
    const w = linkWorld({
      status: async () => ({
        written: 1,
        total: 100,
        done: false,
        cancelled: false,
        error: null,
      }),
      sleep: async () => stopArchiveDownload(A),
    });
    await downloadArchiveParts(A, ["https://x/a.part1.rar"], false, w.deps);
    expect(w.log).toContain("cancel a.part1.rar");
    expect(at(A)?.downloading).toBeNull();
    expect(mod.listRarPackages).not.toHaveBeenCalled();
  });

  it("says so when none of the files is the first part of an archive", async () => {
    const w = linkWorld();
    await downloadArchiveParts(A, ["https://x/a.part2.rar"], false, w.deps);
    expect(at(A)?.error).toBe("no_first_part");
  });
});

describe("where the packages go, and folders left behind", () => {
  const target = {
    dest: "/user/data/ps5upload/pkg_library/zip_a",
    already: 0,
    space: { kind: "fits", need: 5, room: 100, drive: "/user" },
  };

  it("works out the folder and the space once the archive is listed", async () => {
    mod.listRarPackages.mockResolvedValue({ packages: [{ path: "a.pkg", size: 5 }] });
    mod.planArchiveTarget.mockResolvedValue(target);
    await chooseArchive(A, "/x/a.zip");
    expect(at(A)?.target).toEqual(target);
    expect(mod.planArchiveTarget).toHaveBeenCalledWith(A, "/x/a.zip", [
      { path: "a.pkg", size: 5 },
    ]);
  });

  it("does not plan for an archive with no packages", async () => {
    mod.listRarPackages.mockResolvedValue({ packages: [] });
    await chooseArchive(A, "/x/a.zip");
    expect(mod.planArchiveTarget).not.toHaveBeenCalled();
    expect(at(A)?.target).toBeNull();
  });

  it("lists only folders no queued install and no running run still reads", async () => {
    const items = [
      { status: "pending", addr: `${A}:9113`, install: { via: "console-path", path: "/L/rar_q/x.pkg" } },
      { status: "failed", addr: `${A}:9113`, install: { via: "console-path", path: "/L/rar_f/x.pkg" } },
      { status: "running", addr: `${B}:9113`, install: { via: "console-path", path: "/L/rar_b/x.pkg" } },
    ] as never;
    const queued = queuedInstallPaths(items, A);
    expect(queued).toEqual(["/L/rar_q/x.pkg"]);
    expect(leftoverInUse("/L/rar_q", A, queued)).toBe(true);
    expect(leftoverInUse("/L/rar_f", A, queued)).toBe(false);
    expect(leftoverInUse("/L/rar_qq", A, queued)).toBe(false);
    useArchiveInstallStore.setState({
      byHost: { [A]: { ...blank(), busy: true, target: { ...target, dest: "/L/rar_run" } as never } },
    });
    expect(leftoverInUse("/L/rar_run", A, [])).toBe(true);
    expect(leftoverInUse("/L/rar_old", A, [])).toBe(false);
  });

  it("removes the leftovers, keeping the ones it could not delete", async () => {
    mod.findArchiveLeftovers.mockResolvedValue([
      { path: "/L/rar_a", bytes: 10 },
      { path: "/L/rar_b", bytes: 20 },
    ]);
    await scanArchiveLeftovers(A, () => []);
    expect(archiveLeftoversFor(useArchiveInstallStore.getState(), A)?.items).toHaveLength(2);
    mod.removeConsoleFolder.mockImplementation(async (_h: string, p: string) => {
      if (p === "/L/rar_b") throw new Error("busy");
      return true;
    });
    await removeArchiveLeftovers(A, () => []);
    const left = archiveLeftoversFor(useArchiveInstallStore.getState(), A);
    expect(left?.items).toEqual([{ path: "/L/rar_b", bytes: 20 }]);
    expect(left?.error).toContain("/L/rar_b");
    expect(left?.removing).toBe(false);
  });

  it("never removes a folder that became in use after the scan", async () => {
    mod.findArchiveLeftovers.mockResolvedValue([{ path: "/L/rar_a", bytes: 10 }]);
    await scanArchiveLeftovers(A, () => []);
    await removeArchiveLeftovers(A, () => ["/L/rar_a/x.pkg"]);
    expect(mod.removeConsoleFolder).not.toHaveBeenCalled();
  });

  it("asks again before each try: a folder queued during the removal is kept", async () => {
    mod.findArchiveLeftovers.mockResolvedValue([{ path: "/L/rar_a", bytes: 10 }]);
    await scanArchiveLeftovers(A, () => []);
    let queued: string[] = [];
    mod.removeConsoleFolder.mockImplementation(
      async (_h: string, p: string, _w: unknown, mayDelete: () => boolean) => {
        // A first try failed and, meanwhile, an install of a file in it was queued.
        queued = [`${p}/x.pkg`];
        return mayDelete();
      },
    );
    await removeArchiveLeftovers(A, () => queued);
    const left = archiveLeftoversFor(useArchiveInstallStore.getState(), A);
    expect(left?.items).toEqual([{ path: "/L/rar_a", bytes: 10 }]);
    expect(left?.error).toBeNull();
  });

  it("starts no run while leftovers are being removed", async () => {
    useArchiveInstallStore.setState({
      byHost: { [A]: { ...blank(), packages: [{ path: "a.pkg", size: 5 }] as never } },
      leftoversByHost: { [A]: { items: [], scanning: false, removing: true, error: null } },
    });
    await runArchiveInstall(A);
    expect(mod.installRarPackages).not.toHaveBeenCalled();
    expect(at(A)?.busy).toBe(false);
  });
});
