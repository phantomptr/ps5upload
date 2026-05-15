/**
 * Custom ESLint rule: no-untranslated-jsx
 *
 * Flags user-visible text that bypasses the i18n layer (`useTr()` →
 * `tr(...)`). This is the enforcement the locale-parity gate
 * (`scripts/i18n-coverage.mjs`) structurally cannot do: that gate only
 * compares locale files against each other, so a string that was never
 * turned into a key at all is invisible to it. This rule catches the
 * string *at the point it should have become a key*.
 *
 * It reports two things:
 *   1. JSX text nodes      — <span>Cancel</span>
 *   2. UI-facing string-literal attributes — placeholder="Search…",
 *      title="…", alt="…", aria-label="…", label="…"
 *
 * It does NOT flag:
 *   • {expr} children/attributes (already dynamic — tr() lives there)
 *   • whitespace / punctuation / number-only / entity-only text
 *   • anything in ALLOWED_STRINGS or matching ALLOWED_PATTERNS below
 *
 * ── Curating the allowlist ──────────────────────────────────────────
 * ALLOWED_STRINGS / ALLOWED_PATTERNS are the *genuine* non-translatables:
 * units (ms, MB/s), code/identifiers (cargo build, config.ini), brand
 * names (PS5Upload), example values (192.168.1.50). Keep them tight —
 * every entry here is text a non-English user will see in English. If
 * you're unsure, it probably belongs as a real tr() key instead.
 */

// Exact-match strings that are intentionally not translated.
const ALLOWED_STRINGS = new Set([
  // units / symbols
  "ms", "MB/s", "KB", "MB", "GB", "%", "·", "—", "–", "/", "@",
  // brand / proper nouns
  "PS5Upload", "PS5", "GitHub", "PhantomPtr", "@phantomptr", "GPL-3",
  // example values shown verbatim in inputs
  "192.168.1.50",
  // code identifiers / paths / snippets surfaced as literal text
  "cargo build", "client/src-tauri", "ps5upload.elf.gz", "ps5upload.elf",
  "config.ini", "autotune.ini", "/data/shadowmount/autotune.ini",
  "app_id", "ps5-versions.elf",
  // bare file-extension labels surfaced as standalone code chips
  // (e.g. <code>.elf</code> in the "typical ports" reference table).
  // These are language-neutral tokens — translating them would be
  // wrong, not just busywork.
  ".elf", ".bin", ".js", ".lua", ".jar", ".pkg", ".ffpkg", ".exfat",
]);

// Regex allowlist for code-like literals that vary (paths, env lines,
// glob examples). Tested against the trimmed text.
const ALLOWED_PATTERNS = [
  /^[A-Z0-9_]+=.*/, //   FTX2_BANDWIDTH_MBPS=10  # …
  /^[*./\s]*\*\.[a-z]+/i, //   *.pkg  /  eboot.bin  /  PPSA*  /  *.log globs
  /\.(ini|log|gz|elf|bin|pkg|txt)\b/i, //   filenames
  /^\/[\w./-]+$/, //   absolute paths
];

// JSX attributes whose string value is rendered to the user.
const UI_ATTRS = new Set([
  "placeholder",
  "title",
  "alt",
  "label",
  "aria-label",
  "aria-description",
  "aria-placeholder",
  "aria-valuetext",
]);

/** True when `raw` carries text a user would read and expect localized. */
function isTranslatable(raw) {
  const text = raw.replace(/&[a-zA-Z]+;/g, "").trim();
  if (!text) return false;
  // Needs at least one real word (2+ letters) to be "text".
  if (!/[A-Za-z]{2,}/.test(text)) return false;
  if (ALLOWED_STRINGS.has(text)) return false;
  if (ALLOWED_PATTERNS.some((re) => re.test(text))) return false;
  return true;
}

/** @type {import("eslint").Rule.RuleModule} */
const rule = {
  meta: {
    type: "problem",
    docs: {
      description:
        "Disallow hardcoded user-visible strings in JSX; route them through useTr()/tr().",
    },
    messages: {
      jsxText:
        'Hardcoded UI text "{{text}}" — wrap it in tr("some.key", "{{text}}") and add the key to src/i18n/locales/en.ts.',
      attr:
        'Hardcoded {{attr}}="{{text}}" — use {{attr}}={tr("some.key", "{{text}}")} and add the key to src/i18n/locales/en.ts.',
    },
    schema: [],
  },
  create(context) {
    return {
      JSXText(node) {
        if (!isTranslatable(node.value)) return;
        const text = node.value.trim().replace(/\s+/g, " ").slice(0, 60);
        context.report({ node, messageId: "jsxText", data: { text } });
      },
      JSXAttribute(node) {
        const name =
          node.name.type === "JSXNamespacedName"
            ? `${node.name.namespace.name}:${node.name.name.name}`
            : node.name.name;
        if (!UI_ATTRS.has(name)) return;
        const init = node.value;
        if (!init) return;
        let literal = null;
        if (init.type === "Literal" && typeof init.value === "string") {
          literal = init.value;
        } else if (
          init.type === "JSXExpressionContainer" &&
          init.expression.type === "Literal" &&
          typeof init.expression.value === "string"
        ) {
          literal = init.expression.value;
        }
        if (literal === null || !isTranslatable(literal)) return;
        context.report({
          node,
          messageId: "attr",
          data: { attr: name, text: literal.slice(0, 60) },
        });
      },
    };
  },
};

export default {
  rules: { "no-untranslated-jsx": rule },
};
