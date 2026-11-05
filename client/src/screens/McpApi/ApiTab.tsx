import { useEffect, useMemo, useState } from "react";
import { ExternalLink, Play, Search } from "lucide-react";

import { Badge, Button, Callout, Input } from "../../components";
import { openExternalUrl } from "../../lib/openExternalUrl";
import { getEngineUrl } from "../../state/engine";
import { useTr } from "../../state/lang";
import { CopyBlock } from "./CopyBlock";
import { curlFor, filterRoutes, parseRoutes, type ApiRoute } from "./model";

const API_DOC = "https://github.com/phantomptr/ps5upload/blob/main/API.md";
const ROW_LIMIT = 100;

const METHOD_TONE: Record<string, "good" | "accent" | "warn" | "neutral"> = {
  GET: "good",
  POST: "accent",
  DELETE: "warn",
};

/** The two calls that can be tried here: they only read, and need no console. */
const TRY_PATHS = ["/api/version", "/api/jobs"];

export default function ApiTab() {
  const tr = useTr();
  const base = getEngineUrl().replace(/\/+$/, "");
  const [routes, setRoutes] = useState<ApiRoute[] | null>(null);
  const [failed, setFailed] = useState(false);
  const [query, setQuery] = useState("");

  useEffect(() => {
    const stop = new AbortController();
    void fetch(`${base}/api/openapi.json`, { signal: stop.signal })
      .then((res) => res.json())
      .then((doc: unknown) => {
        const list = parseRoutes(doc);
        setRoutes(list);
        setFailed(list.length === 0);
      })
      .catch(() => {
        if (!stop.signal.aborted) setFailed(true);
      });
    return () => stop.abort();
  }, [base]);

  const shown = useMemo(
    () => filterRoutes(routes ?? [], query),
    [routes, query],
  );

  return (
    <div className="space-y-6" data-testid="mcp-api">
      <section className="space-y-3">
        <p className="text-sm text-[var(--color-muted)]">
          {tr(
            "api_intro",
            undefined,
            "Everything the app does goes through its engine, a plain HTTP server you can call from a script or a home automation. It answers while the app is open.",
          )}
        </p>
        <CopyBlock
          label={tr("api_address", undefined, "API address")}
          text={base}
        />
      </section>

      <section className="space-y-3">
        <h2 className="text-sm font-semibold">
          {tr("api_try_title", undefined, "Try it")}
        </h2>
        <div className="grid gap-3 lg:grid-cols-2">
          {TRY_PATHS.map((path) => (
            <TryCall key={path} base={base} path={path} />
          ))}
        </div>
        <CopyBlock
          label={tr(
            "api_console_label",
            undefined,
            "Name a console with addr: in the query for GET, in the JSON body for POST",
          )}
          text={`curl -s "${base}/api/ps5/status?addr=192.168.1.50"`}
        />
        <CopyBlock
          label={tr(
            "api_openapi_label",
            undefined,
            "Save the full reference, for Swagger UI, Scalar or generating a client",
          )}
          text={`curl -s ${base}/api/openapi.json -o ps5upload-openapi.json`}
        />
      </section>

      <section className="space-y-2">
        <h2 className="text-sm font-semibold">
          {tr("api_know_title", undefined, "Good to know")}
        </h2>
        <ul className="list-disc space-y-1 ps-5 text-sm text-[var(--color-muted)]">
          <li>
            {tr(
              "api_know_jobs",
              undefined,
              "Uploads and installs answer with a job at once. Follow the job until it finishes.",
            )}
          </li>
          <li>
            {tr(
              "api_know_versions",
              undefined,
              "Routes can change between versions, so pin a script to the version /api/version reports.",
            )}
          </li>
          <li>
            {tr(
              "api_know_remote",
              undefined,
              "It only answers this computer until you list others in PS5UPLOAD_ALLOW_IP. Remote Play routes also need PS5UPLOAD_ALLOW_AUTOMATION=1.",
            )}
          </li>
          <li>
            {tr(
              "api_know_password",
              undefined,
              "There is no password, so never expose the engine to the internet.",
            )}
          </li>
        </ul>
        <Button
          variant="secondary"
          size="sm"
          onClick={() => void openExternalUrl(API_DOC)}
          rightIcon={<ExternalLink size={14} />}
        >
          {tr("api_full_guide", undefined, "Full API guide")}
        </Button>
      </section>

      <section className="space-y-3">
        <h2 className="text-sm font-semibold">
          {tr("api_routes_title", undefined, "Every route")}
        </h2>
        {failed ? (
          <Callout
            tone="warn"
            title={tr(
              "api_routes_failed",
              undefined,
              "Couldn't load the route list from the engine",
            )}
          />
        ) : (
          <>
            <Input
              block
              value={query}
              onChange={(e) => setQuery(e.target.value)}
              leftIcon={<Search size={14} />}
              placeholder={tr("api_routes_search", undefined, "Search routes")}
              aria-label={tr("api_routes_search", undefined, "Search routes")}
            />
            <p className="text-xs text-[var(--color-muted)]">
              {routes === null
                ? tr("mcp_loading", undefined, "Loading…")
                : shown.length > ROW_LIMIT
                  ? tr(
                      "api_routes_count_limited",
                      { shown: ROW_LIMIT, n: shown.length },
                      `Showing ${ROW_LIMIT} of ${shown.length} routes. Search to narrow them down.`,
                    )
                  : tr(
                      "api_routes_count",
                      { n: shown.length },
                      `${shown.length} routes`,
                    )}
            </p>
            <ul className="space-y-2">
              {shown.slice(0, ROW_LIMIT).map((route) => (
                <RouteRow
                  key={`${route.method} ${route.path}`}
                  route={route}
                  base={base}
                />
              ))}
            </ul>
          </>
        )}
      </section>
    </div>
  );
}

function RouteRow({ route, base }: { route: ApiRoute; base: string }) {
  const tr = useTr();
  return (
    <li
      className="min-w-0 space-y-2 rounded-[var(--radius-card)] border border-[var(--glass-edge)] bg-[var(--color-surface-raised)] px-3 py-2"
      data-testid="mcp-route"
    >
      <div dir="ltr" className="flex flex-wrap items-center gap-2 text-start">
        <Badge tone={METHOD_TONE[route.method] ?? "neutral"}>
          {route.method}
        </Badge>
        <code className="min-w-0 break-all font-mono text-xs font-semibold">
          {route.path}
        </code>
        {route.access === "strict-local" && (
          <Badge tone="warn">
            {tr("api_route_this_computer", undefined, "This computer only")}
          </Badge>
        )}
      </div>
      {route.summary && (
        <p dir="ltr" className="text-start text-xs text-[var(--color-muted)]">
          {route.summary}
        </p>
      )}
      <CopyBlock text={curlFor(route, base)} />
    </li>
  );
}

function TryCall({ base, path }: { base: string; path: string }) {
  const tr = useTr();
  const [running, setRunning] = useState(false);
  const [answer, setAnswer] = useState<string | null>(null);

  const run = async () => {
    setRunning(true);
    try {
      const res = await fetch(`${base}${path}`);
      const text = await res.text();
      let pretty = text;
      try {
        pretty = JSON.stringify(JSON.parse(text), null, 2);
      } catch {
        // Not JSON: as it came.
      }
      setAnswer(`HTTP ${res.status}\n${pretty}`);
    } catch (e) {
      setAnswer(e instanceof Error ? e.message : String(e));
    } finally {
      setRunning(false);
    }
  };

  return (
    <div className="min-w-0 space-y-2">
      <div className="flex items-end gap-2">
        <div className="min-w-0 flex-1">
          <CopyBlock text={`curl -s ${base}${path}`} />
        </div>
        <Button
          variant="secondary"
          size="sm"
          loading={running}
          onClick={() => void run()}
          leftIcon={<Play size={14} />}
        >
          {tr("api_run", undefined, "Run")}
        </Button>
      </div>
      {answer !== null && (
        <pre
          dir="ltr"
          data-testid="api-answer"
          className="max-h-60 overflow-auto rounded-[var(--radius-card)] border border-[var(--color-border)] bg-[var(--color-surface-2)] p-3 text-start font-mono text-xs"
        >
          {answer}
        </pre>
      )}
    </div>
  );
}
