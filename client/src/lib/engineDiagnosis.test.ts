import { describe, expect, it } from "vitest";

import {
  engineProblem,
  isEngineUnreachable,
  type EngineDiagnosis,
} from "./engineDiagnosis";

function d(over: Partial<EngineDiagnosis>): EngineDiagnosis {
  return {
    url: "http://127.0.0.1:19113",
    local: true,
    answering: false,
    probe_error: "error sending request: connection refused",
    child_running: false,
    port_taken: false,
    binary_found: true,
    binary_error: null,
    log_path: null,
    os: "windows",
    ...over,
  };
}

describe("engineProblem", () => {
  it("has nothing to say about an engine that answers", () => {
    expect(engineProblem(d({ answering: true }))).toBe("ok");
  });

  it("calls an engine with no process and a free port stopped", () => {
    expect(engineProblem(d({}))).toBe("stopped");
  });

  it("names a missing engine program before anything else local", () => {
    expect(engineProblem(d({ binary_found: false }))).toBe("missing_binary");
  });

  it("blames a proxy only when the error says so", () => {
    expect(
      engineProblem(
        d({
          port_taken: true,
          child_running: true,
          probe_error: "error sending request: unsupported scheme socks5",
        }),
      ),
    ).toBe("proxy");
  });

  it("separates a port held by something else from a running engine that is stuck", () => {
    expect(engineProblem(d({ port_taken: true, child_running: false }))).toBe(
      "port_blocked",
    );
    expect(engineProblem(d({ port_taken: true, child_running: true }))).toBe(
      "not_answering",
    );
  });

  it("reports a remote engine as down, without local guesses", () => {
    expect(
      engineProblem(
        d({
          local: false,
          url: "http://nas:19113",
          child_running: null,
          port_taken: null,
        }),
      ),
    ).toBe("remote_down");
  });
});

describe("isEngineUnreachable", () => {
  it("recognises the desktop proxy failing to reach its own engine", () => {
    expect(
      isEngineUnreachable(
        "error sending request for url (http://127.0.0.1:19113/api/ps5/status?addr=192.168.0.54:9113)",
      ),
    ).toBe(true);
    expect(
      isEngineUnreachable(
        "error sending request for url (http://localhost:19113/api/x)",
      ),
    ).toBe(true);
  });

  it("does not take a console that refuses for an engine problem", () => {
    expect(isEngineUnreachable("mgmt refused")).toBe(false);
    expect(
      isEngineUnreachable(
        "error sending request for url (http://192.168.0.54:9113/)",
      ),
    ).toBe(false);
  });
});
