import { describe, expect, it } from "vitest";

import type { NpState, RemotePlayReadiness } from "../../../api/ps5";
import {
  nextStep,
  playReady,
  setupProgress,
  setupStates,
  userFacts,
  userSignedIn,
  userWait,
  type SetupInput,
} from "./playSetup";

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
// What the helper answers for a signed-in offline user with no account ID yet (#864).
const NO_ID = R({
  account_uid: 0,
  account_via: "none",
  user_slot: -1,
  account_id_b64: "",
  account_id_raw: 0,
  account_type: "",
  service_enabled: 0,
});
const base: SetupInput = {
  connected: true,
  readiness: R(),
  np: NP(),
  paired: false,
  restOk: false,
  played: false,
  blockers: [],
};

describe("Play setup", () => {
  it("starts at connecting, with everything after it waiting", () => {
    const s = setupStates({ ...base, connected: false, readiness: null, np: null });
    expect(s).toMatchObject({ connect: "todo", user: "wait", account: "wait", signin: "wait", service: "wait", pair: "wait", play: "wait" });
    expect(s.rest).toBe("optional");
    expect(nextStep(s)).toBe("connect");
    expect(playReady(s)).toBe(false);
  });

  it("walks a fresh offline user through each step in order", () => {
    // Nobody signed in.
    let s = setupStates({ ...base, readiness: R({ account_uid: -1, foreground_uid: -1 }), np: null });
    expect(nextStep(s)).toBe("user");
    expect(s.account).toBe("wait");
    // Signed in, no account id: the helper finds no account to pair with (account_uid 0,
    // via none), as it answers for such a user.
    s = setupStates({ ...base, readiness: NO_ID, np: null });
    expect([s.user, s.account, s.signin]).toEqual(["ok", "todo", "wait"]);
    // Activated, not signed in to PSN, service off.
    s = setupStates({ ...base, readiness: R({ service_enabled: 0 }), np: NP({ online_id: "", auth_dat: false }) });
    expect([s.account, s.signin, s.service]).toEqual(["ok", "todo", "wait"]);
    // np-fake-signin ran; the service still off.
    s = setupStates({ ...base, readiness: R({ service_enabled: 0 }), np: NP() });
    expect([s.signin, s.service, s.pair]).toEqual(["ok", "todo", "wait"]);
    // Service on (and allowed for this user): pair is next; the rest-mode step is optional and
    // never in the way (only needed to start Play while the PS5 sleeps).
    s = setupStates({ ...base });
    expect([s.service, s.rest, s.pair]).toEqual(["ok", "optional", "todo"]);
    expect(nextStep(s)).toBe("pair");
    s = setupStates({ ...base, paired: true, played: true });
    expect(nextStep(s)).toBeNull();
    expect(setupProgress(s)).toEqual({ done: 8, total: 8 });
    s = setupStates({ ...base, restOk: true });
    expect(nextStep(s)).toBe("pair");
    s = setupStates({ ...base, restOk: true, paired: true });
    expect(playReady(s)).toBe(true);
    expect(nextStep(s)).toBe("play");
    s = setupStates({ ...base, restOk: true, paired: true, played: true });
    expect(nextStep(s)).toBeNull();
    expect(setupProgress(s)).toEqual({ done: 9, total: 9 });
  });

  it("counts a signed-in user without an account ID as signed in (#864)", () => {
    // In front, no account ID: signed in, and the account ID is the step to do.
    let s = setupStates({ ...base, readiness: NO_ID, np: null });
    expect(nextStep(s)).toBe("account");
    expect(userWait(NO_ID)).toBeNull();
    // Nobody in front (the dashboard), but signed in: a newer helper says so in login_count.
    const behind = { ...NO_ID, foreground_uid: -1, login_count: 1, login_rc: 0, fg_rc: 0 };
    s = setupStates({ ...base, readiness: behind, np: null });
    expect([s.user, s.account]).toEqual(["ok", "todo"]);
    // An older helper with nobody in front and no account user: still open, it can't tell.
    expect(setupStates({ ...base, readiness: { ...NO_ID, foreground_uid: -1 }, np: null }).user).toBe("todo");
    // An activated user found through the login list.
    expect(userSignedIn(R({ foreground_uid: -1, account_via: "login-list" }))).toBe(true);
  });

  it("says why the sign-in step stays open, with the helper's raw answer", () => {
    expect(userWait(null)).toBeNull();
    // Nobody signed in, as the console said.
    const nobody = R({ foreground_uid: -1, account_uid: 0, account_via: "none", login_count: 0, login_rc: 0, fg_rc: 0 });
    expect(userWait(nobody)).toEqual({ why: "none" });
    // The console answered with an error: not read as nobody.
    const failed = { ...nobody, login_count: -1, login_rc: 0x80960002, fg_rc: 0x80960002 };
    expect(userWait(failed)).toEqual({ why: "error", code: "0x80960002" });
    // The list answered; the foreground's error is not the one in the way.
    expect(userWait({ ...nobody, fg_rc: 0x80960002 })).toEqual({ why: "none" });
    // An older helper: no codes, so nobody.
    expect(userWait(R({ foreground_uid: -1, account_uid: 0 }))).toEqual({ why: "none" });
    expect(userFacts(failed)).toBe(
      "foreground_uid=0xffffffff account_uid=0x00000000 account_via=none user_slot=1 login_count=-1 fg_rc=0x80960002 login_rc=0x80960002",
    );
    // Never the account ID.
    expect(userFacts(R())).not.toContain("AAAAAAAAAAE");
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

  it("ticks No payload blocks Remote Play when Hush is off or not loaded", () => {
    const done = { ...base, restOk: true, paired: true };
    expect(setupStates(done).blockers).toBe("ok");
    const s = setupStates({ ...done, blockers: ["hush"] });
    expect(s.blockers).toBe("todo");
    expect(nextStep(s)).toBe("blockers");
    // Still ready to play: a blocker is paused at the start.
    expect(playReady(s)).toBe(true);
    // Unread, or no console: waiting.
    expect(setupStates({ ...done, blockers: null }).blockers).toBe("wait");
    expect(setupStates({ ...done, blockers: undefined }).blockers).toBe("wait");
    expect(setupStates({ ...done, connected: false }).blockers).toBe("wait");
  });

  it("keeps pairing done while the console is away", () => {
    const s = setupStates({ ...base, connected: false, readiness: null, np: null, paired: true });
    expect(s.pair).toBe("ok");
    expect(playReady(s)).toBe(true);
    expect(s.play).toBe("todo");
  });
});
