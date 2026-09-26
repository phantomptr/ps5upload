#!/usr/bin/env node
/*
 * Mobile sizing audit — measurement logic.
 *
 * Walks the app at Android's viewport and reports layout defects that
 * are invisible in code review but obvious on a phone:
 *
 *   1. Interactive elements below the 44x44px touch floor
 *      (docs/v5-mobile-design.md §4.1).
 *   2. Nested scroll containers — a scrollable element inside another.
 *      This is the defect class that made the old More sheet unusable.
 *   3. Horizontal overflow of the document.
 *
 * DRIVER-AGNOSTIC BY DESIGN. This module holds the probe and the report
 * formatting; it does NOT depend on Playwright, because the repo's root
 * package.json deliberately carries zero devDependencies and a browser
 * driver would add a ~500MB download to every dev machine and CI runner.
 *
 * Two ways to run it:
 *
 *   a) Inject `PROBE_SOURCE` into any browser automation you already
 *      have (the probe is self-contained — no closures, no imports) and
 *      feed the results to `summarize()`.
 *
 *   b) Install Playwright and run this file directly:
 *        npm i -D playwright && npx playwright install chromium
 *        node scripts/mobile-audit-probe.mjs
 *      It exits 0 when clean, 1 when there are findings.
 *
 * LIMITATION: this measures the BROWSER build at Android's viewport. It
 * catches layout, sizing and overflow. It does NOT catch WebView-specific
 * rendering, real touch behaviour, or device-reported safe-area insets.
 * Confirm on hardware with `adb exec-out screencap -p`.
 */
import { readFileSync } from "node:fs";

/** Pixel 9 Pro XL: 1344x2992 physical at density 480 → 448x997 CSS px. */
export const VIEWPORT = { width: 448, height: 997 };

/** WCAG 2.5.5 / mobile-design §4.1. */
export const MIN_TARGET = 44;

/** Every route reachable from the nav catalogue, plus the tab roots. */
export const ROUTES = [
  "/home", "/connection", "/whats-new", "/dashboard", "/more", "/payloads",
  "/upload", "/install-package", "/saves", "/screenshots", "/videos",
  "/games", "/installed", "/files", "/search", "/volumes", "/disk-usage",
  "/console", "/processes", "/profile", "/fan-curve", "/remote-play",
  "/notifications", "/cheats", "/game-activity", "/sdk-changer", "/tmdb",
  "/fw-spoof", "/ftp-server", "/smb-browser", "/backup", "/nanodns",
  "/shell", "/tasks", "/stats", "/logs", "/audit-log", "/bug-report",
  "/faq", "/settings", "/about",
];

/**
 * Runs INSIDE the page. Self-contained on purpose so it can be injected
 * as a string by any driver — do not reference module scope from here.
 *
 * Scoped to <main> so the persistent chrome (bottom nav, top bar) is
 * measured once on its own rather than re-reported on all 41 routes.
 */
export function probe(minTarget) {
  const SEL =
    'a, button, [role="button"], input, select, textarea, [tabindex]:not([tabindex="-1"])';

  const isVisible = (el) => {
    const r = el.getBoundingClientRect();
    if (r.width === 0 || r.height === 0) return false;
    const s = getComputedStyle(el);
    return (
      s.display !== "none" && s.visibility !== "hidden" && s.opacity !== "0"
    );
  };

  const labelOf = (el) =>
    (
      el.getAttribute("aria-label") ||
      el.textContent ||
      el.getAttribute("placeholder") ||
      ""
    )
      .trim()
      .replace(/\s+/g, " ")
      .slice(0, 40);

  /** Short, reasonably stable key for allowlisting. */
  const keyOf = (el) => {
    const cls = (el.getAttribute("class") || "")
      .split(/\s+/)
      .filter(Boolean)
      .slice(0, 2)
      .join(".");
    return el.tagName.toLowerCase() + (cls ? "." + cls : "");
  };

  /**
   * Effective tap rect.
   *
   * A checkbox or radio is drawn at ~20px on purpose — drawing it at
   * 44px looks wrong. What the user actually taps is the control PLUS
   * its associated <label for>, because clicking the label toggles the
   * control. Measuring the bare input would report a false failure on a
   * correctly-built row, so union the two.
   */
  const tapRect = (el) => {
    const r = el.getBoundingClientRect();
    const type = (el.getAttribute("type") || "").toLowerCase();
    if (el.tagName !== "INPUT" || (type !== "checkbox" && type !== "radio")) {
      return r;
    }
    const id = el.getAttribute("id");
    const lbl = id
      ? document.querySelector(`label[for="${CSS.escape(id)}"]`)
      : el.closest("label");
    if (!lbl) return r;
    const l = lbl.getBoundingClientRect();
    return {
      width: Math.max(r.right, l.right) - Math.min(r.left, l.left),
      height: Math.max(r.bottom, l.bottom) - Math.min(r.top, l.top),
    };
  };

  const root = document.querySelector("main") || document.body;

  const smallTargets = [...root.querySelectorAll(SEL)]
    .filter(isVisible)
    .map((el) => {
      const r = tapRect(el);
      return {
        w: Math.round(r.width),
        h: Math.round(r.height),
        label: labelOf(el),
        key: keyOf(el),
      };
    })
    .filter((e) => e.h < minTarget || e.w < minTarget);

  const scrollers = [...root.querySelectorAll("*")].filter((el) => {
    const s = getComputedStyle(el);
    return (
      ["auto", "scroll"].includes(s.overflowY) &&
      el.scrollHeight > el.clientHeight
    );
  });
  const nestedScrollers = [
    ...new Set(
      scrollers
        .filter((el) => scrollers.some((o) => o !== el && o.contains(el)))
        .map(keyOf),
    ),
  ];

  return {
    smallTargets,
    nestedScrollers,
    hOverflow:
      document.documentElement.scrollWidth >
      document.documentElement.clientWidth,
  };
}

/** The probe as an injectable string, for drivers that take source. */
export const PROBE_SOURCE = probe.toString();

export function loadAllowlist(path = "scripts/mobile-audit-allowlist.json") {
  try {
    return JSON.parse(readFileSync(path, "utf8"));
  } catch {
    return { routes: {} };
  }
}

/**
 * Drop allowlisted findings and keep only routes that still have any.
 * `results` is `[{ route, ...probeResult }]`.
 */
export function summarize(results, allowlist = { routes: {} }) {
  const report = [];
  for (const r of results) {
    if (r.error) {
      report.push({ route: r.route, error: r.error });
      continue;
    }
    const allowed = new Set(allowlist.routes?.[r.route] || []);
    const smallTargets = (r.smallTargets || []).filter(
      (s) => !allowed.has(s.key),
    );
    const nestedScrollers = (r.nestedScrollers || []).filter(
      (k) => !allowed.has(k),
    );
    if (smallTargets.length || nestedScrollers.length || r.hOverflow) {
      report.push({
        route: r.route,
        smallTargets,
        nestedScrollers,
        hOverflow: !!r.hOverflow,
      });
    }
  }
  return report;
}

export function formatReport(report, routeCount) {
  if (report.length === 0) {
    return `[mobile-audit] ok — ${routeCount} routes clean at ${VIEWPORT.width}x${VIEWPORT.height}`;
  }
  const lines = [`[mobile-audit] ${report.length} route(s) with findings:`, ""];
  for (const r of report) {
    lines.push(`  ${r.route}`);
    if (r.error) {
      lines.push(`      ! ${r.error}`);
      continue;
    }
    if (r.hOverflow) lines.push("      ! horizontal overflow");
    for (const n of r.nestedScrollers)
      lines.push(`      ! nested scroller: ${n}`);
    for (const s of r.smallTargets)
      lines.push(`      ! ${s.w}x${s.h}px  "${s.label}"  (${s.key})`);
  }
  lines.push(
    "",
    "  fix the above, or allowlist the key in scripts/mobile-audit-allowlist.json",
  );
  return lines.join("\n");
}

/* ── Optional Playwright runner ───────────────────────────────────────
 * Only used when this file is executed directly AND playwright resolves.
 * Kept opt-in so the module stays dependency-free for injection use. */
if (import.meta.url === `file://${process.argv[1]}`) {
  const base = process.env.AUDIT_BASE || "http://localhost:1420";
  let chromium;
  try {
    ({ chromium } = await import("playwright"));
  } catch {
    console.error(
      "[mobile-audit] playwright is not installed, so this file cannot drive a\n" +
        "browser itself. Either:\n" +
        "  npm i -D playwright && npx playwright install chromium\n" +
        "or inject PROBE_SOURCE via your own automation and pass the results\n" +
        "to summarize() + formatReport().",
    );
    process.exit(2);
  }

  const browser = await chromium.launch();
  const page = await browser.newPage({ viewport: VIEWPORT });
  const results = [];
  for (const route of ROUTES) {
    try {
      await page.goto(base + route, {
        waitUntil: "networkidle",
        timeout: 20000,
      });
      await page.waitForTimeout(400); // let lazy chunks paint
      results.push({ route, ...(await page.evaluate(probe, MIN_TARGET)) });
    } catch {
      results.push({ route, error: "navigation failed" });
    }
  }
  await browser.close();

  const report = summarize(results, loadAllowlist());
  console.log(formatReport(report, ROUTES.length));
  process.exit(report.length === 0 ? 0 : 1);
}
