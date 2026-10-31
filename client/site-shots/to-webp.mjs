// Turn the screenshots (site-shots/out/*.png, and out/<language>/*.png) into the website's WebP
// assets (site/assets/shots/, and site/assets/shots/<language>/). Needs cwebp (brew install webp).
import { execFileSync } from "node:child_process";
import { mkdirSync, readdirSync, statSync } from "node:fs";

function convert(from, to) {
  mkdirSync(to, { recursive: true });
  for (const f of readdirSync(from)) {
    if (statSync(from + f).isDirectory()) convert(`${from}${f}/`, `${to}${f}/`);
    else if (f.endsWith(".png")) {
      const out = to + f.replace(/\.png$/, ".webp");
      execFileSync("cwebp", ["-quiet", "-q", "82", "-m", "6", from + f, "-o", out]);
      console.log(out);
    }
  }
}
convert(new URL("./out/", import.meta.url).pathname, new URL("../../site/assets/shots/", import.meta.url).pathname);
