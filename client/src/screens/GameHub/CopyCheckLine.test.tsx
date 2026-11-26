import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it, vi } from "vitest";

vi.mock("../../state/lang", async (original) => ({
  ...(await original<typeof import("../../state/lang")>()),
  useTr: () => (key: string, vars?: Record<string, string | number>, fallback?: string) => {
    let s = fallback ?? key;
    for (const [k, v] of Object.entries(vars ?? {})) s = s.replace(`{${k}}`, String(v));
    return s;
  },
}));

import type { CopyCheck, CopyDamage } from "../../api/collection";
import { CopyProblems, runningText } from "./CopyCheckLine";
import { checkPercent } from "../../state/copyChecks";
import type { Translator } from "../../state/lang";

const damage = (d: Partial<CopyDamage> & { kind: string }): CopyDamage => ({
  blocks_bad: 0,
  blocks_total: 0,
  block_bytes: 0,
  bytes_have: 0,
  bytes_need: 0,
  checks_failed: 0,
  ...d,
});

const check = (over: Partial<CopyCheck>): CopyCheck => ({
  path: "/games/Game.pkg",
  state: "problems",
  method: "package",
  bytes_done: 0,
  bytes_total: 0,
  files_done: 0,
  files_total: 0,
  problems: [],
  problems_total: 0,
  error: null,
  started_ms: 0,
  finished_ms: 1,
  ...over,
});

const render = (c: CopyCheck) =>
  renderToStaticMarkup(<CopyProblems check={c} how="verified against its own digests" />);

/** The text a person reads without opening anything. */
const shown = (html: string) => html.slice(0, html.indexOf("<details"));

describe("a copy check that found problems", () => {
  // The lines a user was shown and asked about: "what does this mean? do I have to download
  // it again?"
  const reported = check({
    problems_total: 4,
    problems: [
      { file: "", error: "failed: cnt general digest system", kind: "metadata" },
      {
        file: "",
        error: "failed: outer blocks match their imagedigs entry (56116 of 1529208 failed)",
        kind: "data",
      },
      { file: "", error: "failed: si playgo-chunk.crc (6126352 bytes)", kind: "metadata" },
      { file: "", error: "failed: cnt general digest game", kind: "metadata" },
    ],
    damage: damage({ kind: "data", blocks_bad: 56116, blocks_total: 1529208, block_bytes: 65536 }),
  });

  it("says a damaged file is damaged, by how much, and to get it again", () => {
    const html = render(reported);
    const top = shown(html);
    expect(top).toContain(
      "This file&#x27;s data is damaged: 56,116 of 1,529,208 blocks don&#x27;t match (3.7%, about 3.43 GB).",
    );
    expect(top).toContain("a copy or download was interrupted or went wrong. Copy or download it again.");
    expect(html).toContain('data-verdict="data"');
    // No "(s)", and none of the check's own words until Details is opened.
    expect(html).not.toContain("(s)");
    expect(top).not.toContain("imagedigs");
    expect(top).not.toContain("digest");
    expect(top).not.toContain("failed:");
  });

  it("keeps the technical lines behind Details, for a bug report", () => {
    const html = render(reported);
    const details = html.slice(html.indexOf("<details"));
    expect(details).not.toMatch(/<details[^>]* open/);
    expect(details).toContain(">Details</summary>");
    expect(details).toContain("How it was checked: verified against its own digests.");
    expect(details).toContain("failed: cnt general digest system");
    expect(details).toContain("failed: outer blocks match their imagedigs entry (56116 of 1529208 failed)");
    expect(details).toContain("failed: si playgo-chunk.crc (6126352 bytes)");
  });

  it("says a file cut short is cut short, with its size when the check has it", () => {
    const sized = shown(
      render(
        check({
          problems_total: 1,
          damage: damage({ kind: "truncated", bytes_have: 40 * 2 ** 30, bytes_need: 93 * 2 ** 30 }),
        }),
      ),
    );
    expect(sized).toContain("This file is cut short: it is 40 GB and should be at least 93 GB.");
    expect(sized).toContain("The copy or download stopped before the end. Copy or download it again.");
    const end = shown(render(check({ problems_total: 1, damage: damage({ kind: "truncated" }) })));
    expect(end).toContain("This file is cut short: its end is missing.");
  });

  it("does not call a file damaged when only its header information is off", () => {
    const html = render(check({ problems_total: 2, damage: damage({ kind: "metadata", checks_failed: 2 }) }));
    const top = shown(html);
    expect(top).toContain("The game&#x27;s data is intact");
    expect(top).toContain("Checks that failed: 2.");
    expect(top).toContain("It may still install. If the PS5 refuses it, copy or download it again.");
    expect(top).not.toContain("damaged");
    // A caution, not an error.
    expect(html).toContain("text-[var(--color-warn)]");
    expect(html).not.toContain("text-[var(--color-bad)]");
  });

  it("says when the file's own tables are what is damaged", () => {
    const top = shown(render(check({ problems_total: 3, damage: damage({ kind: "structure", checks_failed: 3 }) })));
    expect(top).toContain("This file&#x27;s own tables are damaged, so the game can&#x27;t be read out of it.");
    expect(top).toContain("Checks that failed: 3.");
  });

  it("counts the problems neutrally for a folder, an archive or an older engine", () => {
    const html = render(
      check({
        method: "files",
        problems_total: 7,
        problems: Array.from({ length: 6 }, (_, i) => ({ file: `data/${i}.bin`, error: "ends early: 1 of 2 bytes" })),
      }),
    );
    const top = shown(html);
    expect(top).toContain("Problems found: 7.");
    expect(top).toContain("This copy may not install or play. Copy or download it again.");
    expect(html).not.toContain("(s)");
    // Five lines, then how many more.
    expect(html).toContain("data/4.bin: ends early");
    expect(html).not.toContain("data/5.bin");
    expect(html).toContain("and 2 more");
  });
});

describe("a running copy check (#951)", () => {
  const tr = ((key: string, vars?: Record<string, string | number>, fallback?: string) => {
    let s = fallback ?? key;
    for (const [k, v] of Object.entries(vars ?? {})) s = s.replace(`{${k}}`, String(v));
    return s;
  }) as Translator;

  it("never says 100% while it runs", () => {
    expect(checkPercent({ bytes_done: 220_484_009_984, bytes_total: 220_484_009_984 })).toBe(99);
    expect(checkPercent({ bytes_done: 50, bytes_total: 100 })).toBe(50);
    expect(checkPercent({ bytes_done: 0, bytes_total: 0 })).toBeNull();
  });

  it("says which read of a package it is on", () => {
    const blocks = runningText(tr, { bytes_done: 40, bytes_total: 100, phase: "blocks" }, 40);
    expect(blocks).toMatch(/^Checking the data blocks: 40% /);
    const playgo = runningText(tr, { bytes_done: 70, bytes_total: 100, phase: "playgo" }, 70);
    expect(playgo).toMatch(/^Checking the PlayGo chunks: 70% /);
    // An engine that names no read: as before.
    expect(runningText(tr, { bytes_done: 70, bytes_total: 100 }, 70)).toMatch(/^Checking 70% /);
  });
});
