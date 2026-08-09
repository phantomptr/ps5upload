export const NANODNS_INI_PATH = "/data/nanodns/nanodns.ini";
export const DEFAULT_NANODNS_LOG_PATH = "/data/nanodns/nanodns.log";

export const OLD_YANDEX_DNS = "77.77.88.88";
export const CURRENT_YANDEX_DNS = "77.88.8.8";

const GENERAL_KEYS = new Set(["log", "debug", "quiet", "bind", "bind6"]);
const GENERAL_SECTIONS = new Set(["general", "settings"]);
const UPSTREAM_SECTIONS = new Set(["upstream", "upstreams"]);

export type NanoDnsGeneration = "legacy" | "modern" | "unknown";
export type NanoDnsVersionSource = "runtime-log" | "config" | "unknown";

export interface NanoDnsVersionDetection {
  /** Exact banner version when a runtime log was available. */
  version: string | null;
  generation: NanoDnsGeneration;
  source: NanoDnsVersionSource;
}

export type NanoDnsMigrationChange = "quiet" | "bind6" | "yandex-dns";

export interface NanoDnsMigrationResult {
  text: string;
  changes: NanoDnsMigrationChange[];
}

type Section = "none" | "general" | "upstream" | "other";

function sectionForLine(line: string): Section | null {
  const match = line.match(/^\s*\[([^\]]+)\]/);
  if (!match) return null;
  const name = match[1].trim().toLowerCase();
  if (GENERAL_SECTIONS.has(name)) return "general";
  if (UPSTREAM_SECTIONS.has(name)) return "upstream";
  return "other";
}

function withoutInlineComment(line: string): string {
  const hash = line.indexOf("#");
  const semicolon = line.indexOf(";");
  let end = line.length;
  if (hash >= 0) end = Math.min(end, hash);
  if (semicolon >= 0) end = Math.min(end, semicolon);
  return line.slice(0, end).trim();
}

function assignmentForLine(
  line: string,
): { key: string; value: string } | null {
  const active = withoutInlineComment(line);
  if (!active || active.startsWith("[") || active.startsWith("#") || active.startsWith(";")) {
    return null;
  }
  const equals = active.indexOf("=");
  if (equals < 0) return null;
  const key = active.slice(0, equals).trim().toLowerCase();
  const value = active.slice(equals + 1).trim();
  if (!key || !value) return null;
  return { key, value };
}

function splitLines(text: string): {
  lines: string[];
  newline: "\n" | "\r\n";
} {
  return {
    lines: text.split(/\r?\n/),
    newline: text.includes("\r\n") ? "\r\n" : "\n",
  };
}

/**
 * Read the last active value for a nanoDNS [general]/[settings] key.
 * nanoDNS also accepts these keys before the first section, and its parser uses
 * the last valid occurrence, so this deliberately mirrors that behavior.
 */
export function nanoDnsGeneralValue(text: string, requestedKey: string): string | null {
  const wanted = requestedKey.trim().toLowerCase();
  if (!GENERAL_KEYS.has(wanted)) return null;

  let section: Section = "none";
  let found: string | null = null;
  for (const line of splitLines(text).lines) {
    const nextSection = sectionForLine(line);
    if (nextSection) {
      section = nextSection;
      continue;
    }
    const assignment = assignmentForLine(line);
    if (
      assignment &&
      assignment.key === wanted &&
      (section === "general" || section === "none")
    ) {
      found = assignment.value;
    }
  }
  return found;
}

/** Resolve the log nanoDNS actually opens, falling back to its compiled path. */
export function nanoDnsLogPath(configText: string): string {
  return nanoDnsGeneralValue(configText, "log") ?? DEFAULT_NANODNS_LOG_PATH;
}

/** True only for an active upstream entry, never a comment or an override. */
export function hasOldYandexDns(configText: string): boolean {
  let section: Section = "none";
  for (const line of splitLines(configText).lines) {
    const nextSection = sectionForLine(line);
    if (nextSection) {
      section = nextSection;
      continue;
    }
    const assignment = assignmentForLine(line);
    if (
      section === "upstream" &&
      assignment &&
      (assignment.key === "server" || assignment.key === "dns") &&
      assignment.value === OLD_YANDEX_DNS
    ) {
      return true;
    }
  }
  return false;
}

function generationForVersion(version: string): NanoDnsGeneration {
  const match = version.match(/^(\d+)\.(\d+)/);
  if (!match) return "unknown";
  const major = Number(match[1]);
  const minor = Number(match[2]);
  return major > 0 || minor >= 4 ? "modern" : "legacy";
}

/**
 * Prefer nanoDNS's startup banner because a 0.4 binary can intentionally run a
 * 0.3 config. Config keys are only a fallback signal when the log is missing.
 */
export function detectNanoDnsVersion(
  logText: string | null | undefined,
  configText: string,
): NanoDnsVersionDetection {
  const banner = logText?.match(/\bnanodns\s+v(\d+\.\d+(?:\.\d+)?(?:[-+][A-Za-z0-9._-]+)?)/i);
  if (banner) {
    return {
      version: banner[1],
      generation: generationForVersion(banner[1]),
      source: "runtime-log",
    };
  }

  if (
    nanoDnsGeneralValue(configText, "quiet") !== null ||
    nanoDnsGeneralValue(configText, "bind6") !== null
  ) {
    return { version: null, generation: "modern", source: "config" };
  }

  return { version: null, generation: "unknown", source: "unknown" };
}

function generalInsertionIndex(lines: string[]): number {
  let generalStart = -1;
  for (let index = 0; index < lines.length; index++) {
    const section = sectionForLine(lines[index]);
    if (section === "general") {
      generalStart = index;
      break;
    }
  }

  if (generalStart < 0) {
    const firstSection = lines.findIndex((line) => sectionForLine(line) !== null);
    let insertion = firstSection < 0 ? lines.length : firstSection;
    while (insertion > 0 && lines[insertion - 1].trim() === "") insertion--;
    return insertion;
  }

  let insertion = lines.length;
  for (let index = generalStart + 1; index < lines.length; index++) {
    if (sectionForLine(lines[index]) !== null) {
      insertion = index;
      break;
    }
  }
  while (insertion > generalStart + 1 && lines[insertion - 1].trim() === "") {
    insertion--;
  }
  return insertion;
}

function appendMissingGeneralSettings(
  text: string,
  settings: ReadonlyArray<readonly [string, string]>,
): string {
  const missing = settings.filter(
    ([key]) => nanoDnsGeneralValue(text, key) === null,
  );
  if (missing.length === 0) return text;

  const { lines, newline } = splitLines(text);
  const hasGeneralSection = lines.some(
    (line) => sectionForLine(line) === "general",
  );
  const insertion = generalInsertionIndex(lines);
  const additions = missing.map(([key, value]) => `${key}=${value}`);

  if (!hasGeneralSection) {
    additions.unshift("[general]");
    if (insertion > 0 && lines[insertion - 1].trim() !== "") {
      additions.unshift("");
    }
  }

  lines.splice(insertion, 0, ...additions);
  if (
    insertion + additions.length < lines.length &&
    lines[insertion + additions.length].trim() !== ""
  ) {
    lines.splice(insertion + additions.length, 0, "");
  }
  return lines.join(newline);
}

/**
 * Change one [general] setting without reserializing the user's file. The last
 * active occurrence wins in nanoDNS, so updating that occurrence is sufficient
 * and preserves comments, aliases, ordering, and unrelated duplicate history.
 */
export function setNanoDnsGeneralValue(
  text: string,
  requestedKey: "quiet" | "bind6",
  value: string,
): string {
  const { lines, newline } = splitLines(text);
  let section: Section = "none";
  let matchIndex = -1;

  for (let index = 0; index < lines.length; index++) {
    const nextSection = sectionForLine(lines[index]);
    if (nextSection) {
      section = nextSection;
      continue;
    }
    const assignment = assignmentForLine(lines[index]);
    if (
      assignment?.key === requestedKey &&
      (section === "general" || section === "none")
    ) {
      matchIndex = index;
    }
  }

  if (matchIndex < 0) {
    return appendMissingGeneralSettings(text, [[requestedKey, value]]);
  }

  const escapedKey = requestedKey.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
  const pattern = new RegExp(
    `^(\\s*${escapedKey}\\s*=\\s*)[^#;]*?(\\s*(?:[#;].*)?)$`,
    "i",
  );
  lines[matchIndex] = lines[matchIndex].replace(
    pattern,
    (_whole, prefix: string, suffix: string) => `${prefix}${value}${suffix}`,
  );
  return lines.join(newline);
}

/**
 * Upgrade only the portable parts of a 0.3 config. This intentionally does not
 * replace the file with 0.4 defaults: custom resolvers, rules, exceptions,
 * comments, and formatting all survive.
 */
export function migrateNanoDns04Config(configText: string): NanoDnsMigrationResult {
  const changes: NanoDnsMigrationChange[] = [];
  const { lines, newline } = splitLines(configText);
  let section: Section = "none";

  for (let index = 0; index < lines.length; index++) {
    const nextSection = sectionForLine(lines[index]);
    if (nextSection) {
      section = nextSection;
      continue;
    }
    const assignment = assignmentForLine(lines[index]);
    if (
      section === "upstream" &&
      assignment &&
      (assignment.key === "server" || assignment.key === "dns") &&
      assignment.value === OLD_YANDEX_DNS
    ) {
      lines[index] = lines[index].replace(OLD_YANDEX_DNS, CURRENT_YANDEX_DNS);
      if (!changes.includes("yandex-dns")) changes.push("yandex-dns");
    }
  }

  let migrated = lines.join(newline);
  if (nanoDnsGeneralValue(migrated, "quiet") === null) changes.push("quiet");
  if (nanoDnsGeneralValue(migrated, "bind6") === null) changes.push("bind6");
  migrated = appendMissingGeneralSettings(migrated, [
    ["quiet", "0"],
    ["bind6", "::1"],
  ]);

  return { text: migrated, changes };
}

/** Correct the 0.3 default typo without opting the file into 0.4 settings. */
export function fixNanoDnsYandexDns(configText: string): NanoDnsMigrationResult {
  const { lines, newline } = splitLines(configText);
  const changes: NanoDnsMigrationChange[] = [];
  let section: Section = "none";

  for (let index = 0; index < lines.length; index++) {
    const nextSection = sectionForLine(lines[index]);
    if (nextSection) {
      section = nextSection;
      continue;
    }
    const assignment = assignmentForLine(lines[index]);
    if (
      section === "upstream" &&
      assignment &&
      (assignment.key === "server" || assignment.key === "dns") &&
      assignment.value === OLD_YANDEX_DNS
    ) {
      lines[index] = lines[index].replace(OLD_YANDEX_DNS, CURRENT_YANDEX_DNS);
      if (!changes.includes("yandex-dns")) changes.push("yandex-dns");
    }
  }

  return { text: lines.join(newline), changes };
}
