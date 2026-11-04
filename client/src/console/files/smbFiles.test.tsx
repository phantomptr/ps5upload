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
import { DriveList } from "./DriveList";
import { busyText, filesErrorText, FilesView, type FilesViewProps } from "./Files";
import { PathBar } from "./PathBar";
import { PathPicker } from "./PathPicker";
import { fieldsOf, fieldsProblem, ShareDialog, shareErrorText } from "./ShareDialog";
import type { SmbShare } from "./smb";
import type { DirState, Entry } from "./useDir";

const mockCall = vi.mocked(call);
const ID = "0a1b2c3d";
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
const dir = (entries: Entry[]): DirState => ({ entries, more: false, loading: false, loaded: true, error: null });
const vol = (path: string): Volume => ({
  path,
  mount_from: "/dev/x",
  fs_type: "ufs",
  total_bytes: 1000,
  free_bytes: 400,
  writable: true,
});
const share = (over: Partial<SmbShare> = {}): SmbShare => ({
  id: ID,
  name: "NAS",
  host: "192.168.1.10",
  port: 445,
  share: "Media",
  user: "bob",
  domain: "",
  start_path: "",
  has_password: true,
  path: `smb://${ID}/`,
  ...over,
});

function props(over: Partial<FilesViewProps> = {}): FilesViewProps {
  return {
    path: `smb://${ID}/${CJK}`,
    volumes: [vol("/data")],
    volumesError: false,
    dir: dir([entry("big.bin", "file", 1 << 30), entry("中文", "dir")]),
    selecting: false,
    selected: [],
    ask: null,
    busy: null,
    message: null,
    tasks: [],
    shares: [share()],
    onShareSaved: vi.fn(),
    onShareRemove: vi.fn(),
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

describe("network shares in Files", () => {
  it("the drive list shows saved shares with their address, and Add", () => {
    const html = renderToStaticMarkup(
      <DriveList volumes={[vol("/data")]} error={false} onOpen={vi.fn()} shares={[share(), share({ id: "11111111", name: CJK, user: "" })]} onShareActions={vi.fn()} onAddShare={vi.fn()} />,
    );
    expect(html).toContain(t("cw_files_smb_title"));
    expect(html).toContain("192.168.1.10/Media");
    expect(html).toContain(CJK);
    expect(html).toContain(t("cw_files_smb_guest")); // the second one signs in as a guest
    expect(html).toContain('data-action="smb-add"');
    expect(html.split("data-share-open=").length - 1).toBe(2);
    // No shares yet: the hint and Add; no section at all for a helper without them.
    expect(renderToStaticMarkup(<DriveList volumes={[]} error={false} onOpen={vi.fn()} shares={[]} onAddShare={vi.fn()} />)).toContain(
      t("cw_files_smb_none"),
    );
    expect(renderToStaticMarkup(<DriveList volumes={[]} error={false} onOpen={vi.fn()} shares={null} />)).not.toContain(
      t("cw_files_smb_title"),
    );
  });

  it("a share's menu opens, edits or asks before removing it", () => {
    const p = props({ path: null, ask: { kind: "share", id: ID } });
    const sheet = elements(FilesView(p)).find((e) => e.type === ActionsSheet)!;
    expect(sheet.props.actions).toEqual(["open", "shareedit", "shareremove"]);
    (sheet.props.onChoose as (a: string) => void)("open");
    expect(p.onOpen).toHaveBeenCalledWith(`smb://${ID}/`);
    (sheet.props.onChoose as (a: string) => void)("shareremove");
    expect(p.onAsk).toHaveBeenCalledWith({ kind: "shareremove", id: ID });

    const r = props({ path: null, ask: { kind: "shareremove", id: ID } });
    const d = elements(FilesView(r)).find((e) => e.type === ConfirmDialog)!;
    expect(d.props.title).toBe(t("cw_files_smb_remove_title", { name: "NAS" }));
    expect(d.props.danger).toBe(true);
    (d.props.onConfirm as () => void)();
    expect(r.onShareRemove).toHaveBeenCalledWith(ID);
    expect(mockCall).not.toHaveBeenCalled();

    const e = props({ path: null, ask: { kind: "shareedit", id: ID } });
    const dlg = elements(FilesView(e)).find((x) => x.type === ShareDialog)!;
    expect((dlg.props.share as SmbShare).id).toBe(ID);
    const add = props({ path: null, ask: { kind: "shareedit", id: null } });
    expect(elements(FilesView(add)).find((x) => x.type === ShareDialog)!.props.share).toBeNull();
  });

  it("a share folder offers only listing tools, New folder and Copy", () => {
    const p = props();
    const html = renderToStaticMarkup(<FilesView {...p} />);
    expect(html).toContain('data-action="mkdir"');
    expect(html).toContain('data-action="select"');
    expect(html).not.toContain('data-action="newfile"');
    expect(html).not.toContain('data-action="pin"');
    expect(html).not.toContain('data-action="search"');
    expect(html).not.toContain('data-action="usage"');
    expect(html).not.toContain('data-action="goto"');
    // the path bar names the share, then the (non-ASCII) folder
    const bar = elements(FilesView(p)).find((e) => e.type === PathBar)!;
    expect(bar.props.shares).toEqual([share()]);
    expect(renderToStaticMarkup(<PathBar path={p.path!} volumes={[]} shares={[share()]} onOpen={vi.fn()} onUp={vi.fn()} />)).toContain(
      CJK,
    );

    const sheet = elements(FilesView(props({ ask: { kind: "sheet", names: ["big.bin"] } }))).find((e) => e.type === ActionsSheet)!;
    expect(sheet.props.actions).toEqual(["clipcopy", "copy"]);
    // Copy to… picks a console folder: the picker starts at the console's drives.
    const pick = elements(FilesView(props({ ask: { kind: "pick", op: "copy", names: ["big.bin"] } }))).find((e) => e.type === PathPicker)!;
    expect(pick.props.start).toBeNull();
  });

  it("a copy's line shows its bytes, and Stop shows for one item when it cancels", () => {
    const busy = { op: "copy" as const, at: 1, total: 1, stopping: false, cancels: true, bytesDone: 512 << 20, bytesTotal: 1 << 30 };
    expect(busyText(busy)).toContain(t("cw_home_percent", { n: 50 }));
    const html = renderToStaticMarkup(<FilesView {...props({ busy, onStopBatch: vi.fn() })} />);
    expect(html).toContain('data-action="stop-batch"');
    // Stop asked: it says the copy is being cancelled, not "after this item"
    expect(busyText({ ...busy, stopping: true })).toBe(t("cw_files_smb_stopping"));
    // a batch of one item that cannot cancel it has nothing for Stop to do
    expect(renderToStaticMarkup(<FilesView {...props({ path: "/data", busy: { ...busy, cancels: undefined }, onStopBatch: vi.fn() })} />)).not.toContain(
      'data-action="stop-batch"',
    );
  });

  it("SMB failures read as what happened", () => {
    expect(filesErrorText(new ApiError(403, "smb_auth"))).toBe(t("cw_files_smb_err_auth"));
    expect(filesErrorText(new ApiError(502, "smb_disconnected"))).toBe(t("cw_files_smb_err_disconnected"));
    expect(filesErrorText(new ApiError(403, "access_denied"))).toBe(t("cw_files_err_denied"));
  });
});

describe("the share dialog", () => {
  it("starts empty for a new share and from the saved one (never its password) when editing", () => {
    expect(fieldsOf(null)).toEqual({ server: "", share: "", user: "", password: "", domain: "", folder: "", name: "" });
    expect(fieldsOf(share({ port: 4450, start_path: "游戏" }))).toEqual({
      server: "192.168.1.10:4450",
      share: "Media",
      user: "bob",
      password: "",
      domain: "",
      folder: "游戏",
      name: "NAS",
    });
    expect(fieldsOf(share({ host: "fe80::1", port: 4450 })).server).toBe("[fe80::1]:4450");
  });

  it("needs a server and a share (a share in the server text counts)", () => {
    const f = fieldsOf(null);
    expect(fieldsProblem(f)).toBe(t("cw_files_smb_need_server"));
    expect(fieldsProblem({ ...f, server: "nas" })).toBe(t("cw_files_smb_need_share"));
    expect(fieldsProblem({ ...f, server: "nas", share: "Media" })).toBeNull();
    expect(fieldsProblem({ ...f, server: "smb://nas/Media" })).toBeNull();
    expect(fieldsProblem({ ...f, server: "\\\\nas\\Media" })).toBeNull();
  });

  it("explains a refused sign-in and an unknown failure", () => {
    expect(shareErrorText(new ApiError(403, "smb_auth"))).toBe(t("cw_files_smb_err_auth"));
    expect(shareErrorText(new ApiError(404, "smb_no_share"))).toBe(t("cw_files_smb_err_no_share"));
    expect(typeof shareErrorText(new Error("boom"))).toBe("string");
  });

  it("renders its fields, with the saved password left out", () => {
    const html = renderToStaticMarkup(<ShareDialog share={share()} onSaved={vi.fn()} onCancel={vi.fn()} />);
    for (const k of ["server", "share", "user", "password", "domain", "folder", "name"]) expect(html).toContain(`data-field="${k}"`);
    expect(html).toContain('type="password"');
    expect(html).toContain(t("cw_files_smb_password_kept"));
    expect(html).toContain('value="192.168.1.10"');
  });
});
