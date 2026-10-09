import { describe, expect, it } from "vitest";
import vectors from "./redaction.vectors.json";
import { createRedactor } from "./redaction";

// The same vectors run against the desktop's Rust redactor (bug_report.rs), so the two cannot drift.
describe("report redaction", () => {
  for (const v of vectors) {
    it(v.name, () => {
      expect(createRedactor({ redact: v.redact }).text(v.in)).toBe(v.out);
    });
  }

  it("numbers addresses across every file of one report", () => {
    const r = createRedactor({ redact: true });
    expect(r.text("a 10.0.0.1")).toBe("a <ip-1>");
    expect(r.text("b 10.0.0.2 c 10.0.0.1")).toBe("b <ip-2> c <ip-1>");
  });
});
