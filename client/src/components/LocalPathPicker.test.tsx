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

import { PickerView, type PickerViewProps } from "./LocalPathPicker";

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
