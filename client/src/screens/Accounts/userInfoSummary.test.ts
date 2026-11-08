import { describe, expect, it } from "vitest";
import type { NpState, ProfileInfo } from "../../api/ps5";
import { accountIdToDecimal } from "./accountId";
import { userInfoFileName, userInfoSummary } from "./userInfoSummary";

describe("accountIdToDecimal", () => {
  it("is exact for a full 64-bit id (no rounding)", () => {
    expect(accountIdToDecimal("0x1122334455667788")).toBe("1234605616436508552");
    expect(accountIdToDecimal("0xffffffffffffffff")).toBe("18446744073709551615");
    expect(accountIdToDecimal("0x1a85")).toBe("6789");
    expect(accountIdToDecimal("6789")).toBe("6789"); // decimal in, decimal out
  });
  it("is empty when there is no id", () => {
    expect(accountIdToDecimal("0x0")).toBe("");
    expect(accountIdToDecimal(null)).toBe("");
    expect(accountIdToDecimal("junk")).toBe("");
  });
});

const info: ProfileInfo = {
  ok: true,
  uid: 0x10000000,
  uid_hex: "0x10000000",
  username: "Alice",
  users: [
    { uid: 0x10000000, uid_hex: "0x10000000", username: "Alice" },
    { uid: 0x10000001, uid_hex: "0x10000001", username: "Bob" },
  ],
  slots: [
    { slot: 1, name: "Alice", type: "PSN", flags: 0x1002, id: "0x1122334455667788", activated: true, offline_activated: true },
    { slot: 3, name: "Old", type: "", flags: 0, id: "0x1234", activated: true, offline_activated: false },
  ],
};

const np: NpState = {
  ok: true,
  slot: 1,
  uid: 0x10000000,
  backup: true,
  backup_slot: 1,
  online_id: "alice_ps",
  dob: "1983-04-12",
  auth_dat: true,
  config_dat: false,
  sub_account: 0,
  offline_adult: 1,
  age: 43,
  login_flag: 1,
};

describe("userInfoSummary", () => {
  const text = userInfoSummary({
    at: new Date(2026, 9, 30, 14, 5),
    appVersion: "9.5.3",
    consoleName: "Living room",
    consoleAddr: "192.168.1.100",
    payloadVersion: "9.5.3",
    kernel: null,
    info,
    np: new Map([[0x10000000, np]]),
  });

  it("lists every user with the account ID in every form", () => {
    expect(text).toContain("1. Alice   [signed in now]");
    expect(text).toContain("Account ID (hex):    0x1122334455667788");
    expect(text).toContain("Account ID (decimal): 1234605616436508552");
    expect(text).toContain("Account ID (base64): iHdmVUQzIhE=");
    expect(text).toContain("Account state:       activated offline");
    expect(text).toContain("Online ID:           alice_ps");
    expect(text).toContain("Sign-in backup kept: yes (slot 1)");
    // A user without an account slot says so.
    expect(text).toContain("2. Bob");
    expect(text).toContain("none found for this user");
  });

  it("keeps account slots no user goes with, and says how to restore", () => {
    expect(text).toContain("OTHER ACCOUNT SLOTS");
    expect(text).toContain('Account slot:        3 ("Old")');
    expect(text).toContain("RESTORING");
    expect(text).toContain("Made:           2026-10-30 14:05");
    expect(text).toContain("Console:        Living room (192.168.1.100)");
  });

  it("names the file after the console and the day", () => {
    expect(userInfoFileName("Living room", new Date(2026, 9, 30))).toBe("ps5upload-users-Living-room-2026-10-30.txt");
    expect(userInfoFileName("///", new Date(2026, 0, 2))).toBe("ps5upload-users-PS5-2026-01-02.txt");
  });
});
