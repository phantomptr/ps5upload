import { describe, expect, it } from "vitest";

import { humanizeJobErrorReason } from "./ps5";

// Read through Vite: the client tsconfig ships no node types (a fs import fails `tsc`).
const RUST = {
  ...import.meta.glob("../../../engine/crates/ps5upload-ava1/src/**/*.rs", { query: "?raw", import: "default", eager: true }),
  ...import.meta.glob("../../../engine/crates/ps5upload-engine/src/**/*.rs", { query: "?raw", import: "default", eager: true }),
  ...import.meta.glob("../../../engine/crates/ps5upload-core/src/**/*.rs", { query: "?raw", import: "default", eager: true }),
} as Record<string, string>;
const PS5_TS = (import.meta.glob("./ps5.ts", { query: "?raw", import: "default", eager: true }) as Record<string, string>)["./ps5.ts"];
const EN_TS = (
  import.meta.glob("../i18n/locales/en.ts", { query: "?raw", import: "default", eager: true }) as Record<string, string>
)["../i18n/locales/en.ts"];

// String literals that LOOK like reasons but are protocol keys / internals, not reasons.
const NOT_REASONS = new Set(["ava1_port"]);

const REASON_LITERAL =
  /"(ava1_[a-z0-9_]+|zip_read_error|[0-9a-z]+_unsupported(?:_layout)?|rar_password_(?:required|wrong)|helper_(?:not_ava1|old|starting|not_running)|not_paired)"/g;

function engineReasons(): Map<string, string> {
  const found = new Map<string, string>();
  for (const [file, src] of Object.entries(RUST)) {
    for (const m of src.matchAll(REASON_LITERAL)) {
      if (!NOT_REASONS.has(m[1])) found.set(m[1], file.replace(/^.*crates\//, ""));
    }
  }
  return found;
}

const REQUIRED = [
  "ava1_busy", "ava1_open_timeout", "ava1_unreachable", "ava1_no_space", "ava1_not_allowed",
  "ava1_exists", "ava1_commit_exists", "ava1_cross_device", "ava1_commit_cross_device",
  "ava1_wrong_console", "ava1_no_identity", "ava1_zip_corrupt", "ava1_7z_corrupt",
  "ava1_7z_encrypted", "ava1_7z_unsafe_path", "ava1_copy_lost", "ava1_copy_failed",
  "ava1_local_io", "ava1_bad_manifest", "zip_read_error", "helper_not_ava1", "not_paired",
  "helper_starting", "ava1_failed", "helper_not_running",
];

describe("every engine reason is humanized", () => {
  it("finds the reasons (guards the scan itself)", () => {
    expect(engineReasons().size).toBeGreaterThan(25);
  });

  it("maps every reason literal found in the engine sources", () => {
    const missing = [...engineReasons()].filter(([r]) => humanizeJobErrorReason(r) === null);
    expect(missing.map(([r, f]) => `${r} (${f})`)).toEqual([]);
  });

  it("maps the reasons the review listed", () => {
    expect(REQUIRED.filter((r) => humanizeJobErrorReason(r) === null)).toEqual([]);
  });

  it("maps ava1_refused_N and shows the code", () => {
    expect(humanizeJobErrorReason("ava1_refused_99")).toContain("99");
  });

  it("suggests Override for an existing destination", () => {
    expect(humanizeJobErrorReason("ava1_exists")).toMatch(/Override/);
    expect(humanizeJobErrorReason("ava1_commit_exists")).toMatch(/Override/);
  });

  it("every joberr key the humanizer uses exists in the English locale", () => {
    const en = EN_TS;
    const code = PS5_TS;
    const keys = [...code.matchAll(/"(joberr\.[a-z0-9_]+)"/g)].map((m) => m[1]);
    expect(keys.length).toBeGreaterThan(30);
    expect(keys.filter((k) => !en.includes(`"${k}"`) && !new RegExp(`^\\s*${k.replace(".", "\\.")}:`, "m").test(en))).toEqual([]);
    for (const r of [...REQUIRED, ...engineReasons().keys()]) {
      expect(humanizeJobErrorReason(r), r).not.toMatch(/^joberr\./);
    }
  });
});
