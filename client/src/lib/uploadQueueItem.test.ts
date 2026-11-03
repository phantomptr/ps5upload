import { describe, expect, it } from "vitest";

import { buildUploadQueueItem, type UploadItemOptions } from "./uploadQueueItem";

const opts: UploadItemOptions = {
  addr: "10.0.0.2:9113",
  destinationVolume: "/data",
  destinationSubpath: "homebrew",
  archiveIntoSubfolder: true,
  reconcileMode: "fast",
  strategy: "resume",
  excludes: [".DS_Store"],
  mountAfterUpload: true,
  mountReadOnly: true,
  registerAfterUpload: true,
  installAfterUpload: true,
  deletePkgAfterInstall: false,
  pkgDir: "/mnt/ext0/ps5upload/pkg_library",
  nonce: "abc",
  now: 1_700_000_000_000,
};
const src = (over: object) => ({ meta: null, wrappedHint: null, zipInfo: null, ...over }) as never;

describe("a picked source as a queue item", () => {
  it("stages a package on the package drive and installs it", () => {
    const it = buildUploadQueueItem(
      src({
        kind: "pkg",
        path: "/dl/g.pkg",
        pkgInfo: { contentId: "UP9000-PPSA01234_00-GAME000000000000", title: "Game", category: "gp", totalBytes: 1 },
      }),
      null,
      opts,
    );
    expect(it.sourceKind).toBe("pkg");
    expect(it.resolvedDest.startsWith("/mnt/ext0/ps5upload/pkg_library/")).toBe(true);
    expect(it).toMatchObject({ displayName: "Game", installAfterUpload: true, strategy: "overwrite", excludes: [] });
  });

  it("lands an image, folder or archive under the destination with only its own options", () => {
    const img = buildUploadQueueItem(src({ kind: "image", path: "/g/G.exfat" }), null, opts);
    expect(img).toMatchObject({ resolvedDest: "/data/homebrew/G.exfat", mountAfterUpload: true, registerAfterUpload: false });
    const game = buildUploadQueueItem(src({ kind: "game-folder", path: "/g/My Game" }), null, opts);
    expect(game).toMatchObject({ resolvedDest: "/data/homebrew/My Game", mountAfterUpload: false, registerAfterUpload: true });
    const arc = buildUploadQueueItem(src({ kind: "archive", path: "/dl/G.rar" }), "pw", opts);
    expect(arc).toMatchObject({ resolvedDest: "/data/homebrew/G", rarPassword: "pw", strategy: "resume" });
  });

  it("sends an archive as the file it is when asked (#645)", () => {
    const arc = buildUploadQueueItem(src({ kind: "archive", path: "/dl/y2jb.zip" }), "pw", {
      ...opts,
      sendArchiveAsIs: true,
    });
    expect(arc).toMatchObject({
      sourceKind: "file",
      sourcePath: "/dl/y2jb.zip",
      resolvedDest: "/data/homebrew/y2jb.zip",
      rarPassword: null,
    });
  });
});
