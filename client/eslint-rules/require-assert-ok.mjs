/**
 * Custom ESLint rule: require-assert-ok
 *
 * The payload answers a refused command with `ok: false` inside an
 * otherwise successful response. `await` therefore reports success, and
 * the screen shows nothing. That is how "Start FTP Server" could fail
 * with `bind_failed` and look like a dead button, and an audit found 12
 * of 42 `ok`-carrying API functions inspecting it nowhere — including
 * ones that create backups and users.
 *
 * The fix belongs in the wrapper, not at each call site: `sendPayload`
 * has always checked and thrown, and `assertOk()` generalises that. This
 * rule enforces it, so the recurring review question becomes a build
 * failure.
 *
 * It flags an exported async function in the API layer whose response
 * type mentions `ok` but whose body neither calls `assertOk(...)` nor
 * inspects `ok` itself.
 *
 * ── Status endpoints are not actions ────────────────────────────────
 * For a *status* query, `ok: false` legitimately means "this console
 * doesn't expose the feature", and screens render that as an empty
 * state. Throwing there would turn graceful degradation into an error
 * banner. Those live in STATUS_ENDPOINTS below.
 *
 * Adding to STATUS_ENDPOINTS is a claim that `ok:false` is *data* for
 * that call. If a user could reasonably say "I pressed the button and
 * nothing happened", it is an action — guard it instead.
 */

/**
 * Endpoints where `ok: false` is a fact about the console, not a
 * failure of the request.
 */
const STATUS_ENDPOINTS = new Set([
  "fwSpoofStatus",
  "profileInfo",
  "appListRunning",
  "discoverPs5",
  "toastPush",
  "cheatsStatus",
  "ftpStatus",
  "remoteplayStatus",
  "smpStatus",
  "activityGet",
  "activityDbQuery",
  "bundledPayloadInfo",
  "bundledPayloadPath",
  "sdkScan",
  "procModulesGet",
  "netInterfacesGet",
  "userListGet",
]);

/** Only the API modules — this is a rule about where guards belong. */
function isApiModule(filename) {
  return /client\/src\/api\/[^/]+\.ts$/.test(filename.replace(/\\/g, "/"));
}

const rule = {
  meta: {
    type: "problem",
    docs: {
      description:
        "API wrappers whose response carries `ok` must reject on ok:false, or be declared a status endpoint",
    },
    schema: [],
    messages: {
      unguarded:
        "`{{name}}` returns a response carrying `ok`, but never checks it — a refusal from the console will read as success. Wrap the result in assertOk(resp, \"…\"), or add it to STATUS_ENDPOINTS in eslint-rules/require-assert-ok.mjs if ok:false is data rather than a failure.",
    },
  },

  create(context) {
    const filename = context.filename ?? context.getFilename();
    if (!isApiModule(filename)) return {};
    const source = context.sourceCode ?? context.getSourceCode();

    /**
     * Interfaces/type aliases in this file that carry an `ok` field.
     *
     * Without this, a wrapper returning `Promise<PowerControlAck>` looks
     * clean simply because the word `ok` is not in its signature — the
     * rule would pass while the bug it exists for sits one indirection
     * away.
     */
    const okTypes = new Set();
    {
      const whole = source.getText();
      const re = /(?:interface|type)\s+(\w+)[^{]*\{([^}]*)\}/g;
      let m;
      while ((m = re.exec(whole)) !== null) {
        if (/\bok\b\s*[?]?\s*:/.test(m[2])) okTypes.add(m[1]);
      }
    }

    return {
      ExportNamedDeclaration(node) {
        const decl = node.declaration;
        if (!decl || decl.type !== "FunctionDeclaration" || !decl.async) return;
        const name = decl.id?.name;
        if (!name || STATUS_ENDPOINTS.has(name)) return;

        // The declared shape: return type, invoke<...> generic, or a
        // named type in this file that carries `ok`.
        const text = source.getText(decl);
        const header = text.slice(0, text.indexOf("{"));
        const namedOk = [...okTypes].some((t) =>
          new RegExp(`\\b${t}\\b`).test(header),
        );
        const declaresOk =
          namedOk ||
          /\bok\b\s*[?]?\s*:/.test(header) ||
          /invoke<[^>]*\bok\b\s*[?]?\s*:/.test(text);
        if (!declaresOk) return;

        // Already guarded, either via the shared helper or by hand.
        const body = source.getText(decl.body);
        if (
          body.includes("assertOk(") ||
          /\bok\s*===\s*false\b/.test(body) ||
          /!\w+\.ok\b/.test(body)
        )
          return;

        /*
         * Deliberately checked by the caller instead.
         *
         * Some screens inspect `ok` themselves because a refusal is part
         * of their UI rather than an error — PowerControl reports "the
         * console declined to shut down" in place, and PeripheralPanel
         * routes every disc-drive action through one runner that checks
         * it once. Throwing from the wrapper would replace a considered
         * message with an exception.
         *
         * The marker is required rather than inferred: a rule inside the
         * API module cannot see call sites, and silently trusting that
         * "someone probably checks" is the assumption that produced this
         * bug class. Writing it down makes the claim greppable and puts
         * it next to the code it describes.
         */
        const leading = source
          .getCommentsBefore(node)
          .map((c) => c.value)
          .join(" ");
        if (/ok-checked-by-caller/.test(leading) || /ok-checked-by-caller/.test(body))
          return;

        context.report({ node: decl.id ?? decl, messageId: "unguarded", data: { name } });
      },
    };
  },
};

export default {
  rules: { "require-assert-ok": rule },
};
