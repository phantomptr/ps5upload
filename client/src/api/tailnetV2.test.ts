import { afterEach, describe, expect, it, vi } from "vitest";
import {
  allowList,
  appTailnetOp,
  deviceTailscale,
  pathSwitch,
  shouldAskSystemChoice,
  slowPathWarning,
  SLOW_PATH_UPLOAD_BYTES,
  tailnetCheck,
  tailnetOp,
  toggleAllowed,
  viaIsTailnet,
  type AppTailnetStatus,
  type TailnetState,
} from "./tailnet";
import { consoleOnTailnet, selfTailnet } from "../state/useTailnet";

function answer(status: number, body: unknown) {
  return vi.fn(async (_url: string, _init?: RequestInit) => new Response(JSON.stringify(body), { status }));
}

afterEach(() => vi.unstubAllGlobals());

const base: AppTailnetStatus = {
  ok: true,
  enabled: false,
  state: "off",
  login_url: "",
  ip: "",
  name: "",
  control_url: "",
  hostname: "",
  home_derp: 0,
  derp_connected: false,
  home_latency_ms: -1,
  key_expiry: 0,
  error: "",
  peers: [],
  consoles: [],
};

describe("the system's Tailscale app", () => {
  it("asks once, only when the app is there and PS5Upload's own is not set up", () => {
    const sys = { detected: true, ip: "100.64.0.2", iface: "utun4", choice: "" as const };
    expect(shouldAskSystemChoice({ ...base, system: sys })).toBe(true);
    expect(shouldAskSystemChoice({ ...base, system: { ...sys, choice: "system" } })).toBe(false);
    expect(shouldAskSystemChoice({ ...base, system: { ...sys, detected: false } })).toBe(false);
    expect(shouldAskSystemChoice({ ...base, signed_in: true, system: sys })).toBe(false);
    expect(shouldAskSystemChoice({ ...base, enabled: true, system: sys })).toBe(false);
    expect(shouldAskSystemChoice(null)).toBe(false);
  });

  it("reads this device as on the system app, and its consoles through it", () => {
    expect(selfTailnet({ ...base, backend: "system" })).toBe("system");
    expect(selfTailnet({ ...base, signed_in: true, backend: "system" })).toBe("paused");
    const st = { ...base, consoles: [{ host: "192.0.2.9", ip: "100.64.0.9", name: "den", via: "system", latency_ms: -1 }] };
    expect(consoleOnTailnet(st, "192.0.2.9")?.via).toBe("system");
  });

  it("sends the choice and the test path", async () => {
    const f = answer(200, base);
    vi.stubGlobal("fetch", f);
    await appTailnetOp("system_choice", { choice: "system" });
    expect(JSON.parse(String(f.mock.calls[0][1]?.body))).toEqual({ op: "system_choice", choice: "system" });
    await appTailnetOp("test_path", { host: "192.0.2.9", secs: 120 });
    expect(JSON.parse(String(f.mock.calls[1][1]?.body))).toEqual({ op: "test_path", host: "192.0.2.9", secs: 120 });
  });
});

describe("the PS5's allow-list", () => {
  it("parses and toggles the list", () => {
    expect(allowList({ allow: "100.64.0.5, 100.64.0.6," })).toEqual(["100.64.0.5", "100.64.0.6"]);
    expect(allowList({ allow: "" })).toEqual([]);
    expect(allowList(null)).toEqual([]);
    expect(toggleAllowed(["a", "b"], "c", true)).toEqual(["a", "b", "c"]);
    expect(toggleAllowed(["a", "b"], "a", false)).toEqual(["b"]);
    expect(toggleAllowed(["a"], "a", true)).toEqual(["a"]);
  });

  it("sends allow, rename and renew to the console", async () => {
    const f = answer(200, { ok: true });
    vi.stubGlobal("fetch", f);
    await tailnetOp("192.0.2.9", "allow", { allow_mode: "listed", allow: "100.64.0.5" });
    expect(JSON.parse(String(f.mock.calls[0][1]?.body))).toEqual({
      addr: "192.0.2.9",
      op: "allow",
      allow_mode: "listed",
      allow: "100.64.0.5",
    });
    await tailnetOp("192.0.2.9", "rename", { hostname: "den-ps5u" });
    expect(JSON.parse(String(f.mock.calls[1][1]?.body)).op).toBe("rename");
    await tailnetOp("192.0.2.9", "renew");
    expect(JSON.parse(String(f.mock.calls[2][1]?.body))).toEqual({ addr: "192.0.2.9", op: "renew" });
  });
});

describe("paths", () => {
  it("tells a switch to and back from Tailscale, not the first read", () => {
    expect(viaIsTailnet("lan")).toBe(false);
    expect(viaIsTailnet("system")).toBe(true);
    expect(pathSwitch(null, "relayed")).toBeNull();
    expect(pathSwitch("lan", "direct")).toBe("tailnet");
    expect(pathSwitch("direct", "relayed")).toBeNull();
    expect(pathSwitch("system", "lan")).toBe("home");
    expect(pathSwitch("lan", "lan")).toBeNull();
  });

  it("warns before a large upload on a relayed path or phone data only", () => {
    const big = SLOW_PATH_UPLOAD_BYTES + 1;
    expect(slowPathWarning(big, "relayed", false)).toBe("relayed");
    expect(slowPathWarning(big, "direct", false)).toBeNull();
    expect(slowPathWarning(big, "lan", true)).toBe("cellular");
    expect(slowPathWarning(SLOW_PATH_UPLOAD_BYTES, "relayed", true)).toBeNull();
  });

  it("reads the check by path, and says this device's Tailscale in one word", async () => {
    const body = {
      home: [{ port: 9021, open: false, error: "timeout" }],
      tailnet: { backend: "builtin", ip: "100.64.0.9", online: true, checks: [{ port: 9120, open: true, error: null }] },
      tailnet_ip: "100.64.0.9",
      used: "builtin",
      device: {
        backend: "builtin" as const,
        enabled: true,
        signed_in: true,
        state: "running" as TailnetState,
        system_detected: false,
      },
    };
    vi.stubGlobal("fetch", answer(200, body));
    const c = await tailnetCheck("192.0.2.9");
    expect(c?.used).toBe("builtin");
    expect(deviceTailscale(c?.device)).toBe("builtin-on");
    expect(deviceTailscale({ ...body.device, backend: "none" as const, enabled: false })).toBe("builtin-paused");
    expect(deviceTailscale({ ...body.device, backend: "none" as const, enabled: false, signed_in: false, system_detected: true })).toBe(
      "system",
    );
    expect(deviceTailscale({ ...body.device, backend: "none" as const, enabled: false, signed_in: false })).toBe("signed-out");
    expect(deviceTailscale({ ...body.device, backend: "system" as const })).toBe("system");
    vi.stubGlobal("fetch", answer(404, {}));
    expect(await tailnetCheck("192.0.2.9")).toBeNull();
  });
});
