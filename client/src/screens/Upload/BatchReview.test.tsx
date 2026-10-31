import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it, vi } from "vitest";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));

import { BatchReview, type BatchReviewProps } from "./BatchReview";

const row = (id: string, over: object = {}) =>
  ({
    id,
    path: `/g/${id}`,
    isDir: true,
    size: null,
    include: true,
    status: "ready",
    source: { kind: "game-folder", path: `/g/${id}`, meta: null, wrappedHint: null, zipInfo: null },
    error: null,
    password: null,
    ...over,
  }) as BatchReviewProps["rows"][number];

const props = (over: Partial<BatchReviewProps> = {}): BatchReviewProps => ({
  rows: [row("A"), row("B", { status: "needs-password", source: null, path: "/dl/B.rar", isDir: false })],
  check: { issues: new Map(), skip: new Set(), space: [], blocked: false },
  destFor: (r) => (r.source ? `/data/homebrew/${r.id}` : null),
  sizeFor: () => 1024,
  kindLabel: () => "Game folder",
  destination: <div data-slot="destination" />,
  strategy: "resume",
  onStrategy: () => {},
  options: null,
  addCount: 1,
  canAdd: true,
  onAdd: () => {},
  onRemove: () => {},
  onInclude: () => {},
  onPassword: () => {},
  onClear: () => {},
  ...over,
});

describe("the Upload review list", () => {
  it("shows each source, where it lands, and asks an encrypted archive for its password", () => {
    const out = renderToStaticMarkup(<BatchReview {...props()} />);
    expect(out).toContain("2 sources");
    expect(out).toContain("→ /data/homebrew/A");
    expect(out).toContain('type="password"');
    expect(out).toContain("Add 1 to queue");
    expect(out).toContain(`data-slot="destination"`);
  });

  it("shows what blocks adding, and disables Add", () => {
    const out = renderToStaticMarkup(
      <BatchReview
        {...props({
          canAdd: false,
          check: {
            issues: new Map([["A", [{ key: "batch_same_dest", vars: { dest: "/x" }, text: "same destination as another row (/x); remove one" }]]]),
            skip: new Set(),
            space: [{ key: "batch_space_short", vars: { drive: "/data", size: "2 GB", free: "1 GB" }, text: "/data: needs 2 GB, only 1 GB free" }],
            blocked: true,
          },
        })}
      />,
    );
    expect(out).toContain("same destination as another row (/x)");
    expect(out).toContain("/data: needs 2 GB, only 1 GB free");
    expect(out).toMatch(/<button[^>]*disabled=""[^>]*>(?:(?!<\/button>)[\s\S])*Upload 1 now/);
  });

  it("leads with Stream & install when the list holds packages, and says the rest upload", () => {
    const out = renderToStaticMarkup(
      <BatchReview {...props({ streamCount: 2, restCount: 1, canStream: true, onStream: () => {} })} />,
    );
    expect(out).toContain("Stream &amp; install 2");
    expect(out).toContain("everything else uploads as usual, in the same click.");
    // Stream & install comes first; Upload now is no longer the primary button.
    expect(out.indexOf("batch-stream-install")).toBeLessThan(out.indexOf("Upload 1 now"));
  });

  it("offers no Stream & install without packages", () => {
    const out = renderToStaticMarkup(<BatchReview {...props({ streamCount: 0, onStream: () => {} })} />);
    expect(out).not.toContain("batch-stream-install");
    expect(out).not.toContain("uploads as usual");
  });
});
