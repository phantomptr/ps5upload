import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";
import faqText from "virtual:doc/faq";
import { parseFaq } from "../lib/faqDoc";
import { MarkdownView, resolveDocLink } from "./MarkdownView";

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

describe("MarkdownView lists", () => {
  it("keeps a wrapped bullet's indented lines in the same item", () => {
    const out = html(
      [
        "- **The limit was too high.** It held each connection to",
        "  the limit instead of the whole transfer.",
        "- Second item",
        "",
        "After the list.",
      ].join("\n"),
    );
    expect((out.match(/<li[ >]/g) ?? []).length).toBe(2);
    expect(out).toContain("connection to the limit instead");
    // The continuation is not a paragraph of its own.
    expect((out.match(/<p[ >]/g) ?? []).length).toBe(1);
  });

  it("joins wrapped lines in numbered lists too", () => {
    const out = html(["1. First step that", "   wraps", "2. Second"].join("\n"));
    expect((out.match(/<li[ >]/g) ?? []).length).toBe(2);
    expect(out).toContain("First step that wraps");
  });
});

describe("MarkdownView and FAQ.md's grammar", () => {
  it("renders a fence indented under a list item as code, one command a line", () => {
    const out = html("2. Step:\n\n   ```sh\n   one --flag\n     two\n   ```\n\n3. Next\n");
    expect(out).toContain("<pre");
    expect(out).toContain("one --flag\n  two");
    expect(out).not.toContain("```");
  });

  it("does not swallow a fence right under an item as its continuation", () => {
    const out = html("1. Step:\n   ```\n   cmd\n   ```\n");
    expect(out).toMatch(/<code>cmd<\/code>/);
    expect(out).not.toContain("```");
  });

  it("keeps counting when a code block splits a list", () => {
    const out = html("1. a\n\n```\nx\n```\n\n2. b\n\n- sub\n\n3. c\n");
    expect(out).toMatch(/<ol[^>]* start="1"/);
    expect(out).toMatch(/<ol[^>]* start="2"/);
    expect(out).toMatch(/<ol[^>]* start="3"/);
  });

  it("renders a #### heading as a heading", () => {
    const out = html("#### Windows 11");
    expect(out).toContain("<h4");
    expect(out).not.toContain("####");
  });

  it("parses code and links inside bold and link labels", () => {
    const out = html("**`.rar` works** and [`engine/compose.yaml`](engine/compose.yaml)");
    expect(out).toMatch(/<strong[^>]*><code[^>]*>\.rar<\/code> works<\/strong>/);
    expect(out).not.toContain("`");
  });

  it("sends a repository path to GitHub and leaves full addresses alone", () => {
    expect(resolveDocLink("engine/compose.yaml")).toBe(
      "https://github.com/phantomptr/ps5upload/blob/main/engine/compose.yaml",
    );
    expect(resolveDocLink("./FAQ.md")).toBe("https://github.com/phantomptr/ps5upload/blob/main/FAQ.md");
    expect(resolveDocLink("https://a.example/b")).toBe("https://a.example/b");
    expect(resolveDocLink("mailto:a@b.example")).toBe("mailto:a@b.example");
    expect(html("[f](engine/compose.yaml)")).toContain(
      'href="https://github.com/phantomptr/ps5upload/blob/main/engine/compose.yaml"',
    );
  });

  it("does not read underscores inside a name as emphasis", () => {
    const out = html("Set PS5UPLOAD_ALLOW_IP_LIST and _this_ too");
    expect(out).toContain("PS5UPLOAD_ALLOW_IP_LIST");
    expect(out).toMatch(/<em[^>]*>this<\/em>/);
  });

  it("the real FAQ: no raw fences or heading marks in any answer", () => {
    const doc = parseFaq(faqText);
    const answers = doc.topics.flatMap((t) => [t.intro, ...t.items.map((q) => q.answer)]);
    expect(answers.length).toBeGreaterThan(100);
    for (const a of answers) {
      const out = html(a);
      expect(out, a.slice(0, 80)).not.toContain("```");
      // A `# comment` inside a code block is fine; a heading mark in the text is not.
      expect(out.replace(/<pre[\s\S]*?<\/pre>/g, ""), a.slice(0, 80)).not.toMatch(/>#{1,6} /);
      expect(a, a.slice(0, 80)).not.toMatch(/\n\s*-{3,}\s*$/);
    }
    const white = doc.topics
      .flatMap((t) => t.items)
      .find((q) => q.answer.includes("LIBGL_ALWAYS_SOFTWARE=1 ./PS5Upload.sh"));
    expect(html(white!.answer)).toMatch(/<code>LIBGL_ALWAYS_SOFTWARE=1 \.\/PS5Upload\.sh<\/code>/);
  });
});
