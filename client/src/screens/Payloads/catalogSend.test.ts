// @ts-expect-error -- the app tsconfig has no node types; Vitest runs this under Node.
import { readFileSync } from "node:fs";
import { describe, expect, it } from "vitest";

import { sentViaPayloadManager } from "../../api/ps5";

const probesRs: string = readFileSync(
  new URL("../../../src-tauri/src/commands/probes.rs", import.meta.url).pathname,
  "utf8",
);
const catalog: string = readFileSync(new URL("./CatalogPanel.tsx", import.meta.url).pathname, "utf8");

// The catalogue's "Sent" notification said ":9021" even when :9021 was dead
// and payload_send had launched the file through Payload Manager.
describe("which route a catalogue send took", () => {
  it("recognises payload_send's Payload Manager status", () => {
    expect(probesRs).toContain("through Payload Manager on {ip}:8084 instead");
    expect(
      sentViaPayloadManager(
        "connect 1.2.3.4:9021: refused; launched 1000 bytes through Payload Manager on 1.2.3.4:8084 instead",
      ),
    ).toBe(true);
    expect(sentViaPayloadManager("sent 1000 bytes to 1.2.3.4:9021")).toBe(false);
    expect(sentViaPayloadManager(undefined)).toBe(false);
  });

  it("says so in the notification, and doesn't count custom repos as curated", () => {
    expect(catalog).toContain('"payloads_sent_body_pm"');
    expect(catalog).toContain("catalog.filter((p) => !p.is_custom).length");
  });
});
