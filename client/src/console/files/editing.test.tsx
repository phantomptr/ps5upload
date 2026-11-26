import { isValidElement, type ReactElement, type ReactNode } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it, vi } from "vitest";

vi.mock("../api", async (importOriginal) => {
  const real = await importOriginal<typeof import("../api")>();
  return { ...real, call: vi.fn(), callBlob: vi.fn(), callRaw: vi.fn() };
});

import { ApiError } from "../api";
import type { Volume } from "../home/Home";
import { t } from "../strings";
import { TextPrompt } from "./TextPrompt";
import { FilesView, sheetActions, type FilesViewProps } from "./Files";
import { PermissionsDialog, permErrorText } from "./PermissionsDialog";
import { UploadDialog, uploadSummaryText } from "./UploadDialog";
import type { DirState, Entry } from "./useDir";
import { editableOf, Viewer } from "./Viewer";
import { editorText, lineEndings, saveErrorText, TextEditor } from "./viewer/TextEditor";

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
const actions = (node: ReactNode) => elements(node).map((e) => e.props["data-action"] ?? e.props.action).filter(Boolean);

const entry = (name: string, kind: Entry["kind"] = "file", size = 10): Entry => ({ name, kind, size, mtime: 0 });
const dir = (entries: Entry[]): DirState => ({ entries, more: false, loading: false, loaded: true, error: null });
const vol = (path: string, fs_type = "ufs"): Volume => ({ path, mount_from: "/dev/x", fs_type, total_bytes: 1000, free_bytes: 400, writable: true });
const ENTRIES = [entry("cfg", "dir"), entry("app.conf"), entry("big.bin", "file", 5 * 1024 * 1024)];

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

describe("text editing", () => {
  it("edits a file only when it was read whole, unchanged, as UTF-8", () => {
    const buf = new TextEncoder().encode("a=1\n").buffer as ArrayBuffer;
    expect(editableOf(buf, { mtime: 3, size: 4 })).toEqual({ text: "a=1\n", stamp: { mtime: 3, size: 4 } });
    expect(editableOf(buf, { mtime: 3, size: 5 })).toBeNull(); // changed while read, or cut at the cap
    expect(editableOf(buf, null)).toBeNull(); // no stamp: a save could not be checked
    expect(editableOf(new Uint8Array([0x41, 0xc3]).buffer, { mtime: 0, size: 2 })).toBeNull();
  });
  it("any small file offers Edit as text; New file is a folder action", () => {
    expect(sheetActions(["app.conf"], ENTRIES, null, false)).toEqual(expect.arrayContaining(["edit", "newfile"]));
    expect(sheetActions(["big.bin"], ENTRIES, null, false)).not.toContain("edit");
    expect(sheetActions(["cfg"], ENTRIES, false, false)).not.toContain("edit");
    const onAsk = vi.fn();
    const sheet = elements(FilesView(props({ onAsk, ask: { kind: "sheet", names: ["app.conf"] } }))).find((e) => typeof e.props.onChoose === "function");
    (sheet!.props.onChoose as (a: string) => void)("edit");
    expect(onAsk).toHaveBeenCalledWith({ kind: "view", name: "app.conf", type: "text", edit: true });
    expect(actions(FilesView(props()))).toContain("newfile");
  });
  it("New file asks for a name; the viewer opens in the editor for edit", () => {
    const onNewFile = vi.fn();
    const prompt = find(FilesView(props({ onNewFile, ask: { kind: "newfile" } })), TextPrompt);
    expect(prompt.props.title).toBe(t("cw_files_new_file"));
    (prompt.props.onSubmit as (v: string) => void)("a.cfg");
    expect(onNewFile).toHaveBeenCalledWith("a.cfg");
    const v = find(FilesView(props({ ask: { kind: "view", name: "app.conf", type: "text", edit: true } })), Viewer);
    expect(v.props.startEditing).toBe(true);
  });
  it("the editor shows LF text, Save off until something changes", () => {
    const html = renderToStaticMarkup(<TextEditor path="/d/a.ini" text={"a=1\r\nb=2\r\n"} stamp={{ mtime: 1, size: 10 }} onSaved={vi.fn()} onClose={vi.fn()} />);
    expect(html).toContain("a=1\nb=2\n");
    expect(html).toMatch(/data-action="edit-save"[^>]*disabled=""/);
  });
  it("saves with the file's dominant line ending and says when lone CRs will change", () => {
    expect(lineEndings("a\r\nb\r\nc\n")).toEqual({ crlf: true, loneCr: false });
    expect(lineEndings("a\nb\nc\r\n")).toEqual({ crlf: false, loneCr: false });
    expect(lineEndings("a\rb\nc")).toEqual({ crlf: false, loneCr: true });
    expect(editorText("a\r\nb\rc\n")).toBe("a\nb\nc\n");
    const note = renderToStaticMarkup(<TextEditor path="/d/a" text={"a\rb"} stamp={{ mtime: 1, size: 3 }} onSaved={vi.fn()} onClose={vi.fn()} />);
    expect(note).toContain("data-edit-eol");
    const plain = renderToStaticMarkup(<TextEditor path="/d/a" text={"a\nb"} stamp={{ mtime: 1, size: 3 }} onSaved={vi.fn()} onClose={vi.fn()} />);
    expect(plain).not.toContain("data-edit-eol");
  });
  it("says why a save was refused", () => {
    expect(saveErrorText("binary")).toBe(t("cw_files_edit_binary"));
    expect(saveErrorText(new ApiError(413, "too_large"))).toBe(t("cw_files_edit_too_large"));
    expect(saveErrorText(new ApiError(404, "gone"))).toBe(t("cw_files_edit_gone"));
    expect(saveErrorText(new ApiError(507, "no_space"))).toBe(t("cw_files_edit_no_space"));
    // a protected folder in plain words, not "That did not work (path_not_allowed)"
    expect(saveErrorText(new ApiError(403, "path_not_allowed"))).toBe(t("cw_files_err_denied"));
  });
});

describe("permissions editor", () => {
  it("says a refused change, a lost job and a protected folder in words", () => {
    expect(permErrorText(new ApiError(500, "fs_chmod_failed"))).toBe(t("cw_files_perm_failed"));
    expect(permErrorText("gone")).toBe(t("cw_files_op_gone"));
    expect(permErrorText(new ApiError(403, "path_not_allowed"))).toBe(t("cw_files_err_denied"));
  });

  it("shows the toggles and octal value; a folder can apply to its contents", () => {
    const html = renderToStaticMarkup(<PermissionsDialog name="cfg" path="/data/cfg" mode={0o40755} dir onDone={vi.fn()} onCancel={vi.fn()} />);
    expect(html).toContain('data-perm-value="">0755<');
    expect(html).toContain("rwxr-xr-x");
    expect((html.match(/data-perm="/g) ?? []).length).toBe(9);
    expect(html).toContain('data-perm="owner-read" data-nav-default');
    expect(html).toContain('data-perm="group-write" aria-pressed="false"');
    expect(html).toContain('data-action="perm-recursive"');
    expect(html).not.toContain("data-perm-note");
    const file = renderToStaticMarkup(<PermissionsDialog name="a" path="/mnt/usb0/a" mode={0o644} dir={false} fsType="exfat" onDone={vi.fn()} onCancel={vi.fn()} />);
    expect(file).not.toContain("perm-recursive");
    expect(file).toContain("data-perm-note");
  });
});

describe("upload and download (another device only)", () => {
  it("Upload here and Download show only when the page is open on a phone or PC", () => {
    expect(actions(FilesView(props()))).not.toContain("upload");
    expect(actions(FilesView(props({ remote: true })))).toContain("upload");
    expect(sheetActions(["app.conf"], ENTRIES, null, true)).toContain("download");
    expect(sheetActions(["cfg"], ENTRIES, false, true)).toContain("download");
    expect(sheetActions(["app.conf"], ENTRIES, null, false)).not.toContain("download");
    const onDownload = vi.fn();
    const sheet = elements(FilesView(props({ remote: true, onDownload, ask: { kind: "sheet", names: ["cfg"] } }))).find(
      (e) => typeof e.props.onChoose === "function",
    );
    (sheet!.props.onChoose as (a: string) => void)("download");
    expect(onDownload).toHaveBeenCalledWith("/data/x/cfg");
  });
  it("the upload dialog says the drive's free space and an exFAT drive's note", () => {
    const up = find(FilesView(props({ remote: true, volumes: [vol("/data", "exfat")], ask: { kind: "upload" } })), UploadDialog);
    expect(up.props).toMatchObject({ dir: "/data/x", free: 400 });
    expect(up.props.fsNote).toBe(t("cw_files_perm_not_kept", { fs: "exfat" }));
    const html = renderToStaticMarkup(<UploadDialog dir="/data/x" free={400} onDone={vi.fn()} onClose={vi.fn()} />);
    expect(html).toContain('data-action="up-files"');
    expect(html).toContain(`${t("cw_files_free", { free: "400 B" })} · ${t("cw_files_up_hint")}`);
  });
  it("sums up an upload", () => {
    const s = uploadSummaryText({ uploaded: ["a", "b"], skipped: ["c"], failed: [{ name: "d", error: new ApiError(507, "no_space") }], stopped: [] });
    expect(s).toBe(
      [t("cw_files_up_done", { n: 2 }), t("cw_files_up_skipped", { n: 1 }), t("cw_files_up_failed", { n: 1, reason: t("cw_files_up_no_space") })].join(" · "),
    );
    const denied = uploadSummaryText({ uploaded: [], skipped: [], failed: [{ name: "d", error: new ApiError(403, "path_not_allowed") }], stopped: [] });
    expect(denied).toContain(t("cw_files_err_denied"));
    expect(denied).not.toContain("path_not_allowed");
  });
});
