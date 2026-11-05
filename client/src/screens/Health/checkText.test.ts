import { describe, expect, it } from "vitest";

import type { HealthCheck } from "../../api/ps5";
import { checkDetail, checkRemedy, checkTitle } from "./checkText";

/** Records the key asked for, and answers the English with its values filled in. */
const keys: string[] = [];
const tr = ((key: string, vars: Record<string, unknown> | undefined, fallback: string) => {
  keys.push(key);
  return fallback.replace(/\{(\w+)\}/g, (_, k: string) => String(vars?.[k] ?? `{${k}}`));
}) as never;

const check = (over: Partial<HealthCheck>): HealthCheck =>
  ({ id: "x", title: "T", category: "storage", status: "pass", detail: "D", remedy: "R", ...over }) as HealthCheck;

describe("the health report's sentences", () => {
  it("say a known check through its keys, with its values", () => {
    const c = check({
      id: "internal_free_space",
      detail_id: "storage",
      values: { free: "1 GiB", total: "2 GiB", pct: "50.0" },
      remedy_id: "space_tight",
    });
    expect(checkDetail(tr, c)).toBe("1 GiB free of 2 GiB (50.0%).");
    expect(checkTitle(tr, c)).toBe("Console storage");
    expect(checkRemedy(tr, c)).toContain("tight");
    expect(keys).toContain("health_d_storage");
  });

  it("fall back to the engine's English for what they do not know", () => {
    const c = check({ id: "new_check", detail_id: "new", remedy_id: "new" });
    expect(checkTitle(tr, c)).toBe("T");
    expect(checkDetail(tr, c)).toBe("D");
    expect(checkRemedy(tr, c)).toBe("R");
    const old = check({});
    expect(checkDetail(tr, old)).toBe("D");
  });
});
