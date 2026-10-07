import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it, vi } from "vitest";

vi.mock("../state/lang", () => ({
  useTr: () =>
    (key: string, vars?: Record<string, string | number>, fallback?: string) => {
      let s = fallback ?? key;
      for (const [k, v] of Object.entries(vars ?? {})) s = s.replace(`{${k}}`, String(v));
      return s;
    },
}));

import {
  consolePickResult,
  pickerHiddenForViewer,
  PickerView,
  type PickerViewProps,
} from "./LocalPathPicker";

// No DOM here: render to markup and check what a user would see and could press.
const noop = () => {};
const dir = (name: string) => ({ name, path: `/games/${name}`, is_dir: true, size: 0 });
const file = (name: string, size = 1024) => ({ name, path: `/games/${name}`, is_dir: false, size });

function html(p: Partial<PickerViewProps>) {
  const props: PickerViewProps = {
    title: "Choose a file",
    mode: "file",
    entries: [],
    cwdLabel: "NAS › games",
    canGoUp: true,
    loading: false,
    error: null,
    hint: null,
    hasMore: false,
    granted: true,
    roots: [],
    remote: true,
    actions: false,
    onOpenDir: noop,
    onPickFile: noop,
    onUp: noop,
    onUseFolder: noop,
    onLoadMore: noop,
    onRetry: noop,
    onEditConnection: noop,
    onInstall: noop,
    onSend: noop,
    onCancel: noop,
    onRoot: noop,
    onRequestAccess: noop,
    ...p,
  };
  return renderToStaticMarkup(<PickerView {...props} />);
}

describe("the in-app picker", () => {
  it("filters files by extension but always shows folders", () => {
    const out = html({
      entries: [dir("ps5"), file("a.pkg"), file("notes.txt")],
      filters: [{ name: "PKG", extensions: ["pkg"] }],
    });
    expect(out).toContain("ps5");
    expect(out).toContain("a.pkg");
    expect(out).not.toContain("notes.txt");
  });

  it("offers Load more while the server has more", () => {
    expect(html({ entries: [file("a.pkg")], hasMore: true })).toContain("Load more");
    expect(html({ entries: [file("a.pkg")], hasMore: false })).not.toContain("Load more");
  });

  it("shows the hint and a way to fix the connection", () => {
    const out = html({
      error: "Sign-in failed: STATUS_ACCOUNT_DISABLED",
      hint: "The share's Guest account is disabled.",
    });
    expect(out).toContain("Guest account is disabled");
    expect(out).toContain("Edit connection");
    expect(out).toContain("Retry");
    // A local folder has no connection to edit.
    expect(html({ remote: false, error: "denied" })).not.toContain("Edit connection");
  });

  it("offers Install and Send on a .pkg only when opened for browsing", () => {
    const browsing = html({ entries: [file("a.pkg"), file("b.exfat")], actions: true });
    expect((browsing.match(/>Install</g) ?? []).length).toBe(1);
    expect((browsing.match(/>Send to PS5</g) ?? []).length).toBe(2);
    expect(html({ entries: [file("a.pkg")] })).not.toContain(">Install<");
  });

  it("names where it is, by server", () => {
    expect(html({ cwdLabel: "NAS › games" })).toContain("NAS › games");
  });
});

describe("picking a folder or an image", () => {
  it("offers files and the open folder in one browser", () => {
    const out = html({ mode: "any", entries: [dir("g"), file("PPSA01234.exfat")], remote: false });
    expect(out).toContain("Use this folder");
    // The image row is a live button, not a disabled one.
    expect(out).toMatch(/<button type="button" class="[^"]*"[^>]*>(?:(?!disabled).)*PPSA01234\.exfat/);
  });
});

describe("picking several", () => {
  it("gives each pickable row a checkbox and counts the picks on the Add button", () => {
    const out = html({
      multiple: true,
      selected: ["/games/a.pkg", "/games/b.pkg"],
      entries: [dir("sub"), file("a.pkg"), file("b.pkg"), file("c.pkg")],
    });
    // Files are pickable in file mode; the folder only opens.
    expect((out.match(/type="checkbox"/g) ?? []).length).toBe(3);
    expect((out.match(/checked=""/g) ?? []).length).toBe(2);
    expect(out).toContain("Add 2");
    // The hint wraps on a phone rather than being cut off mid-word.
    expect(out).toMatch(/<span class="(?:(?!truncate)[^"])*">Tick files in any folder/);
  });

  it("lets folders be ticked when picking folders", () => {
    const out = html({ multiple: true, mode: "folder", selected: [], entries: [dir("g1"), dir("g2")] });
    expect((out.match(/type="checkbox"/g) ?? []).length).toBe(2);
    // Nothing ticked yet: the Add button waits.
    expect(out).toMatch(/<button[^>]*disabled=""[^>]*>(?:(?!<\/button>).)*Add 0/);
  });

  it("offers only Add once folders are ticked, so the ticks are never dropped", () => {
    const none = html({ multiple: true, mode: "folder", selected: [], entries: [dir("g1")] });
    expect(none).toContain("Use this folder");
    const some = html({ multiple: true, mode: "folder", selected: ["/games/g1"], entries: [dir("g1")] });
    expect(some).not.toContain("Use this folder");
    expect(some).toContain("Add 1");
  });

  it("stays a plain one-tap list when picking one", () => {
    expect(html({ entries: [file("a.pkg")] })).not.toContain('type="checkbox"');
  });
});

describe("the console as a source", () => {
  it("answers a pick on the PS5 as a ps5:// path on its host", () => {
    expect(consolePickResult("192.168.86.99:9113", "/data/homebrew/G.exfat")).toBe(
      "ps5://192.168.86.99/data/homebrew/G.exfat",
    );
    expect(consolePickResult("10.0.0.2", "/")).toBe("ps5://10.0.0.2/");
  });
});

describe("viewing a package from the picker", () => {
  it("hides the picker while the viewer is open and brings it back when the viewer closes", () => {
    // Not viewing: the picker shows whatever the viewer is doing for someone else.
    expect(pickerHiddenForViewer(false, true)).toBe(false);
    // View pressed and the viewer is up: the picker steps aside, keeping its folder.
    expect(pickerHiddenForViewer(true, true)).toBe(true);
    // The viewer was closed: back to the picker, in the folder it was left in.
    expect(pickerHiddenForViewer(true, false)).toBe(false);
  });
});
