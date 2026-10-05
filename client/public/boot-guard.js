/*
 * Boot guard: plain ES5, loaded as a classic script BEFORE the app bundle.
 *
 * An old WebView (macOS 11 / Safari, an old Android System WebView, ...) can fail to parse or run
 * the bundle, and the user is then left with a blank white window and no clue why (#352). This file
 * must therefore stay free of any syntax newer than ES5 (no const/let, arrow functions, template
 * strings, ?. or ??): if it failed to parse it would be no better than the bundle. A test parses
 * it and runs it against a stub DOM.
 *
 * It shows a plain-DOM notice when:
 *   - a basic platform feature the app needs is missing, or
 *   - a script fails to parse/load (a SyntaxError, or a <script> that errors), or
 *   - nothing has rendered into #root after BOOT_TIMEOUT_MS.
 * The notice removes itself if the app does start after all.
 */
(function (w, d) {
  "use strict";
  var BOOT_TIMEOUT_MS = 12000;
  var shown = false;
  var box = null;

  function mounted() {
    var r = d.getElementById("root");
    return !!(r && r.firstChild);
  }

  function missingFeatures() {
    var m = [];
    if (typeof w.Promise !== "function") m.push("Promise");
    if (typeof w.fetch !== "function") m.push("fetch");
    if (typeof w.Map !== "function" || typeof w.Set !== "function") m.push("Map/Set");
    if (typeof Object.assign !== "function") m.push("Object.assign");
    if (typeof Object.entries !== "function") m.push("Object.entries");
    if (typeof Array.prototype.includes !== "function") m.push("Array.includes");
    if (w.CSS && typeof w.CSS.supports === "function" && !w.CSS.supports("display", "grid")) m.push("CSS grid");
    return m;
  }

  function show(reason) {
    if (shown || mounted()) return;
    shown = true;
    box = d.createElement("div");
    box.id = "ps5u-boot-notice";
    box.setAttribute("role", "alert");
    box.style.cssText =
      "font:16px/1.5 -apple-system,BlinkMacSystemFont,Segoe UI,Roboto,sans-serif;max-width:560px;margin:10vh auto;padding:24px;color:#111;background:#fff;border:1px solid #ccc;border-radius:8px";
    var h = d.createElement("h1");
    h.style.cssText = "font-size:20px;margin:0 0 12px";
    h.appendChild(d.createTextNode("PS5 Upload cannot start in this web view"));
    var p = d.createElement("p");
    p.appendChild(
      d.createTextNode(
        "The system component that draws this app is too old or blocked part of it. Update your operating system (on a Mac, to a newer macOS than Big Sur 11) or the browser, then open PS5 Upload again.",
      ),
    );
    var q = d.createElement("p");
    q.style.cssText = "font-size:13px;color:#555;word-break:break-word";
    var ua = (w.navigator && w.navigator.userAgent) || "unknown";
    q.appendChild(d.createTextNode("Reason: " + reason + " | " + ua + " | Include this when you report the problem."));
    box.appendChild(h);
    box.appendChild(p);
    box.appendChild(q);
    (d.body || d.documentElement).appendChild(box);
  }

  function hide() {
    if (box && box.parentNode) box.parentNode.removeChild(box);
    box = null;
    shown = false;
  }

  var missing = missingFeatures();
  if (missing.length) show("missing " + missing.join(", "));

  w.addEventListener(
    "error",
    function (e) {
      var t = e && e.target;
      if (t && t !== w && t.tagName === "SCRIPT") {
        show("script failed to load");
        return;
      }
      var msg = (e && e.message) || "";
      if (/SyntaxError|Unexpected (token|identifier|number|end)|BigInt|not a function|is not defined/i.test(msg)) {
        show(msg);
      }
    },
    true,
  );

  w.setTimeout(function check() {
    if (mounted()) {
      if (shown) hide();
      return;
    }
    show("the app did not start within " + BOOT_TIMEOUT_MS / 1000 + " seconds");
    w.setTimeout(check, 1000);
  }, BOOT_TIMEOUT_MS);
})(window, document);
