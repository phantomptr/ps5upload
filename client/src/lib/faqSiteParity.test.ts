import { describe, expect, it } from "vitest";

import faqText from "virtual:doc/faq";

// @ts-expect-error plain .mjs script, no type declarations
import { markdown, parseFaq as siteParse, renderFaq } from "../../../scripts/site-faq.mjs";
import { parseFaq } from "./faqDoc";

// The website's FAQ page (scripts/site-faq.mjs) and the app's FAQ screen read the same FAQ.md.
// Their ids must match, so `faq/#<id>` on the site and the web UI's `/faq?item=<id>` (faqItemUrl)
// name the same answer.
describe("the website's FAQ page", () => {
  it("gives every topic and question the app's id", () => {
    const app = parseFaq(faqText);
    const site = siteParse(faqText);
    expect(site.groups).toEqual(app.groups);
    expect(site.topics.map((t: { id: string }) => t.id)).toEqual(app.topics.map((t) => t.id));
    expect(site.topics.flatMap((t: { items: Array<{ id: string }> }) => t.items.map((i) => i.id))).toEqual(
      app.topics.flatMap((t) => t.items.map((i) => i.id)),
    );
  });

  it("lets no HTML from the markdown through", () => {
    const html = markdown(
      'Say <script>alert(1)</script> and <img src=x onerror=alert(1)>, a [bad](javascript:alert(1)) link, a <https://example.com> one and `<b>`.',
    );
    // Only as text: no tag, no javascript: link.
    expect(html).not.toMatch(/<script|<img|href="javascript:/);
    expect(html).toContain("&lt;script&gt;");
    expect(html).toContain('<a href="https://example.com">https://example.com</a>');
    expect(html).toContain("<code>&lt;b&gt;</code>");
  });

  it("keeps names with underscores whole, and lists, code and tables", () => {
    expect(markdown("Set PS5UPLOAD_ALLOW_IP and _this_.")).toBe("<p>Set PS5UPLOAD_ALLOW_IP and <em>this</em>.</p>");
    expect(markdown("- one\n  more\n- two\n  - inner")).toBe("<ul><li>one more</li><li>two<ul><li>inner</li></ul></li></ul>");
    expect(markdown("```sh\n# not a heading\n```")).toBe("<pre><code># not a heading</code></pre>");
    expect(markdown("| a | b |\n|---|---|\n| 1 | 2 |")).toContain("<td>1</td><td>2</td>");
  });

  it("renders the whole FAQ", () => {
    const { count, body } = renderFaq(faqText);
    expect(count).toBeGreaterThan(100);
    expect(body).not.toMatch(/<script|<[^>]*\son\w+=/i);
  });
});
