#!/usr/bin/env node
// Writes site/altstore.json: the iPhone source (AltStore, SideStore, Feather) at the website's own
// address, https://ps5upload.phantomptr.com/altstore.json. Run by .github/workflows/pages.yml.
//
// It is a copy of the newest promoted release's altstore.json (scripts/gen-altstore-source.py).
// When that can't be had, the copy the site serves now is kept: a source that disappears makes
// Feather drop it with no message. Failing to find either never stops the website's deploy.
import { writeFileSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

const out = path.join(path.dirname(fileURLToPath(import.meta.url)), "..", "site", "altstore.json");
const SOURCES = [
  "https://github.com/phantomptr/ps5upload/releases/latest/download/altstore.json",
  "https://ps5upload.phantomptr.com/altstore.json",
];

for (const url of SOURCES) {
  try {
    const res = await fetch(url);
    if (!res.ok) throw new Error(`answered ${res.status}`);
    const source = await res.json();
    const version = source.apps?.[0]?.versions?.[0];
    if (!version?.downloadURL) throw new Error("not a source with a download");
    writeFileSync(out, `${JSON.stringify(source, null, 2)}\n`);
    console.log(`site/altstore.json: ${version.version} from ${url}`);
    process.exit(0);
  } catch (e) {
    console.warn(`::warning::${url}: ${e.message}`);
  }
}
console.warn("::warning::no iPhone source to publish; site/altstore.json not written");
