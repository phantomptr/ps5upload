import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";
import { MarkdownView } from "./MarkdownView";

// FAQ.md renders BOTH on GitHub and inside the app. A table added for the
// GitHub view used to show up here as raw pipe characters, so these pin the
// in-app rendering of the support matrix.
const TABLE = [
  "| OS / arch | `.zip` | `.rar` |",
  "|---|---|---|",
  "| Linux x86_64 | Decompress first | **Stream** |",
  "| Android arm64-v8a | Decompress first | not supported |",
].join("\n");

const html = (md: string) => renderToStaticMarkup(<MarkdownView source={md} />);

describe("MarkdownView tables", () => {
  it("renders a pipe table as a real table", () => {
    const out = html(TABLE);
    expect(out).toContain("<table");
    expect((out.match(/<th[ >]/g) ?? []).length).toBe(3);
    expect((out.match(/<tr[ >]/g) ?? []).length).toBe(3); // header + 2 body
    expect(out).toContain("Android arm64-v8a");
  });

  it("keeps inline formatting inside cells", () => {
    const out = html(TABLE);
    expect(out).toMatch(/<strong[^>]*>Stream<\/strong>/);
    expect(out).not.toContain("**Stream**");
    // `.zip` in the header is a code span, not literal backticks.
    expect(out).toContain("<code");
    expect(out).not.toContain("`.zip`");
  });

  it("does not leave raw pipes behind", () => {
    expect(html(TABLE)).not.toContain("|---|");
  });

  it("does not mistake a paragraph containing a pipe for a table", () => {
    const out = html("Run `a | b` to pipe output.");
    expect(out).not.toContain("<table");
  });

  it("leaves ordinary markdown alone", () => {
    const out = html("## Heading\n\n- one\n- two");
    expect(out).not.toContain("<table");
    expect((out.match(/<li[ >]/g) ?? []).length).toBe(2);
  });
});
