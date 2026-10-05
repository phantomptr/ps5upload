import { beforeEach, describe, expect, it } from "vitest";

import { useUploadSettingsStore } from "./uploadSettings";

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
