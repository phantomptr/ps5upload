import { useEffect, useMemo, useState } from "react";
import { Search } from "lucide-react";

import { Badge, Callout, Input } from "../../components";
import { getEngineUrl } from "../../state/engine";
import { useTr } from "../../state/lang";
import { filterTools, mcpUrl, parseTools, type McpTool } from "./model";

/** Rows drawn before a search narrows them down. */
const ROW_LIMIT = 150;

export default function ToolsTab() {
  const tr = useTr();
  const [tools, setTools] = useState<McpTool[] | null>(null);
  const [failed, setFailed] = useState(false);
  const [query, setQuery] = useState("");

  useEffect(() => {
    const stop = new AbortController();
    void fetch(mcpUrl(getEngineUrl()), {
      method: "POST",
      signal: stop.signal,
      headers: {
        "content-type": "application/json",
        "MCP-Protocol-Version": "2025-06-18",
      },
      body: JSON.stringify({ jsonrpc: "2.0", id: 1, method: "tools/list" }),
    })
      .then((res) => res.json())
      .then((answer: unknown) => {
        const list = parseTools(answer);
        setTools(list);
        setFailed(list.length === 0);
      })
      .catch(() => {
        if (!stop.signal.aborted) setFailed(true);
      });
    return () => stop.abort();
  }, []);

  const shown = useMemo(() => filterTools(tools ?? [], query), [tools, query]);

  if (failed) {
    return (
      <Callout
        tone="warn"
        title={tr(
          "mcp_tools_failed_title",
          undefined,
          "Couldn't list the tools",
        )}
      >
        {tr(
          "mcp_tools_failed_body",
          undefined,
          "The engine only shows this list to a page on its own computer. Your agent can still see every tool once it's connected.",
        )}
      </Callout>
    );
  }

  return (
    <div className="space-y-3" data-testid="mcp-tools">
      <Input
        block
        value={query}
        onChange={(e) => setQuery(e.target.value)}
        leftIcon={<Search size={14} />}
        placeholder={tr("mcp_tools_search", undefined, "Search tools")}
        aria-label={tr("mcp_tools_search", undefined, "Search tools")}
      />
      <p className="text-xs text-[var(--color-muted)]">
        {tools === null
          ? tr("mcp_loading", undefined, "Loading…")
          : tr("mcp_tools_count", { n: shown.length }, `${shown.length} tools`)}
      </p>
      <ul className="divide-y divide-[var(--color-border)] rounded-[var(--radius-panel)] border border-[var(--glass-edge)] bg-[var(--color-surface-raised)]">
        {shown.slice(0, ROW_LIMIT).map((tool) => (
          <li
            key={tool.name}
            className="space-y-1 px-4 py-3"
            data-testid="mcp-tool"
          >
            <div className="flex flex-wrap items-center gap-2">
              <code dir="ltr" className="font-mono text-sm font-semibold">
                {tool.name}
              </code>
              {tool.destructive && (
                <Badge tone="warn">
                  {tr(
                    "mcp_tool_can_change",
                    undefined,
                    "Can lose or interrupt something",
                  )}
                </Badge>
              )}
              {tool.readOnly && (
                <Badge tone="good">
                  {tr("mcp_tool_read_only", undefined, "Read-only")}
                </Badge>
              )}
            </div>
            {/* The engine writes these in English. */}
            <p
              dir="ltr"
              className="text-start text-xs leading-relaxed text-[var(--color-muted)]"
            >
              {tool.description}
            </p>
          </li>
        ))}
      </ul>
    </div>
  );
}
