import { describe, expect, it } from "vitest";
import { renderToStaticMarkup } from "react-dom/server";

import type { SyncPair, SyncRow, SyncSuggestion } from "../../api/saveSync";
import {
  applyItems,
  canSync,
  defaultDirection,
  directionsFor,
  initialSelection,
  needsFirstStart,
  promptFor,
  sortRows,
  suggestionId,
} from "./saveSync";
import { SyncPromptView } from "./SaveSyncPrompt";

function row(key: string, status: SyncRow["status"], kind = "ps5"): SyncRow {
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
  };
}

const pair: SyncPair = {
  id: "p1",
  a: { host: "10.0.0.1", user_id: 1 },
  b: { host: "10.0.0.2", user_id: 2 },
  excluded: [],
};
const profiles = [
  { host: "10.0.0.1", name: "Living room" },
  { host: "10.0.0.2", name: "Bedroom" },
];

function suggestion(over: Partial<SyncSuggestion> = {}): SyncSuggestion {
  return {
    pair: "p1",
    key: "PPSA01234",
    title_id: "PPSA01234",
    kind: "ps5",
    direction: "a_to_b",
    state: "ready",
    at_ms: 10,
    ...over,
  };
}

describe("save sync: which way each save goes", () => {
  it("sends from the side that changed, and asks when both did or it was never synced", () => {
    expect(defaultDirection(row("A", "a_newer"))).toBe("a_to_b");
    expect(defaultDirection(row("B", "b_newer"))).toBe("b_to_a");
    for (const s of ["both_changed", "first_time", "in_step", "unavailable", "excluded"] as const) {
      expect(defaultDirection(row("X", s))).toBeNull();
    }
    expect(directionsFor(row("C", "both_changed"))).toEqual(["a_to_b", "b_to_a"]);
    expect(directionsFor(row("A", "a_newer"))).toEqual(["a_to_b"]);
    expect(directionsFor(row("I", "in_step"))).toEqual([]);
  });

  it("asks for one start of a PS5 game where it has no save, and sends an emulator's as it is", () => {
    expect(needsFirstStart(row("P", "only_on_a"))).toBe("b");
    expect(needsFirstStart(row("P", "only_on_b"))).toBe("a");
    expect(canSync(row("P", "only_on_a"))).toBe(false);
    const emu = row("switch:0100", "only_on_a", "switch");
    expect(needsFirstStart(emu)).toBeNull();
    expect(defaultDirection(emu)).toBe("a_to_b");
  });

  it("picks the one-sided changes at first and sends a conflict only once picked", () => {
    const rows = [row("A", "a_newer"), row("C", "both_changed"), row("I", "in_step"), row("U", "unavailable")];
    const sel = initialSelection(rows);
    expect([...sel]).toEqual(["A"]);
    expect(applyItems(rows, sel, {})).toEqual([{ key: "A", direction: "a_to_b" }]);
    sel.add("C");
    // Selected with no pick yet: left out.
    expect(applyItems(rows, sel, {})).toEqual([{ key: "A", direction: "a_to_b" }]);
    expect(applyItems(rows, sel, { C: "b_to_a" })).toEqual([
      { key: "A", direction: "a_to_b" },
      { key: "C", direction: "b_to_a" },
    ]);
    // A pick against the changed side is not taken: it goes from the side that changed.
    expect(applyItems(rows, sel, { A: "b_to_a" })).toEqual([{ key: "A", direction: "a_to_b" }]);
    // Nothing for a save in step or unavailable, even if selected.
    expect(applyItems(rows, new Set(["I", "U"]), { I: "a_to_b" })).toEqual([]);
  });

  it("lists what needs the user first", () => {
    const sorted = sortRows([row("Z", "in_step"), row("B", "a_newer"), row("A", "both_changed"), row("U", "unavailable")]);
    expect(sorted.map((r) => r.key)).toEqual(["A", "B", "U", "Z"]);
  });
});

describe("save sync: the prompt after a game closes", () => {
  it("shows the oldest suggestion of a pair still set up, less those put off", () => {
    const old = suggestion({ key: "OLD", title_id: "OLD", at_ms: 1 });
    const newer = suggestion({ at_ms: 5, direction: "b_to_a" });
    const gone = suggestion({ pair: "removed", at_ms: 0 });
    const p = promptFor([newer, old, gone], [pair], profiles, new Set());
    expect(p?.suggestion.key).toBe("OLD");
    expect([p?.from, p?.to]).toEqual(["Living room", "Bedroom"]);
    const q = promptFor([newer, old], [pair], profiles, new Set([suggestionId(old)]));
    expect(q?.suggestion.key).toBe("PPSA01234");
    expect([q?.from, q?.to]).toEqual(["Bedroom", "Living room"]);
    expect(promptFor([], [pair], profiles, new Set())).toBeNull();
  });

  it("offers Send only when the other side is unchanged and answering", () => {
    const noop = () => {};
    const view = (s: SyncSuggestion) =>
      renderToStaticMarkup(
        <SyncPromptView
          prompt={{ suggestion: s, pair, from: "Living room", to: "Bedroom" }}
          busy={false}
          error={null}
          onSend={noop}
          onOpen={noop}
          onLater={noop}
        />,
      );
    const ready = view(suggestion());
    expect(ready).toContain("save to Bedroom?");
    expect(ready).toContain('data-testid="save-sync-send"');
    expect(ready).toContain("Not now");
    const waiting = view(suggestion({ state: "waiting" }));
    expect(waiting).toContain("Bedroom is off or resting");
    expect(waiting).not.toContain('data-testid="save-sync-send"');
    const conflict = view(suggestion({ state: "conflict" }));
    expect(conflict).toContain("changed on both PS5s");
    expect(conflict).not.toContain('data-testid="save-sync-send"');
  });
});

describe("an engine without save sync", () => {
  it("reads as nothing set up, never a crash", async () => {
    const { syncView } = await import("../../api/saveSync");
    for (const raw of [null, undefined, {}, "x", { pairs: {}, suggestions: null }, { ok: false, error: "not found" }]) {
      expect(syncView(raw)).toEqual({ pairs: [], suggestions: [], applying: [] });
    }
  });
});
