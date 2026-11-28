import { describe, expect, it } from "vitest";
import { renderToStaticMarkup } from "react-dom/server";

import { t } from "../strings";
import { STALE_MS, SaveSyncCard, linesFor, parseSaveSync, reasonText, type SyncGame } from "./saveSync";

const NOW = 1_800_000_000_000;

function game(over: Partial<SyncGame> = {}): SyncGame {
  return {
    title_id: "PPSA01234",
    user: "1e24b13c",
    other: "Fat",
    state: "in_step",
    reason: null,
    here: false,
    copied_ms: null,
    copied_here: null,
    ...over,
  };
}

/** What the helper's /api/save-sync answers: the mesh document as the engine wrote it. */
function answer(engines: Record<string, unknown>) {
  return { ok: true, rev: 3, doc: { v: 1, engines } };
}

describe("save sync on the console page", () => {
  it("reads every engine's entry and leaves out what it can't read", () => {
    const e = parseSaveSync(
      answer({
        desk: { name: "Desk Mac", at_ms: NOW - 60_000, games: [game(), { title_id: 5 }, game({ state: "odd" as never })] },
        docker: { name: "NAS", at_ms: NOW - 1000, games: [game({ title_id: "PPSA09999", user: "00000002" })] },
        broken: "x",
      }),
    );
    expect(e.map((x) => x.device)).toEqual(["NAS", "Desk Mac"]);
    expect(e[1].games).toHaveLength(1);
    // Nothing written yet, an older helper's error, or an unreadable document: nothing to show.
    expect(parseSaveSync({ ok: true, rev: 0, doc: null })).toEqual([]);
    expect(parseSaveSync({ ok: false, err: "doc_unreadable" })).toEqual([]);
    expect(parseSaveSync(null)).toEqual([]);
  });

  it("shows only the games of the user picked", () => {
    const e = parseSaveSync(answer({ desk: { name: "Desk", at_ms: NOW, games: [game(), game({ user: "00000002" })] } }));
    expect(linesFor(e, "1E24B13C")).toHaveLength(1);
    expect(linesFor(e, "00000003")).toHaveLength(0);
    expect(linesFor(e, null)).toHaveLength(0);
  });

  it("says what each game waits for, from this console's side", () => {
    expect(reasonText(game({ state: "waiting", reason: "offline" }))).toBe(t("cw_sync_wait_offline", { console: "Fat" }));
    expect(reasonText(game({ state: "waiting", reason: "running", here: true }))).toBe(t("cw_sync_wait_running_here"));
    expect(reasonText(game({ state: "waiting", reason: "running" }))).toBe(t("cw_sync_wait_running_there", { console: "Fat" }));
    expect(reasonText(game({ state: "waiting", reason: "no_save", here: true }))).toBe(t("cw_sync_no_save_here"));
    expect(reasonText(game({ state: "waiting", reason: "no_save" }))).toBe(t("cw_sync_no_save_there", { console: "Fat" }));
    expect(reasonText(game({ state: "conflict" }))).toBe(t("cw_sync_conflict"));
    expect(reasonText(game({ state: "cant_sync", reason: "failed" }))).toBe(t("cw_sync_failed"));
    expect(reasonText(game())).toBeNull();
    expect(reasonText(game({ copied_ms: NOW, copied_here: true }))).toContain(t("cw_sync_last_here", { when: "" }).trim());
  });

  it("renders nothing for a user with no game kept in step, and says when an engine went quiet", () => {
    const e = parseSaveSync(answer({ desk: { name: "Desk Mac", at_ms: NOW - STALE_MS, games: [game({ state: "conflict" })] } }));
    const names = new Map([["PPSA01234", "LEGO Horizon Adventures"]]);
    expect(renderToStaticMarkup(<SaveSyncCard entries={e} user="00000009" names={names} now={NOW} />)).toBe("");
    expect(renderToStaticMarkup(<SaveSyncCard entries={null} user="1e24b13c" names={names} now={NOW} />)).toBe("");
    const html = renderToStaticMarkup(<SaveSyncCard entries={e} user="1e24b13c" names={names} now={NOW} />);
    expect(html).toContain("LEGO Horizon Adventures");
    expect(html).toContain(t("cw_sync_choose"));
    expect(html).toContain("Desk Mac");
    expect(html).toContain(t("cw_sync_stale", { when: "", device: "Desk Mac" }).split(".")[0].split(" ")[0]);
    const fresh = renderToStaticMarkup(<SaveSyncCard entries={e} user="1e24b13c" names={names} now={NOW - STALE_MS + 1} />);
    expect(fresh).not.toBe(html);
  });
});
