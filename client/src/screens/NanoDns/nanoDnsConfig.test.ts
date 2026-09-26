import { describe, expect, it } from "vitest";

import {
  CURRENT_YANDEX_DNS,
  DEFAULT_NANODNS_LOG_PATH,
  OLD_YANDEX_DNS,
  detectNanoDnsVersion,
  fixNanoDnsYandexDns,
  hasOldYandexDns,
  migrateNanoDns04Config,
  nanoDnsGeneralValue,
  nanoDnsLogPath,
  setNanoDnsGeneralValue,
} from "./nanoDnsConfig";

const CONFIG_03 = `# nanoDNS 0.3 config
[general]
log=/data/nanodns/custom.log
debug=0
bind=127.0.0.1

[upstream]
server=1.1.1.1
server=${OLD_YANDEX_DNS}

[overrides]
custom.example=192.168.1.20
`;

const CONFIG_04 = `# nanoDNS 0.4 config
[general]
log=/data/nanodns/nanodns.log
debug=0
quiet=1
bind=0.0.0.0
bind6=off

[upstream]
server=${CURRENT_YANDEX_DNS}
`;

describe("detectNanoDnsVersion", () => {
  it("detects 0.3 and 0.4 from their real startup banner shape", () => {
    expect(
      detectNanoDnsVersion(
        "====\nNanoDNS v0.3\nBuild: Aug 8 2026\n====\n",
        CONFIG_04,
      ),
    ).toEqual({
      version: "0.3",
      generation: "legacy",
      source: "runtime-log",
    });
    expect(
      detectNanoDnsVersion(
        "====\nnanoDNS v0.4\nBuild: Aug 8 2026\n====\n",
        CONFIG_03,
      ),
    ).toEqual({
      version: "0.4",
      generation: "modern",
      source: "runtime-log",
    });
  });

  it("trusts the runtime over the config because 0.4 keys are ignored by 0.3", () => {
    expect(detectNanoDnsVersion("NanoDNS v0.3\n", CONFIG_04).generation).toBe(
      "legacy",
    );
  });

  it("infers a 0.4-style config only when no runtime banner is available", () => {
    expect(detectNanoDnsVersion(null, CONFIG_04)).toEqual({
      version: null,
      generation: "modern",
      source: "config",
    });
    expect(detectNanoDnsVersion("unrelated log", CONFIG_03)).toEqual({
      version: null,
      generation: "unknown",
      source: "unknown",
    });
  });
});

describe("nanoDNS config parsing", () => {
  it("reads general aliases, top-level keys, inline comments, and last-value wins", () => {
    const config = `log=/first.log
[settings]
LOG = /second.log # active path
quiet=0
quiet = 1 ; last wins
`;
    expect(nanoDnsLogPath(config)).toBe("/second.log");
    expect(nanoDnsGeneralValue(config, "quiet")).toBe("1");
  });

  it("uses the compiled log path when config does not set one", () => {
    expect(nanoDnsLogPath("[general]\nbind=127.0.0.1\n")).toBe(
      DEFAULT_NANODNS_LOG_PATH,
    );
  });

  it("finds the old Yandex address only in active upstream entries", () => {
    expect(hasOldYandexDns(CONFIG_03)).toBe(true);
    expect(
      hasOldYandexDns(`[upstream]\n# server=${OLD_YANDEX_DNS}\n[overrides]\nx=${OLD_YANDEX_DNS}\n`),
    ).toBe(false);
  });
});

describe("migrateNanoDns04Config", () => {
  it("preserves a 0.3 config while adding 0.4 settings and fixing Yandex", () => {
    const result = migrateNanoDns04Config(CONFIG_03);
    expect(result.changes).toEqual(["yandex-dns", "quiet", "bind6"]);
    expect(result.text).toContain("quiet=0\nbind6=::1\n\n[upstream]");
    expect(result.text).toContain(`server=${CURRENT_YANDEX_DNS}`);
    expect(result.text).toContain("custom.example=192.168.1.20");
    expect(result.text).toContain("log=/data/nanodns/custom.log");
  });

  it("is idempotent for an existing 0.4 config", () => {
    const first = migrateNanoDns04Config(CONFIG_04);
    const second = migrateNanoDns04Config(first.text);
    expect(first).toEqual({ text: CONFIG_04, changes: [] });
    expect(second).toEqual(first);
  });

  it("preserves CRLF, comments, aliases, and custom values", () => {
    const config = [
      "# keep me",
      "[settings]",
      "quiet = 1 ; no popup",
      "bind=0.0.0.0",
      "",
      "[upstreams]",
      `dns = ${OLD_YANDEX_DNS} # old default`,
      "server=9.9.9.9",
      "",
    ].join("\r\n");
    const result = migrateNanoDns04Config(config);
    expect(result.text).toContain("quiet = 1 ; no popup\r\n");
    expect(result.text).toContain("bind6=::1\r\n\r\n[upstreams]");
    expect(result.text).toContain(
      `dns = ${CURRENT_YANDEX_DNS} # old default`,
    );
    expect(result.text).not.toMatch(/(^|[^\r])\n/);
  });

  it("creates a general section when an unusual config has none", () => {
    const result = migrateNanoDns04Config(
      `[upstream]\nserver=${OLD_YANDEX_DNS}\n`,
    );
    expect(result.text).toBe(
      `[general]\nquiet=0\nbind6=::1\n\n[upstream]\nserver=${CURRENT_YANDEX_DNS}\n`,
    );
  });

  it("creates a clean config skeleton from an empty file", () => {
    expect(migrateNanoDns04Config("").text).toBe(
      "[general]\nquiet=0\nbind6=::1\n",
    );
  });
});

describe("targeted edits", () => {
  it("updates the last active setting and preserves its inline comment", () => {
    const config = `[general]\nquiet=0\nquiet = 0 ; effective\n`;
    expect(setNanoDnsGeneralValue(config, "quiet", "1")).toBe(
      `[general]\nquiet=0\nquiet = 1 ; effective\n`,
    );
  });

  it("adds a missing setting without changing line endings", () => {
    const config = "[general]\r\nbind=127.0.0.1\r\n";
    expect(setNanoDnsGeneralValue(config, "bind6", "::")).toBe(
      "[general]\r\nbind=127.0.0.1\r\nbind6=::\r\n",
    );
  });

  it("can fix Yandex for 0.3 without adding 0.4-only keys", () => {
    const result = fixNanoDnsYandexDns(CONFIG_03);
    expect(result.changes).toEqual(["yandex-dns"]);
    expect(result.text).toContain(`server=${CURRENT_YANDEX_DNS}`);
    expect(result.text).not.toContain("quiet=");
    expect(result.text).not.toContain("bind6=");
  });
});
