import { describe, expect, it } from "vitest";
import { renderToStaticMarkup } from "react-dom/server";

import type { AutoView, SyncGame, SyncPair, SyncRow } from "../../api/saveSync";
import { autoConsole, autoOf, autoTone, canTurnOn, firstCopyOf, keptInStep, newCopies, otherSide } from "./saveSync";
import { AutoSyncPanel } from "./SaveSyncAuto";

function row(key: string, status: SyncRow["status"], kind = "ps5", auto: AutoView | null = null): SyncRow {
  return {
    key,
    title_id: key,
    kind,
    status,
    reason: null,
    reason_side: null,
    a: { path: "/a", size: 1, changed_ms: null },
    b: { path: "/b", size: 2, changed_ms: null },
    synced_ms: null,
    last_error: null,
    auto,
  };
}

const pair: SyncPair = {
  id: "p1",
  a: { host: "10.0.0.1", user_id: 1 },
  b: { host: "10.0.0.2", user_id: 2 },
  excluded: [],
  auto: ["PPSA01234"],
  wake: false,
};
const profiles = [
  { host: "10.0.0.1", name: "Pro" },
  { host: "10.0.0.2", name: "Fat" },
];

function view(over: Partial<AutoView> = {}): AutoView {
  return {
    state: "in_step",
    title_id: "PPSA01234",
    kind: "ps5",
    side: null,
    reason: null,
    detail: null,
    direction: null,
    first: false,
    kept: [],
    since_ms: 1,
    fails: 0,
    woke_ms: null,
    copied_ms: null,
    copied_direction: null,
    ...over,
  };
}

function panel(v: AutoView | null, canTurnOn = true) {
  const noop = () => {};
  return renderToStaticMarkup(
    <AutoSyncPanel
      view={v}
      canTurnOn={canTurnOn}
      onLabel="Keep this game's save in step on Fat (Sam)"
      a="Pro"
      b="Fat"
      busy={false}
      error={null}
      onToggle={noop}
      onSyncNow={noop}
      onKeep={noop}
    />,
  );
}

describe("save sync: keeping a game in step", () => {
  it("only a save the pair keeps in step has a state, and one just turned on waits for the next look", () => {
    expect(keptInStep(pair, "PPSA01234")).toBe(true);
    expect(keptInStep(pair, "PPSA09999")).toBe(false);
    expect(keptInStep({ ...pair, auto: undefined }, "PPSA01234")).toBe(false);
    expect(autoOf(row("PPSA09999", "in_step"), pair)).toBeNull();
    const fresh = autoOf(row("PPSA01234", "a_newer"), pair);
    expect([fresh?.state, fresh?.reason]).toEqual(["waiting", "queued"]);
    const known = view({ state: "conflict" });
    expect(autoOf(row("PPSA01234", "both_changed", "ps5", known), pair)).toBe(known);
  });

  it("is offered for every save but a PS4 one", () => {
    expect(canTurnOn(row("A", "in_step"))).toBe(true);
    expect(canTurnOn(row("A", "both_changed"))).toBe(true);
    expect(canTurnOn(row("A", "unavailable", "ps4"))).toBe(false);
    // A PS5 save the other user has none of yet: it waits for the first copy or their own save.
    expect(canTurnOn(row("A", "only_on_a"))).toBe(true);
    // An emulator's goes into the same place there.
    expect(canTurnOn(row("A", "only_on_a", "ps2"))).toBe(true);
  });

  it("offers the first copy of a PS5 save only one side has, with the engine's answer", () => {
    expect(firstCopyOf(row("A", "only_on_a"))).toEqual({ to: "b", ok: false, why: null, code: null });
    expect(firstCopyOf({ ...row("A", "only_on_b"), first_copy: { ok: true } })).toEqual({
      to: "a",
      ok: true,
      why: null,
      code: null,
    });
    const old = firstCopyOf({ ...row("A", "only_on_a"), first_copy: { ok: false, why: "too old", code: "old_helper" } });
    expect([old?.why, old?.code]).toEqual(["too old", "old_helper"]);
    // Both have it, an emulator's save, or a console away: nothing to copy that way.
    expect(firstCopyOf(row("A", "a_newer"))).toBeNull();
    expect(firstCopyOf(row("A", "only_on_a", "ps2"))).toBeNull();
    expect(firstCopyOf({ ...row("A", "only_on_a"), reason: "offline" })).toBeNull();
  });

  it("names the other console from the one shown, and the console a state is about", () => {
    expect(otherSide(pair, "10.0.0.1")).toBe("b");
    expect(otherSide(pair, "10.0.0.2:9113")).toBe("a");
    expect(autoConsole(view({ side: "b" }), pair, profiles)).toBe("Fat");
    expect(autoConsole(view({ side: "a" }), pair, profiles)).toBe("Pro");
    expect(autoConsole(view(), pair, profiles)).toBeNull();
    expect(autoTone("in_step")).toBe("good");
    expect(autoTone("conflict")).toBe("warn");
    expect(autoTone("cant_sync")).toBe("bad");
    expect(autoTone("waiting")).toBe("neutral");
  });

  it("shows each state plainly", () => {
    const off = panel(null);
    expect(off).toContain("Keep this game&#x27;s save in step on Fat (Sam)");
    expect(off).not.toContain("Sync now");
    expect(panel(null, false)).toBe("");

    const inStep = panel(view({ copied_ms: Date.now() - 60_000, copied_direction: "a_to_b" }));
    expect(inStep).toContain("In step");
    expect(inStep).toContain("Last sent");
    expect(inStep).toContain("to Fat");
    expect(inStep).toContain("Sync now");

    const waiting = panel(view({ state: "waiting", side: "b", reason: "offline", direction: "a_to_b" }));
    expect(waiting).toContain("Waiting for Fat");
    expect(waiting).toContain("Fat is off or resting");

    const running = panel(view({ state: "waiting", side: "a", reason: "running" }));
    expect(running).toContain("Waiting for Pro");
    expect(running).toContain("The game is running on Pro");

    const conflict = panel(view({ state: "conflict", kept: ["x.zip", "y.zip"] }));
    expect(conflict).toContain("Conflict: choose");
    expect(conflict).toContain("Both are backed up");
    expect(conflict).toContain("Keep Pro&#x27;s save");
    expect(conflict).toContain("Keep Fat&#x27;s save");
    expect(conflict).not.toContain("Sync now");
    expect(panel(view({ state: "conflict", first: true }))).toContain("Not synced yet");

    const refused = panel(
      view({ state: "cant_sync", side: "b", reason: "refused", detail: "this helper can't change a save's owner" }),
    );
    expect(refused).toContain("Can&#x27;t sync");
    expect(refused).toContain("can&#x27;t be moved safely");
    expect(refused).toContain("change a save&#x27;s owner");

    const failed = panel(view({ state: "cant_sync", reason: "failed", fails: 3, detail: "lost the connection" }));
    expect(failed).toContain("Copying failed 3 times (lost the connection)");

    const noSave = panel(view({ state: "cant_sync", side: "b", reason: "no_save" }));
    expect(noSave).toContain("for the user on Fat yet");
  });

  it("tells of a copy made by itself once, and nothing from before the app looked", () => {
    const game = (copied_ms: number | null): SyncGame => ({
      ...view({ copied_ms, copied_direction: "a_to_b" }),
      pair: "p1",
      key: "PPSA01234",
      a: pair.a,
      b: pair.b,
      synced_ms: copied_ms,
    });
    const first = newCopies([game(5)], null);
    expect(first.fresh).toEqual([]);
    const same = newCopies([game(5)], first.seen);
    expect(same.fresh).toEqual([]);
    const later = newCopies([game(9)], same.seen);
    expect(later.fresh.map((g) => g.copied_ms)).toEqual([9]);
    // A save turned on later, with a copy made since the last look.
    const added = newCopies([game(9), { ...game(3), key: "PPSA05555" }], later.seen);
    expect(added.fresh.map((g) => g.key)).toEqual(["PPSA05555"]);
  });
});
