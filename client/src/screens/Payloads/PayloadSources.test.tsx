import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";

import type { PayloadSource, SourcePreview } from "../../api/payloadSources";
import {
  AddSourceView,
  patternText,
  SourceCardView,
  sourceProblemText,
  type AddSourceProps,
  type SourceCardProps,
} from "./PayloadSources";

// No DOM here (vitest runs in node): static markup, asserting on what is shown.
const tr = ((_k: string, vars: Record<string, unknown> | undefined, fallback: string) =>
  fallback.replace(/\{(\w+)\}/g, (_, n) => String(vars?.[n] ?? ""))) as never;
const noop = () => undefined;
const SHA = "ab".repeat(32);

const source = (over: Partial<PayloadSource> = {}): PayloadSource => ({
  id: "custom-0a1b2c3d4e5f",
  name: "kstuff",
  kind: "github",
  project_url: "https://github.com/me/kstuff",
  owner: "me",
  repo: "kstuff",
  file_name: "kstuff-v1.6.0.elf",
  file_pattern: "kstuff-v*.elf",
  update_available: false,
  ...over,
});

const have = {
  tag: "v1.6.0",
  file_name: "kstuff-v1.6.0.elf",
  size: 2048,
  sha256: SHA,
  published_at: "",
  downloaded_at: 0,
  checksum: "matched",
  path: "/data/payloads/custom-0a1b2c3d4e5f/kstuff-v1.6.0.elf",
};

const latest = (tag: string) => ({
  tag,
  release_name: tag,
  file_name: `kstuff-${tag}.elf`,
  size: 4096,
  published_at: "",
  prerelease: false,
  html_url: "",
});

const card = (over: Partial<SourceCardProps> = {}) =>
  renderToStaticMarkup(
    <SourceCardView
      tr={tr}
      source={source()}
      busy={null}
      actionError={null}
      onDownload={noop}
      onSend={noop}
      onChangeFile={noop}
      onRemove={noop}
      onOpenProject={noop}
      {...over}
    />,
  );

const buttons = (html: string) => [...html.matchAll(/<button[^>]*>(?:<[^>]+>)*([^<]*)/g)].map((m) => m[1].trim()).filter(Boolean);

describe("a payload source's card", () => {
  it("before a download: the file, its rule, the newest release and Download", () => {
    const out = card({ source: source({ latest: latest("v1.6.0") }) });
    expect(out).toContain("kstuff-v1.6.0.elf");
    expect(out).toContain("Newer releases: a file named like kstuff-v*.elf (* is the version).");
    expect(out).toContain("Not downloaded yet. Newest: v1.6.0");
    expect(buttons(out)).toEqual(["me/kstuff", "Download", "Change file", "Remove source"]);
    expect(out).not.toContain("SHA-256");
  });

  it("downloaded: shows exactly what goes to the console, and Send", () => {
    const out = card({ source: source({ have, latest: latest("v1.6.0") }) });
    expect(out).toContain('data-testid="psrc-sha256"');
    expect(out).toContain(SHA);
    expect(out).toContain("2.00 KiB");
    expect(out).toContain("v1.6.0");
    expect(out).toContain("Matches the checksum its release lists.");
    expect(buttons(out)).toEqual(["me/kstuff", "Send to PS5", "Change file", "Remove source"]);
  });

  it("a newer release: New version available and Update", () => {
    const out = card({ source: source({ have, latest: latest("v1.7.0"), update_available: true }) });
    expect(out).toContain("New version available: v1.7.0");
    expect(buttons(out)).toEqual(["me/kstuff", "Update", "Send to PS5", "Change file", "Remove source"]);
  });

  it("an entry from before 12.6.0 asks for its file and keeps its copy", () => {
    const out = card({ source: source({ file_name: "", file_pattern: "", have: { ...have, tag: "", checksum: "" } }) });
    expect(out).toContain("Added before this version: pick the file this source downloads.");
    expect(out).toContain("unknown");
    expect(buttons(out)).toEqual(["me/kstuff", "Send to PS5", "Pick the file", "Remove source"]);
  });

  it("says why a check failed, in words", () => {
    const out = card({
      source: source({ error: { code: "no_matching_file", message: "x" } }),
    });
    expect(out).toContain("None of the newest releases has a file named like kstuff-v*.elf.");
    expect(buttons(out)).toEqual(["me/kstuff", "Change file", "Remove source"]);
  });

  it("busy: every button waits", () => {
    const out = card({ source: source({ have, latest: latest("v1.7.0"), update_available: true }), busy: "download" });
    const disabled = [...out.matchAll(/<button[^>]*disabled=""/g)].length;
    expect(disabled).toBe(4);
  });
});

describe("source errors", () => {
  const at = (s: number) => `T${s}`;
  it("a rate limit says when to try again", () => {
    expect(sourceProblemText(tr, { code: "rate_limited", message: "", retry_at: 99 }, "", at)).toBe(
      "Too many checks for now. Try again after T99.",
    );
    expect(sourceProblemText(tr, { code: "rate_limited", message: "" }, "", at)).toBe(
      "Too many checks for now. Try again in a few minutes.",
    );
  });

  it("address refusals are plain", () => {
    expect(sourceProblemText(tr, { code: "url_not_https", message: "" })).toBe("Only https:// addresses can be added.");
    expect(sourceProblemText(tr, { code: "url_host_not_allowed", message: "" })).toContain("this computer");
    expect(sourceProblemText(tr, { code: "something_new", message: "The engine's words." })).toBe("The engine's words.");
  });

  it("the file rule in words", () => {
    expect(patternText(tr, "kstuff.elf", "kstuff.elf")).toBe("Newer releases: a file with this exact name.");
    expect(patternText(tr, "k-1.0.elf", "k-*.elf")).toContain("k-*.elf");
  });
});

const preview: SourcePreview = {
  id: "custom-0a1b2c3d4e5f",
  name: "kstuff",
  kind: "github",
  project_url: "https://github.com/me/kstuff",
  already_added: false,
  current_file: "",
  releases: [
    {
      tag: "v1.6.0",
      name: "v1.6.0",
      published_at: "",
      prerelease: false,
      html_url: "",
      files: [
        { name: "kstuff-v1.6.0.elf", size: 2048, pattern: "kstuff-v*.elf" },
        { name: "kstuff-v1.6.0-debug.elf", size: 4096, pattern: "kstuff-v*-debug.elf" },
      ],
    },
  ],
};

const addForm = (over: Partial<AddSourceProps> = {}) =>
  renderToStaticMarkup(
    <AddSourceView
      tr={tr}
      changing={null}
      url="https://github.com/me/kstuff"
      preview={null}
      picked=""
      busy={false}
      error={null}
      onUrl={noop}
      onLookUp={noop}
      onPick={noop}
      onAdd={noop}
      onCancel={noop}
      {...over}
    />,
  );

describe("adding a source", () => {
  it("starts with the address", () => {
    const out = addForm();
    expect(out).toContain("Add a payload source");
    expect(out).toContain('type="url"');
    expect(out).toContain("Show its files");
    expect(out).not.toContain("Only add projects you trust");
  });

  it("then lists the newest release's files, with the trust note, and waits for a pick", () => {
    const out = addForm({ preview });
    expect(out).toContain("Pick the payload file (release v1.6.0):");
    expect([...out.matchAll(/type="radio"/g)]).toHaveLength(2);
    expect(out).toContain(
      "PS5Upload will download and send whatever this project publishes under that name. Only add projects you trust.",
    );
    expect(out).toMatch(/<button[^>]*disabled=""[^>]*>(?:<[^>]+>)*Add source/);
  });

  it("a picked file shows its rule and can be added", () => {
    const out = addForm({ preview, picked: "kstuff-v1.6.0.elf" });
    expect(out).toContain("Newer releases: a file named like kstuff-v*.elf");
    expect(out).not.toMatch(/<button[^>]*disabled=""[^>]*>(?:<[^>]+>)*Add source/);
  });

  it("a project without payload files says so", () => {
    const out = addForm({ preview: { ...preview, releases: [] } });
    expect(out).toContain("The newest releases of this project have no .elf or .bin file.");
    expect(out).not.toContain("Add source");
  });

  it("changing the file: no address, no note, Use this file", () => {
    const out = addForm({ changing: { name: "kstuff" }, preview, picked: "kstuff-v1.6.0.elf" });
    expect(out).toContain("Pick the file for kstuff");
    expect(out).not.toContain('type="url"');
    expect(out).not.toContain("Only add projects you trust");
    expect(out).toContain("Use this file");
  });

  it("shows a refusal", () => {
    expect(addForm({ error: "Only https:// addresses can be added." })).toContain('role="alert"');
  });
});
