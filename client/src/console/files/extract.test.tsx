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
import { ExtractDialog, extractCauseText, extractErrorText } from "./ExtractDialog";
import { busyText, extractMessage, FilesView, sheetActions, type FilesViewProps } from "./Files";
import { extractInfo, startOp, type ExtractInfo } from "./opJob";
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
    expect(onExtract).toHaveBeenCalledWith("g.zip", start, "hunter2", "/data/began");
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
    expect(done?.text).toContain(t("cw_files_extract_done", { n: 12, name: "g" }));
    expect(done?.text).toContain(t("cw_files_extract_done_skipped", { n: 3 }));
    expect(done?.text).toContain(t("cw_files_extract_done_unsafe", { n: 1 }));
    expect(done?.text).not.toContain(t("cw_files_extract_done_links", { n: 0 }));
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
