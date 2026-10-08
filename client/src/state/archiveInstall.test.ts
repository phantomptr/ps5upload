import { beforeEach, describe, expect, it, vi } from "vitest";

const mod = vi.hoisted(() => ({
  listRarPackages: vi.fn(),
  installRarPackages: vi.fn(),
}));
vi.mock("./rarPackages", async (orig) => ({
  ...(await orig<typeof import("./rarPackages")>()),
  listRarPackages: mod.listRarPackages,
  installRarPackages: mod.installRarPackages,
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
  useArchiveInstallStore.setState({ byHost: {} });
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
