import { isValidElement, type ReactElement, type ReactNode } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it, vi } from "vitest";

vi.mock("../api", async (importOriginal) => {
  const real = await importOriginal<typeof import("../api")>();
  return { ...real, call: vi.fn() };
});

import { formatBytes } from "../../lib/format";
import { ApiError, call } from "../api";
import type { Volume } from "../home/Home";
import { t } from "../strings";
import { ActionsSheet } from "./ActionsSheet";
import { ExtractDialog, extractAfter, extractCauseText, extractErrorText, freeNoteText, itemsPasswordAsk } from "./ExtractDialog";
import { busyText, extractMessage, FilesView, sheetActions, type FilesViewProps } from "./Files";
import { extractInfo, listArchiveTop, planArchiveFree, skipTop, startOp, topListing, type ExtractInfo, type TopListing } from "./opJob";
import type { FreePlan } from "../../lib/archiveName";
import { TextPrompt } from "./TextPrompt";
import type { DirState, Entry } from "./useDir";

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
function find<T>(node: ReactNode, type: T): ReactElement<Record<string, unknown>> {
  const el = elements(node).find((e) => e.type === type);
  if (!el) throw new Error("not rendered");
  return el;
}

const entry = (name: string, kind: Entry["kind"] = "file", size = 10): Entry => ({ name, kind, size, mtime: 0 });
const dir = (entries: Entry[]): DirState => ({ entries, more: false, loading: false, loaded: true, error: null });
const vol = (path: string): Volume => ({ path, mount_from: "/dev/x", fs_type: "ufs", total_bytes: 1000, free_bytes: 400, writable: true });
const ENTRIES = [entry("Saves", "dir"), entry("g.zip"), entry("g.part1.rar"), entry("g.part2.rar"), entry("x.7z.001"), entry("x.7z.002"), entry("b.png")];

function props(over: Partial<FilesViewProps> = {}): FilesViewProps {
  return {
    path: "/data/x",
    volumes: [vol("/data")],
    volumesError: false,
    dir: dir(ENTRIES),
    selecting: false,
    selected: [],
    ask: null,
    busy: null,
    message: null,
    tasks: [],
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
    ...over,
  };
}

const INFO: ExtractInfo = { name: "g", dest: "/data/x/g", size: 3 * 1024 ** 3, volumes: 1, free: 100 * 1024 ** 3, exists: false, kind: "zip" };

describe("extract: which files", () => {
  it("offers Extract on archives and first volumes only", () => {
    expect(sheetActions(["g.zip"], ENTRIES, null)).toContain("extract");
    expect(sheetActions(["g.part1.rar"], ENTRIES, null)).toContain("extract");
    expect(sheetActions(["g.part2.rar"], ENTRIES, null)).not.toContain("extract");
    expect(sheetActions(["x.7z.001"], ENTRIES, null)).toContain("extract");
    expect(sheetActions(["x.7z.002"], ENTRIES, null)).not.toContain("extract");
    expect(sheetActions(["b.png"], ENTRIES, null)).not.toContain("extract");
    expect(sheetActions(["Saves"], ENTRIES, false)).not.toContain("extract");
  });

  it("the sheet opens the dialog; a running batch holds it", () => {
    const onAsk = vi.fn();
    const choose = find(FilesView(props({ onAsk, ask: { kind: "sheet", names: ["g.zip"] } })), ActionsSheet).props.onChoose as (a: string) => void;
    choose("extract");
    expect(onAsk).toHaveBeenCalledWith({ kind: "extract", name: "g.zip" });
    const html = renderToStaticMarkup(<ActionsSheet title="x" actions={["extract", "info"]} busy onChoose={vi.fn()} onClose={vi.fn()} />);
    expect(html).toMatch(/data-sheet="extract"[^>]*disabled/);
  });
});

describe("extract dialog", () => {
  const go = (i: ExtractInfo, busy = false) =>
    renderToStaticMarkup(<ExtractDialog dir="/data/x" name="g.zip" path="/data/x/g.zip" busy={busy} onStart={vi.fn()} onCancel={vi.fn()} initial={i} />);

  it("says where it writes, the size and the parts", () => {
    const html = go({ ...INFO, volumes: 3 });
    expect(html).toContain("/data/x/g");
    expect(html).toContain(t("cw_files_extract_size_parts", { size: formatBytes(3 * 1024 ** 3), n: 3 }));
    expect(html).not.toMatch(/data-action="extract-go"[^>]*disabled/);
    expect(html).not.toContain('data-action="extract-merge"');
    expect(go(INFO, true)).toMatch(/data-action="extract-go"[^>]*disabled/);
  });

  it("a folder already there offers Keep both, Merge and Merge and replace instead of Extract", () => {
    const html = go({ ...INFO, exists: true });
    expect(html).toMatch(/data-extract-blocked="exists"/);
    expect(html).toContain('data-action="extract-keep"');
    expect(html).toContain('data-action="extract-merge"');
    expect(html).toContain('data-action="extract-replace"');
    // Confirming Merge and replace: the dialog stays mounted under the confirm.
    const asked = renderToStaticMarkup(
      <ExtractDialog dir="/data/x" name="g.zip" path="/data/x/g.zip" busy={false} onStart={vi.fn()} onCancel={vi.fn()} initial={{ ...INFO, exists: true }} confirming />,
    );
    expect(asked).toContain('data-action="extract-replace"');
    expect(asked.indexOf('data-action="extract-replace"')).toBeLessThan(asked.indexOf('role="alertdialog"'));
    expect(html).not.toContain('data-action="extract-go"');
  });

  it("offers Change… to extract elsewhere; beside the archive needs no way back", () => {
    const html = go(INFO);
    expect(html).toContain('data-action="extract-to"');
    expect(html).toContain(t("cw_files_extract_change"));
    expect(html).not.toContain('data-action="extract-beside"');
  });

  it("another drive: writes there, shows that drive's free space, and checks the folder before Extract", () => {
    const html = renderToStaticMarkup(
      <ExtractDialog
        dir="/data/x"
        name="g.zip"
        path="/data/x/g.zip"
        busy={false}
        onStart={vi.fn()}
        onCancel={vi.fn()}
        initial={INFO}
        volumes={[vol("/data"), { ...vol("/mnt/usb0"), free_bytes: 777 }]}
        startParent="/mnt/usb0/games"
        taken={() => new Promise(() => undefined)}
      />,
    );
    expect(html).toMatch(/data-extract-out=""[^>]*>\/mnt\/usb0\/games\/g</);
    expect(html).toMatch(new RegExp(`data-extract-free=""[^>]*>${formatBytes(777)}<`));
    expect(html).toContain('data-action="extract-beside"');
    expect(html).toContain(t("cw_files_extract_beside").replace("'", "&#x27;"));
    // Whether "g" is there already is not known yet: Extract waits for the check.
    expect(html).toMatch(/data-action="extract-go"[^>]*disabled/);
  });

  it("Files hands the dialog the console's drives for Change…", () => {
    const el = find(FilesView(props({ ask: { kind: "extract", name: "g.zip" } })), ExtractDialog);
    expect(el.props.volumes).toEqual([vol("/data")]);
  });

  it("Merge and Extract hand their choice to onExtract", () => {
    const onExtract = vi.fn();
    const el = find(FilesView(props({ onExtract, ask: { kind: "extract", name: "g.zip" } })), ExtractDialog);
    expect(el.props.path).toBe("/data/x/g.zip");
    (el.props.onStart as (s: unknown) => void)({ dest: "/data/x/g", merge: true, replace: false });
    expect(onExtract).toHaveBeenCalledWith("g.zip", { dest: "/data/x/g", merge: true, replace: false });
  });

  it("asks for a password with a masked box and runs the same extract again", () => {
    const onExtract = vi.fn();
    const start = { dest: "/data/x/g", merge: false, replace: false };
    const el = find(FilesView(props({ onExtract, ask: { kind: "extractpw", name: "g.zip", start, wrong: true, dir: "/data/began" } })), TextPrompt);
    expect(el.props.secret).toBe(true);
    const html = renderToStaticMarkup(el);
    expect(html).toContain('type="password"');
    expect(html).toContain(t("cw_files_extract_pw_wrong"));
    expect((el.props.check as (v: string) => string | null)("")).toBe(t("cw_files_extract_pw_empty"));
    (el.props.onSubmit as (v: string) => void)("hunter2");
    // With the folder the extract began in: the folder shown may be another by now.
    expect(onExtract).toHaveBeenCalledWith("g.zip", start, "hunter2", "/data/began", false);
  });
});

describe("extract requests", () => {
  it("info and start call the helper's routes with the contract's bodies", async () => {
    const mocked = vi.mocked(call);
    mocked.mockResolvedValueOnce(INFO).mockResolvedValueOnce({ job: "abc" });
    await extractInfo("/data/x/g.zip");
    expect(mocked).toHaveBeenNthCalledWith(1, "/api/fs/extract/info", { path: "/data/x/g.zip" });
    const job = await startOp("/api/fs/extract", { path: "/data/x/g.zip", dest: "/data/x/g", password: "pw", merge: true });
    expect(job).toBe("abc");
    expect(mocked).toHaveBeenNthCalledWith(2, "/api/fs/extract", { path: "/data/x/g.zip", dest: "/data/x/g", password: "pw", merge: true });
  });
});

describe("extract lines", () => {
  const status = { job: "j", state: "done" as const, files_done: 1, files_total: 1, bytes_done: 1, bytes_total: 1, message: null, result: null, code: null };

  it("batch line with progress and Stop", () => {
    const b = { op: "extract" as const, at: 1, total: 1, stopping: false, paths: ["/data/x/g.zip", "/data/x"], bytesDone: 512, bytesTotal: 2048 };
    const line = busyText(b);
    expect(line).toContain(t("cw_files_extract_batch", { name: "g.zip" }));
    expect(line).toContain(t("cw_home_percent", { n: 25 }));
    const html = renderToStaticMarkup(<FilesView {...props({ busy: b, onStopBatch: vi.fn() })} />);
    expect(html).toContain('data-action="stop-batch"');
  });

  it("done names the count and what was left out", () => {
    const done = extractMessage(
      { end: "done", status: { ...status, result: '{"path":"/data/x/g","files":12,"dirs":2,"bytes":9,"skipped":3,"unsafe":1,"links":0}' } },
      false,
    );
    expect(done?.tone).toBe("info");
    expect(done?.text).toContain(t("cw_files_extract_done_v2", { n: 12, name: "g" }));
    expect(done?.text).toContain(t("cw_files_extract_done_skipped_v2", { n: 3 }));
    expect(done?.text).toContain(t("cw_files_extract_done_unsafe_v2", { n: 1 }));
    expect(done?.text).not.toContain(t("cw_files_extract_done_links_v2", { n: 0 }));
    // one reads right: "Files: 1", "unsafe paths: 1" (it said "1 files", "1 with unsafe paths were")
    const one = extractMessage(
      { end: "done", status: { ...status, result: '{"path":"/data/x/g","files":1,"dirs":0,"bytes":9,"skipped":0,"unsafe":1,"links":1}' } },
      false,
    );
    expect(one?.text).toBe("Extracted into “g”. Files: 1. Not extracted, unsafe paths: 1. Links or special files not extracted: 1.");
  });

  it("stopped says what stays; failures say why", () => {
    expect(extractMessage({ end: "cancelled", status }, false)?.text).toBe(t("cw_files_extract_stopped"));
    expect(extractMessage({ end: "cancelled", status }, true)?.text).toBe(t("cw_files_extract_stopped_merge"));
    const failed = (message: string) => extractMessage({ end: "failed", status: { ...status, state: "failed", message } }, false)?.text;
    expect(failed("fs_extract_no_space: needs 9 bytes, 1 free")).toBe(t("cw_files_extract_failed", { reason: t("cw_files_extract_err_no_space") }));
    expect(failed("fs_extract_rar_encrypted")).toBe(t("cw_files_extract_failed", { reason: t("cw_files_extract_err_rar_encrypted") }));
    expect(failed("fs_extract_corrupt: CRC")).toBe(t("cw_files_extract_failed", { reason: t("cw_files_extract_err_corrupt") }));
    expect(failed("fs_extract_loader_unreachable")).toBe(t("cw_files_extract_failed", { reason: t("cw_files_extract_err_loader") }));
    expect(failed("fs_extract_weird")).toContain("fs_extract_weird");
    expect(extractMessage({ end: "error", error: new ApiError(409, "fs_extract_busy") }, false)?.text).toBe(
      t("cw_files_extract_failed", { reason: t("cw_files_extract_err_busy") }),
    );
    expect(extractMessage({ end: "error", error: new ApiError(409, "convert_busy") }, false)?.text).toBe(
      t("cw_files_extract_failed", { reason: t("cw_convert_err_busy") }),
    );
  });

  it("every cause in the contract has a sentence", () => {
    for (const c of [
      "busy", "password_required", "wrong_password", "rar_encrypted", "unsupported_method", "corrupt", "truncated", "not_archive",
      "missing_volume", "loader_unreachable", "not_started", "died", "no_memory", "write_failed", "open_failed", "path_not_allowed",
      "bad_dest", "missing_path", "no_space", "destination_exists",
    ]) {
      expect(extractCauseText(`fs_extract_${c}`), c).toBeTruthy();
    }
    expect(extractErrorText(new ApiError(409, "fs_extract_missing_volume: g.part2.rar"))).toBe(t("cw_files_extract_err_missing_volume"));
  });
});

describe("extract: choose items (#969)", () => {
  const listing: TopListing = { top: ["Game", "readme.txt"], dirs: ["Game"], more: false };

  it("leaves unticked top-level entries out with empty renames", () => {
    expect(skipTop(["readme.txt"])).toBe("readme.txt/");
    expect(skipTop(["a", "b c"])).toBe("a//b c/");
  });

  it("reads a listing job's result; an old helper's answer is no listing", () => {
    expect(topListing({ top: "Game/readme.txt", top_dirs: "Game", top_more: false, listed: true })).toEqual(listing);
    expect(topListing({ path: "/data/x/g", files: 3 })).toBeNull();
    expect(topListing(null)).toBeNull();
  });

  it("lists the items as switches, all on, and Extract hands the ones turned off", () => {
    const onStart = vi.fn();
    const html = renderToStaticMarkup(
      <ExtractDialog dir="/data/x" name="g.zip" path="/data/x/g.zip" busy={false} onStart={vi.fn()} onCancel={vi.fn()} initial={INFO} />,
    );
    expect(html).toContain('data-action="extract-choose"');
    expect(html).toContain(t("cw_files_extract_choose"));
    const listed = renderToStaticMarkup(
      <ExtractDialog
        dir="/data/x"
        name="g.zip"
        path="/data/x/g.zip"
        busy={false}
        onStart={onStart}
        onCancel={vi.fn()}
        initial={INFO}
        startItems={listing}
      />,
    );
    expect(listed).toContain('data-extract-item="Game"');
    expect(listed).toContain("Game/");
    expect((listed.match(/aria-checked="true"/g) ?? []).length).toBe(2);
    expect(listed).toContain(t("cw_files_status_filtered", { n: 2, total: 2 }));
    expect(listed).not.toContain('data-action="extract-choose"');
  });

  it("a password-protected archive asks for its password to list, then lists with it", async () => {
    expect(itemsPasswordAsk(new ApiError(500, "fs_extract_password_required"))).toEqual({ wrong: false });
    expect(itemsPasswordAsk(new ApiError(500, "fs_extract_wrong_password"))).toEqual({ wrong: true });
    expect(itemsPasswordAsk(new ApiError(500, "fs_extract_list_failed"))).toBeNull();
    expect(itemsPasswordAsk(new Error("x"))).toBeNull();
    const asked = renderToStaticMarkup(
      <ExtractDialog
        dir="/data/x"
        name="g.zip"
        path="/data/x/g.zip"
        busy={false}
        onStart={vi.fn()}
        onCancel={vi.fn()}
        initial={INFO}
        startItemsPw={{ wrong: false }}
      />,
    );
    expect(asked).toContain("data-extract-items-password");
    expect(asked).toContain('type="password"');
    expect(asked).toContain(t("cw_files_extract_pw_body"));
    // Choose items waits for the password.
    expect(asked).toMatch(/<button[^>]*disabled=""[^>]*data-action="extract-choose"|<button[^>]*data-action="extract-choose"[^>]*disabled=""/);
    const wrong = renderToStaticMarkup(
      <ExtractDialog dir="/data/x" name="g.zip" path="/data/x/g.zip" busy={false} onStart={vi.fn()} onCancel={vi.fn()} initial={INFO} startItemsPw={{ wrong: true }} />,
    );
    expect(wrong).toContain(t("cw_files_extract_pw_wrong"));
    // The listing goes with the password.
    const mocked = vi.mocked(call);
    mocked.mockReset();
    mocked.mockResolvedValueOnce({ job: "lp" });
    const deps = {
      status: vi.fn(async () => ({
        job: "lp",
        state: "done" as const,
        files_done: 0,
        files_total: 0,
        bytes_done: 0,
        bytes_total: 0,
        message: null,
        result: JSON.stringify({ top: "Game/readme.txt", top_dirs: "Game", top_more: false, listed: true }),
        code: null,
      })),
      cancel: vi.fn(async () => {}),
      sleep: vi.fn(async () => {}),
    };
    await listArchiveTop("/data/x/g.zip", "/data/x", "s3cret", deps);
    expect(mocked).toHaveBeenCalledWith("/api/fs/extract", { path: "/data/x/g.zip", dest: "/data/x", list: true, password: "s3cret" });
  });

  it("listing asks the helper for list mode and reads its result", async () => {
    const mocked = vi.mocked(call);
    mocked.mockReset();
    mocked.mockResolvedValueOnce({ job: "lj" });
    const deps = {
      status: vi.fn(async () => ({
        job: "lj",
        state: "done" as const,
        files_done: 0,
        files_total: 0,
        bytes_done: 0,
        bytes_total: 0,
        message: null,
        result: JSON.stringify({ top: "Game/readme.txt", top_dirs: "Game", top_more: false, listed: true }),
        code: null,
      })),
      cancel: vi.fn(async () => {}),
      sleep: vi.fn(async () => {}),
    };
    await expect(listArchiveTop("/data/x/g.zip", "/data/x", undefined, deps)).resolves.toEqual(listing);
    expect(mocked).toHaveBeenCalledWith("/api/fs/extract", { path: "/data/x/g.zip", dest: "/data/x", list: true });
    // A helper that ignored list mode refuses the folder that is there: no listing, nothing written.
    mocked.mockResolvedValueOnce({ job: "old" });
    deps.status.mockResolvedValueOnce({ ...(await deps.status()), state: "failed" as never, message: "fs_extract_destination_exists" as never, result: null as never });
    await expect(listArchiveTop("/data/x/g.zip", "/data/x", undefined, deps)).rejects.toMatchObject({ code: "fs_extract_destination_exists" });
  });
});

describe("extract: delete the archive as it's unpacked", () => {
  const status = { job: "j", state: "done" as const, files_done: 1, files_total: 1, bytes_done: 1, bytes_total: 1, message: null, result: null, code: null };
  const plan: FreePlan = { mode: "parts", parts: 8, archiveBytes: 8000, unpacked: 9000, peak: 2000, sameDrive: true, known: true, why: "" };
  const dialog = (over: Record<string, unknown> = {}) =>
    renderToStaticMarkup(
      <ExtractDialog dir="/data/x" name="x.7z.001" path="/data/x/x.7z.001" busy={false} onStart={vi.fn()} onCancel={vi.fn()} initial={INFO} {...over} />,
    );

  // As the markup writes it.
  const esc = (s: string) => s.replace(/&/g, "&amp;").replace(/'/g, "&#x27;").replace(/"/g, "&quot;").replace(/</g, "&lt;");

  it("is off for every archive, beside Delete after extracting", () => {
    const html = dialog();
    expect(html).toContain('data-action="extract-free"');
    expect(html).toContain(esc(t("cw_files_extract_free")));
    expect(html).toContain(esc(t("cw_files_extract_free_hint")));
    expect(html).toContain('data-action="extract-delete-after"');
    expect(html).not.toContain("data-extract-free-note");
  });

  it("on, it says how much less room it needs and hides Delete after extracting", () => {
    const html = dialog({ startFree: true, startFreePlan: plan });
    expect(html).toContain(esc(t("cw_files_extract_free_parts", { n: 8, size: formatBytes(7000) })));
    expect(html).not.toContain('data-action="extract-delete-after"');
    expect(dialog({ startFree: true })).toContain(esc(t("cw_files_extract_free_checking")));
    expect(dialog({ startFree: true, startFreePlan: null })).toContain(esc(t("cw_files_extract_free_old")));
  });

  it("says plainly when it saves nothing while it runs, or can't be done", () => {
    expect(freeNoteText({ ...plan, mode: "end" })).toBe(t("cw_files_extract_free_end"));
    expect(freeNoteText({ ...plan, sameDrive: false })).toBe(t("cw_files_extract_free_other_drive"));
    expect(freeNoteText({ ...plan, known: false })).toBe(t("cw_files_extract_free_parts_plain", { n: 8 }));
    expect(freeNoteText({ ...plan, mode: "", why: "fs_extract_free_refused: x" })).toBe(t("cw_files_extract_err_free_refused"));
    expect(extractCauseText("fs_extract_free_refused")).toBe(t("cw_files_extract_err_free_refused"));
  });

  it("is never on with items left out, and then Delete after extracting is what is sent", () => {
    expect(extractAfter(true, false, false)).toEqual({ deleteAfter: false, free: true });
    expect(extractAfter(true, false, true)).toEqual({ deleteAfter: false, free: true });
    expect(extractAfter(true, true, true)).toEqual({ deleteAfter: true });
    expect(extractAfter(false, false, true)).toEqual({ deleteAfter: true });
    const listed = dialog({ startFree: true, startFreePlan: plan, startItems: { top: ["Game", "extra"], dirs: ["Game"], more: false } });
    expect(listed).toContain('data-action="extract-free"');
  });

  it("asks the helper for the plan with a listing, and reads it back", async () => {
    const mocked = vi.mocked(call);
    mocked.mockReset();
    mocked.mockResolvedValueOnce({ job: "p1" });
    const deps = {
      status: vi.fn(async () => ({
        ...status,
        result: JSON.stringify({
          path: "/data/x",
          top: "Game",
          top_dirs: "Game",
          top_more: false,
          listed: true,
          free: { mode: "parts", parts: 8, archive_bytes: 8000, unpacked: 9000, peak: 2000, same_drive: true, known: true, why: "" },
        }),
      })),
      cancel: vi.fn(async () => {}),
      sleep: vi.fn(async () => {}),
    };
    const got = await planArchiveFree("/data/x/x.7z.001", "/data/x", undefined, deps);
    expect(mocked).toHaveBeenNthCalledWith(1, "/api/fs/extract", { path: "/data/x/x.7z.001", dest: "/data/x", list: true, free: true });
    expect(got).toEqual(plan);
    // A listing without the member (an older helper) is no plan.
    expect(topListing({ top: "Game", top_dirs: "", top_more: false, listed: true })).not.toHaveProperty("free");
  });

  it("done says what was freed; a stop after parts went says why, what is gone and where the files are", () => {
    const done = extractMessage(
      { end: "done", status: { ...status, result: '{"path":"/data/x/x","files":3,"parts_freed":4,"bytes_freed":4096}' } },
      false,
    );
    expect(done?.text).toContain(t("cw_files_extract_freed", { size: formatBytes(4096) }));
    const failed = (message: string) => extractMessage({ end: "failed", status: { ...status, state: "failed", message } }, false);
    const out = failed("fs_extract_partly_freed: 3 5 x.7z.004.partial-deleted | /data/x/x | fs_extract_no_space: x/a, 0 free");
    expect(out?.tone).toBe("error");
    expect(out?.text).toBe(
      t("cw_files_extract_err_partly_freed", { reason: t("cw_files_extract_err_no_space"), folder: "/data/x/x", n: 5 }),
    );
    expect(failed("fs_extract_partly_freed: 1 7 x.7z.002.partial-deleted | /data/x/x | fs_extract_cancelled")?.text).toBe(
      t("cw_files_extract_err_partly_freed", { reason: t("cw_files_extract_free_stopped"), folder: "/data/x/x", n: 7 }),
    );
  });
});
