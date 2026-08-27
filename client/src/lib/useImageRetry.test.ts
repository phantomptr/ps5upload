import { describe, expect, it } from "vitest";
import { pickImageSrc, retryUrl, shouldSkipDirect } from "./useImageRetry";

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

describe("pickImageSrc", () => {
  const cover = "http://127.0.0.1:19113/api/ps5/app-icon?addr=x&title_id=Y";
  const data = "data:image/png;base64,AAAA";
  const viaIpc = "data:image/png;base64,BBBB";

  it("uses cached bytes over everything, so a revisit needs no transport", () => {
    // The whole point of the fix: before this, a re-mounted screen ignored
    // bytes it was already holding and re-ran the transport negotiation.
    expect(pickImageSrc(data, viaIpc, cover, 0, false)).toBe(data);
  });

  it("prefers a resolved fallback over the direct url", () => {
    // The fallback only exists because the direct url already failed.
    expect(pickImageSrc(undefined, viaIpc, cover, 1, false)).toBe(viaIpc);
  });

  it("falls back to the direct url when nothing else is available", () => {
    expect(pickImageSrc(undefined, null, cover, 0, false)).toBe(cover);
  });

  it("still yields cached bytes after the direct url has given up", () => {
    // `failed` only condemns the direct transport, never the image.
    expect(pickImageSrc(data, null, cover, 2, true)).toBe(data);
  });

  it("yields null when there is nothing at all to show", () => {
    expect(pickImageSrc(undefined, null, cover, 2, true)).toBeNull();
    expect(pickImageSrc(null, null, null, 0, false)).toBeNull();
  });
});

describe("shouldSkipDirect", () => {
  const cover = "http://127.0.0.1:19113/api/ps5/app-icon?addr=x&title_id=Y";

  it("skips the direct attempt once the session knows it is blocked", () => {
    // This is what stops every cover paying two failures and a 1.2-2.4s
    // stagger before reaching the transport that works.
    expect(shouldSkipDirect(cover, undefined, true, true)).toBe(true);
  });

  it("does not skip while the direct transport still looks healthy", () => {
    // The direct url is cheaper; never abandon it without evidence.
    expect(shouldSkipDirect(cover, undefined, true, false)).toBe(false);
  });

  it("does not skip when there is no other transport to skip to", () => {
    // Skipping here would mean showing a glyph instead of trying at all.
    expect(shouldSkipDirect(cover, undefined, false, true)).toBe(false);
  });

  it("does not fetch anything when the bytes are already cached", () => {
    expect(shouldSkipDirect(cover, "data:image/png;base64,AAAA", true, true)).toBe(
      false,
    );
  });

  it("does nothing when there is no image to load", () => {
    expect(shouldSkipDirect(null, undefined, true, true)).toBe(false);
  });
});
