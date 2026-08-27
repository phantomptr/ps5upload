import { describe, expect, it } from "vitest";
import { retryUrl } from "./useImageRetry";

describe("retryUrl", () => {
  const cover =
    "http://127.0.0.1:19113/api/ps5/app-icon?addr=10.0.0.5%3A9114&title_id=CUSA00900";

  it("returns the original url on the first attempt", () => {
    expect(retryUrl(cover, 0, false)).toBe(cover);
  });

  it("merges the cache-buster into an existing query string", () => {
    // Every cover URL already has `?addr=...`, so a retry must append with
    // `&`. A second `?` would make the engine reject the request and the
    // retry would fail permanently — the exact bug this is meant to fix.
    const out = retryUrl(cover, 1, false);
    expect(out).toBe(`${cover}&_retry=1`);
    expect(out?.match(/\?/g)).toHaveLength(1);
  });

  it("uses ? when the url has no query string", () => {
    expect(retryUrl("http://host/icon.png", 2, false)).toBe(
      "http://host/icon.png?_retry=2",
    );
  });

  it("changes on every attempt so the browser cannot serve the failure from cache", () => {
    const first = retryUrl(cover, 1, false);
    const second = retryUrl(cover, 2, false);
    expect(first).not.toBe(second);
  });

  it("returns null once the retries are spent", () => {
    expect(retryUrl(cover, 2, true)).toBeNull();
  });

  it("returns null when there is no source", () => {
    expect(retryUrl(null, 0, false)).toBeNull();
  });
});
