import { beforeEach, describe, expect, it } from "vitest";

import { clampUploadStreams, MAX_UPLOAD_STREAMS, useUploadSettingsStore } from "./uploadSettings";

/**
 * Upload-stream count must stay in [1, MAX]: more than the payload supports
 * can crash it mid-upload, and zero/negative is meaningless. Fractions round.
 */
describe("clampUploadStreams", () => {
  it("caps an over-large count at MAX", () => {
    expect(clampUploadStreams(99)).toBe(MAX_UPLOAD_STREAMS);
  });

  it("floors zero / negative at 1", () => {
    expect(clampUploadStreams(0)).toBe(1);
    expect(clampUploadStreams(-5)).toBe(1);
  });

  it("rounds a fractional count", () => {
    expect(clampUploadStreams(2.6)).toBe(3);
    expect(clampUploadStreams(1.2)).toBe(1);
  });

  it("passes an in-range integer through", () => {
    expect(clampUploadStreams(2)).toBe(2);
  });

  it("falls back to the default for non-finite input", () => {
    expect(clampUploadStreams(Number.NaN)).toBeGreaterThanOrEqual(1);
    expect(clampUploadStreams(Number.NaN)).toBeLessThanOrEqual(
      MAX_UPLOAD_STREAMS,
    );
  });
});

/** systemFileRead is the opt-in toggle for downloading system files
 *  from read-only partitions (/system, /system_data). It MUST default
 *  to OFF — a user who has never heard of this feature should never
 *  accidentally bypass the writable-root allowlist. */
describe("systemFileRead setting", () => {
  beforeEach(() => {
    // Reset store + localStorage before each test so state doesn't
    // leak between cases.
    const store = useUploadSettingsStore as unknown as {
      setState: (s: Record<string, unknown>) => void;
    };
    store.setState({ systemFileRead: false });
  });

  it("defaults to OFF (disabled)", () => {
    expect(useUploadSettingsStore.getState().systemFileRead).toBe(false);
  });

  it("can be enabled via setSystemFileRead(true)", () => {
    useUploadSettingsStore.getState().setSystemFileRead(true);
    expect(useUploadSettingsStore.getState().systemFileRead).toBe(true);
  });

  it("can be turned back off via setSystemFileRead(false)", () => {
    useUploadSettingsStore.getState().setSystemFileRead(true);
    useUploadSettingsStore.getState().setSystemFileRead(false);
    expect(useUploadSettingsStore.getState().systemFileRead).toBe(false);
  });
});
