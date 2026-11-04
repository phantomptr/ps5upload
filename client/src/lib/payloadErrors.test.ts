import { describe, expect, it } from "vitest";

import {
  WORDED_ERROR_CODES,
  isDownloadCancelled,
  payloadErrorText,
  usbSkippedText,
} from "./payloadErrors";

/** Returns the key and vars, so a test sees what was asked for. */
const tr = (key: string, vars?: Record<string, string | number>) =>
  vars ? `${key}:${JSON.stringify(vars)}` : key;
/** The app's tr when a language has no such key: the fallback. */
const english = (_k: string, _v?: Record<string, string | number>, fallback?: string) => fallback ?? "";

describe("payloadErrorText", () => {
  it("words a code it knows", () => {
    expect(payloadErrorText(tr, new Error('repo_host_is_ip: "1.2.3.4" is an IP'))).toBe(
      "payloads_err_repo_host_ip",
    );
    expect(payloadErrorText(tr, "usb_not_enough_space: The payloads need 5 bytes")).toBe(
      "usb_err_not_enough_space",
    );
  });

  it("carries the detail where the wording has one", () => {
    expect(payloadErrorText(tr, "custom_repos_unreadable: /x/custom-repos.json: bad")).toBe(
      'payloads_err_custom_repos_unreadable:{"detail":"/x/custom-repos.json: bad"}',
    );
    expect(payloadErrorText(english, "usb_drive_gone: /Volumes/X is not a folder")).toContain("Plug it in again");
    expect(payloadErrorText(english, "usb_write_failed: disk full")).toBe(
      "Couldn't write to the drive, so nothing on it was changed. (disk full)",
    );
  });

  it("leaves everything else as it came", () => {
    for (const m of ["fetch https://x: HTTP 404 (Not Found)", "some_other_code: text", "plain"]) {
      expect(payloadErrorText(tr, new Error(m))).toBe(m);
    }
    expect(payloadErrorText(tr, new Error("Error: plain"))).toBe("plain");
  });

  it("every code the Rust side marks is worded here", () => {
    for (const code of [
      "repo_host_is_ip",
      "repo_host_gitlab",
      "custom_repos_unreadable",
      "download_cancelled",
      "redirect_refused",
      "invalid_checksum_file",
    ]) {
      expect(WORDED_ERROR_CODES.has(code)).toBe(true);
    }
  });
});

describe("isDownloadCancelled", () => {
  it("tells a user's Cancel from a failure", () => {
    expect(isDownloadCancelled(new Error("download_cancelled: The download was cancelled."))).toBe(true);
    expect(isDownloadCancelled("Error: download_cancelled: x")).toBe(true);
    expect(isDownloadCancelled(new Error("read chunk: timed out"))).toBe(false);
  });
});

describe("usbSkippedText", () => {
  it("words an id that is no longer known and leaves the rest", () => {
    expect(usbSkippedText(tr, "custom-0a: unknown_payload")).toBe('usb_skipped_unknown:{"id":"custom-0a"}');
    expect(usbSkippedText(tr, "kstuff: no local cache (download via Payloads tab first)")).toBe(
      "kstuff: no local cache (download via Payloads tab first)",
    );
  });
});
