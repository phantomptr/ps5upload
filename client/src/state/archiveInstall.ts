import { create } from "zustand";

import type { RarPackage } from "../api/links";
import { hostOf } from "../lib/addr";
import type { RarPasswordProblem } from "../lib/rarPassword";
import { useTaskStore } from "./tasks";
import {
  archiveKindOf,
  installRarPackages,
  isArchiveFirstVolume,
  listRarPackages,
  type RarInstallResult,
  type RarPhase,
} from "./rarPackages";

/**
 * "Install the packages inside an archive", kept outside the Install screen.
 *
 * The chosen archive, what it holds, the unpack's progress and the outcome used to be the
 * card's own state: leaving the screen long enough dropped them while the unpack and the
 * installs carried on unseen. Here they belong to the console, so the card shows the same
 * run whenever it is looked at.
 */
export interface ArchiveInstallState {
  archive: string;
  /** null until listed (or while a password is needed). */
  packages: RarPackage[] | null;
  password: string | null;
  passwordProblem: RarPasswordProblem | null;
  inspecting: boolean;
  /** The archive's parts are being downloaded from links: which part, and how far. */
  downloading: {
    index: number;
    count: number;
    written: number;
    total: number;
    stopRequested: boolean;
  } | null;
  /** Where the downloaded parts are on this computer (so the user can find or delete them). */
  downloaded: string[];
  busy: boolean;
  phase: RarPhase | null;
  result: RarInstallResult | null;
  /** A raw error, or `not_first` / `not_archive` / `no_first_part` for those the screen
   *  words itself. */
  error: string | null;
}

interface Store {
  byHost: Record<string, ArchiveInstallState>;
}

export const useArchiveInstallStore = create<Store>(() => ({ byHost: {} }));

export function archiveInstallFor(
  s: Store,
  host: string,
): ArchiveInstallState | null {
  return s.byHost[hostOf(host)] ?? null;
}

function put(host: string, patch: Partial<ArchiveInstallState>) {
  const key = hostOf(host);
  useArchiveInstallStore.setState((s) => {
    const cur = s.byHost[key];
    if (!cur) return s;
    return { byHost: { ...s.byHost, [key]: { ...cur, ...patch } } };
  });
}

const fresh = (archive: string): ArchiveInstallState => ({
  archive,
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
});

async function inspect(host: string, path: string, password: string | null) {
  put(host, { inspecting: true, error: null, result: null });
  const r = await listRarPackages(path, password);
  // The user chose another archive (or cleared) while this one was being read.
  if (
    archiveInstallFor(useArchiveInstallStore.getState(), host)?.archive !== path
  )
    return;
  if (r.password) {
    put(host, {
      inspecting: false,
      passwordProblem: r.password,
      packages: null,
    });
    return;
  }
  if (r.error) {
    put(host, {
      inspecting: false,
      passwordProblem: null,
      packages: null,
      error: r.error,
    });
    return;
  }
  put(host, { inspecting: false, passwordProblem: null, packages: r.packages });
}

/** The user picked `path`: remember it for this console and read what it holds. */
export async function chooseArchive(host: string, path: string): Promise<void> {
  const key = hostOf(host);
  const held = archiveInstallFor(useArchiveInstallStore.getState(), host);
  // Not over a run or a download of parts that is still going (its own last step, opening
  // the first part, arrives here with `downloading` already at its last part).
  if (held?.busy) return;
  const state = fresh(path);
  const set = (s: ArchiveInstallState) =>
    useArchiveInstallStore.setState((st) => ({
      byHost: { ...st.byHost, [key]: s },
    }));
  if (archiveKindOf(path) === null)
    return set({ ...state, error: "not_archive" });
  if (!isArchiveFirstVolume(path)) return set({ ...state, error: "not_first" });
  set(state);
  await inspect(host, path, null);
}

export async function submitArchivePassword(
  host: string,
  password: string,
): Promise<void> {
  const cur = archiveInstallFor(useArchiveInstallStore.getState(), host);
  if (!cur || cur.busy) return;
  put(host, { password });
  await inspect(host, cur.archive, password);
}

/** The Tasks line for a phase. English like the other task details, which are not translated. */
function phaseDetail(p: RarPhase): string {
  switch (p.phase) {
    case "listing":
      return "Reading the archive";
    case "unpacking":
      return "Unpacking the packages to the PS5";
    case "reading":
      return `Checking package ${p.index} of ${p.count}`;
    case "installing":
      return `Installing ${p.index} of ${p.count}: ${p.name}`;
  }
}

/** Unpacks the chosen archive's packages to the console and installs each. Resolves when the
 *  run ends; a run already going is left alone. */
export async function runArchiveInstall(host: string): Promise<void> {
  const cur = archiveInstallFor(useArchiveInstallStore.getState(), host);
  if (!cur || cur.busy) return;
  put(host, {
    busy: true,
    result: null,
    error: null,
    phase: { phase: "listing" },
  });
  // In Tasks as well, so the run is visible (and its end is told) from any screen.
  const name = cur.archive.replace(/\\/g, "/").split("/").pop() ?? cur.archive;
  const tasks = useTaskStore.getState();
  const taskId = tasks.registerTask({
    kind: "upload-archive",
    origin: "pkg.archive",
    label: `Packages from ${name}`,
    consoleId: host,
    status: "running",
  });
  try {
    const r = await installRarPackages({
      host,
      archivePath: cur.archive,
      password: cur.password,
      onPhase: (phase) => {
        put(host, { phase });
        useTaskStore.getState().updateTask(taskId, {
          detail: phaseDetail(phase),
          ...(phase.phase === "unpacking" && phase.total > 0
            ? {
                progress: {
                  current: phase.sent,
                  total: phase.total,
                  unit: "bytes" as const,
                },
              }
            : {}),
        });
      },
    });
    put(host, {
      result: r,
      ...(r.password ? { passwordProblem: r.password } : {}),
    });
    useTaskStore
      .getState()
      .finishTask(taskId, r.ok ? "done" : "failed", { detail: r.message });
  } catch (e) {
    const message = e instanceof Error ? e.message : String(e);
    put(host, { error: message });
    useTaskStore.getState().finishTask(taskId, "failed", { detail: message });
  } finally {
    put(host, { busy: false, phase: null });
  }
}

/** Forgets this console's archive (the user is done with it). A running one stays. */
export function clearArchiveInstall(host: string): void {
  const key = hostOf(host);
  useArchiveInstallStore.setState((s) => {
    if (!s.byHost[key] || s.byHost[key].busy) return s;
    const rest = { ...s.byHost };
    delete rest[key];
    return { byHost: rest };
  });
}

// ── An archive whose parts are behind download links ─────────────────────────────────────

/** The links in a pasted block: one per line (or separated by spaces), http(s) only, each
 *  once, in the order given. */
export function parseArchiveLinks(text: string): string[] {
  const out: string[] = [];
  for (const raw of text.split(/\s+/)) {
    const link = raw.trim();
    if (!/^https?:\/\/\S+$/i.test(link) || out.includes(link)) continue;
    out.push(link);
  }
  return out;
}

interface LinkStatus {
  written?: number;
  total?: number;
  done?: boolean;
  cancelled?: boolean;
  error?: string | null;
}

/** What downloading the parts needs from the app. Injected so it is testable. */
export interface ArchiveLinkDeps {
  /** Starts one link downloading to this computer, keeping the file's own name. */
  start: (
    url: string,
    insecureTls: boolean,
  ) => Promise<{ download_id?: string; path?: string; total?: number }>;
  status: (id: string) => Promise<LinkStatus>;
  cancel: (id: string) => Promise<unknown>;
  sleep: (ms: number) => Promise<void>;
}

/**
 * Downloads an archive's parts from `links` to this computer, one after another, then opens
 * the first part as this console's archive (which lists its packages, ready to install).
 *
 * A multi-part RAR is one archive in several files; each link is one file. They are saved
 * side by side under their own names, which is how the parts find each other.
 */
export async function downloadArchiveParts(
  host: string,
  links: string[],
  insecureTls: boolean,
  deps: ArchiveLinkDeps,
): Promise<void> {
  const key = hostOf(host);
  const cur = archiveInstallFor(useArchiveInstallStore.getState(), host);
  if (cur?.busy || cur?.downloading || links.length === 0) return;
  useArchiveInstallStore.setState((st) => ({
    byHost: {
      ...st.byHost,
      [key]: {
        ...fresh(""),
        downloading: {
          index: 1,
          count: links.length,
          written: 0,
          total: 0,
          stopRequested: false,
        },
      },
    },
  }));
  const paths: string[] = [];
  const fail = (error: string) =>
    put(host, { downloading: null, downloaded: paths, error });
  const stopped = () =>
    archiveInstallFor(useArchiveInstallStore.getState(), host)?.downloading
      ?.stopRequested ?? true;

  for (const [i, url] of links.entries()) {
    const which = `part ${i + 1} of ${links.length}`;
    put(host, {
      downloading: {
        index: i + 1,
        count: links.length,
        written: 0,
        total: 0,
        stopRequested: false,
      },
    });
    let started: { download_id?: string; path?: string; total?: number };
    try {
      started = await deps.start(url, insecureTls);
    } catch (e) {
      return fail(
        `Could not start downloading ${which}: ${e instanceof Error ? e.message : String(e)}`,
      );
    }
    const id = started.download_id;
    if (!id || !started.path)
      return fail(`The engine did not start downloading ${which}.`);
    for (;;) {
      let st: LinkStatus;
      try {
        st = await deps.status(id);
      } catch (e) {
        return fail(
          `Lost track of ${which}: ${e instanceof Error ? e.message : String(e)}`,
        );
      }
      if (st.error) return fail(`Downloading ${which} failed: ${st.error}`);
      if (st.cancelled)
        return put(host, { downloading: null, downloaded: paths });
      if (st.done) break;
      put(host, {
        downloading: {
          index: i + 1,
          count: links.length,
          written: st.written ?? 0,
          total: st.total ?? started.total ?? 0,
          stopRequested: false,
        },
      });
      await deps.sleep(1000);
      if (stopped()) {
        await deps.cancel(id).catch(() => {});
        return put(host, { downloading: null, downloaded: paths });
      }
    }
    paths.push(started.path);
  }

  const first = paths.find((p) => isArchiveFirstVolume(p));
  if (!first) return fail("no_first_part");
  // Open it exactly as if the user had chosen that file, keeping the list of what was fetched.
  await chooseArchive(host, first);
  put(host, { downloaded: paths });
}

/** Asks a running download of parts to stop after cancelling the part in flight. */
export function stopArchiveDownload(host: string): void {
  const cur = archiveInstallFor(useArchiveInstallStore.getState(), host);
  if (cur?.downloading)
    put(host, { downloading: { ...cur.downloading, stopRequested: true } });
}
