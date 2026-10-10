import { afterEach, describe, expect, it, vi } from "vitest";

import type { GameMeta } from "../api/ps5";
import { GAME_META_TTL_MS, cachedGameMeta, forgetGameMeta, peekGameMeta } from "./gameMetaCache";

const meta = (title: string | null): GameMeta => ({
  title,
  title_id: title ? "PPSA01341" : null,
  content_id: null,
  content_version: null,
  application_category_type: null,
  has_icon: false,
});

afterEach(() => {
  forgetGameMeta();
  vi.useRealTimers();
});

describe("game meta cache", () => {
  it("reads a folder once and answers the next visit from memory", async () => {
    const load = vi.fn(async () => meta("Astro Bot"));
    await cachedGameMeta("192.168.86.100", "/data/homebrew/A", load);
    const again = await cachedGameMeta("192.168.86.100:9120", "/data/homebrew/A", load);
    expect(again.title).toBe("Astro Bot");
    expect(load).toHaveBeenCalledTimes(1);
    expect(peekGameMeta("192.168.86.100", "/data/homebrew/A")?.title).toBe("Astro Bot");
  });

  it("shares one request between concurrent rows", async () => {
    const load = vi.fn(async () => meta("Astro Bot"));
    await Promise.all([
      cachedGameMeta("h", "/p", load),
      cachedGameMeta("h", "/p", load),
    ]);
    expect(load).toHaveBeenCalledTimes(1);
  });

  it("keeps consoles apart", async () => {
    const load = vi.fn(async () => meta("Astro Bot"));
    await cachedGameMeta("10.0.0.1", "/p", load);
    await cachedGameMeta("10.0.0.2", "/p", load);
    expect(load).toHaveBeenCalledTimes(2);
  });

  it("does not keep a failed (blank) read", async () => {
    const load = vi.fn(async () => meta(null));
    await cachedGameMeta("h", "/p", load);
    await cachedGameMeta("h", "/p", load);
    expect(load).toHaveBeenCalledTimes(2);
  });

  it("reads again once the entry is older than the TTL", async () => {
    vi.useFakeTimers();
    const load = vi.fn(async () => meta("Astro Bot"));
    await cachedGameMeta("h", "/p", load);
    vi.advanceTimersByTime(GAME_META_TTL_MS + 1);
    await cachedGameMeta("h", "/p", load);
    expect(load).toHaveBeenCalledTimes(2);
  });
});
