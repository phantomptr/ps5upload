import { describe, expect, it } from "vitest";

import {
  addCommand,
  agentState,
  claudeDesktopConfig,
  curlFor,
  filterRoutes,
  filterTools,
  isLoopback,
  mcpUrl,
  parseRoutes,
  parseTools,
  type AgentStatus,
} from "./model";

const URL = "http://127.0.0.1:19113/mcp";

function status(over: Partial<AgentStatus>): AgentStatus {
  return {
    agent: "codex",
    found: true,
    configured: false,
    saved_url: null,
    expected_url: URL,
    can_remove: true,
    read_failed: false,
    ...over,
  };
}

describe("agentState", () => {
  it("tells apart every state an agent can be in", () => {
    expect(agentState(status({ found: false }))).toBe("no_cli");
    expect(agentState(status({ read_failed: true }))).toBe("unknown");
    expect(agentState(status({}))).toBe("not_added");
    expect(agentState(status({ configured: true, saved_url: URL }))).toBe(
      "added",
    );
    expect(agentState(status({ configured: true, saved_url: `${URL}/` }))).toBe(
      "added",
    );
  });

  it("notices an entry that points somewhere else, as after the engine moved port", () => {
    expect(
      agentState(
        status({ configured: true, saved_url: "http://127.0.0.1:50000/mcp" }),
      ),
    ).toBe("elsewhere");
    // The stdio bridge has no address.
    expect(agentState(status({ configured: true, saved_url: null }))).toBe(
      "elsewhere",
    );
  });
});

describe("setup text", () => {
  it("gives each agent the command its CLI takes", () => {
    expect(addCommand("claude-code", URL)).toBe(
      `claude mcp add --scope user --transport http ps5upload ${URL}`,
    );
    expect(addCommand("codex", URL)).toBe(
      `codex mcp add ps5upload --url ${URL}`,
    );
    expect(addCommand("opencode", URL)).toBe(
      `opencode mcp add ps5upload --url ${URL}`,
    );
  });

  it("builds the MCP address from the engine's, with or without a slash", () => {
    expect(mcpUrl("http://127.0.0.1:19113")).toBe(URL);
    expect(mcpUrl("http://127.0.0.1:19113/")).toBe(URL);
  });

  it("tells the bridge the engine's address only when it isn't the default", () => {
    const args = (engine: string) =>
      JSON.parse(claudeDesktopConfig("/x/engine", engine)).mcpServers.ps5upload
        .args;
    expect(args("http://127.0.0.1:19113")).toEqual(["mcp"]);
    expect(args("http://127.0.0.1:50000")).toEqual([
      "mcp",
      "--url",
      "http://127.0.0.1:50000",
    ]);
  });

  it("knows this computer from another", () => {
    expect(isLoopback("http://127.0.0.1:19113")).toBe(true);
    expect(isLoopback("http://localhost:19113")).toBe(true);
    expect(isLoopback("http://192.168.1.20:19113")).toBe(false);
    expect(isLoopback("not a url")).toBe(false);
  });
});

describe("the tool list", () => {
  const answer = {
    result: {
      tools: [
        {
          name: "engine_version",
          title: "Engine version",
          description: "The version.",
          annotations: { readOnlyHint: true, destructiveHint: false },
        },
        {
          name: "files_delete",
          description: "Delete a file.",
          annotations: { destructiveHint: true },
        },
        { description: "no name" },
      ],
    },
  };

  it("reads names and hints, skipping what has no name", () => {
    const tools = parseTools(answer);
    expect(tools.map((t) => t.name)).toEqual([
      "engine_version",
      "files_delete",
    ]);
    expect(tools[0].readOnly).toBe(true);
    expect(tools[1].destructive).toBe(true);
  });

  it("answers nothing for an error", () => {
    expect(parseTools({ error: { message: "no" } })).toEqual([]);
    expect(parseTools(null)).toEqual([]);
  });

  it("searches every word across the fields", () => {
    const tools = parseTools(answer);
    expect(filterTools(tools, "delete file").map((t) => t.name)).toEqual([
      "files_delete",
    ]);
    expect(filterTools(tools, "VERSION").map((t) => t.name)).toEqual([
      "engine_version",
    ]);
    expect(filterTools(tools, "")).toHaveLength(2);
    expect(filterTools(tools, "nothing like this")).toEqual([]);
  });
});

describe("the route list", () => {
  const doc = {
    paths: {
      "/api/version": {
        get: {
          summary: "Engine version",
          "x-ps5upload": { access: "loopback" },
        },
      },
      "/api/ps5/rp/start": {
        post: {
          summary: "Start a session",
          requestBody: {},
          "x-ps5upload": { access: "strict-local" },
        },
        parameters: [],
      },
      "/api/queue": { get: { summary: "List" }, delete: { summary: "Clear" } },
    },
  };

  it("makes one row per method, in path order, ignoring what isn't a method", () => {
    const rows = parseRoutes(doc).map((r) => `${r.method} ${r.path}`);
    expect(rows).toEqual([
      "POST /api/ps5/rp/start",
      "DELETE /api/queue",
      "GET /api/queue",
      "GET /api/version",
    ]);
  });

  it("keeps who a route answers", () => {
    expect(parseRoutes(doc)[0].access).toBe("strict-local");
    expect(parseRoutes({})).toEqual([]);
  });

  it("searches paths, methods and summaries", () => {
    const routes = parseRoutes(doc);
    expect(filterRoutes(routes, "delete queue")).toHaveLength(1);
    expect(filterRoutes(routes, "session")).toHaveLength(1);
  });

  it("writes a curl line to start from", () => {
    const [start, del, get] = parseRoutes(doc);
    const base = "http://127.0.0.1:19113/";
    expect(curlFor(start, base)).toBe(
      `curl -s -X POST "http://127.0.0.1:19113/api/ps5/rp/start" -H 'content-type: application/json' -d '{}'`,
    );
    expect(curlFor(del, base)).toBe(
      `curl -s -X DELETE "http://127.0.0.1:19113/api/queue"`,
    );
    expect(curlFor(get, base)).toBe(
      `curl -s "http://127.0.0.1:19113/api/queue"`,
    );
  });
});
