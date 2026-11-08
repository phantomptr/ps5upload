// What the MCP & API screen shows, as plain data: the setup commands for each agent, the MCP tool
// list and the API's routes. No React here, so it can all be tested on its own.

import { safeGetItem, safeRemoveItem, safeSetItem } from "../../lib/safeStorage";

export type AgentId = "claude-code" | "codex" | "opencode";

/** What the desktop shell reports for one agent (commands/agent_mcp.rs). */
export interface AgentStatus {
  agent: AgentId;
  found: boolean;
  configured: boolean;
  saved_url: string | null;
  expected_url: string;
  can_remove: boolean;
  read_failed: boolean;
}

export type AgentState =
  "no_cli" | "unknown" | "not_added" | "added" | "bridge" | "elsewhere";

export function agentState(s: AgentStatus): AgentState {
  if (!s.found) return "no_cli";
  if (s.read_failed) return "unknown";
  if (!s.configured) return "not_added";
  // An entry that runs the stdio bridge has no address to compare, and works.
  if (s.saved_url === null) return "bridge";
  return sameUrl(s.saved_url, s.expected_url) ? "added" : "elsewhere";
}

function sameUrl(a: string | null, b: string): boolean {
  const trim = (u: string) => u.trim().replace(/\/+$/, "");
  return a !== null && trim(a) === trim(b);
}

const SAVED_URL_KEY = "ps5upload.mcp.savedUrl";

/** Keeps the address agents were last seen using, so a later launch can tell when the engine moved
 *  without starting every agent's CLI each time. */
export function rememberAgentUrls(statuses: readonly AgentStatus[]): void {
  const added = statuses.find((s) => agentState(s) === "added");
  if (added) safeSetItem(SAVED_URL_KEY, added.expected_url);
  else if (staleAgents(statuses).length === 0) safeRemoveItem(SAVED_URL_KEY);
}

/** The engine isn't where agents were last seen using it. */
export function agentsMayBeStale(currentMcpUrl: string): boolean {
  const saved = safeGetItem(SAVED_URL_KEY);
  return saved !== null && !sameUrl(saved, currentMcpUrl);
}

export function staleAgents(statuses: readonly AgentStatus[]): AgentStatus[] {
  return statuses.filter((s) => agentState(s) === "elsewhere");
}

/** The agents with one-click setup, in the order they're shown. */
export const AGENTS: ReadonlyArray<{
  id: AgentId;
  name: string;
  installUrl: string;
}> = [
  {
    id: "claude-code",
    name: "Claude Code",
    installUrl: "https://claude.com/product/claude-code",
  },
  { id: "codex", name: "Codex", installUrl: "https://github.com/openai/codex" },
  { id: "opencode", name: "OpenCode", installUrl: "https://opencode.ai" },
];

export function mcpUrl(engineUrl: string): string {
  return `${engineUrl.replace(/\/+$/, "")}/mcp`;
}

/** Whether agents may change things through the MCP server (engine mcp/access.rs). `read_only` is
 *  what is in force; `setting` is this switch; `locked` means PS5UPLOAD_MCP_READ_ONLY in the
 *  engine's environment holds it read-only whatever the switch says. */
export interface AgentAccess {
  read_only: boolean;
  setting: boolean;
  locked: boolean;
}

function agentAccessUrl(engineUrl: string): string {
  return `${engineUrl.replace(/\/+$/, "")}/api/mcp/access`;
}

function parseAgentAccess(doc: unknown): AgentAccess {
  const d = (doc ?? {}) as Record<string, unknown>;
  if (typeof d.read_only !== "boolean") {
    throw new Error("The engine did not say whether agents can change things.");
  }
  return {
    read_only: d.read_only,
    setting: d.setting === true,
    locked: d.locked === true,
  };
}

async function accessReply(res: Response): Promise<AgentAccess> {
  const doc: unknown = await res.json().catch(() => null);
  if (!res.ok) {
    const error = (doc as { error?: unknown } | null)?.error;
    throw new Error(typeof error === "string" ? error : `HTTP ${res.status}`);
  }
  return parseAgentAccess(doc);
}

export async function readAgentAccess(
  engineUrl: string,
  signal?: AbortSignal,
): Promise<AgentAccess> {
  return accessReply(await fetch(agentAccessUrl(engineUrl), { signal }));
}

/** `readOnly`: agents may only look. Applies at once, no restart. */
export async function writeAgentAccess(
  engineUrl: string,
  readOnly: boolean,
): Promise<AgentAccess> {
  return accessReply(
    await fetch(agentAccessUrl(engineUrl), {
      method: "PUT",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ read_only: readOnly }),
    }),
  );
}

/** Quoted so a shell takes it whole. A URL here has no quote of its own to escape. */
function q(url: string): string {
  return /^[\w:/.\-[\]]+$/.test(url) ? url : `"${url}"`;
}

/** The command that adds the server by hand: the same one the Add button runs. */
export function addCommand(agent: AgentId, url: string): string {
  switch (agent) {
    case "claude-code":
      return `claude mcp add --scope user --transport http ps5upload ${q(url)}`;
    case "codex":
      return `codex mcp add ps5upload --url ${q(url)}`;
    case "opencode":
      return `opencode mcp add ps5upload --url ${q(url)}`;
  }
}

/** The command that shows whether it connected. */
export function checkCommand(agent: AgentId): string {
  switch (agent) {
    case "claude-code":
      return "claude mcp list";
    case "codex":
      return "codex mcp list";
    case "opencode":
      return "opencode mcp list";
  }
}

export function geminiCommand(url: string): string {
  return `gemini mcp add --scope user --transport http ps5upload ${q(url)}`;
}

export function cursorConfig(url: string): string {
  return JSON.stringify({ mcpServers: { ps5upload: { url } } }, null, 2);
}

/** Claude Desktop starts the bridge itself. Its custom connectors run from the cloud, so they can't
 *  reach this computer. The engine's address goes along when it isn't the bridge's default. */
export function claudeDesktopConfig(
  enginePath: string,
  engineUrl: string,
): string {
  const base = engineUrl.replace(/\/+$/, "");
  const args =
    base === "http://127.0.0.1:19113" ? ["mcp"] : ["mcp", "--url", base];
  return JSON.stringify(
    { mcpServers: { ps5upload: { command: enginePath, args } } },
    null,
    2,
  );
}

export function curlToolsCommand(url: string): string {
  return [
    `curl -s -X POST ${q(url)} -H 'content-type: application/json' \\`,
    `  -H 'MCP-Protocol-Version: 2025-06-18' \\`,
    `  -d '{"jsonrpc":"2.0","id":1,"method":"tools/list"}'`,
  ].join("\n");
}

export function isLoopback(url: string): boolean {
  try {
    const host = new URL(url).hostname;
    return host === "localhost" || host === "[::1]" || host.startsWith("127.");
  } catch {
    return false;
  }
}

// ── MCP tools ─────────────────────────────────────────────────────────────

export interface McpTool {
  name: string;
  title: string;
  description: string;
  destructive: boolean;
  readOnly: boolean;
}

interface RawTool {
  name?: unknown;
  title?: unknown;
  description?: unknown;
  annotations?: { destructiveHint?: unknown; readOnlyHint?: unknown };
}

export function parseTools(answer: unknown): McpTool[] {
  const tools = (answer as { result?: { tools?: RawTool[] } } | null)?.result
    ?.tools;
  if (!Array.isArray(tools)) return [];
  return tools.flatMap((t) => {
    if (typeof t?.name !== "string") return [];
    return [
      {
        name: t.name,
        title: typeof t.title === "string" ? t.title : "",
        description: typeof t.description === "string" ? t.description : "",
        destructive: t.annotations?.destructiveHint === true,
        readOnly: t.annotations?.readOnlyHint === true,
      },
    ];
  });
}

function matches(query: string, ...fields: string[]): boolean {
  const words = query.toLowerCase().split(/\s+/).filter(Boolean);
  const text = fields.join(" ").toLowerCase();
  return words.every((w) => text.includes(w));
}

export function filterTools(
  tools: readonly McpTool[],
  query: string,
): McpTool[] {
  return tools.filter((t) => matches(query, t.name, t.title, t.description));
}

// ── API routes ────────────────────────────────────────────────────────────

export interface ApiRoute {
  method: string;
  path: string;
  summary: string;
  access: string;
  /** It takes a JSON body. */
  needsBody: boolean;
}

const METHODS = ["get", "post", "put", "patch", "delete"];

interface RawOperation {
  summary?: unknown;
  requestBody?: unknown;
  "x-ps5upload"?: { access?: unknown };
}

export function parseRoutes(doc: unknown): ApiRoute[] {
  const paths = (
    doc as { paths?: Record<string, Record<string, RawOperation>> } | null
  )?.paths;
  if (!paths || typeof paths !== "object") return [];
  const routes: ApiRoute[] = [];
  for (const [path, item] of Object.entries(paths)) {
    for (const method of METHODS) {
      const op = item?.[method];
      if (!op) continue;
      const access = op["x-ps5upload"]?.access;
      routes.push({
        method: method.toUpperCase(),
        path,
        summary: typeof op.summary === "string" ? op.summary : "",
        access: typeof access === "string" ? access : "",
        needsBody: !!op.requestBody,
      });
    }
  }
  return routes.sort(
    (a, b) => a.path.localeCompare(b.path) || a.method.localeCompare(b.method),
  );
}

export function filterRoutes(
  routes: readonly ApiRoute[],
  query: string,
): ApiRoute[] {
  return routes.filter((r) => matches(query, r.method, r.path, r.summary));
}

/** A curl line to start from. `{name}` in a path is left for the reader to fill in. */
export function curlFor(route: ApiRoute, engineUrl: string): string {
  const url = `${engineUrl.replace(/\/+$/, "")}${route.path}`;
  if (route.method === "GET") return `curl -s "${url}"`;
  const body = route.needsBody
    ? ` -H 'content-type: application/json' -d '{}'`
    : "";
  return `curl -s -X ${route.method} "${url}"${body}`;
}
