import js from "@eslint/js";
import reactHooks from "eslint-plugin-react-hooks";
import tseslint from "typescript-eslint";
import localRules from "./eslint-rules/no-untranslated-jsx.mjs";

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
      local: localRules,
    },
    rules: {
      ...reactHooks.configs.recommended.rules,
      "react-hooks/immutability": "off",
      "react-hooks/set-state-in-effect": "off",
      // Enforcement gate: every user-visible string must go through
      // useTr()/tr(). The locale-parity gate (scripts/i18n-coverage.mjs)
      // can only see keys that already exist — this catches strings
      // before they ever become (missing) keys. See
      // eslint-rules/no-untranslated-jsx.mjs. "error" because the
      // existing backlog (178 strings) has been fully extracted — a
      // new hardcoded string is now a build failure, not a warning.
      "local/no-untranslated-jsx": "error",
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
