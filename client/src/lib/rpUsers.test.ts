import { describe, expect, it } from "vitest";

import { chosenRpUser, rpUsersOf, withChosen, withPairing, withoutRpUser } from "./rpUsers";

// Fake keys for tests only, never a real Remote Play pairing.
const R1 = "31613262336334640000000000000000";
const K1 = "ffeeddccbbaa99887766554433221100";
const R2 = "35653666376138620000000000000000";
const K2 = "00112233445566778899aabbccddeeff";
const R3 = "39616262636364640000000000000000";
const K3 = "0f0e0d0c0b0a09080706050403020100";

/** A console paired before 11.5: its keys, no list. */
const legacy = { wake_credential: "12345", wake_regist_key: R1, wake_rp_key: K1 };

describe("paired PS5 users", () => {
  it("reads a pairing from before the list as its one user, chosen", () => {
    const users = rpUsersOf(legacy);
    expect(users).toHaveLength(1);
    expect(users[0]).toMatchObject({ name: "", wake_regist_key: R1, wake_credential: "12345" });
    expect(chosenRpUser(legacy)).toBe(R1);
    expect(rpUsersOf(null)).toEqual([]);
    expect(rpUsersOf({ wake_regist_key: "zz", wake_rp_key: K1 })).toEqual([]);
  });

  it("adds another user's pairing, keeps the first, and chooses the new one", () => {
    const p = { ...legacy, ...withPairing(legacy, { account_id: "QUFBQUFBQUI=", name: "Bob", wake_credential: "43", wake_regist_key: R2.toUpperCase(), wake_rp_key: K2 }) };
    expect(p.rp_users?.map((u) => u.name)).toEqual(["", "Bob"]);
    expect(p.wake_regist_key).toBe(R2);
    expect(p.wake_credential).toBe("43");
    // The same account again replaces its pairing and keeps its name.
    const again = { ...p, ...withPairing(p, { account_id: "QUFBQUFBQUI=", wake_regist_key: R3, wake_rp_key: K3 }) };
    expect(again.rp_users).toHaveLength(2);
    expect(again.rp_users?.[1]).toMatchObject({ name: "Bob", wake_regist_key: R3 });
  });

  it("chooses a user by its keys, so Start and Wake use them", () => {
    const p = { ...legacy, ...withPairing(legacy, { account_id: "QUFBQUFBQUI=", name: "Bob", wake_regist_key: R2, wake_rp_key: K2 }) };
    const back = { ...p, ...withChosen(p, R1) };
    expect(back.wake_regist_key).toBe(R1);
    expect(back.wake_rp_key).toBe(K1);
    expect(back.wake_credential).toBe("12345");
    expect(back.rp_users).toHaveLength(2);
    expect(withChosen(p, R3)).toBeNull();
  });

  it("forgets one user; the chosen one gone, the next is chosen; the last gone, unpaired", () => {
    const p = { ...legacy, ...withPairing(legacy, { account_id: "QUFBQUFBQUI=", name: "Bob", wake_regist_key: R2, wake_rp_key: K2 }) };
    const keptChosen = { ...p, ...withoutRpUser(p, R1) };
    expect(keptChosen.wake_regist_key).toBe(R2);
    expect(rpUsersOf(keptChosen).map((u) => u.name)).toEqual(["Bob"]);
    const lostChosen = { ...p, ...withoutRpUser(p, R2) };
    expect(lostChosen.wake_regist_key).toBe(R1);
    const none = { ...keptChosen, ...withoutRpUser(keptChosen, R2) };
    expect(none.wake_regist_key).toBeNull();
    expect(none.wake_rp_key).toBeNull();
    expect(none.rp_users).toBeNull();
    expect(rpUsersOf(none)).toEqual([]);
  });
});
