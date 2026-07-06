import { afterEach, describe, expect, it } from "vitest";
import { effectiveUploadStreams } from "./uploadStreams";
import { useUploadSettingsStore } from "../state/uploadSettings";
import { useConnectionStore } from "../state/connection";

// These run in vitest's default `node` environment (no `window`), so the
// upload-settings store initialises `uploadStreams` from its hard default
// rather than localStorage — exactly the "fresh install" path we want to pin.

afterEach(() => {
  // Restore the shipped defaults so cross-test ordering can't leak state.
  useUploadSettingsStore.setState({ uploadStreams: 1 });
  useConnectionStore.setState({
    maxTransferStreams: undefined,
    runtimeByHost: {},
  });
});

describe("upload streams default", () => {
  it("defaults to single stream (1) on a fresh install", () => {
    // Regression guard for the v2.24.x payload-crash fix: multi-stream
    // (the old default of 4) drove concurrent transactions that crashed the
    // payload listener mid-upload on some consoles. The safe default is 1.
    expect(useUploadSettingsStore.getState().uploadStreams).toBe(1);
  });
});

describe("effectiveUploadStreams", () => {
  it("clamps the user's setting to what the payload advertises", () => {
    useUploadSettingsStore.setState({ uploadStreams: 4 });
    useConnectionStore.setState({ maxTransferStreams: 2 });
    expect(effectiveUploadStreams()).toBe(2);
  });

  it("treats a payload that advertises nothing as single-stream", () => {
    // An old payload predating multi-stream → undefined → 1, so multi-stream
    // silently no-ops instead of sending frames a single-stream payload can't
    // service in parallel.
    useUploadSettingsStore.setState({ uploadStreams: 4 });
    useConnectionStore.setState({ maxTransferStreams: undefined });
    expect(effectiveUploadStreams()).toBe(1);
  });

  it("never returns less than 1 even if the setting is somehow 0", () => {
    useUploadSettingsStore.setState({ uploadStreams: 0 });
    useConnectionStore.setState({ maxTransferStreams: 4 });
    expect(effectiveUploadStreams()).toBe(1);
  });

  it("passes the user's choice through when the payload can service it", () => {
    useUploadSettingsStore.setState({ uploadStreams: 3 });
    useConnectionStore.setState({ maxTransferStreams: 4 });
    expect(effectiveUploadStreams()).toBe(3);
  });

  it("clamps to the TARGET console's advertised max, not the active tab's", () => {
    // Two consoles uploading in parallel: the active tab (mirrored flat
    // field) advertises 4 streams, but the transfer targets a console
    // whose payload only services 1. Reading the active tab's capability
    // for the background console's transfer was the multi-console bug.
    useUploadSettingsStore.setState({ uploadStreams: 4 });
    useConnectionStore.setState({
      maxTransferStreams: 4,
      runtimeByHost: {
        "192.168.86.99": {
          payloadStatus: "up",
          payloadVersion: "2.30.0",
          ps5Kernel: null,
          ucredElevated: true,
          maxTransferStreams: 1,
          transferAlive: null,
        },
        "192.168.86.100": {
          payloadStatus: "up",
          payloadVersion: "2.30.0",
          ps5Kernel: null,
          ucredElevated: true,
          maxTransferStreams: 4,
          transferAlive: null,
        },
      },
    });
    // Port-tolerant: queue items pass `ip:9113` transfer addrs.
    expect(effectiveUploadStreams("192.168.86.99:9113")).toBe(1);
    expect(effectiveUploadStreams("192.168.86.100:9113")).toBe(4);
  });

  it("treats an addr the poller hasn't probed yet as single-stream", () => {
    useUploadSettingsStore.setState({ uploadStreams: 4 });
    useConnectionStore.setState({ maxTransferStreams: 4, runtimeByHost: {} });
    expect(effectiveUploadStreams("10.0.0.7:9113")).toBe(1);
  });
});
