import { beforeEach, describe, expect, it } from "vitest";

import {
  clearHelperSend,
  helperSendFor,
  runHelperSend,
  useHelperSendStore,
  type HelperSendDeps,
  type HelperSendText,
} from "./helperSend";

const A = "192.168.0.5";
const B = "192.168.0.6";

const text: HelperSendText = {
  checkingLoader: "checking loader",
  stuck: "loader stuck",
  sending: (elf) => `sending ${elf}`,
  waiting: "waiting for boot",
  running: "helper running",
  timeout: (tail) => `did not come up.${tail}`,
  notPaired: () => "pair with your PS5",
  engineUnreachable: (e: string) => `engine unreachable: ${e}`,
};

function world(over: Partial<HelperSendDeps> = {}) {
  const log: string[] = [];
  let release: (() => void) | null = null;
  const gate = { p: null as Promise<void> | null };
  const deps: HelperSendDeps = {
    waitForLoader: async () => "ready",
    bundledPath: async () => "/app/ps5upload.elf",
    send: async (host) => {
      log.push(`send ${host}`);
      if (gate.p) await gate.p;
    },
    check: async () => ({ reachable: true }),
    isNotPaired: (e) => e.includes("not paired"),
    sleep: async () => {},
    setProbing: (host, on) => log.push(`probing ${host} ${on}`),
    onUp: (host) => log.push(`up ${host}`),
    onOk: (host, msg) => log.push(`ok ${host} ${msg}`),
    onNotPaired: (host) => log.push(`pairing ${host}`),
    maxAttempts: 3,
    ...over,
  };
  return {
    deps,
    log,
    /** Holds the send until the returned function is called. */
    holdSend() {
      gate.p = new Promise<void>((r) => (release = r));
      return () => {
        gate.p = null;
        release?.();
      };
    },
  };
}

const at = (host: string) => helperSendFor(useHelperSendStore.getState(), host);
const tick = () => new Promise((r) => setTimeout(r, 0));

describe("runHelperSend", () => {
  beforeEach(() => useHelperSendStore.setState({ byHost: {} }));

  it("is busy with its phase and message in the store while sending, with no screen mounted", async () => {
    const w = world();
    const release = w.holdSend();
    const p = runHelperSend(A, w.deps, text);
    await tick();
    expect(at(A)).toMatchObject({
      state: "busy",
      phase: "sending",
      msg: "sending /app/ps5upload.elf",
    });
    release();
    await p;
  });

  it("ends ok: nothing transient is left and the console is reported up", async () => {
    const w = world();
    expect(await runHelperSend(A, w.deps, text)).toBe("ok");
    expect(at(A)).toBeNull();
    expect(w.log).toEqual([
      `probing ${A} true`,
      `send ${A}`,
      `up ${A}`,
      `ok ${A} helper running`,
    ]);
  });

  it("does not send to a stuck loader", async () => {
    const w = world({ waitForLoader: async () => "stuck" });
    expect(await runHelperSend(A, w.deps, text)).toBe("fail");
    expect(at(A)).toMatchObject({ state: "fail", msg: "loader stuck" });
    expect(w.log).not.toContain(`send ${A}`);
    expect(w.log[w.log.length - 1]).toBe(`probing ${A} false`);
  });

  it("keeps the send's own error when the send fails", async () => {
    const w = world({
      send: async () => {
        throw new Error("connect 9021: refused");
      },
    });
    await runHelperSend(A, w.deps, text);
    expect(at(A)).toMatchObject({
      state: "fail",
      msg: "connect 9021: refused",
    });
  });

  it("gives up after its attempts with the last probe error", async () => {
    let probes = 0;
    const w = world({
      check: async () => {
        probes += 1;
        return { reachable: false, error: "mgmt refused" };
      },
    });
    expect(await runHelperSend(A, w.deps, text)).toBe("fail");
    expect(probes).toBe(3);
    expect(at(A)).toMatchObject({
      state: "fail",
      msg: "did not come up. Last probe: mgmt refused.",
    });
    expect(w.log[w.log.length - 1]).toBe(`probing ${A} false`);
  });

  it("says the app's own engine is unreachable instead of blaming the helper", async () => {
    const engineErr =
      "error sending request for url (http://127.0.0.1:19113/api/ps5/status?addr=192.168.0.54:9113)";
    let probes = 0;
    const w = world({
      maxAttempts: 20,
      check: async () => {
        probes += 1;
        throw new Error(engineErr);
      },
    });
    expect(await runHelperSend(A, w.deps, text)).toBe("fail");
    // Three misses in a row end the wait; the full 20 would only delay the same answer.
    expect(probes).toBe(3);
    expect(at(A)).toMatchObject({
      state: "fail",
      msg: `engine unreachable: ${engineErr}`,
    });
  });

  it("names the engine when the send itself could not reach it", async () => {
    const w = world({
      send: async () => {
        throw new Error(
          "error sending request for url (http://127.0.0.1:19113/api/payload/send)",
        );
      },
    });
    expect(await runHelperSend(A, w.deps, text)).toBe("fail");
    expect(at(A)?.msg).toMatch(/^engine unreachable: /);
  });

  it("stops polling and opens pairing when the helper answers but is not paired", async () => {
    let probes = 0;
    const w = world({
      check: async () => {
        probes += 1;
        return { reachable: false, error: "this device is not paired" };
      },
    });
    expect(await runHelperSend(A, w.deps, text)).toBe("fail");
    expect(probes).toBe(1);
    expect(at(A)).toMatchObject({ state: "fail", msg: "pair with your PS5" });
    expect(w.log).toContain(`pairing ${A}`);
  });

  it("ignores a second send to a console that is already being sent to", async () => {
    const w = world();
    const release = w.holdSend();
    const first = runHelperSend(A, w.deps, text);
    await tick();
    expect(await runHelperSend(A, w.deps, text)).toBe("busy");
    release();
    await first;
    expect(w.log.filter((l) => l.startsWith("send"))).toHaveLength(1);
  });

  it("keeps each console's send apart", async () => {
    const w = world();
    const release = w.holdSend();
    const a = runHelperSend(A, w.deps, text);
    await tick();
    expect(at(A)?.state).toBe("busy");
    expect(at(B)).toBeNull();
    release();
    await a;
  });

  it("clear forgets a failure and leaves a running send alone", async () => {
    const w = world({ waitForLoader: async () => "stuck" });
    await runHelperSend(A, w.deps, text);
    clearHelperSend(A);
    expect(at(A)).toBeNull();
    const w2 = world();
    const release = w2.holdSend();
    const p = runHelperSend(A, w2.deps, text);
    await tick();
    clearHelperSend(A);
    expect(at(A)?.state).toBe("busy");
    release();
    await p;
  });
});
