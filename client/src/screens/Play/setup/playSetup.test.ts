import { describe, expect, it } from "vitest";

import type { NpState, RemotePlayReadiness } from "../../../api/ps5";
import { nextStep, playReady, setupProgress, setupStates, type SetupInput } from "./playSetup";

const R = (o: Partial<RemotePlayReadiness> = {}): RemotePlayReadiness => ({
  fw_magic: 0x13600000,
  has_per_user: 1,
  foreground_uid: 0x10000000,
  account_uid: 0x10000000,
  account_via: "foreground",
  user_slot: 1,
  account_id_b64: "AAAAAAAAAAE=",
  account_id_raw: 1,
  account_type: "np",
  service_enabled: 1,
  user_enabled: 1,
  symbols_ok: 1,
  registry_err: 0,
  ...o,
});
const NP = (o: Partial<NpState> = {}): NpState => ({
  ok: true,
  slot: 1,
  uid: 0x10000000,
  backup: false,
  backup_slot: 0,
  online_id: "player",
  dob: "1983-04-12",
  auth_dat: true,
  config_dat: true,
  sub_account: null,
  offline_adult: null,
  age: null,
  login_flag: null,
  ...o,
});
const base: SetupInput = { connected: true, readiness: R(), np: NP(), paired: false, restOk: false, played: false };

describe("Play setup", () => {
  it("starts at connecting, with everything after it waiting", () => {
    const s = setupStates({ ...base, connected: false, readiness: null, np: null });
    expect(s).toMatchObject({ connect: "todo", user: "wait", account: "wait", signin: "wait", service: "wait", pair: "wait", play: "wait" });
    expect(s.rest).toBe("manual");
    expect(nextStep(s)).toBe("connect");
    expect(playReady(s)).toBe(false);
  });

  it("walks a fresh offline user through each step in order", () => {
    // Nobody signed in.
    let s = setupStates({ ...base, readiness: R({ account_uid: -1 }), np: null });
    expect(nextStep(s)).toBe("user");
    expect(s.account).toBe("wait");
    // Signed in, no account id.
    s = setupStates({ ...base, readiness: R({ account_id_raw: 0, account_type: "", service_enabled: 0 }), np: null });
    expect([s.user, s.account, s.signin]).toEqual(["ok", "todo", "wait"]);
    // Activated, not signed in to PSN, service off.
    s = setupStates({ ...base, readiness: R({ service_enabled: 0 }), np: NP({ online_id: "", auth_dat: false }) });
    expect([s.account, s.signin, s.service]).toEqual(["ok", "todo", "wait"]);
    // np-fake-signin ran; the service still off.
    s = setupStates({ ...base, readiness: R({ service_enabled: 0 }), np: NP() });
    expect([s.signin, s.service, s.pair]).toEqual(["ok", "todo", "wait"]);
    // Service on (and allowed for this user): pair is next after the manual rest step.
    s = setupStates({ ...base });
    expect([s.service, s.rest, s.pair]).toEqual(["ok", "manual", "todo"]);
    expect(nextStep(s)).toBe("rest");
    s = setupStates({ ...base, restOk: true });
    expect(nextStep(s)).toBe("pair");
    s = setupStates({ ...base, restOk: true, paired: true });
    expect(playReady(s)).toBe(true);
    expect(nextStep(s)).toBe("play");
    s = setupStates({ ...base, restOk: true, paired: true, played: true });
    expect(nextStep(s)).toBeNull();
    expect(setupProgress(s)).toEqual({ done: 8, total: 8 });
  });

  it("needs this user allowed on firmware that asks per user", () => {
    const s = setupStates({ ...base, readiness: R({ has_per_user: 1, user_enabled: 0 }) });
    expect(s.service).toBe("todo");
    expect(setupStates({ ...base, readiness: R({ has_per_user: 0, user_enabled: 0 }) }).service).toBe("ok");
  });

  it("counts the service already on as signed in when the PSN state can't be read", () => {
    expect(setupStates({ ...base, np: null }).signin).toBe("ok");
    expect(setupStates({ ...base, np: null, readiness: R({ service_enabled: 0 }) }).signin).toBe("todo");
  });

  it("keeps pairing done while the console is away", () => {
    const s = setupStates({ ...base, connected: false, readiness: null, np: null, paired: true });
    expect(s.pair).toBe("ok");
    expect(playReady(s)).toBe(true);
    expect(s.play).toBe("todo");
  });
});
