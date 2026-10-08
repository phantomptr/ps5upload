import { describe, expect, it } from "vitest";

import { parseFaq, searchFaq, slugOf } from "./faqDoc";

const MD = `# FAQ

Intro line.

## Getting started

Topic intro.

**Q: How do I connect?**
Enter the PS5's address and press Check.

**Q: A question that is long enough to wrap
onto a second line?**
The answer, with a code \`0x80B2116F\`.

### A sub-heading inside the answer

More of the same answer.

## Install Package

**Q: What is Stream & install?**
The PS5 installs straight from this computer.

- a list item
`;

describe("parseFaq", () => {
  const doc = parseFaq(MD);

  it("splits the document into topics, each with its questions", () => {
    expect(doc.intro).toContain("Intro line.");
    expect(doc.topics.map((t) => t.title)).toEqual([
      "Getting started",
      "Install Package",
    ]);
    expect(doc.topics[0].intro).toContain("Topic intro.");
    expect(doc.topics[0].items.map((i) => i.question)).toEqual([
      "How do I connect?",
      "A question that is long enough to wrap onto a second line?",
    ]);
  });

  it("keeps everything up to the next question as the answer, sub-headings included", () => {
    const a = doc.topics[0].items[1].answer;
    expect(a).toContain("0x80B2116F");
    expect(a).toContain("### A sub-heading inside the answer");
    expect(a).toContain("More of the same answer.");
    expect(doc.topics[1].items[0].answer).toContain("- a list item");
  });

  it("gives topics and questions stable ids for links", () => {
    expect(doc.topics[0].id).toBe("getting-started");
    expect(doc.topics[0].items[0].id).toBe("getting-started--how-do-i-connect");
    expect(slugOf("Stream & install: what's that?")).toBe(
      "stream-install-what-s-that",
    );
    // Two questions with the same words still get different ids.
    const twice = parseFaq("## T\n\n**Q: Same?**\na\n\n**Q: Same?**\nb\n");
    expect(new Set(twice.topics[0].items.map((i) => i.id)).size).toBe(2);
  });

  it("groups topics under the document's top-level headings, in order", () => {
    const d = parseFaq(
      "# FAQ\n\nHello.\n\n# Start here\n\n## One\n\n**Q: A?**\na\n\n# Help\n\n## Two\n\ntext\n\n```sh\n# not a heading\n```\n\n## Three\n\nmore\n",
    );
    expect(d.intro).toBe("Hello.");
    expect(d.groups).toEqual(["Start here", "Help"]);
    expect(d.topics.map((t) => [t.group, t.title])).toEqual([
      ["Start here", "One"],
      ["Help", "Two"],
      ["Help", "Three"],
    ]);
    expect(d.topics[1].intro).toContain("# not a heading");
    // A document with no groups still parses: every topic is in the unnamed one.
    expect(parseFaq("# FAQ\n\n## One\n\nx\n").groups).toEqual([]);
  });

  it("shows a question as plain text", () => {
    const d = parseFaq("## T\n\n**Q: What is a `.pkg` file?**\nA package.\n");
    expect(d.topics[0].items[0].question).toBe("What is a .pkg file?");
  });

  it("a topic with no questions keeps its text as the intro", () => {
    const d = parseFaq("## Disclaimer\n\nUse at your own risk.\n");
    expect(d.topics[0].items).toEqual([]);
    expect(d.topics[0].intro).toContain("Use at your own risk.");
  });
});

describe("searchFaq", () => {
  const doc = parseFaq(MD);

  it("finds questions by every word, in the question or its answer", () => {
    expect(
      searchFaq(doc, "stream install").map((h) => h.item?.question),
    ).toEqual(["What is Stream & install?"]);
    expect(searchFaq(doc, "0x80b2116f")[0].item?.question).toContain(
      "long enough to wrap",
    );
    expect(searchFaq(doc, "nothing-matches-this")).toEqual([]);
    expect(searchFaq(doc, "   ")).toEqual([]);
  });

  it("puts a match in the question above a match only in an answer", () => {
    const d = parseFaq(
      "## T\n\n**Q: About fans?**\nNothing here.\n\n**Q: Something else?**\nThe fan is mentioned in the answer only. fan fan fan.\n",
    );
    expect(searchFaq(d, "fan").map((h) => h.item?.question)).toEqual([
      "About fans?",
      "Something else?",
    ]);
  });

  it("returns a short excerpt around the match, and the topic it is in", () => {
    const [hit] = searchFaq(doc, "0x80B2116F");
    expect(hit.topic.title).toBe("Getting started");
    expect(hit.excerpt).toContain("0x80B2116F");
    expect(hit.excerpt.length).toBeLessThan(260);
  });

  it("finds a topic's own text when it has no questions", () => {
    const d = parseFaq("## Disclaimer\n\nUse at your own risk.\n");
    const [hit] = searchFaq(d, "risk");
    expect(hit.topic.title).toBe("Disclaimer");
    expect(hit.item).toBeNull();
  });
});
