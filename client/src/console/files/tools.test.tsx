import { isValidElement, type ReactElement, type ReactNode } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it, vi } from "vitest";

vi.mock("../api", async (importOriginal) => {
  const real = await importOriginal<typeof import("../api")>();
  return { ...real, call: vi.fn(), callBlob: vi.fn() };
});

import { formatBytes } from "../../lib/format";
import { ApiError } from "../api";
import type { Volume } from "../home/Home";
import { t } from "../strings";
import { FilesView, type FilesViewProps } from "./Files";
import { PhoneUploadDialog } from "./UploadDialog";
import { SearchDialog, SearchView, indexText, resultsText } from "./search/SearchDialog";
import { INITIAL_SEARCH, type SearchState } from "./search/search";
import type { DirState, Entry } from "./useDir";
import { UsageDialog, UsageView, rowSize, usageLine } from "./usage/UsageDialog";
import type { UsageSnapshot } from "./usage/usage";

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
const vol = (path: string): Volume => ({ path, mount_from: "/dev/ssd0.user", fs_type: "ufs", total_bytes: 1000, free_bytes: 400, writable: true });
const ENTRIES = [entry("sce_sys", "dir"), entry("b.png"), entry("notes.txt"), entry("a.jpg"), entry("eboot.bin"), entry("c.PNG")];

function props(over: Partial<FilesViewProps> = {}): FilesViewProps {
  return {
    path: "/data/games/Astro",
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

describe("uploading from a phone", () => {
  it("a phone or PC gets Upload here; the console's own browser gets Upload from a phone instead", () => {
    const remote = renderToStaticMarkup(<>{FilesView(props({ remote: true }))}</>);
    expect(remote).toContain('data-action="upload"');
    expect(remote).not.toContain('data-action="phone-upload"');
    const tv = renderToStaticMarkup(<>{FilesView(props())}</>);
    expect(tv).toContain('data-action="phone-upload"');
    expect(tv).not.toContain('data-action="upload"');
  });

  it("Upload from a phone shows how: the folder and, with LAN access off, Settings one press away", () => {
    const onOpenSettings = vi.fn();
    const d = find(FilesView(props({ ask: { kind: "phoneupload" }, onOpenSettings })), PhoneUploadDialog);
    expect(d.props.dir).toBe("/data/games/Astro");
    expect(d.props.onOpenSettings).toBe(onOpenSettings);
    const html = renderToStaticMarkup(<PhoneUploadDialog dir="/data/games/Astro" onOpenSettings={onOpenSettings} onClose={vi.fn()} />);
    expect(html).toContain(t("cw_files_up_phone"));
  });
});

describe("search page", () => {
  it("is a Files tool: the toolbar button, and the dialog for the folder shown", () => {
    const onReveal = vi.fn();
    const html = renderToStaticMarkup(<>{FilesView(props({ onReveal }))}</>);
    expect(html).toContain('data-action="search"');
    expect(html).toContain('data-action="usage"');
    expect(renderToStaticMarkup(<>{FilesView(props())}</>)).not.toContain('data-action="search"');
    const d = find(FilesView(props({ onReveal, ask: { kind: "search" } })), SearchDialog);
    expect(d.props.folder).toBe("/data/games/Astro");
    (d.props.onReveal as (dir: string, name: string) => void)("/data/x", "eboot.bin");
    expect(onReveal).toHaveBeenCalledWith("/data/x", "eboot.bin");
  });

  const base = (over: Partial<SearchState>): SearchState => ({ ...INITIAL_SEARCH, ...over });
  const view = (s: SearchState, field = "eboot", ready = true) =>
    renderToStaticMarkup(
      <SearchView
        state={s}
        ready={ready}
        field={field}
        scope="all"
        folder="/data/g"
        onField={vi.fn()}
        onScope={vi.fn()}
        onSubmit={vi.fn()}
        onCancel={vi.fn()}
        onRebuild={vi.fn()}
        onReveal={vi.fn()}
        onClose={vi.fn()}
      />,
    );

  it("building: the count, a Cancel button, and the search that waits", () => {
    const s = base({ index: { phase: "building", files: 12345, truncated: false, cancelled: false, roots: ["/data"] }, pending: "*eboot*" });
    const html = view(s);
    expect(html).toContain(t("cw_files_search_building", { n: (12345).toLocaleString() }));
    expect(html).toContain('data-action="index-cancel"');
    expect(html).toContain(t("cw_files_search_waiting", { q: "*eboot*" }));
    expect(indexText(base({ ...s, stopping: true }))).toBe(t("cw_files_search_stopping"));
  });

  it("results: name, folder and size per row; more-than-shown said", () => {
    const s = base({
      index: { phase: "ready", files: 9, truncated: false, cancelled: true, roots: ["/data"] },
      done: { pattern: "*eboot*", scope: "all", results: { hits: [{ path: "/data/g/eboot.bin", name: "eboot.bin", folder: "/data/g", size: 2048 }], truncated: true } },
    });
    const html = view(s);
    expect(html).toContain('data-hit="/data/g/eboot.bin"');
    expect(html).toContain("eboot.bin");
    expect(html).toContain("/data/g");
    expect(html).toContain("2.00 KiB");
    expect(html).toContain(t("cw_files_search_more", { n: 1 }));
    expect(html).toContain(t("cw_files_search_partial"));
    expect(html).toContain('data-action="index-rebuild"');
    expect(resultsText(base({ done: { pattern: "*x*", scope: "all", results: { hits: [], truncated: false } } }))).toBe(
      t("cw_files_search_none", { q: "*x*" }),
    );
    // nothing in this folder but the helper stopped at its limit: not "nothing matches"
    expect(resultsText(base({ done: { pattern: "*x*", scope: "folder", results: { hits: [], truncated: true } } }))).toBe(
      t("cw_files_search_none_capped"),
    );
  });

  it("an older helper, a bad pattern, an error", () => {
    expect(view(base({ unsupported: true }))).toContain(t("cw_files_search_unsupported"));
    const bad = view(base({ index: { phase: "idle", files: 0, truncated: false, cancelled: false, roots: null } }), 'a"b');
    expect(bad).toContain(t("cw_files_search_bad"));
    expect(bad).toMatch(/data-action="search-go"[^>]*disabled/);
    expect(bad).toContain(t("cw_files_search_no_index"));
    expect(view(base({ error: new ApiError(500, "oom") }))).toContain("oom");
    // the drives have not loaded: nothing to search yet
    expect(view(base({}), "eboot", false)).toMatch(/data-action="search-go"[^>]*disabled/);
    expect(view(base({}), "eboot", true)).not.toMatch(/data-action="search-go"[^>]*disabled/);
  });
});

describe("disk usage page", () => {
  it("is a Files tool; a row opens its folder in Files", () => {
    const onReveal = vi.fn();
    const d = find(FilesView(props({ onReveal, ask: { kind: "usage" } })), UsageDialog);
    expect(d.props.folder).toBe("/data/games/Astro");
    (d.props.onOpen as (p: string) => void)("/data/games");
    expect(onReveal).toHaveBeenCalledWith("/data/games", null);
  });

  const snap: UsageSnapshot = {
    root: "/data",
    rows: [
      { name: "", path: "/data", files: true, bytes: 150, entries: 2, state: "done" },
      { name: "music", path: "/data/music", files: false, bytes: 700, entries: 1, state: "done" },
      { name: "games", path: "/data/games", files: false, bytes: 8000, entries: 3, state: "walking" },
      { name: "zz", path: "/data/zz", files: false, bytes: 0, entries: 0, state: "waiting" },
    ],
    measured: 1,
    folders: 3,
    entries: 6,
    end: null,
  };

  it("bars biggest first, drill-down and open buttons, Cancel while measuring", () => {
    const html = renderToStaticMarkup(
      <UsageView
        root="/data"
        canUp={false}
        snap={snap}
        refused={false}
        onUp={vi.fn()}
        onDrill={vi.fn()}
        onOpen={vi.fn()}
        onCancel={vi.fn()}
        onAgain={vi.fn()}
        onClose={vi.fn()}
      />,
    );
    const order = ["games", "music", "", "zz"].map((n) => html.indexOf(`data-usage-row="${n}"`));
    expect(order).toEqual([...order].sort((a, b) => a - b));
    expect(html).toContain('style="display:block;width:100%"');
    expect(html).toContain('data-usage-drill="/data/games"');
    expect(html).not.toContain('data-usage-drill="/data"');
    expect(html).toContain('data-action="usage-cancel"');
    expect(html).toMatch(/data-action="usage-up" aria-disabled="true"/);
    expect(html).toContain(t("cw_files_usage_running", { at: 1, n: 3, items: "6" }));
    expect(rowSize(snap.rows[2])).toMatch(/\+$/);
    expect(rowSize(snap.rows[3])).toBe("…");
  });

  it("how it ended", () => {
    expect(usageLine({ ...snap, end: "done" }, false)?.text).toBe(t("cw_files_usage_done_v2", { size: formatBytes(8850), items: "6" }));
    expect(usageLine({ ...snap, end: "cancelled" }, false)?.tone).toBe("info");
    expect(usageLine(null, true)?.text).toBe(t("cw_files_usage_busy"));
    expect(usageLine({ ...snap, end: "error", error: new ApiError(401, "token") }, false)).toBeNull();
  });
});
