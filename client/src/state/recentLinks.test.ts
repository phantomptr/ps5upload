import { beforeEach, describe, expect, it } from "vitest";

import {
  MAX_RECENT_LINKS,
  linkLabel,
  recentLinksFor,
  useRecentLinksStore,
} from "./recentLinks";

const A = "10.0.0.5";
const B = "10.0.0.6";
const s = () => useRecentLinksStore.getState();

beforeEach(() => useRecentLinksStore.setState({ byHost: {} }));

describe("recent links", () => {
  it("remembers a link with the name the user gave it, newest first, per console", () => {
    s().remember(
      A,
      { url: "https://x/a.pkg", name: "Astro base", mode: "stream" },
      1,
    );
    s().remember(A, { url: "https://x/b.pkg", name: "", mode: "direct" }, 2);
    expect(recentLinksFor(s(), A).map((l) => l.url)).toEqual([
      "https://x/b.pkg",
      "https://x/a.pkg",
    ]);
    expect(recentLinksFor(s(), A)[1]).toMatchObject({
      name: "Astro base",
      mode: "stream",
    });
    expect(recentLinksFor(s(), B)).toEqual([]);
  });

  it("using a link again moves it to the top and keeps its name unless a new one is given", () => {
    s().remember(
      A,
      { url: "https://x/a.pkg", name: "Astro base", mode: "stream" },
      1,
    );
    s().remember(A, { url: "https://x/b.pkg", name: "", mode: "stream" }, 2);
    s().remember(A, { url: "https://x/a.pkg", name: "", mode: "download" }, 3);
    const list = recentLinksFor(s(), A);
    expect(list.map((l) => l.url)).toEqual([
      "https://x/a.pkg",
      "https://x/b.pkg",
    ]);
    expect(list[0]).toMatchObject({
      name: "Astro base",
      mode: "download",
      usedAt: 3,
    });
    s().remember(
      A,
      { url: "https://x/a.pkg", name: "Astro v2", mode: "download" },
      4,
    );
    expect(recentLinksFor(s(), A)[0].name).toBe("Astro v2");
  });

  it("keeps a bounded list, renames and forgets", () => {
    for (let i = 0; i < MAX_RECENT_LINKS + 5; i++) {
      s().remember(
        A,
        { url: `https://x/${i}.pkg`, name: "", mode: "stream" },
        i,
      );
    }
    expect(recentLinksFor(s(), A)).toHaveLength(MAX_RECENT_LINKS);
    const top = recentLinksFor(s(), A)[0].url;
    s().rename(A, top, "  The newest  ");
    expect(recentLinksFor(s(), A)[0].name).toBe("The newest");
    s().forget(A, top);
    expect(recentLinksFor(s(), A).some((l) => l.url === top)).toBe(false);
  });

  it("labels a link by its name, else by the file its address ends in, else by its host", () => {
    expect(linkLabel({ url: "https://x/a.pkg", name: "Astro base" })).toBe(
      "Astro base",
    );
    expect(
      linkLabel({ url: "https://x/dl/My%20Game.pkg?token=1", name: "" }),
    ).toBe("My Game.pkg");
    expect(linkLabel({ url: "https://files.example.com/", name: "" })).toBe(
      "files.example.com",
    );
    expect(linkLabel({ url: "not a url", name: "" })).toBe("not a url");
  });
});
