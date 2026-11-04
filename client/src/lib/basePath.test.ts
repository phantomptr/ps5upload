import { afterEach, describe, expect, it, vi } from "vitest";

import { pageBasePath } from "./basePath";

const withMeta = (content: string | null) =>
  vi.stubGlobal("document", {
    querySelector: () =>
      content === null ? null : { getAttribute: () => content },
  });

describe("pageBasePath", () => {
  afterEach(() => {
    vi.unstubAllGlobals();
    vi.unstubAllEnvs();
  });

  it("reads what the engine wrote into the page", () => {
    withMeta("/ps5");
    expect(pageBasePath()).toBe("/ps5");
    withMeta("");
    expect(pageBasePath()).toBe("");
  });

  it("the page's word beats a prefix baked into the build", () => {
    vi.stubEnv("VITE_BASE_URL", "/old/");
    withMeta("/ps5");
    expect(pageBasePath()).toBe("/ps5");
  });

  it("falls back to a baked prefix, and a relative build has none", () => {
    withMeta(null);
    vi.stubEnv("VITE_BASE_URL", "/ps5/");
    expect(pageBasePath()).toBe("/ps5");
    vi.stubEnv("VITE_BASE_URL", "./");
    expect(pageBasePath()).toBe("");
    vi.stubEnv("VITE_BASE_URL", "/");
    expect(pageBasePath()).toBe("");
    vi.stubEnv("VITE_BASE_URL", "");
    expect(pageBasePath()).toBe("");
  });
});
