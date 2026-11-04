import { describe, expect, it } from "vitest";

import en from "../i18n/locales/en";
import { CATALOG_KEYS, catalogText } from "./payloadCatalogText";

const words = en as unknown as Record<string, string>;

/** A translator that knows the English table, as `useTr()` does. */
const tr = (key: string, vars: Record<string, string | number> | undefined, fallback: string) => {
  const text = words[key] ?? fallback;
  return text.replace(/\{(\w+)\}/g, (m, k: string) => String(vars?.[k] ?? m));
};

const entry = (over: Partial<Parameters<typeof catalogText>[1]>) => ({
  id: "etahen",
  role: "rust role",
  description: "rust description",
  is_custom: false,
  repo_owner: "someone",
  repo_name: "tool",
  ...over,
});

describe("catalogText", () => {
  it("has words for every catalogue id it lists", () => {
    for (const [id, [role, description]] of Object.entries(CATALOG_KEYS)) {
      expect(words[role], `${id} role`).toBeTruthy();
      expect(words[description], `${id} description`).toBeTruthy();
    }
  });

  it("uses the table over the shell's English", () => {
    expect(catalogText(tr, entry({})).role).toBe(words.payload_role_etahen);
    expect(catalogText(tr, entry({})).description).toBe(words.payload_desc_etahen);
  });

  it("keeps the shell's text for an entry this build does not know", () => {
    expect(catalogText(tr, entry({ id: "from-a-newer-catalogue" }))).toEqual({
      role: "rust role",
      description: "rust description",
    });
  });

  it("names a custom repo in its description", () => {
    const text = catalogText(tr, entry({ id: "custom-1a2b", is_custom: true }));
    expect(text.role).toBe(words.payload_role_custom);
    expect(text.description).toContain("someone/tool");
  });
});
