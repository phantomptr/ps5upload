#!/usr/bin/env node
/**
 * scripts/gen-pad-mappings.mjs
 *
 * Builds client/src/lib/padMappingTable.ts from SDL's GameControllerDB (gamecontrollerdb.txt,
 * https://github.com/mdqinc/SDL_GameControllerDB, zlib licence): for the controllers people play
 * with that a browser may report without the "standard" layout (Gamepad.mapping === ""), where
 * each standard button and stick comes from in the controller's own (raw HID) order.
 *
 * Only the vendors listed below go in, and only the desktop platforms (Windows, macOS, Linux):
 * Android and iOS browsers lay every controller they support out the standard way. Microsoft's
 * own pads are left out (XInput, GameController and both browsers' own mappings make them
 * standard everywhere). Identical mappings are stored once.
 *
 * Usage:
 *   node scripts/gen-pad-mappings.mjs                       # fetch the pinned DB commit
 *   node scripts/gen-pad-mappings.mjs path/to/gamecontrollerdb.txt [commit]
 */
import fs from "node:fs";
import path from "node:path";

const repoRoot = path.resolve(import.meta.dirname, "..");
const OUT = path.join(repoRoot, "client/src/lib/padMappingTable.ts");
/** The DB commit the table was built from (bump it and re-run to refresh). */
const PINNED = "07af1d50187e94c861c9f9f5d8a55b148f11306b";

/** Vendors whose pads go in (USB vendor id → who). */
const VENDORS = {
  "2dc8": "8BitDo",
  "057e": "Nintendo",
  "054c": "Sony",
  "046d": "Logitech",
  "0079": "DragonRise (generic USB Gamepad)",
  "20d6": "PowerA",
  "24c6": "PowerA",
  "0e6f": "PDP",
  "0f0d": "Hori",
  "1532": "Razer",
  "27f8": "Razer (Kishi)",
  "358a": "Backbone",
  "3537": "GameSir",
  "04b4": "Flydigi",
  "18d1": "Google (Stadia)",
  "28de": "Valve (Steam Controller)",
};
/** GameSir's older pads use Apple's vendor id: only those, by name. */
const BY_NAME = { "05ac": /gamesir/i };

const XINPUT_NAME = /xbox|x-?input|\b360\b/i;

const PLATFORMS = { "Windows": "w", "Mac OS X": "m", "Linux": "l" };

/** The standard layout's buttons, in order (W3C Gamepad "standard" mapping, then the touchpad
 *  click Chrome adds for PlayStation pads), and its four stick axes. */
const BUTTONS = [
  "a", "b", "x", "y", "leftshoulder", "rightshoulder", "lefttrigger", "righttrigger", "back", "start",
  "leftstick", "rightstick", "dpup", "dpdown", "dpleft", "dpright", "guide", "touchpad",
];
const AXES = ["leftx", "lefty", "rightx", "righty"];

/** SDL2 GUID → { vendor, product } (hex, 4 digits), or null for a GUID made from a name. */
function ids(guid) {
  if (!/^[0-9a-f]{32}$/.test(guid)) return null;
  const word = (i) => guid.slice(i + 2, i + 4) + guid.slice(i, i + 2);
  // Bytes 4-5 vendor, 8-9 product (little endian); 6-7 and 10-11 are zero for a USB/BT id.
  if (word(12) !== "0000" || word(20) !== "0000") return null;
  const vendor = word(8);
  const product = word(16);
  if (vendor === "0000") return null;
  return { vendor, product };
}

const SRC = /^[+-]?(?:b\d+|a\d+~?|h0\.(?:1|2|4|8))$/;

/** An SDL mapping → our token string: the standard buttons then the sticks, comma-separated,
 *  each "" (absent) or SDL's own source (b3, a4, +a1, -a0, a5~, h0.1). Null when it cannot be
 *  stored (a split stick, a second hat). */
function encode(fields) {
  const m = new Map();
  for (const f of fields) {
    const i = f.indexOf(":");
    if (i > 0) m.set(f.slice(0, i), f.slice(i + 1));
  }
  // A stick made of two halves (+leftx:h0.2): a Joy-Con held sideways, its hat as the stick.
  for (const ax of AXES) {
    if (m.has(`+${ax}`) || m.has(`-${ax}`)) {
      if (m.get(`+${ax}`) === "h0.2" && m.get(`-${ax}`) === "h0.8") m.set(ax, "hx");
      else if (m.get(`+${ax}`) === "h0.4" && m.get(`-${ax}`) === "h0.1") m.set(ax, "hy");
      else return null;
    }
  }
  const out = [];
  for (const b of BUTTONS) {
    const v = m.get(b) ?? "";
    if (v && !SRC.test(v)) return null;
    out.push(v);
  }
  for (const a of AXES) {
    const v = m.get(a) ?? "";
    if (v && v !== "hx" && v !== "hy" && !/^a\d+~?$/.test(v)) return null;
    out.push(v);
  }
  return out.join(",").replace(/,+$/, "");
}

async function readDb() {
  const file = process.argv[2];
  if (file) return { text: fs.readFileSync(file, "utf8"), commit: process.argv[3] ?? "local file" };
  const url = `https://raw.githubusercontent.com/mdqinc/SDL_GameControllerDB/${PINNED}/gamecontrollerdb.txt`;
  const res = await fetch(url);
  if (!res.ok) throw new Error(`${url}: ${res.status}`);
  return { text: await res.text(), commit: PINNED };
}

const { text, commit } = await readDb();
/** key (platform letter + vendor + product) → mapping → how many DB lines gave it. */
const seen = new Map();
const names = new Map();
let lines = 0;
for (const line of text.split(/\r?\n/)) {
  if (!line || line.startsWith("#")) continue;
  const fields = line.split(",").filter(Boolean);
  const plat = fields.find((f) => f.startsWith("platform:"))?.slice("platform:".length);
  const p = PLATFORMS[plat];
  if (!p) continue;
  const id = ids(fields[0]);
  if (!id) continue;
  const name = fields[1];
  if (!(id.vendor in VENDORS) && !(BY_NAME[id.vendor]?.test(name))) continue;
  // An Xbox-style pad on Windows is an XInput pad there: the browser lays it out the standard way.
  if (p === "w" && XINPUT_NAME.test(name)) continue;
  const enc = encode(fields.slice(2));
  if (!enc) continue;
  lines++;
  const key = `${p}${id.vendor}${id.product}`;
  if (!seen.has(key)) seen.set(key, new Map());
  const byMap = seen.get(key);
  byMap.set(enc, (byMap.get(enc) ?? 0) + 1);
  if (!names.has(key)) names.set(key, name);
}

// One mapping per controller and platform: the one most DB lines agree on (the first on a tie).
const maps = [];
const table = {};
let conflicts = 0;
let shadowed = 0;
for (const [key, byMap] of [...seen].sort(([a], [b]) => a.localeCompare(b))) {
  // Browsers on Windows read a pad's HID buttons and axes as they do on macOS (Chrome by HID usage
  // on both, Firefox in descriptor order on both), while the DB's Windows lines are DirectInput's
  // order: the macOS line serves Windows too, and a Windows line is kept only for a pad with none.
  if (key.startsWith("w") && seen.has(`m${key.slice(1)}`)) {
    shadowed++;
    continue;
  }
  if (byMap.size > 1) conflicts++;
  const [best] = [...byMap].sort((a, b) => b[1] - a[1])[0];
  let at = maps.indexOf(best);
  if (at < 0) at = maps.push(best) - 1;
  table[key] = at;
}

const body = `// GENERATED by scripts/gen-pad-mappings.mjs: do not edit by hand, re-run it.
//
// Where each standard button and stick comes from on controllers a browser may report without the
// standard layout, by platform ("m" macOS, which serves Windows too; "w" Windows for a pad with no
// macOS line; "l" Linux), USB vendor and product id.
// Built from SDL_GameControllerDB (https://github.com/mdqinc/SDL_GameControllerDB, commit
// ${commit}), used under the zlib licence:
//
//   Copyright (C) 1997-2025 Sam Lantinga <slouken@libsdl.org>
//   This software is provided 'as-is', without any express or implied warranty. In no event will
//   the authors be held liable for any damages arising from the use of this software. Permission
//   is granted to anyone to use this software for any purpose, including commercial applications,
//   and to alter it and redistribute it freely, subject to the following restrictions:
//   1. The origin of this software must not be misrepresented; you must not claim that you wrote
//      the original software. If you use this software in a product, an acknowledgment in the
//      product documentation would be appreciated but is not required.
//   2. Altered source versions must be plainly marked as such, and must not be misrepresented as
//      being the original software.
//   3. This notice may not be removed or altered from any source distribution.
//
// Altered: only some vendors' pads (${Object.values(VENDORS).filter((v, i, a) => a.indexOf(v) === i).join(", ")}),
// one mapping per controller and platform, re-encoded. A mapping lists, comma-separated, the
// sources of the standard buttons (✕ ○ □ △ L1 R1 L2 R2 Create Options L3 R3 ↑ ↓ ← → PS
// touchpad, by position: the bottom face button is always the first) then the sticks (left x, y,
// right x, y), each in SDL's notation: b3 a button, a4 an axis (a5~ inverted), +a1 / -a0 half an
// axis, h0.1 / h0.2 / h0.4 / h0.8 the hat's up / right / down / left, hx / hy the hat as a stick.

/** Mappings, each stored once. */
export const PAD_MAPS: readonly string[] = ${JSON.stringify(maps, null, 2)};

/** "<platform><vendor><product>" → index in PAD_MAPS. */
export const PAD_TABLE: Readonly<Record<string, number>> = {
${rows(Object.entries(table).map(([k, v]) => `${k}:${v},`))}
};
`;

/** Entries several to a line. */
function rows(items, per = 8) {
  const out = [];
  for (let i = 0; i < items.length; i += per) out.push(`  ${items.slice(i, i + per).join(" ")}`);
  return out.join("\n");
}
fs.writeFileSync(OUT, body);
const bytes = Buffer.byteLength(body);
process.stdout.write(
  `${path.relative(repoRoot, OUT)}: ${Object.keys(table).length} controllers (from ${lines} DB lines, ${conflicts} with differing lines, ${shadowed} Windows lines served by macOS's), ${maps.length} distinct mappings, ${(bytes / 1024).toFixed(1)} KiB\n`,
);
