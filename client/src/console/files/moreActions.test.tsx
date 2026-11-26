import { isValidElement, type ReactElement, type ReactNode } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it, vi } from "vitest";

vi.mock("../api", async (importOriginal) => {
  const real = await importOriginal<typeof import("../api")>();
  return { ...real, call: vi.fn() };
});

import { ApiError } from "../api";
import type { Volume } from "../home/Home";
import { t } from "../strings";
import { ActionsSheet } from "./ActionsSheet";
import { BulkRenameDialog, EMPTY_FIELDS } from "./BulkRenameDialog";
import { busyText, entryNamesKept, FilesView, renameItems, renameItemsKept, renameMessage, sheetActions, unpackMessage, type FilesViewProps } from "./Files";
import { InfoDialog, countText, walkNote } from "./InfoDialog";
import { UnpackDialog } from "./UnpackDialog";
import { VerifyDialog } from "./VerifyDialog";
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

const entry = (name: string, kind: Entry["kind"] = "file", size = 10): Entry => ({ name, kind, size, mtime: 0 });
const dir = (entries: Entry[]): DirState => ({ entries, more: false, loading: false, loaded: true, error: null });
const vol = (path: string): Volume => ({ path, mount_from: "/dev/x", fs_type: "ufs", total_bytes: 1000, free_bytes: 400, writable: true });

const ENTRIES = [entry("Saves", "dir"), entry("b.png"), entry("a.png"), entry("game.FFPFSC", "file", 5e9), entry("lnk", "link")];

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

function find<T>(node: ReactNode, type: T): ReactElement<Record<string, unknown>> {
  const el = elements(node).find((e) => e.type === type);
  if (!el) throw new Error("not rendered");
  return el;
}

describe("actions by item type", () => {
  it("a file can be verified; only a .ffpfsc can be decompressed; a folder neither", () => {
    expect(sheetActions(["b.png"], ENTRIES, null)).toContain("verify");
    expect(sheetActions(["b.png"], ENTRIES, null)).not.toContain("unpack");
    expect(sheetActions(["game.FFPFSC"], ENTRIES, null)).toEqual(expect.arrayContaining(["verify", "unpack"]));
    const folder = sheetActions(["Saves"], ENTRIES, false);
    expect(folder).not.toContain("verify");
    expect(folder).not.toContain("unpack");
    expect(folder).toContain("pin");
    // a link is not a regular file: no verify
    expect(sheetActions(["lnk"], ENTRIES, null)).not.toContain("verify");
  });

  it("one item offers Rename and the pattern rename; a selection only the pattern rename", () => {
    expect(sheetActions(["b.png"], ENTRIES, null)).toEqual(expect.arrayContaining(["rename", "bulkrename"]));
    const many = sheetActions(["a.png", "b.png"], ENTRIES, null);
    expect(many).toContain("bulkrename");
    expect(many).not.toContain("rename");
    expect(many).not.toContain("verify");
    expect(many).not.toContain("info");
  });

  it("the sheet's choices open their dialogs", () => {
    const onAsk = vi.fn();
    const sheet = (names: string[]) =>
      find(FilesView(props({ onAsk, ask: { kind: "sheet", names } })), ActionsSheet).props.onChoose as (a: string) => void;
    sheet(["game.FFPFSC"])("unpack");
    sheet(["b.png"])("verify");
    sheet(["a.png", "b.png"])("bulkrename");
    expect(onAsk.mock.calls.map((c) => c[0])).toEqual([
      { kind: "unpack", name: "game.FFPFSC" },
      { kind: "verify", name: "b.png" },
      { kind: "bulkrename", names: ["a.png", "b.png"] },
    ]);
  });

  it("a running batch holds bulk rename and decompress", () => {
    const html = renderToStaticMarkup(
      <ActionsSheet title="x" actions={["bulkrename", "unpack", "verify", "info"]} busy onChoose={vi.fn()} onClose={vi.fn()} />,
    );
    expect(html).toMatch(/data-sheet="bulkrename"[^>]*disabled/);
    expect(html).toMatch(/data-sheet="unpack"[^>]*disabled/);
    expect(html).not.toMatch(/data-sheet="verify"[^>]*disabled/);
  });
});

describe("bulk rename flow", () => {
  it("opens with the items in the order the list shows them and every name of the folder", () => {
    const shown = [ENTRIES[0], ENTRIES[2], ENTRIES[1]];
    const el = find(FilesView(props({ shown, ask: { kind: "bulkrename", names: ["b.png", "a.png", "Saves"] } })), BulkRenameDialog);
    expect(el.props.items).toEqual([
      { name: "Saves", dir: true },
      { name: "a.png", dir: false },
      { name: "b.png", dir: false },
    ]);
    expect(el.props.existing).toEqual(ENTRIES.map((e) => e.name));
    expect(renameItems(["gone"], shown, ENTRIES)).toEqual([{ name: "gone", dir: false }]);
  });

  it("confirming hands the plan's rows to onBulkRename", () => {
    const onBulkRename = vi.fn();
    const el = find(FilesView(props({ onBulkRename, ask: { kind: "bulkrename", names: ["a.png"] } })), BulkRenameDialog);
    (el.props.onRename as (p: unknown) => void)({ rows: [{ from: "a.png", to: "z.png", problem: null }], ok: true, first: null });
    expect(onBulkRename).toHaveBeenCalledWith([{ from: "a.png", to: "z.png", problem: null }]);
  });

  it("previews old → new and holds Rename on a collision, saying why", () => {
    const ok = renderToStaticMarkup(
      <BulkRenameDialog
        items={[{ name: "a.png", dir: false }, { name: "b.png", dir: false }]}
        existing={["a.png", "b.png"]}
        initial={{ ...EMPTY_FIELDS, kind: "suffix", text: "-old" }}
        onRename={vi.fn()}
        onCancel={vi.fn()}
      />,
    );
    expect(ok).toContain("a-old.png");
    expect(ok).toContain("b-old.png");
    expect(ok).not.toMatch(/data-action="br-go"[^>]*disabled/);
    const clash = renderToStaticMarkup(
      <BulkRenameDialog
        items={[{ name: "a.png", dir: false }]}
        existing={["a.png", "b.png"]}
        initial={{ ...EMPTY_FIELDS, find: "a", replace: "b" }}
        onRename={vi.fn()}
        onCancel={vi.fn()}
      />,
    );
    expect(clash).toMatch(/data-br-problem="exists"/);
    expect(clash).toContain(t("cw_files_br_err_exists", { name: "b.png" }));
    expect(clash).toMatch(/data-action="br-go"[^>]*disabled/);
    // nothing typed yet: no rows, Rename held
    const empty = renderToStaticMarkup(
      <BulkRenameDialog items={[{ name: "a.png", dir: false }]} existing={["a.png"]} onRename={vi.fn()} onCancel={vi.fn()} />,
    );
    expect(empty).toContain(t("cw_files_br_waiting"));
    expect(empty).toMatch(/data-action="br-go"[^>]*disabled/);
  });

  it("progress, done, stopped and failed lines", () => {
    expect(busyText({ op: "rename", at: 2, total: 5, stopping: false })).toBe(t("cw_files_br_batch", { at: 2, total: 5 }));
    const base = { started: [], moved: [], failed: [], jobFailed: [], skipped: [] };
    expect(renameMessage({ ...base, done: [["a", "b"]], failedRow: null })?.text).toBe(t("cw_files_br_done", { n: 1 }));
    expect(renameMessage({ ...base, skipped: ["c"], done: [["a", "b"]], failedRow: null })?.text).toBe(
      t("cw_files_br_stopped", { done: 1, rest: 1 }),
    );
    const failed = renameMessage({
      ...base,
      failed: [{ item: "b", error: new ApiError(409, "exists") }],
      skipped: ["c"],
      done: [["a", "x"]],
      failedRow: { from: "b", to: "y", problem: null },
    });
    expect(failed?.tone).toBe("error");
    expect(failed?.text).toContain("“b”");
    expect(failed?.text).toContain(t("cw_files_err_exists"));
  });
});

describe("folder size", () => {
  it("a folder's Info offers Calculate folder size", () => {
    // InfoDialog reads the stat in an effect; the size block shows only for a folder (checked in the component).
    expect(renderToStaticMarkup(<InfoDialog dir="/data" name="x" path="/data/x" onClose={vi.fn()} />)).toContain(t("cw_home_loading"));
  });

  it("count and partial notes", () => {
    const c = { files: 1234, folders: 5, bytes: 2048, entries: 1239, unreadable: 0 };
    expect(countText(c)).toContain("1,234");
    expect(walkNote({ ...c, end: "done" })).toBeNull();
    expect(walkNote({ ...c, end: "capped" })).toContain("200,000");
    expect(walkNote({ ...c, end: "timeout" })).toContain("60");
    expect(walkNote({ ...c, end: "cancelled" })).toBe(t("cw_files_size_cancelled"));
    expect(walkNote({ ...c, end: "done", unreadable: 2 })).toBe(t("cw_files_size_unreadable_v2", { n: "2" }));
  });
});

describe("verify", () => {
  it("opens for the file's path and shows both values, copyable", () => {
    const el = find(FilesView(props({ ask: { kind: "verify", name: "b.png" } })), VerifyDialog);
    expect(el.props.path).toBe("/data/x/b.png");
    const html = renderToStaticMarkup(
      <VerifyDialog path="/data/x/b.png" name="b.png" onClose={vi.fn()} initial={{ step: "done", hash: "af13", crc: "CBF43926", size: 10 }} />,
    );
    expect(html).toMatch(/data-verify="blake3"[^>]*value="af13"|value="af13"[^>]*data-verify="blake3"/);
    expect(html).toContain('value="CBF43926"');
    expect(html).toContain('readOnly=""');
  });
});

describe("decompress", () => {
  const info = { name: "game.exfat", size: 40 * 1024 ** 3, need: 41 * 1024 ** 3, free: 100 * 1024 ** 3, exists: false, compressed: true };

  it("opens for the .ffpfsc, with Start held while a batch runs", () => {
    const el = find(
      FilesView(props({ ask: { kind: "unpack", name: "game.FFPFSC" }, busy: { op: "copy", at: 1, total: 2, stopping: false } })),
      UnpackDialog,
    );
    expect(el.props.path).toBe("/data/x/game.FFPFSC");
    expect(el.props.dir).toBe("/data/x");
    expect(el.props.busy).toBe(true);
  });

  it("says what it writes and where; refuses too little room; offers Keep both for a taken name", () => {
    const go = (i: typeof info, name: string | null = null) =>
      renderToStaticMarkup(
        <UnpackDialog dir="/data/x" name="game.ffpfsc" path="/data/x/game.ffpfsc" busy={false} onStart={vi.fn()} onCancel={vi.fn()} initial={i} initialName={name} />,
      );
    const ok = go(info);
    expect(ok).toContain("/data/x/game.exfat");
    expect(ok).not.toMatch(/data-action="unpack-go"[^>]*disabled/);
    const full = go({ ...info, free: 10 * 1024 ** 3 });
    expect(full).toMatch(/data-unpack-blocked="space"/);
    expect(full).toMatch(/data-action="unpack-go"[^>]*disabled/);
    const taken = go({ ...info, exists: true });
    expect(taken).toMatch(/data-unpack-blocked="exists"/);
    expect(taken).toContain('data-action="unpack-keep"');
    expect(taken).toMatch(/data-action="unpack-go"[^>]*disabled/);
    const kept = go({ ...info, exists: true }, "game (2).exfat");
    expect(kept).toContain("/data/x/game (2).exfat");
    expect(kept).not.toMatch(/data-action="unpack-go"[^>]*disabled/);
  });

  it("Start passes the Keep both name to onUnpack", () => {
    const onUnpack = vi.fn();
    const el = find(FilesView(props({ onUnpack, ask: { kind: "unpack", name: "game.FFPFSC" } })), UnpackDialog);
    (el.props.onStart as (n: string | null) => void)("game (2).exfat");
    expect(onUnpack).toHaveBeenCalledWith("game.FFPFSC", "game (2).exfat");
  });

  it("bulk rename inputs keep their identity across renders (the plan memo hits)", () => {
    const entries = [{ name: "a.png", kind: "file" }, { name: "b.png", kind: "file" }] as unknown as Parameters<typeof entryNamesKept>[0];
    const names = ["a.png"];
    expect(renameItemsKept(names, entries, entries)).toBe(renameItemsKept(names, entries, entries));
    expect(entryNamesKept(entries)).toBe(entryNamesKept(entries));
    expect(entryNamesKept([...entries])).not.toBe(entryNamesKept(entries));
    const p = props({ ask: { kind: "bulkrename", names } });
    const one = find(FilesView(p), BulkRenameDialog);
    const two = find(FilesView(p), BulkRenameDialog);
    expect(two.props.items).toBe(one.props.items);
    expect(two.props.existing).toBe(one.props.existing);
  });

  it("its batch line has Stop; done and failed lines", () => {
    const html = renderToStaticMarkup(
      <FilesView {...props({ busy: { op: "unpack", at: 1, total: 1, stopping: false, paths: ["/data/x/game.FFPFSC", "/data/x"] }, onStopBatch: vi.fn() })} />,
    );
    expect(html).toContain(t("cw_files_unpack_batch", { name: "game.FFPFSC" }));
    expect(html).toContain('data-action="stop-batch"');
    // Bytes reported by the job: percent + bytes, like Verify.
    const mid = busyText({ op: "unpack", at: 1, total: 1, stopping: false, paths: ["/data/x/game.FFPFSC"], bytesDone: 512, bytesTotal: 2048 });
    expect(mid).toContain(t("cw_home_percent", { n: 25 }));
    expect(mid).toContain(" / ");
    expect(busyText({ op: "unpack", at: 1, total: 1, stopping: false, paths: ["/data/x/game.FFPFSC"] })).toBe(t("cw_files_unpack_batch", { name: "game.FFPFSC" }));
    const status = { job: "j", state: "done" as const, files_done: 1, files_total: 1, bytes_done: 1, bytes_total: 1, message: null, result: null, code: null };
    expect(unpackMessage({ end: "done", status: { ...status, result: '{"path":"/data/x/game.exfat","size":1}' } }, "game.FFPFSC")?.text).toBe(
      t("cw_files_unpack_done", { name: "game.exfat", source: "game.FFPFSC" }),
    );
    const failed = unpackMessage({ end: "failed", status: { ...status, state: "failed", result: null, message: "fs_unpack_no_space: needs 9 bytes, 1 free" } }, "g");
    expect(failed).toEqual({ tone: "error", text: t("cw_files_unpack_failed", { reason: t("cw_files_unpack_no_space_short") }) });
    // An image of another file system is unsupported, not damaged.
    const other = unpackMessage({ end: "failed", status: { ...status, state: "failed", result: null, message: "fs_unpack_not_exfat" } }, "g");
    expect(other?.text).toBe(t("cw_files_unpack_failed", { reason: t("cw_files_unpack_not_image") }));
    const bad = unpackMessage({ end: "failed", status: { ...status, state: "failed", result: null, message: "fs_unpack_corrupt_block 3" } }, "g");
    expect(bad?.text).toBe(t("cw_files_unpack_failed", { reason: t("cw_files_unpack_damaged") }));
    const odd = unpackMessage({ end: "failed", status: { ...status, state: "failed", result: null, message: "fs_unpack_weird" } }, "g");
    expect(odd?.text).toContain("fs_unpack_weird");
    expect(unpackMessage({ end: "cancelled", status }, "g")?.text).toBe(t("cw_files_unpack_stopped"));
    expect(unpackMessage({ end: "error", error: new ApiError(409, "fs_unpack_busy") }, "g")?.text).toBe(
      t("cw_files_unpack_failed", { reason: t("cw_files_unpack_busy") }),
    );
  });
});
