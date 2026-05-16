import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

// inspectFolder is called from pickFolder; we control its
// resolution timing to provoke the stale-result race.
vi.mock("../api/ps5", () => {
  return {
    inspectFolder: vi.fn(),
  };
});

import { inspectFolder, type FolderInspection } from "../api/ps5";
import { useUploadStore } from "./upload";

// Cast for type-safe vi.mocked() access without pulling extra deps.
const mockedInspect = vi.mocked(inspectFolder);
type InspectResolver = (v: FolderInspection) => void;
type InspectRejector = (e: unknown) => void;

function freshState() {
  // Zustand stores are module singletons. Reset between tests so
  // leaked source/destination from one case doesn't poison the next.
  useUploadStore.setState({
    source: null,
    detecting: false,
    detectError: null,
    mountAfterUpload: false,
    mountReadOnly: true,
    destinationVolume: null,
    destinationSubpath: "homebrew",
    excludeMode: "all",
  });
}

/** Build a fake inspectFolder result that flags the path as a game
 *  (meta_source !== "none") so pickFolder takes the game-folder
 *  branch. Field shape mirrors `FolderInspectResult` in api/ps5.ts —
 *  changes there will surface as compile errors here. */
function fakeInspectResult() {
  return {
    result: {
      path: "/folder",
      title: null,
      title_id: null,
      content_id: null,
      content_version: null,
      application_category_type: null,
      icon0_path: null,
      total_size: 0,
      file_count: 0,
      skipped_paths: [],
      meta_source: "param.sfo" as const,
    },
    wrapped_hint: null,
  };
}

describe("useUploadStore.pickFolder stale-result race", () => {
  beforeEach(() => {
    freshState();
    mockedInspect.mockReset();
  });
  afterEach(() => {
    freshState();
  });

  it("ignores an inspect that resolves after the user picks a different folder", async () => {
    // Two inspects in flight: A's resolves AFTER B's, so without
    // the guard A would clobber source.path back to /folderA.
    let resolveA!: InspectResolver;
    let resolveB!: InspectResolver;
    const pendingA = new Promise<FolderInspection>((r) => {
      resolveA = r;
    });
    const pendingB = new Promise<FolderInspection>((r) => {
      resolveB = r;
    });
    mockedInspect
      .mockImplementationOnce(() => pendingA)
      .mockImplementationOnce(() => pendingB);

    const store = useUploadStore.getState();
    const pickAPromise = store.pickFolder("/folderA");
    // After the first set, optimistic source is /folderA.
    expect(useUploadStore.getState().source?.path).toBe("/folderA");
    const pickBPromise = store.pickFolder("/folderB");
    // Optimistic set for B overrides.
    expect(useUploadStore.getState().source?.path).toBe("/folderB");

    // B's inspect resolves first → applies cleanly.
    resolveB(fakeInspectResult());
    await pickBPromise;
    expect(useUploadStore.getState().source?.path).toBe("/folderB");
    expect(useUploadStore.getState().source?.kind).toBe("game-folder");

    // Now A's inspect (the stale one) resolves. The guard must
    // drop the result so source.path stays /folderB.
    resolveA(fakeInspectResult());
    await pickAPromise;
    expect(useUploadStore.getState().source?.path).toBe("/folderB");
  });

  it("ignores an inspect that resolves after a follow-up pickFile", async () => {
    let resolveA!: InspectResolver;
    mockedInspect.mockImplementationOnce(
      () =>
        new Promise<FolderInspection>((r) => {
          resolveA = r;
        }),
    );

    const store = useUploadStore.getState();
    const pickAPromise = store.pickFolder("/folderA");
    expect(useUploadStore.getState().source?.path).toBe("/folderA");

    // User abandons the folder pick and picks a file instead.
    await store.pickFile("/file.ffpkg");
    expect(useUploadStore.getState().source?.path).toBe("/file.ffpkg");
    expect(useUploadStore.getState().source?.kind).toBe("image");

    // Stale folder inspect resolves — must not touch source.
    resolveA(fakeInspectResult());
    await pickAPromise;
    expect(useUploadStore.getState().source?.path).toBe("/file.ffpkg");
    expect(useUploadStore.getState().source?.kind).toBe("image");
  });

  it("ignores a stale inspect failure too (doesn't surface old error)", async () => {
    let rejectA!: InspectRejector;
    mockedInspect.mockImplementationOnce(
      () =>
        new Promise<FolderInspection>((_resolve, reject) => {
          rejectA = reject;
        }),
    );

    const store = useUploadStore.getState();
    const pickAPromise = store.pickFolder("/folderA");
    await store.pickFile("/file.ffpkg");
    expect(useUploadStore.getState().detectError).toBeNull();

    rejectA(new Error("stale failure for /folderA"));
    await pickAPromise;
    // detectError must stay null — the failure was for an
    // abandoned pick. Surfacing it would confuse the user about a
    // source they're no longer looking at.
    expect(useUploadStore.getState().detectError).toBeNull();
    expect(useUploadStore.getState().source?.path).toBe("/file.ffpkg");
  });

  it("applies the inspect result normally when nothing else races", async () => {
    mockedInspect.mockResolvedValueOnce(fakeInspectResult());

    const store = useUploadStore.getState();
    await store.pickFolder("/folderOnly");
    expect(useUploadStore.getState().source?.path).toBe("/folderOnly");
    expect(useUploadStore.getState().source?.kind).toBe("game-folder");
    expect(useUploadStore.getState().detecting).toBe(false);
  });
});
