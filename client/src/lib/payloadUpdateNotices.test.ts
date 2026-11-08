import { describe, expect, it } from "vitest";

import { payloadUpdateNotices } from "./payloadUpdateNotices";

const row = (id: string, update: boolean) => ({
  payload_id: id,
  display_name: id.toUpperCase(),
  is_custom: false,
  installed_version: "v1",
  latest_version: update ? "v2" : "v1",
  update_available: update,
  prerelease: false,
  published_at: "",
  html_url: "",
});

const applied = (id: string) => ({ payload_id: id, display_name: id, from: "v1", to: "v2", at: 1, rollback: false });

describe("payload update notices", () => {
  it("announces nothing on the first run, only takes the counters", () => {
    const r = payloadUpdateNotices(
      { rows: [row("a", true)], applied: [applied("a")], available_seq: 3, applied_seq: 2 },
      null,
    );
    expect(r.notices).toEqual([]);
    expect(r.seen).toEqual({ available_seq: 3, applied_seq: 2 });
  });

  it("announces updates found since the last look", () => {
    const r = payloadUpdateNotices(
      { rows: [row("a", true), row("b", false), row("c", true)], applied: [], available_seq: 2, applied_seq: 0 },
      { available_seq: 1, applied_seq: 0 },
    );
    expect(r.notices).toEqual([{ kind: "available", names: ["A", "C"] }]);
  });

  it("announces only the updates applied since the last look", () => {
    const r = payloadUpdateNotices(
      { rows: [], applied: [applied("new"), applied("old")], available_seq: 1, applied_seq: 5 },
      { available_seq: 1, applied_seq: 4 },
    );
    expect(r.notices).toEqual([{ kind: "applied", items: [applied("new")] }]);
    expect(r.seen.applied_seq).toBe(5);
  });

  it("stays quiet when nothing moved, or the updates were already applied", () => {
    expect(
      payloadUpdateNotices(
        { rows: [row("a", true)], applied: [], available_seq: 1, applied_seq: 0 },
        { available_seq: 1, applied_seq: 0 },
      ).notices,
    ).toEqual([]);
    expect(
      payloadUpdateNotices(
        { rows: [row("a", false)], applied: [], available_seq: 2, applied_seq: 0 },
        { available_seq: 1, applied_seq: 0 },
      ).notices,
    ).toEqual([]);
  });
});
