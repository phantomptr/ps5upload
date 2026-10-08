import { describe, expect, it } from "vitest";

import type { ProfileInfo, ProfileSlot } from "../../api/ps5";
import { npSignInReadiness } from "./npSignIn";

function slot(over: Partial<ProfileSlot>): ProfileSlot {
  return {
    slot: 1,
    name: "Ava",
    type: "np",
    flags: 0x1002,
    id: "0x1234",
    activated: true,
    offline_activated: true,
    ...over,
  };
}

function info(over: Partial<ProfileInfo>): ProfileInfo {
  return {
    ok: true,
    uid: 0x10000000,
    uid_hex: "10000000",
    username: "Ava",
    users: [],
    slots: [slot({})],
    ...over,
  };
}

describe("npSignInReadiness", () => {
  it("is ready when the signed-in user's slot has an account id", () => {
    expect(npSignInReadiness(info({}))).toEqual({
      kind: "ready",
      username: "Ava",
      slot: 1,
    });
  });

  it("asks for offline activation when the slot's id is zero", () => {
    expect(
      npSignInReadiness(info({ slots: [slot({ id: "0x0000000000000000" })] }))
        .kind,
    ).toBe("not_activated");
    expect(
      npSignInReadiness(info({ slots: [slot({ activated: false })] })).kind,
    ).toBe("not_activated");
  });

  it("matches the slot by the signed-in user's name, as np-fake-signin does", () => {
    const r = npSignInReadiness(
      info({ slots: [slot({ slot: 1, name: "Other" }), slot({ slot: 3 })] }),
    );
    expect(r).toEqual({ kind: "ready", username: "Ava", slot: 3 });
    expect(
      npSignInReadiness(info({ slots: [slot({ name: "Other" })] })).kind,
    ).toBe("no_slot");
  });

  it("needs a signed-in user, and waits for the first read", () => {
    expect(npSignInReadiness(info({ uid: 0, username: "" })).kind).toBe(
      "no_user",
    );
    expect(npSignInReadiness(null).kind).toBe("loading");
  });
});
