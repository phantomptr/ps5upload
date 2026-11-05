import { useCallback, useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { Download, ExternalLink, Plus, RefreshCw, Trash2 } from "lucide-react";

import { Badge, Button, Callout } from "../../components";
import { FoldSection } from "../../components/FoldSection";
import { openExternalUrl } from "../../lib/openExternalUrl";
import { isMobile } from "../../lib/platform";
import { isTauriEnv } from "../../lib/tauriEnv";
import { getEngineUrl } from "../../state/engine";
import { useTr } from "../../state/lang";
import { CopyBlock } from "./CopyBlock";
import {
  AGENTS,
  addCommand,
  agentState,
  checkCommand,
  claudeDesktopConfig,
  curlToolsCommand,
  cursorConfig,
  geminiCommand,
  isLoopback,
  mcpUrl,
  rememberAgentUrls,
  type AgentId,
  type AgentStatus,
} from "./model";

const MCP_DOC = "https://github.com/phantomptr/ps5upload/blob/main/MCP.md";

function errorText(e: unknown): string {
  return e instanceof Error ? e.message : String(e);
}

export default function ConnectTab() {
  const tr = useTr();
  const engineUrl = getEngineUrl();
  const url = mcpUrl(engineUrl);
  const desktop = isTauriEnv() && !isMobile();
  const phoneApp = isTauriEnv() && isMobile();

  const [statuses, setStatuses] = useState<AgentStatus[] | null>(null);
  const [checking, setChecking] = useState(desktop);
  const [bridgePath, setBridgePath] = useState("");

  const refresh = useCallback(async () => {
    if (!desktop) return;
    setChecking(true);
    try {
      setStatuses(await invoke<AgentStatus[]>("agent_mcp_status"));
    } catch {
      setStatuses([]);
    } finally {
      setChecking(false);
    }
  }, [desktop]);

  useEffect(() => {
    void refresh();
    if (desktop) {
      void invoke<string>("agent_mcp_bridge_path")
        .then(setBridgePath)
        .catch(() => setBridgePath(""));
    }
  }, [desktop, refresh]);

  useEffect(() => {
    if (statuses?.length) rememberAgentUrls(statuses);
  }, [statuses]);

  const [engineVersion, setEngineVersion] = useState("");
  useEffect(() => {
    const stop = new AbortController();
    void fetch(`${engineUrl.replace(/\/+$/, "")}/api/version`, {
      signal: stop.signal,
    })
      .then((res) => res.json())
      .then((doc: { version?: unknown }) => {
        if (typeof doc?.version === "string") setEngineVersion(doc.version);
      })
      .catch(() => setEngineVersion(""));
    return () => stop.abort();
  }, [engineUrl]);

  const onChanged = (next: AgentStatus) => {
    setStatuses((list) =>
      (list ?? []).map((s) => (s.agent === next.agent ? next : s)),
    );
  };

  return (
    <div className="space-y-6" data-testid="mcp-connect">
      {phoneApp && (
        <Callout
          tone="info"
          title={tr("mcp_phone_title", undefined, "Set this up on a computer")}
        >
          {tr(
            "mcp_phone_body",
            undefined,
            "The engine inside this phone app is for the app itself. To use an agent or a script, run them on a computer with the PS5Upload app open, or with the Docker engine.",
          )}
        </Callout>
      )}
      {!isTauriEnv() && !isLoopback(engineUrl) && (
        <Callout
          tone="info"
          title={tr(
            "mcp_other_computer_title",
            undefined,
            "This engine is on another computer",
          )}
        >
          {tr(
            "mcp_other_computer_body",
            undefined,
            "An agent on that computer can use these steps with its own 127.0.0.1 address. An agent anywhere else also needs its address in the engine's PS5UPLOAD_ALLOW_IP setting.",
          )}
        </Callout>
      )}

      <section className="space-y-3">
        <p className="text-sm text-[var(--color-muted)]">
          {tr(
            "mcp_intro",
            undefined,
            "MCP lets an AI agent drive your PS5 with tools it understands: send files, install games, check temperatures, and more. Add it once, then just ask.",
          )}
        </p>
        <CopyBlock
          label={tr("mcp_address", undefined, "MCP address")}
          text={url}
        />
      </section>

      <section className="space-y-3">
        <div className="flex items-center justify-between gap-3">
          <h2 className="text-sm font-semibold">
            {tr("mcp_agents_title", undefined, "Add it to your agent")}
          </h2>
          {desktop && (
            <Button
              variant="ghost"
              size="sm"
              loading={checking}
              onClick={() => void refresh()}
              leftIcon={<RefreshCw size={14} />}
            >
              {tr("mcp_check_again", undefined, "Check again")}
            </Button>
          )}
        </div>
        <div className="grid gap-3 lg:grid-cols-3">
          {AGENTS.map((agent) => (
            <AgentCard
              key={agent.id}
              id={agent.id}
              name={agent.name}
              installUrl={agent.installUrl}
              url={url}
              desktop={desktop}
              checking={checking && !statuses}
              status={statuses?.find((s) => s.agent === agent.id)}
              onChanged={onChanged}
            />
          ))}
        </div>
        <p className="text-xs text-[var(--color-muted)]">
          {tr(
            "mcp_after_adding",
            undefined,
            "Start a new session in your agent to load it. Keep PS5Upload open while the agent works: the MCP server is part of its engine.",
          )}
        </p>
      </section>

      <section className="space-y-2" data-testid="mcp-keep-updated">
        <h2 className="text-sm font-semibold">
          {tr("mcp_update_title", undefined, "Keeping it up to date")}
        </h2>
        <ul className="list-disc space-y-1 ps-5 text-sm text-[var(--color-muted)]">
          <li>
            {tr(
              "mcp_update_app",
              undefined,
              "When PS5Upload updates, there's nothing to do here. Agents get their tools from the engine, so new ones appear the next time you start a session. A session already open keeps the old list.",
            )}
          </li>
          <li>
            {tr(
              "mcp_update_address",
              undefined,
              "If the engine's address changes, the app tells you when it starts, and the agent's card here shows Update. One click fixes it.",
            )}
          </li>
          <li>
            {tr(
              "mcp_update_by_hand",
              undefined,
              "The app can't see Gemini CLI, Cursor, Claude Desktop or your own scripts. If the address here changes, or one stops connecting after an update, copy its setup again from Other agents and apps.",
            )}
          </li>
        </ul>
        {engineVersion && (
          <p className="text-xs text-[var(--color-muted)]">
            {tr(
              "mcp_update_version",
              { version: engineVersion },
              `This engine is version ${engineVersion}. To see what an agent is connected to, ask it which version the PS5Upload engine is.`,
            )}
          </p>
        )}
      </section>

      <section className="space-y-3">
        <h2 className="text-sm font-semibold">
          {tr("mcp_try_title", undefined, "Then try asking")}
        </h2>
        <div className="grid gap-2 lg:grid-cols-3">
          <CopyBlock
            plain
            text={tr(
              "mcp_try_status",
              undefined,
              "How hot is my PS5 right now?",
            )}
          />
          <CopyBlock
            plain
            text={tr(
              "mcp_try_space",
              undefined,
              "What's using the most space on my PS5?",
            )}
          />
          <CopyBlock
            plain
            text={tr(
              "mcp_try_install",
              undefined,
              "Install this package on my PS5 and tell me when it's done.",
            )}
          />
        </div>
      </section>

      <Callout
        tone="warn"
        title={tr(
          "mcp_safety_title",
          undefined,
          "An agent can do whatever you can",
        )}
      >
        <div className="space-y-2">
          <p>
            {tr(
              "mcp_safety_body",
              undefined,
              'There is no password, and it can delete files on your console. Keep tools that change things on "ask" in your agent, and read each console command before you allow it.',
            )}
          </p>
          <p>
            {tr(
              "mcp_safety_read_only",
              undefined,
              "To only let it look, set this in the engine's environment and restart the engine:",
            )}
          </p>
          <CopyBlock text="PS5UPLOAD_MCP_READ_ONLY=1" />
        </div>
      </Callout>

      <FoldSection
        id="mcp-other-clients"
        title={tr(
          "mcp_other_clients_title",
          undefined,
          "Other agents and apps",
        )}
      >
        <div className="space-y-4 pt-2">
          <CopyBlock
            label={tr(
              "mcp_gemini_label",
              undefined,
              "Gemini CLI: run this in a terminal",
            )}
            text={geminiCommand(url)}
          />
          <CopyBlock
            label={tr(
              "mcp_cursor_label",
              undefined,
              "Cursor: put this in ~/.cursor/mcp.json",
            )}
            text={cursorConfig(url)}
          />
          {bridgePath && (
            <CopyBlock
              label={tr(
                "mcp_claude_desktop_label",
                undefined,
                "Claude Desktop: Settings → Developer → Edit Config, then restart it",
              )}
              text={claudeDesktopConfig(bridgePath, engineUrl)}
            />
          )}
          <CopyBlock
            label={tr(
              "mcp_curl_label",
              undefined,
              "Anything else, or to test it by hand",
            )}
            text={curlToolsCommand(url)}
          />
          <Button
            variant="secondary"
            size="sm"
            onClick={() => void openExternalUrl(MCP_DOC)}
            rightIcon={<ExternalLink size={14} />}
          >
            {tr("mcp_full_guide", undefined, "Full MCP guide")}
          </Button>
        </div>
      </FoldSection>
    </div>
  );
}

function AgentCard({
  id,
  name,
  installUrl,
  url,
  desktop,
  checking,
  status,
  onChanged,
}: {
  id: AgentId;
  name: string;
  installUrl: string;
  url: string;
  desktop: boolean;
  checking: boolean;
  status?: AgentStatus;
  onChanged: (next: AgentStatus) => void;
}) {
  const tr = useTr();
  const [busy, setBusy] = useState<"add" | "remove" | null>(null);
  const [error, setError] = useState("");
  const state = status ? agentState(status) : null;

  const act = async (kind: "add" | "remove") => {
    setBusy(kind);
    setError("");
    try {
      onChanged(
        await invoke<AgentStatus>(
          kind === "add" ? "agent_mcp_add" : "agent_mcp_remove",
          { agent: id },
        ),
      );
    } catch (e) {
      setError(errorText(e));
    } finally {
      setBusy(null);
    }
  };

  // Where the engine really answers: the shell's address when it has one.
  const command = addCommand(id, status?.expected_url ?? url);

  return (
    <div
      className="flex min-w-0 flex-col gap-3 rounded-[var(--radius-panel)] border border-[var(--glass-edge)] bg-[var(--color-surface-raised)] p-4"
      data-testid={`mcp-agent-${id}`}
    >
      <div className="flex flex-wrap items-center justify-between gap-2">
        <h3 className="font-semibold">{name}</h3>
        {checking && (
          <Badge>{tr("mcp_state_checking", undefined, "Checking…")}</Badge>
        )}
        {(state === "added" || state === "bridge") && (
          <Badge tone="good" dot>
            {tr("mcp_state_added", undefined, "Added")}
          </Badge>
        )}
        {state === "not_added" && (
          <Badge>{tr("mcp_state_not_added", undefined, "Not added")}</Badge>
        )}
        {state === "no_cli" && (
          <Badge tone="warn">
            {tr("mcp_state_no_cli", undefined, "Not found on this computer")}
          </Badge>
        )}
        {state === "elsewhere" && (
          <Badge tone="warn">
            {tr("mcp_state_elsewhere", undefined, "Points elsewhere")}
          </Badge>
        )}
        {state === "unknown" && (
          <Badge tone="warn">
            {tr("mcp_state_unknown", undefined, "Couldn't check")}
          </Badge>
        )}
      </div>

      {state === "elsewhere" && (
        <p className="text-xs text-[var(--color-muted)]">
          {tr(
            "mcp_elsewhere_body",
            undefined,
            "Its saved entry isn't this engine's address, which can happen when the engine moves to another port. Update fixes it.",
          )}
        </p>
      )}

      {state === "bridge" && (
        <p className="text-xs text-[var(--color-muted)]">
          {tr(
            "mcp_bridge_body",
            undefined,
            "It runs the engine's bridge program instead of using this address. That works too. Update switches it to the address.",
          )}
        </p>
      )}

      {desktop &&
        (state === "not_added" ||
          state === "elsewhere" ||
          state === "bridge" ||
          state === "unknown") && (
          <Button
            variant="primary"
            size="sm"
            loading={busy === "add"}
            disabled={busy !== null}
            onClick={() => void act("add")}
            leftIcon={<Plus size={14} />}
          >
            {state === "elsewhere" || state === "bridge"
              ? tr("mcp_update", undefined, "Update")
              : tr("mcp_add_to", { name }, `Add to ${name}`)}
          </Button>
        )}
      {state === "no_cli" && (
        <Button
          variant="secondary"
          size="sm"
          onClick={() => void openExternalUrl(installUrl)}
          leftIcon={<Download size={14} />}
        >
          {tr("mcp_get_agent", { name }, `Get ${name}`)}
        </Button>
      )}
      {(state === "added" || state === "bridge") && status?.can_remove && (
        <Button
          variant="ghost"
          size="sm"
          loading={busy === "remove"}
          disabled={busy !== null}
          onClick={() => void act("remove")}
          leftIcon={<Trash2 size={14} />}
        >
          {tr("mcp_remove", undefined, "Remove")}
        </Button>
      )}
      {(state === "added" || state === "bridge") && status && !status.can_remove && (
        <p className="text-xs text-[var(--color-muted)]">
          {tr(
            "mcp_opencode_remove",
            undefined,
            'To remove it, delete the ps5upload entry under "mcp" in OpenCode\'s config file.',
          )}
        </p>
      )}

      {error && (
        <Callout
          tone="error"
          title={tr("mcp_change_failed", undefined, "That didn't work")}
        >
          <pre
            dir="ltr"
            className="whitespace-pre-wrap break-words text-start font-mono text-xs"
          >
            {error}
          </pre>
        </Callout>
      )}

      <div className="mt-auto space-y-2">
        <CopyBlock
          label={tr("mcp_by_hand", undefined, "Or run this in a terminal")}
          text={command}
        />
        <CopyBlock
          label={tr("mcp_check_label", undefined, "See that it connected")}
          text={checkCommand(id)}
        />
      </div>
    </div>
  );
}
