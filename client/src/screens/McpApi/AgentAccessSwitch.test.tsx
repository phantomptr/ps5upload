import { isValidElement, type ReactElement, type ReactNode } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import { afterEach, describe, expect, it, vi } from "vitest";

import { Toggle } from "../../components";
import type { Translator } from "../../state/lang";
import { AgentAccessView } from "./AgentAccessSwitch";
import { readAgentAccess, writeAgentAccess, type AgentAccess } from "./model";

const tr = ((key: string, vars?: Record<string, unknown>, fallback?: string) => {
  let text = fallback ?? key;
  for (const [k, v] of Object.entries(vars ?? {})) text = text.replace(`{${k}}`, String(v));
  return text;
}) as Translator;

const access = (over: Partial<AgentAccess> = {}): AgentAccess => ({
  read_only: false,
  setting: false,
  locked: false,
  ...over,
});

function elements(node: ReactNode): ReactElement<Record<string, unknown>>[] {
  const out: ReactElement<Record<string, unknown>>[] = [];
  const walk = (n: ReactNode) => {
    if (Array.isArray(n)) return n.forEach(walk);
    if (!isValidElement(n)) return;
    const el = n as ReactElement<Record<string, unknown>>;
    out.push(el);
    walk(el.props.children as ReactNode);
  };
  walk(node);
  return out;
}

const toggleOf = (node: ReactNode) => elements(node).find((e) => e.type === Toggle)!;

describe("MCP & API → Agents can change things", () => {
  it("is on for full access and off when agents may only look", () => {
    const on = renderToStaticMarkup(
      <AgentAccessView tr={tr} access={access()} busy={false} error="" onChange={() => {}} />,
    );
    expect(on).toContain("Agents can change things");
    expect(on).toContain('aria-checked="true"');
    expect(on).toContain("agents can only look");
    expect(on).toContain("could still reach the engine");
    expect(on).not.toContain("set by the engine&#x27;s environment");

    const off = renderToStaticMarkup(
      <AgentAccessView
        tr={tr}
        access={access({ read_only: true, setting: true })}
        busy={false}
        error=""
        onChange={() => {}}
      />,
    );
    expect(off).toContain('aria-checked="false"');
  });

  it("is off and cannot be switched when the engine's environment sets it", () => {
    const view = AgentAccessView({
      tr,
      access: access({ read_only: true, locked: true }),
      busy: false,
      error: "",
      onChange: () => {},
    });
    const toggle = toggleOf(view);
    expect(toggle.props.checked).toBe(false);
    expect(toggle.props.disabled).toBe(true);
    expect(renderToStaticMarkup(view)).toContain("Off, set by the engine&#x27;s environment.");
  });

  it("waits for the engine, and holds still while saving", () => {
    const unknown = AgentAccessView({ tr, access: null, busy: false, error: "", onChange: () => {} });
    expect(toggleOf(unknown).props.disabled).toBe(true);
    const saving = AgentAccessView({ tr, access: access(), busy: true, error: "", onChange: () => {} });
    expect(toggleOf(saving).props.disabled).toBe(true);
  });

  it("hands the switch's new position on, and shows a failure", () => {
    const onChange = vi.fn();
    const view = AgentAccessView({ tr, access: access(), busy: false, error: "", onChange });
    expect(toggleOf(view).props.disabled).toBe(false);
    (toggleOf(view).props.onChange as (v: boolean) => void)(false);
    expect(onChange).toHaveBeenCalledWith(false);

    const failed = renderToStaticMarkup(
      <AgentAccessView tr={tr} access={access()} busy={false} error="disk full" onChange={() => {}} />,
    );
    expect(failed).toContain("Couldn&#x27;t change it: disk full");
  });
});

describe("the engine's /api/mcp/access", () => {
  afterEach(() => vi.unstubAllGlobals());

  it("reads the switch and whether the environment locks it", async () => {
    const fetch = vi.fn(async (_url: string) =>
      new Response(JSON.stringify({ read_only: true, setting: false, locked: true })),
    );
    vi.stubGlobal("fetch", fetch);
    await expect(readAgentAccess("http://127.0.0.1:19113/")).resolves.toEqual({
      read_only: true,
      setting: false,
      locked: true,
    });
    expect(fetch.mock.calls[0][0]).toBe("http://127.0.0.1:19113/api/mcp/access");
  });

  it("saves the switch as read_only, the opposite of 'can change things'", async () => {
    const fetch = vi.fn(async (_url: string, init?: RequestInit) => {
      const body = JSON.parse(String(init?.body)) as { read_only: boolean };
      return new Response(
        JSON.stringify({ read_only: body.read_only, setting: body.read_only, locked: false }),
      );
    });
    vi.stubGlobal("fetch", fetch);
    const saved = await writeAgentAccess("http://127.0.0.1:19113", true);
    expect(saved.read_only).toBe(true);
    const [url, init] = fetch.mock.calls[0];
    expect(url).toBe("http://127.0.0.1:19113/api/mcp/access");
    expect(init?.method).toBe("PUT");
    expect(JSON.parse(String(init?.body))).toEqual({ read_only: true });
  });

  it("turns an engine's refusal or an older engine into an error", async () => {
    vi.stubGlobal(
      "fetch",
      vi.fn(async () => new Response(JSON.stringify({ error: "disk full" }), { status: 500 })),
    );
    await expect(writeAgentAccess("http://e", false)).rejects.toThrow("disk full");
    vi.stubGlobal("fetch", vi.fn(async () => new Response("not found", { status: 404 })));
    await expect(readAgentAccess("http://e")).rejects.toThrow("HTTP 404");
    vi.stubGlobal("fetch", vi.fn(async () => new Response(JSON.stringify({}))));
    await expect(readAgentAccess("http://e")).rejects.toThrow();
  });
});
