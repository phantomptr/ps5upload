import { Bot, Plug, Webhook, Wrench } from "lucide-react";

import TabbedShell, { type TabbedShellTab } from "../../layout/TabbedShell";
import ApiTab from "./ApiTab";
import ConnectTab from "./ConnectTab";
import ToolsTab from "./ToolsTab";

type TabId = "connect" | "tools" | "api";

const TABS: ReadonlyArray<TabbedShellTab<TabId>> = [
  {
    id: "connect",
    icon: Bot,
    key: "mcp_tab_connect",
    fallback: "Connect an agent",
  },
  { id: "tools", icon: Wrench, key: "mcp_tab_tools", fallback: "Agent tools" },
  { id: "api", icon: Webhook, key: "mcp_tab_api", fallback: "API" },
];

/** MCP & API (/mcp-api): add the engine's MCP server to an agent, see its tools, and use the API. */
export default function McpApiScreen() {
  return (
    <TabbedShell
      idPrefix="mcp-api"
      titleIcon={Plug}
      titleKey="mcp_api_nav"
      titleFallback="MCP & API"
      tabs={TABS}
      renderPanel={(id) =>
        id === "tools" ? (
          <ToolsTab />
        ) : id === "api" ? (
          <ApiTab />
        ) : (
          <ConnectTab />
        )
      }
    />
  );
}
