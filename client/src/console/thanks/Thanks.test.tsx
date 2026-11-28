import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";
import supporters from "../../data/supporters.json";
import { t } from "../strings";
import { Thanks } from "./Thanks";

describe("Thanks & Support on the console", () => {
  it("lists every supporter from the one list, most first, and counts the anonymous", () => {
    const html = renderToStaticMarkup(<Thanks />);
    const list = supporters as { names: string[]; anonymous: number };
    expect(list.names.length).toBeGreaterThan(0);
    const wall = html.slice(html.indexOf("data-thanks-names"));
    let at = 0;
    for (const name of list.names.slice(0, 5)) {
      const esc = name.replace(/&/g, "&amp;").replace(/'/g, "&#x27;");
      const i = wall.indexOf(`>${esc}<`, at);
      expect(i).toBeGreaterThan(-1);
      at = i;
    }
    expect(html).toContain(t("cw_thanks_count_named", { n: list.names.length }));
    if (list.anonymous) expect(html).toContain(t("cw_thanks_anonymous", { n: list.anonymous }));
  });
  it("draws the two codes on the page (no network, nothing leaves Minion)", () => {
    const html = renderToStaticMarkup(<Thanks list={{ names: ["Ann"], anonymous: 0 }} />);
    expect((html.match(/class="cw-thanks-qr"/g) ?? []).length).toBe(2);
    expect(html).toContain(t("cw_thanks_kofi"));
    expect(html).toContain(t("cw_thanks_more"));
    expect(html).not.toContain("<a ");
    expect(html).not.toContain(t("cw_thanks_anonymous", { n: 0 }));
  });
});
