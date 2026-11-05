import { describe, expect, it } from "vitest";

import { queueItemViewPath } from "./queueView";

const base = { addr: "192.0.2.99:9113", sourcePath: "", sourceKind: "pkg" } as never;
const item = (over: object) => ({ ...(base as object), ...over }) as Parameters<typeof queueItemViewPath>[0];

describe("what a queue row opens in the viewer", () => {
  it("reads installs where the package is", () => {
    expect(queueItemViewPath(item({ sourceKind: "install", install: { via: "stream", source: "/dl/a.pkg" } }))).toBe("/dl/a.pkg");
    expect(
      queueItemViewPath(item({ sourceKind: "install", install: { via: "library", path: "/data/pkg/a.pkg" } })),
    ).toBe("ps5://192.0.2.99/data/pkg/a.pkg");
    expect(
      queueItemViewPath(item({ sourceKind: "install", install: { via: "external", pkg: { path: "/mnt/usb0/b.pkg" } } })),
    ).toBe("ps5://192.0.2.99/mnt/usb0/b.pkg");
    expect(queueItemViewPath(item({ sourceKind: "install", install: { via: "link", url: "https://x/a.pkg" } }))).toBeNull();
  });

  it("reads uploads from their source when it is a package, image or game folder", () => {
    expect(queueItemViewPath(item({ sourceKind: "pkg", sourcePath: "/dl/a.pkg" }))).toBe("/dl/a.pkg");
    expect(queueItemViewPath(item({ sourceKind: "image", sourcePath: "/g/G.exfat" }))).toBe("/g/G.exfat");
    expect(queueItemViewPath(item({ sourceKind: "folder", sourcePath: "/g/My Game" }))).toBe("/g/My Game");
    expect(queueItemViewPath(item({ sourceKind: "file", sourcePath: "/x/notes.txt" }))).toBeNull();
  });
});
