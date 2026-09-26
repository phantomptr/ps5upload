import js from "@eslint/js";
import reactHooks from "eslint-plugin-react-hooks";
import tseslint from "typescript-eslint";
import localRules from "./eslint-rules/no-untranslated-jsx.mjs";
import requireAssertOk from "./eslint-rules/require-assert-ok.mjs";

const browserGlobals = {
  AbortController: "readonly",
  Blob: "readonly",
  CustomEvent: "readonly",
  Event: "readonly",
  EventSource: "readonly",
  File: "readonly",
  FormData: "readonly",
  HTMLAnchorElement: "readonly",
  HTMLElement: "readonly",
  Image: "readonly",
  KeyboardEvent: "readonly",
  MouseEvent: "readonly",
  RequestInit: "readonly",
  Response: "readonly",
  URL: "readonly",
  URLSearchParams: "readonly",
  console: "readonly",
  crypto: "readonly",
  document: "readonly",
  fetch: "readonly",
  localStorage: "readonly",
  navigator: "readonly",
  setInterval: "readonly",
  setTimeout: "readonly",
  clearInterval: "readonly",
  clearTimeout: "readonly",
  window: "readonly",
};

export default tseslint.config(
  {
    ignores: [
      "dist/**",
      "node_modules/**",
      "src-tauri/**",
      "coverage/**",
      "vite.config.ts",
      // Build-time scripts run in Node, not the browser, so the
      // browser-globals config here gives false positives ("console"
      // unknown). They're tooling, not shipped code.
      "scripts/**",
    ],
  },
  js.configs.recommended,
  ...tseslint.configs.recommended,
  {
    files: ["src/**/*.{ts,tsx}"],
    languageOptions: {
      ecmaVersion: "latest",
      sourceType: "module",
      globals: browserGlobals,
    },
    plugins: {
      "react-hooks": reactHooks,
      local: {
        rules: { ...localRules.rules, ...requireAssertOk.rules },
      },
    },
    rules: {
      ...reactHooks.configs.recommended.rules,
      "react-hooks/immutability": "off",
      "react-hooks/set-state-in-effect": "off",
      // No BigInt literals. The build targets safari13 (see vite.config.ts)
      // so the app survives old Android System WebViews, and BigInt is ES2020
      // that — unlike `?.` or `??=` — CANNOT be down-levelled. Rolldown emits
      // the literal as-is with a TOLERATED_TRANSFORM warning, which is easy to
      // miss because the build still succeeds; on a WebView without BigInt it
      // is a parse-time SyntaxError for the whole chunk, so it takes out the
      // entire bundle rather than the one feature that used it.
      //
      // This has been walked into twice (lib/wakeState.ts, then
      // screens/Profile). Both times the fix was exact string arithmetic,
      // which is what this rule points you at.
      "no-restricted-syntax": [
        "error",
        {
          selector: "Literal[bigint]",
          message:
            "BigInt literals cannot be down-levelled for the safari13 build target and will break the whole bundle on old WebViews. Use string arithmetic instead (see decimalToHex in screens/Profile, or lib/wakeState).",
        },
      ],
      // Enforcement gate: every user-visible string must go through
      // useTr()/tr(). The locale-parity gate (scripts/i18n-coverage.mjs)
      // can only see keys that already exist — this catches strings
      // before they ever become (missing) keys. See
      // eslint-rules/no-untranslated-jsx.mjs. "error" because the
      // existing backlog (178 strings) has been fully extracted — a
      // new hardcoded string is now a build failure, not a warning.
      "local/no-untranslated-jsx": "error",
      // A refused action must not read as success — see the rule header.
      "local/require-assert-ok": "error",
      "@typescript-eslint/no-explicit-any": "off",
      "@typescript-eslint/no-unused-vars": [
        "error",
        {
          argsIgnorePattern: "^_",
          varsIgnorePattern: "^_",
          caughtErrorsIgnorePattern: "^_",
        },
      ],
    },
  },
  {
    files: ["src/**/*.test.{ts,tsx}"],
    languageOptions: {
      globals: {
        ...browserGlobals,
      },
    },
  },
);
