import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it, vi } from "vitest";
import { FilePropertiesView } from "./FileProperties";

vi.mock("../../state/lang", () => ({
  useLangStore: { getState: () => ({ lang: "en" }) },
  useTr: () => (k: string, _v: unknown, fallback?: string) => fallback ?? k,
}));

describe("file properties", () => {
  const props = {
    path: "/data/icon0.png",
    stat: { kind: "file", size: 123, mtime: 0, mode: 0o100644, dev: 1 },
    mode: "0644",
    recursive: false,
    busy: false,
    error: null,
    fsType: "ufs",
    onMode: vi.fn(),
    onRecursive: vi.fn(),
    onApply: vi.fn(),
    onClose: vi.fn(),
  };
  it("shows current metadata and allows making an icon read-only without enabling recursion", () => {
    const html = renderToStaticMarkup(
      <FilePropertiesView {...props} mode="0444" />,
    );
    expect(html).toContain("/data/icon0.png");
    expect(html).toContain("123");
    expect(html).toContain("0644");
    expect(html).toContain('value="0444"');
    expect(html).not.toContain("Apply to everything inside");
  });
  it("rejects invalid octal permission input", () => {
    const html = renderToStaticMarkup(
      <FilePropertiesView {...props} mode="888" />,
    );
    const apply = html
      .match(/<button\b[^>]*>[\s\S]*?<\/button>/g)
      ?.find((button) => button.includes(">Apply<"));
    expect(apply).toContain('disabled=""');
  });
  it("explains that exFAT does not retain permissions and offers recursion only for folders", () => {
    const html = renderToStaticMarkup(
      <FilePropertiesView
        {...props}
        fsType="exfat"
        stat={{ ...props.stat, kind: "dir" }}
      />,
    );
    expect(html).toContain("does not keep Unix permissions");
    expect(html).toContain("Apply to everything inside");
  });
});
