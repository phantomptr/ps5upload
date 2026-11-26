// Turn the screenshots (site-shots/out/*.png, and out/<language>/*.png) into the website's WebP
// assets (site/assets/shots/, and site/assets/shots/<language>/). Needs cwebp (brew install webp).
// Several run at once: a full set is some 1,400 pictures.
import { execFile } from "node:child_process";
import { mkdirSync, readdirSync, statSync } from "node:fs";
import { availableParallelism } from "node:os";
import { promisify } from "node:util";

const run = promisify(execFile);

/** Every picture under `from`, as [png, webp]. */
function pictures(from, to) {
  mkdirSync(to, { recursive: true });
  const found = [];
  for (const f of readdirSync(from).sort()) {
    if (statSync(from + f).isDirectory()) found.push(...pictures(`${from}${f}/`, `${to}${f}/`));
    else if (f.endsWith(".png")) found.push([from + f, to + f.replace(/\.png$/, ".webp")]);
  }
  return found;
}

const todo = pictures(new URL("./out/", import.meta.url).pathname, new URL("../../site/assets/shots/", import.meta.url).pathname);
const total = todo.length;
async function worker() {
  for (let next = todo.pop(); next; next = todo.pop()) {
    await run("cwebp", ["-quiet", "-q", "78", "-m", "6", next[0], "-o", next[1]]);
  }
}
await Promise.all(Array.from({ length: Math.min(8, availableParallelism()) }, worker));
console.log(`${total} pictures`);
