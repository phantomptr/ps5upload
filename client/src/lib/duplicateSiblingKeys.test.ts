import { describe, expect, it } from "vitest";

/**
 * Two sibling JSX elements must never share a `key`.
 *
 * React reconciles a child list by key. Two siblings with the same key is
 * undefined behaviour, and the way it actually failed here is worth
 * knowing, because nothing about it looks like a key bug: the Console tab
 * grew a NEW copy of the Fan threshold card on every 5-second sensor poll,
 * reaching dozens of cards the longer the page stayed open. It is silent in
 * a production build — React only warns about duplicate keys in
 * development — so it reached a release and a user reported it (v5.13.0).
 *
 * The cause is easy to reintroduce: `key={host ?? ""}` is a natural thing
 * to write on a card that should reset when the console changes, and the
 * second person to want that writes the identical expression on the card
 * next to it. Nothing flags the collision.
 */

const SOURCES = import.meta.glob("../**/*.tsx", {
  query: "?raw",
  import: "default",
  eager: true,
}) as Record<string, string>;

export interface DuplicateKey {
  key: string;
  lines: number[];
}

/**
 * Duplicate `key=` expressions among what are, by indentation, sibling
 * elements.
 *
 * Siblings in one container all sit at the same indentation, and any line
 * between two of them belongs to that container too — so it is indented at
 * least as far as the elements themselves. A line that dedents past that
 * means a container closed, and the two elements are not siblings.
 *
 * `.map()` callbacks are the common false positive: two different lists can
 * legitimately both use `key={item.id}`. Those sit inside separate JSX
 * blocks, so the dedent rule separates them.
 */
export function findDuplicateSiblingKeys(src: string): DuplicateKey[] {
  const lines = src.split("\n");
  const found: { expr: string; indent: number; line: number }[] = [];
  lines.forEach((text, i) => {
    const m = /^(\s*)key=\{(.+)\}\s*$/.exec(text);
    if (m) found.push({ expr: m[2].trim(), indent: m[1].length, line: i + 1 });
  });

  const indentOf = (text: string) => text.length - text.trimStart().length;
  const out: DuplicateKey[] = [];

  for (let a = 0; a < found.length; a++) {
    for (let b = a + 1; b < found.length; b++) {
      if (found[a].expr !== found[b].expr) continue;
      if (found[a].indent !== found[b].indent) continue;
      // The element owning the key is one level out from the key prop.
      const floor = found[a].indent - 2;
      let sameParent = true;
      for (let i = found[a].line; i < found[b].line - 1; i++) {
        const t = lines[i];
        if (!t.trim()) continue;
        if (indentOf(t) < floor) {
          sameParent = false;
          break;
        }
      }
      if (!sameParent) continue;
      const hit = out.find((d) => d.key === found[a].expr);
      if (hit) {
        if (!hit.lines.includes(found[b].line)) hit.lines.push(found[b].line);
      } else {
        out.push({ key: found[a].expr, lines: [found[a].line, found[b].line] });
      }
    }
  }
  return out;
}

describe("findDuplicateSiblingKeys", () => {
  it("flags two siblings sharing a key", () => {
    const src = [
      '        <div className="grid">',
      "          <CardA",
      '            key={host ?? ""}',
      "          />",
      "          <CardB",
      '            key={host ?? ""}',
      "          />",
      "        </div>",
    ].join("\n");
    const dups = findDuplicateSiblingKeys(src);
    expect(dups).toHaveLength(1);
    expect(dups[0].lines).toEqual([3, 6]);
  });

  it("allows the same key expression in two separate lists", () => {
    // Both are `key={p.id}`, but each belongs to its own container, so
    // neither can collide with the other during reconciliation.
    const src = [
      "        <div>",
      "          {a.map((p) => (",
      "            <Row",
      "              key={p.id}",
      "            />",
      "          ))}",
      "        </div>",
      "        <div>",
      "          {b.map((p) => (",
      "            <Row",
      "              key={p.id}",
      "            />",
      "          ))}",
      "        </div>",
    ].join("\n");
    expect(findDuplicateSiblingKeys(src)).toEqual([]);
  });

  it("does not flag distinct key expressions", () => {
    const src = [
      "        <div>",
      "          <CardA",
      '            key={`fan-${host}`}',
      "          />",
      "          <CardB",
      '            key={`time-${host}`}',
      "          />",
      "        </div>",
    ].join("\n");
    expect(findDuplicateSiblingKeys(src)).toEqual([]);
  });
});

describe("no component renders sibling elements with the same key", () => {
  it("holds across every .tsx file", () => {
    const offenders: string[] = [];
    for (const [path, src] of Object.entries(SOURCES)) {
      if (/\.test\.tsx?$/.test(path)) continue;
      for (const d of findDuplicateSiblingKeys(src)) {
        offenders.push(`${path}: key={${d.key}} at lines ${d.lines.join(", ")}`);
      }
    }
    // Give each sibling its own key — e.g. key={`fan-${host}`} and
    // key={`systime-${host}`} — so React can tell them apart.
    expect(offenders).toEqual([]);
  });

  it("actually scanned the components", () => {
    expect(Object.keys(SOURCES).length).toBeGreaterThan(30);
    expect(Object.keys(SOURCES)).toContain("../screens/Hardware/index.tsx");
  });
});
