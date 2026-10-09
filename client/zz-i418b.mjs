import { chromium } from "@playwright/test";
const URL = process.env.U || "http://127.0.0.1:19989";
const b = await chromium.launch();
const ctx = await b.newContext({ viewport: { width: 1280, height: 900 } });
await ctx.addInitScript(() => { if (!localStorage.getItem("ps5upload.host")) localStorage.setItem("ps5upload.host", "192.168.86.99"); });
const page = await ctx.newPage();
const t0 = Date.now(); const t = () => ((Date.now() - t0) / 1000).toFixed(1) + "s";
page.on("framenavigated", (f) => { if (f === page.mainFrame()) console.log(t(), "NAV", f.url()); });
page.on("pageerror", (e) => console.log(t(), "PAGEERROR", e.message));
page.on("console", (m) => { if (/chunk|import|reload|module/i.test(m.text())) console.log(t(), "CONSOLE", m.text().slice(0, 300)); });
page.on("response", (r) => { if (r.status() >= 400 && !r.url().includes("/api/")) console.log(t(), "HTTP", r.status(), r.url()); });
await page.goto(`${URL}/install-package`);
console.log(t(), "loaded");
await page.waitForTimeout(3000);
await page.route("**/api/pkg/**", async (r) => { if (r.request().method() === "POST" && /install|stream|session/.test(r.request().url())) console.log(t(), "REQ", r.request().method(), r.request().url()); await r.continue(); });
await page.evaluate(() => { window.__marker = "still-here"; });
// A pairing prompt (unpaired container) is closed: the reload, if any, happens before pairing matters.
const closeBtn = page.getByRole("dialog").getByRole("button", { name: "Close" });
if (await closeBtn.count()) await closeBtn.first().click().catch(() => {});
await page.waitForTimeout(500);
const main = page.locator("[data-scroll-root]:visible");
const sec = main.locator("section").filter({ hasText: "Install a package file" });
console.log(t(), "click Stream & install");
await sec.getByRole("button", { name: /Stream & install/ }).click();
await page.waitForTimeout(2500);
await page.screenshot({ path: "/tmp/claude-501/i418-picker.png" });
const dlg = page.getByRole("dialog");
console.log(t(), "dialog:", (await dlg.innerText().catch(() => "(none)")).slice(0, 400).replace(/\n/g, " | "));
const target = process.env.DIR || "ps5upload-i418-test";
if (target.startsWith("/")) {
  const up = dlg.getByRole("button", { name: /up|parent/i });
  console.log("up buttons:", await up.count(), await dlg.getByRole("button").evaluateAll((bs) => bs.map((b) => b.getAttribute("aria-label") || b.textContent?.trim()).slice(0, 6)));
  if (await up.count()) await up.first().click(); else await dlg.getByRole("button").nth(1).click();
  await page.waitForTimeout(1200);
}
await dlg.getByText(target.replace(/^\//, ""), { exact: true }).click();
await page.waitForTimeout(1500);
console.log(t(), "in folder:", (await dlg.innerText()).slice(0, 200).replace(/\n/g, " | "));
await dlg.getByText("umtx2.github.pkg", { exact: true }).click();
await page.waitForTimeout(800);
const btns = await dlg.getByRole("button").allInnerTexts().catch(() => []);
console.log(t(), "dialog buttons:", btns);
const choose = dlg.getByRole("button", { name: /Choose|Select|Open|Use/ });
if (await choose.count()) { console.log(t(), "click", await choose.first().innerText()); await choose.first().click(); }
for (let i = 0; i < 8; i++) { await page.waitForTimeout(1000); }
await page.screenshot({ path: "/tmp/claude-501/i418-after.png" });
console.log(t(), "marker after:", await page.evaluate(() => window.__marker));
console.log("chunk-reload-at:", await page.evaluate(() => localStorage.getItem("ps5upload.chunk-reload-at")));
await b.close();
