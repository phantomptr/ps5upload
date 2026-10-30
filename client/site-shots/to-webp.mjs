// Turn the screenshots (site-shots/out/*.png) into the website's WebP assets
// (site/assets/shots/*.webp). Needs cwebp (brew install webp).
import { execFileSync } from "node:child_process";
import { mkdirSync, readdirSync } from "node:fs";

const from = new URL("./out/", import.meta.url).pathname;
const to = new URL("../../site/assets/shots/", import.meta.url).pathname;
mkdirSync(to, { recursive: true });
for (const f of readdirSync(from).filter((n) => n.endsWith(".png"))) {
  const out = to + f.replace(/\.png$/, ".webp");
  execFileSync("cwebp", ["-quiet", "-q", "82", "-m", "6", from + f, "-o", out]);
  console.log(out);
}
