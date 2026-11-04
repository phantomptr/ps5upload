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
import { DirList } from "./DirList";
import { batchMessage, busyText, failedNamesIn, filesErrorText, FilesView, pasteMessage, upFrom, viewKind, type FilesViewProps } from "./Files";
import { clearClipboard, getClipboard, setClipboard } from "./clipboard";
import { ConflictDialog } from "./ConflictDialog";
import { IntoItselfError, renameEntry } from "./move";
import { TextPrompt } from "./TextPrompt";
import type { DirState, Entry } from "./useDir";

const mockCall = vi.mocked(call);

const CJK = "完蛋！我被美女包围了！";

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

const dir = (entries: Entry[], over: Partial<DirState> = {}): DirState => ({
  entries,
  more: false,
  loading: false,
  loaded: true,
  error: null,
  ...over,
});

const vol = (path: string, mount_from = "/dev/x"): Volume => ({
  path,
  mount_from,
  fs_type: "ufs",
  total_bytes: 1000,
  free_bytes: 400,
  writable: true,
});

function props(over: Partial<FilesViewProps> = {}): FilesViewProps {
  return {
    path: "/data/games",
    volumes: [vol("/data"), vol("/mnt/usb0", "/dev/da0s1")],
    volumesError: false,
    dir: dir([entry("a.txt"), entry("b"), entry("c")]),
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

beforeEach(() => {
  mockCall.mockReset();
  mockCall.mockResolvedValue({} as never);
});

describe("Files", () => {
  it("delete_confirm_names_item_and_count", () => {
    // Delete in the actions sheet only asks.
    const p = props({ ask: { kind: "sheet", names: ["a.txt", "b", "c"] } });
    const sheet = elements(FilesView(p)).find((e) => e.type === ActionsSheet);
    expect(sheet).toBeDefined();
    (sheet!.props.onChoose as (a: string) => void)("delete");
    expect(p.onAsk).toHaveBeenCalledWith({ kind: "delete", names: ["a.txt", "b", "c"] });
    expect(mockCall).not.toHaveBeenCalled();

    // Several: the count in the title, the first item named in the body.
    const many = props({ ask: { kind: "delete", names: ["a.txt", "b", "c"] } });
    const d = elements(FilesView(many)).find((e) => e.type === ConfirmDialog)!;
    expect(d.props.title).toBe(t("cw_files_del_many", { n: 3 }));
    expect(d.props.title).toContain("3");
    expect(d.props.body).toContain(t("cw_files_del_list_many", { name: "a.txt", more: 2 }));
    expect(d.props.body).toContain("a.txt");
    expect(d.props.danger).toBe(true);
    (d.props.onConfirm as () => void)();
    expect(many.onConfirmDelete).toHaveBeenCalledWith(["a.txt", "b", "c"]);

    // One: named in the title.
    const one = props({ ask: { kind: "delete", names: [CJK] } });
    const d1 = elements(FilesView(one)).find((e) => e.type === ConfirmDialog)!;
    expect(d1.props.title).toBe(t("cw_files_del_one", { name: CJK }));
    expect(d1.props.title).toContain(CJK);
  });

  it("fix_permissions_is_offered_asks_then_reports_done", () => {
    for (const names of [["b"], ["a.txt", "b", "c"]]) {
      const p = props({ ask: { kind: "sheet", names } });
      const sheet = elements(FilesView(p)).find((e) => e.type === ActionsSheet)!;
      expect(sheet.props.actions).toContain("fixperm");
      (sheet.props.onChoose as (a: string) => void)("fixperm");
      expect(p.onAsk).toHaveBeenCalledWith({ kind: "fixperm", names });
      expect(mockCall).not.toHaveBeenCalled();
    }
    const q = props({ ask: { kind: "fixperm", names: ["b", "c"] } });
    const d = elements(FilesView(q)).find((e) => e.type === ConfirmDialog)!;
    expect(d.props.body).toBe(t("cw_files_fixperm_body"));
    expect(d.props.title).toBe(t("cw_files_fixperm_many", { n: 2 })); // the count, as Delete says it
    expect(d.props.danger).toBeUndefined();
    (d.props.onConfirm as () => void)();
    expect(q.onConfirmFixPerm).toHaveBeenCalledWith(["b", "c"]);
    expect(batchMessage({ started: [], moved: [], failed: [], jobFailed: [], skipped: [] } as never, "chmod")?.text).toBe(
      t("cw_files_fixperm_done"),
    );
  });

  it("unicode_names_render_and_round_trip", async () => {
    const p = props({ dir: dir([entry(CJK, "dir")]) });
    expect(renderToStaticMarkup(<FilesView {...p} />)).toContain(CJK);

    // The row carries the name unchanged; ⋯ asks for its sheet with that name.
    const list = DirList({
      entries: p.dir.entries,
      selecting: false,
      selected: [],
      onEntry: p.onEntry,
      onToggleItem: p.onToggleItem,
      onActions: (name) => p.onAsk({ kind: "sheet", names: [name] }),
    });
    const row = elements(list).find((e) => e.props["data-entry"] !== undefined)!;
    expect(row.props["data-entry"]).toBe(CJK);
    const html = renderToStaticMarkup(list);
    expect(html).toContain(CJK);

    // Rename's text box starts with the name; the new name goes back with the old one as-is.
    const r = props({ ask: { kind: "rename", name: CJK } });
    const prompt = elements(FilesView(r)).find((e) => e.type === TextPrompt)!;
    expect(prompt.props.initial).toBe(CJK);
    (prompt.props.onSubmit as (v: string) => void)(`${CJK} 2`);
    expect(r.onRename).toHaveBeenCalledWith(CJK, `${CJK} 2`);

    await renameEntry("/data/games", CJK, `${CJK} 2`);
    expect(mockCall).toHaveBeenCalledWith("/api/fs/rename", {
      from: `/data/games/${CJK}`,
      to: `/data/games/${CJK} 2`,
    });
    // And it survives JSON both ways (what call() sends).
    expect(JSON.parse(JSON.stringify(mockCall.mock.calls[0][1])).from).toBe(`/data/games/${CJK}`);
  });

  it("the drive list shows each drive with its free space", () => {
    const html = renderToStaticMarkup(<FilesView {...props({ path: null })} />);
    expect(html).toContain(t("cw_home_vol_internal"));
    expect(html).toContain(t("cw_home_vol_usb"));
    expect(html.split("data-drive=").length - 1).toBe(2);
  });

  it("○ goes up one folder, and from a drive's top to the drive list", () => {
    const vols = [vol("/data"), vol("/mnt/usb0")];
    expect(upFrom("/data/a/b", vols)).toBe("/data/a");
    expect(upFrom("/data", vols)).toBeNull();
    expect(upFrom("/mnt/usb0/x", vols)).toBe("/mnt/usb0");
    expect(upFrom("/mnt/usb0", vols)).toBeNull();
    expect(upFrom(null, vols)).toBeNull();
  });

  it("copy and move are off while a batch runs; it shows which item and a Stop", () => {
    const onStopBatch = vi.fn();
    const p = props({
      ask: { kind: "sheet", names: ["a.txt"] },
      busy: { op: "copy", at: 3, total: 40, stopping: false },
      onStopBatch,
    });
    const sheet = elements(FilesView(p)).find((e) => e.type === ActionsSheet)!;
    expect(sheet.props.busy).toBe(true);
    const html = renderToStaticMarkup(<FilesView {...p} />);
    expect(html).toContain(t("cw_files_batch_copy", { at: 3, total: 40 }));
    const stop = elements(FilesView(p)).find((e) => e.props["data-action"] === "stop-batch")!;
    (stop.props.onClick as () => void)();
    expect(onStopBatch).toHaveBeenCalled();
    // Once stopping, no second Stop.
    const stopping = props({ busy: { op: "delete", at: 2, total: 5, stopping: true }, onStopBatch });
    expect(renderToStaticMarkup(<FilesView {...stopping} />)).toContain(t("cw_files_batch_stopping"));
    expect(elements(FilesView(stopping)).some((e) => e.props["data-action"] === "stop-batch")).toBe(false);
  });

  it("a single copy, move or delete can be stopped too: Stop cancels the item running", () => {
    for (const op of ["copy", "move", "delete", "chmod"] as const) {
      const p = props({ busy: { op, at: 1, total: 1, stopping: false, cancels: true }, onStopBatch: vi.fn() });
      expect(elements(FilesView(p)).some((e) => e.props["data-action"] === "stop-batch"), op).toBe(true);
    }
    // What it says while the job stops: the copy by name, the others in general, never "after this item".
    expect(busyText({ op: "copy", at: 1, total: 1, stopping: true, cancels: true })).toBe(t("cw_files_smb_stopping"));
    expect(busyText({ op: "delete", at: 1, total: 1, stopping: true, cancels: true })).toBe(t("cw_files_batch_cancelling"));
    expect(busyText({ op: "move", at: 1, total: 3, stopping: true, cancels: true })).toBe(t("cw_files_batch_cancelling"));
  });

  it("a finished batch says what failed, what a stop left out, else done", () => {
    const r = { started: [], moved: [], failed: [], jobFailed: [], skipped: [] };
    expect(batchMessage(r)).toEqual({ tone: "info", text: t("cw_files_done") });
    expect(batchMessage({ ...r, jobFailed: ["/a"] })?.text).toBe(t("cw_files_batch_failed", { n: 1 }));
    expect(batchMessage({ ...r, skipped: ["/a", "/b"] })?.text).toBe(t("cw_files_batch_stopped", { n: 2 }));
    expect(batchMessage({ ...r, failed: [{ item: "/a", error: new ApiError(403, "x") }] })?.tone).toBe("error");
    // Stop cancelled the one running: it is named (it may be partly done), the rest counted.
    expect(batchMessage({ ...r, cancelled: ["/data/G"] })).toEqual({ tone: "info", text: t("cw_files_batch_cut_short", { name: "G" }) });
    expect(batchMessage({ ...r, cancelled: ["/data/G"], skipped: ["/data/H", "/data/I"] })?.text).toBe(
      `${t("cw_files_batch_cut_short", { name: "G" })} ${t("cw_files_batch_stopped", { n: 2 })}`,
    );
  });

  it("only images and text open in the viewer", () => {
    expect(viewKind("a.PNG")).toBe("image");
    expect(viewKind("a.jpg")).toBe("image");
    expect(viewKind("x.log")).toBe("text");
    expect(viewKind("x.xml")).toBe("text");
    expect(viewKind("eboot.bin")).toBeNull();
    expect(viewKind("noext")).toBeNull();
  });

  it("select mode: ✕ on a row toggles it instead of opening", () => {
    const onEntry = vi.fn();
    const onToggleItem = vi.fn();
    const list = DirList({
      entries: [entry("a.txt")],
      selecting: true,
      selected: ["a.txt"],
      onEntry,
      onToggleItem,
      onActions: () => {},
    });
    const main = elements(list).find((e) => e.props["data-entry-main"] !== undefined)!;
    expect(main.props["aria-pressed"]).toBe(true);
    (main.props.onClick as () => void)();
    expect(onToggleItem).toHaveBeenCalledWith("a.txt");
    expect(onEntry).not.toHaveBeenCalled();
  });

  it("errors name what went wrong", () => {
    expect(filesErrorText(new ApiError(403, "forbidden"))).toBe(t("cw_files_err_denied"));
    expect(filesErrorText(new ApiError(409, "exists"))).toBe(t("cw_files_err_exists"));
    expect(filesErrorText(new ApiError(401, "token"))).toBeNull();
    expect(filesErrorText(new IntoItselfError("/data/a"))).toBe(t("cw_files_err_into_itself"));
  });

  it("409_shows_exists_only_for_an_exist_code", () => {
    expect(filesErrorText(new ApiError(409, "the destination already exists"))).toBe(t("cw_files_err_exists"));
    expect(filesErrorText(new ApiError(409, "busy"))).toBe(t("cw_home_err_busy"));
    expect(filesErrorText(new ApiError(409, "job_in_progress"))).toBe(
      t("cw_home_err_failed", { code: "job_in_progress" }),
    );
  });
});

describe("Files listing controls", () => {
  const many = Array.from({ length: 700 }, (_, i) => entry(`f${i}.txt`));
  it("shows the toolbar, status line and a row window with a Show more rows button", () => {
    const html = renderToStaticMarkup(
      <>
        {FilesView(
          props({
            dir: dir(many),
            shown: many,
            status: { items: 700, total: 700, selected: 1, bytes: 10, folders: 0 },
            rowLimit: 300,
            onMoreRows: vi.fn(),
            onView: vi.fn(),
            selecting: true,
            selected: ["f1.txt"],
          }),
        )}
      </>,
    );
    expect(html).toContain('data-action="select-all"');
    expect(html).not.toContain('data-action="sortview"'); // the selection bar replaces the right side
    expect(html).not.toContain('data-action="filter-clear"');
    const filtered = renderToStaticMarkup(
      <>
        {FilesView(
          props({
            dir: dir(many),
            shown: many,
            status: { items: 700, total: 700, selected: 0, bytes: 0, folders: 0 },
            view: { sort: "name", dir: "asc", hidden: false, filter: "f1" },
            onView: vi.fn(),
            selecting: true,
          }),
        )}
      </>,
    );
    expect(filtered).toContain('data-action="filter-clear"');
    expect(filtered).toContain(t("cw_files_filter_on"));
    const plain = renderToStaticMarkup(<>{FilesView(props({ onView: vi.fn(), onGoTo: vi.fn(), prefs: { favorites: [], recent: [], last: null }, onPin: vi.fn(), onUnpin: vi.fn() }))}</>);
    for (const a of ["pin", "goto", "mkdir", "select", "sortview", "filter"]) expect(plain).toContain(`data-action="${a}"`);
    expect(plain).toContain('data-tip="Sort and view"');
    expect(html).toContain("700 items · 1 selected (10 B)");
    expect(html).toContain('data-action="more-rows"');
    expect(html.match(/data-entry="/g)).toHaveLength(300);
  });
  it("offers Load all when more pages wait, and explains an empty filter", () => {
    const more = renderToStaticMarkup(<>{FilesView(props({ dir: dir([entry("a")], { more: true }), onView: vi.fn(), onLoadAll: vi.fn() }))}</>);
    expect(more).toContain('data-action="load-all"');
    const none = renderToStaticMarkup(<>{FilesView(props({ dir: dir([entry("a")]), shown: [], onView: vi.fn() }))}</>);
    expect(none).toContain(t("cw_files_none_match"));
  });

  it("clipboard: Copy / Cut in the sheet, the bar in the status line, kept across folders", () => {
    for (const names of [["b"], ["a.txt", "b", "c"]]) {
      const p = props({ ask: { kind: "sheet", names }, onClip: vi.fn() });
      const sheet = elements(FilesView(p)).find((e) => e.type === ActionsSheet)!;
      expect(sheet.props.actions).toEqual(expect.arrayContaining(["clipcopy", "cut"]));
      (sheet.props.onChoose as (a: string) => void)("cut");
      expect(p.onClip).toHaveBeenCalledWith("cut", names);
      (sheet.props.onChoose as (a: string) => void)("clipcopy");
      expect(p.onClip).toHaveBeenCalledWith("copy", names);
    }
    clearClipboard();
    setClipboard("cut", ["x", "y", "z"].map((n) => ({ path: `/data/a/${n}`, dir: false })));
    // Another folder, another render: the clipboard is page state, not the folder's.
    for (const path of ["/data/games", "/mnt/usb0"]) {
      const onPaste = vi.fn();
      const onClipClear = vi.fn();
      const p = props({
        path,
        clip: getClipboard(),
        onPaste,
        onClipClear,
        status: { items: 3, total: 3, selected: 0, bytes: 0, folders: 0 },
      });
      const html = renderToStaticMarkup(<>{FilesView(p)}</>);
      expect(html).toContain("3 items to paste (cut)");
      expect(html).toContain('data-action="paste"');
      expect(html).toContain('data-action="clip-clear"');
      // In the status line's row, beside the item count.
      expect(html).toMatch(/cw-status-row.*3 items.*cw-clip/);
    }
    expect(getClipboard()?.items).toHaveLength(3);
    const one = renderToStaticMarkup(
      <>{FilesView(props({ clip: { mode: "copy", items: [{ path: "/data/a/x.txt", dir: false }] }, onPaste: vi.fn() }))}</>,
    );
    expect(one).toContain("x.txt to paste (copy)");
    // A batch running: Paste waits for it.
    const busy = renderToStaticMarkup(
      <>{FilesView(props({ clip: getClipboard(), onPaste: vi.fn(), busy: { op: "copy", at: 1, total: 2, stopping: false } }))}</>,
    );
    expect(busy).toMatch(/data-action="paste" disabled=""/);
    clearClipboard();
  });

  it("a waiting paste conflict shows its dialog over anything else", () => {
    const answer = vi.fn();
    const conflict = { item: "/data/a/x.txt", dest: "/data/games/x.txt", destDir: false, canReplace: true, left: 2 };
    const p = props({ ask: { kind: "sheet", names: ["b"] }, conflict: { conflict, answer } });
    const els = elements(FilesView(p));
    const d = els.find((e) => e.type === ConflictDialog)!;
    expect(els.find((e) => e.type === ActionsSheet)).toBeUndefined();
    (d.props.onAnswer as (x: unknown) => void)({ choice: "skip", all: false });
    expect(answer).toHaveBeenCalledWith({ choice: "skip", all: false });

    const html = renderToStaticMarkup(<ConflictDialog conflict={conflict} onAnswer={vi.fn()} />);
    expect(html).toContain(t("cw_files_conflict_title", { name: "x.txt" }));
    for (const c of ["skip", "keep", "replace"]) expect(html).toContain(`data-conflict="${c}"`);
    expect(html).toContain(t("cw_files_conflict_all", { n: 2 }));
    // Skip is the safe default; Replace is in the danger colour.
    expect(html).toMatch(/data-conflict="skip" data-nav-default/);
    expect(html).toMatch(/is-danger" data-conflict="replace"/);
    // The item itself (copy into its own folder): no Replace; the last one: no Apply to all.
    const self = renderToStaticMarkup(
      <ConflictDialog conflict={{ ...conflict, canReplace: false, left: 0 }} onAnswer={vi.fn()} />,
    );
    expect(self).not.toContain('data-conflict="replace"');
    expect(self).not.toContain('data-action="conflict-all"');
    // Confirming Replace: the dialog stays mounted under the confirm (○ returns to Replace).
    const asked = renderToStaticMarkup(<ConflictDialog conflict={conflict} onAnswer={vi.fn()} asking />);
    expect(asked).toContain('data-conflict="replace"');
    expect(asked.indexOf('data-conflict="replace"')).toBeLessThan(asked.indexOf('role="alertdialog"'));
  });

  it("paste messages: skipped, already here, failures", () => {
    const base = { started: [], moved: [], failed: [], jobFailed: [], skipped: [], done: [], left: [], halted: [], declined: [], here: [] };
    expect(pasteMessage({ ...base, done: ["/a"] })?.text).toBe(t("cw_files_done"));
    expect(pasteMessage({ ...base, done: ["/a"], declined: ["/b", "/c"] })?.text).toBe(t("cw_files_paste_done_skipped", { n: 2 }));
    expect(pasteMessage({ ...base, here: ["/a"] })?.text).toBe(t("cw_files_paste_here_already"));
    expect(pasteMessage({ ...base, skipped: ["/a"] })?.text).toBe(t("cw_files_batch_stopped", { n: 1 }));
    expect(pasteMessage({ ...base, failed: [{ item: "/a", error: new IntoItselfError("/a") }] })?.text).toContain(
      t("cw_files_err_into_itself"),
    );
    // Stop during a Replace's copy: that item is named, the rest counted.
    expect(pasteMessage({ ...base, halted: ["/data/a/G"], skipped: ["/data/a/G"] })?.text).toBe(
      t("cw_files_paste_halted", { name: "G" }),
    );
    expect(pasteMessage({ ...base, halted: ["/data/a/G"], skipped: ["/data/a/G", "/data/a/H"] })?.text).toBe(
      `${t("cw_files_paste_halted", { name: "G" })} ${t("cw_files_batch_stopped", { n: 1 })}`,
    );
  });
});

describe("failed items stay selected after a batch", () => {
  it("keeps the folder's refused and failed items, once each, and nothing from elsewhere", () => {
    const r = {
      started: ["/data/a"],
      moved: [],
      failed: [
        { item: "/data/b", error: new Error("x") },
        { item: "/mnt/usb0/src/c", error: new Error("x") },
      ],
      jobFailed: ["/data/d", "/data/b"],
      skipped: ["/data/e"],
    };
    expect(failedNamesIn(r, "/data")).toEqual(["b", "d"]);
    expect(failedNamesIn(r, "/mnt/usb0")).toEqual([]);
    expect(failedNamesIn({ started: [], moved: [], failed: [], jobFailed: ["/x"], skipped: [] }, "/")).toEqual(["x"]);
  });
});
