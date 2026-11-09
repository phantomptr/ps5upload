import { describe, expect, it } from "vitest";

import type { ProfileInfo, ProfileSlot } from "../../api/ps5";
import { NP_TEMPLATE_DOB, npBackupTarget, npSignInReadiness, npUndo } from "./npSignIn";

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

describe("the sign-in backup", () => {
  it("is about the foreground user and the slot with its name", () => {
    expect(npBackupTarget(info({}))).toEqual({ uid: 0x10000000, slot: 1, username: "Ava" });
  });

  it("has no target, rather than throwing, when an answer leaves fields out", () => {
    const partial = { uid: 0 } as unknown as Parameters<typeof npBackupTarget>[0];
    expect(npBackupTarget(partial)).toBeNull();
  });

  // Measured on the Pro (13.60): the helper read no foreground user (uid 0) while the one
  // local user was signed in, so Undo still has to find that user.
  it("falls back to the only local user when no foreground user is read", () => {
    const pro = info({
      uid: 0,
      username: "",
      users: [{ uid: 268435458, uid_hex: "0x10000002", username: "PhantomPtr" }],
      slots: [slot({ name: "PhantomPtr", flags: 6 })],
    });
    expect(npBackupTarget(pro)).toEqual({ uid: 268435458, slot: 1, username: "PhantomPtr" });
    expect(
      npBackupTarget({
        ...pro,
        users: [...pro.users, { uid: 7, uid_hex: "0x7", username: "Other" }],
      }),
    ).toBeNull();
  });

  it("offers Undo with a kept copy, and explains when there is none after np-fake-signin", () => {
    expect(npUndo({ backup: true, dob: NP_TEMPLATE_DOB })).toBe("restore");
    expect(npUndo({ backup: false, dob: NP_TEMPLATE_DOB })).toBe("no_backup");
    expect(npUndo({ backup: false, dob: "1990-01-01" })).toBe("none");
    expect(npUndo(null)).toBe("none");
  });
});
