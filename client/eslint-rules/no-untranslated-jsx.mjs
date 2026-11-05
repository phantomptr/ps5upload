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
 *   3. The same attributes built without tr(): a template literal
 *      (title={`Unmount ${path}`}), or a literal or template in a branch of
 *      `a ? "x" : "y"` / `a ?? "x"` / `a || "x"`.
 *
 * It does NOT flag:
 *   • other {expr} children/attributes (tr() lives there). A template made
 *     only of placeholders and punctuation has no text of its own, so
 *     title={`${tr("a")}: ${name}`} is fine.
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
  "ms", "MB/s", "MiB/s", "MiB", "KB", "MB", "GB", "%", "·", "—", "–", "/", "@",
  // brand / proper nouns
  "PS5Upload", "PS5", "GitHub", "Discord", "PhantomPtr", "@phantomptr", "GPL-3",
  // payload names — identifiers of a binary, not prose. Their .elf siblings
  // ("a53_ppr_install_fast.elf", "shadowmountplus.elf") already match the
  // filename pattern below; this one is written without the extension.
  "kstuff",
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
  /^[A-Z0-9_]+=.*/, //   PS5UPLOAD_BANDWIDTH_MBPS=10  # …
  /^[*./\s]*\*\.[a-z]+/i, //   *.pkg  /  eboot.bin  /  PPSA*  /  *.log globs
  /\.(ini|log|gz|elf|bin|pkg|txt)\b/i, //   filenames
  /^\/[\w./-]+$/, //   absolute paths
  /^0x[0-9a-f]+$/i, //   hex example values (account ids, offsets)
  /^(rtmps?|srt|https?):\/\//i, //   URL examples in a placeholder
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

/**
 * The string parts of an attribute's expression a user would read as they are:
 * literals and the fixed text of templates, through `?:`, `??` and `||` (never
 * into a call, so tr(...) stays out). Each result is the text with a template's
 * placeholders dropped, so `Unmount ${path}` gives "Unmount".
 */
function literalParts(expr) {
  switch (expr.type) {
    case "Literal":
      return typeof expr.value === "string" ? [expr.value] : [];
    case "TemplateLiteral":
      return [expr.quasis.map((q) => q.value.cooked ?? "").join(" ")];
    case "ConditionalExpression":
      return [...literalParts(expr.consequent), ...literalParts(expr.alternate)];
    case "LogicalExpression":
      // `a && "x"`: only "x" can be text. `a ?? "x"` / `a || "x"`: either side.
      return expr.operator === "&&"
        ? literalParts(expr.right)
        : [...literalParts(expr.left), ...literalParts(expr.right)];
    case "TSAsExpression":
    case "TSNonNullExpression":
      return literalParts(expr.expression);
    default:
      return [];
  }
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
        let parts = [];
        if (init.type === "Literal" && typeof init.value === "string") {
          parts = [init.value];
        } else if (init.type === "JSXExpressionContainer") {
          parts = literalParts(init.expression);
        }
        for (const literal of parts) {
          if (!isTranslatable(literal)) continue;
          context.report({
            node,
            messageId: "attr",
            data: { attr: name, text: literal.trim().slice(0, 60) },
          });
        }
      },
    };
  },
};

export default {
  rules: { "no-untranslated-jsx": rule },
};
