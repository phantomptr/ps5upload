import { isValidElement, type ReactElement, type ReactNode } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it, vi } from "vitest";

vi.mock("../../state/lang", async (orig) => ({
  ...(await orig<typeof import("../../state/lang")>()),
  useTr: () => (key: string, _vars?: unknown, fallback?: string) => fallback ?? key,
}));

const { MinionView, RuntimePutView, amprEmuPutText, minionStatus, minionUrl } = await import("./MinionSettings");
const { Toggle } = await import("../../components");

const tr = (key: string, _vars?: unknown, fallback?: string) => fallback ?? key;
const state = (over: Record<string, unknown> = {}) => ({
  supported: true as const,
  tile: true,
  tile_busy: false,
  tile_failed: false,
  tile_result: "",
  enabled: true,
  ...over,
});

/** The elements MinionView returns (its own tree; child components are not rendered). */
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

describe("Settings → Minion", () => {
  it("shows one on/off switch, with the tile buttons under it only while on", () => {
    const on = renderToStaticMarkup(
      <MinionView up state={state()} busy={false} onApply={() => {}} />,
    );
    expect(on).toContain('role="switch"');
    expect(on).toContain('aria-checked="true"');
    expect(on).toContain("PS5Upload Minion: On");
    expect(on).toContain("Show / restore Minion");
    expect(on).toContain("Off removes the home-screen tile");

    const off = renderToStaticMarkup(
      <MinionView up state={state({ enabled: false })} busy={false} onApply={() => {}} />,
    );
    expect(off).toContain('aria-checked="false"');
    expect(off).toContain("PS5Upload Minion: Off");
    expect(off).not.toContain("Show / restore Minion");
    expect(off).toContain("Minion is off: its home-screen tile is removed");
  });

  it("the switch sends enabled, and is locked while the helper works", () => {
    const onApply = vi.fn();
    const view = MinionView({ up: true, state: state(), busy: false, onApply });
    const toggle = elements(view).find((e) => e.type === Toggle);
    expect(toggle).toBeTruthy();
    (toggle?.props.onChange as (v: boolean) => void)(false);
    expect(onApply).toHaveBeenCalledWith({ enabled: false });

    const busy = MinionView({ up: true, state: state({ tile_busy: true }), busy: false, onApply });
    expect(elements(busy).find((e) => e.type === Toggle)?.props.disabled).toBe(true);
  });

  it("an older helper keeps the tile buttons and asks for an update instead of a switch", () => {
    const old = { supported: true as const, tile: true, tile_busy: false, tile_failed: false };
    const html = renderToStaticMarkup(
      <MinionView up state={old} busy={false} onApply={() => {}} />,
    );
    expect(html).not.toContain('role="switch"');
    expect(html).toContain("Update the helper to turn PS5Upload Minion off completely.");
    expect(html).toContain("Hide Minion");
  });

  it("says what turning off did to the tile: removed, hidden or failed", () => {
    expect(minionStatus(state({ enabled: false, tile_busy: true }), tr)).toBe(
      "Updating the home screen…",
    );
    expect(minionStatus(state({ enabled: false, tile_result: "removed" }), tr)).toContain(
      "tile is removed",
    );
    expect(minionStatus(state({ enabled: false, tile_result: "hidden" }), tr)).toContain(
      "icon is hidden instead",
    );
    expect(
      minionStatus(state({ enabled: false, tile_failed: true, tile_result: "failed" }), tr),
    ).toContain("could not be removed");
    expect(minionStatus(state(), tr)).toBe("The home-screen tile is shown.");
    expect(minionStatus(state({ tile: false }), tr)).toBe("The home-screen tile is hidden.");
  });
  it("says why the tile could not be added, also when the helper tried at its start", () => {
    const fill = (key: string, vars?: Record<string, unknown>, fallback?: string) =>
      (fallback ?? key).replace(/\{(\w+)\}/g, (_, k: string) => String(vars?.[k] ?? ""));
    expect(
      minionStatus(
        state({ tile_failed: true, tile_result: "failed", tile_error: "not_listed" }),
        fill as unknown as typeof tr,
      ),
    ).toBe("Could not add PS5Upload Minion to the home screen (not_listed). Try again.");
    // An older helper sends no reason: the plain message.
    expect(minionStatus(state({ tile_failed: true, tile_result: "failed" }), tr)).toBe(
      "The home screen update failed. Try again.",
    );
  });
  it("a current helper offers network access and Remove; an older one neither", () => {
    const html = renderToStaticMarkup(
      <MinionView
        up
        host="192.168.1.9"
        state={state({ lan: true, saved: true, port: 9180 })}
        busy={false}
        onApply={() => {}}
      />,
    );
    expect(html).toContain("Open Minion from other devices on your network");
    expect(html).toContain("http://192.168.1.9:9180/");
    expect(html).toContain("Remove Minion from this PS5");
    const old = renderToStaticMarkup(
      <MinionView up state={state()} busy={false} onApply={() => {}} />,
    );
    expect(old).not.toContain("other devices on your network");
    expect(old).not.toContain("Remove Minion");
  });

  it("Remove is offered while off too, and says what it deleted", () => {
    const onRemove = vi.fn();
    const off = state({ enabled: false, lan: false, saved: true, removed: 2 });
    const html = renderToStaticMarkup(
      <MinionView up state={off} busy={false} onApply={() => {}} onRemove={onRemove} />,
    );
    expect(html).toContain("of its files on the PS5 were deleted");
    expect(html).not.toContain("other devices on your network");
    const view = MinionView({ up: true, state: off, busy: false, onApply: () => {}, onRemove });
    const remove = elements(view).find(
      (e) => typeof e.props.onClick === "function" && e.props.variant === "danger",
    );
    (remove?.props.onClick as () => void)();
    expect(onRemove).toHaveBeenCalled();
  });

  it("offers the AMPR emulator for Convert on the PS5 as a button of its own, and says how it went", () => {
    const text = amprEmuPutText(tr);
    expect(text.label).toBe("Put the AMPR emulator on this PS5");
    expect(text.note).toContain("only when you ask");
    expect(text.doneText("0.4.2.1")).toBe("ampr_emu {version} is on this PS5.");
    const onPut = vi.fn();
    const view = RuntimePutView({ ...text, busy: false, done: null, error: null, onPut });
    const html = renderToStaticMarkup(view);
    expect(html).toContain("Put the AMPR emulator on this PS5");
    expect(html).toContain('data-runtime-put="ampr-emu"');
    expect(html).not.toContain('role="status"');
    expect(html).not.toContain('role="alert"');
    const button = elements(view).find((e) => typeof e.props.onClick === "function");
    (button?.props.onClick as () => void)();
    expect(onPut).toHaveBeenCalledTimes(1);

    const done = renderToStaticMarkup(
      RuntimePutView({ ...text, busy: false, done: "ampr_emu 0.4.2.1 is on this PS5.", error: null, onPut }),
    );
    expect(done).toContain('role="status"');
    expect(done).toContain("ampr_emu 0.4.2.1 is on this PS5.");
    const failed = RuntimePutView({ ...text, busy: true, done: null, error: "offline", onPut });
    expect(renderToStaticMarkup(failed)).toContain('role="alert"');
    expect(elements(failed).find((e) => typeof e.props.onClick === "function")?.props.disabled).toBe(true);
  });

  it("builds the network address, IPv6 in brackets", () => {
    expect(minionUrl("10.0.0.2:9113")).toBe("http://10.0.0.2:9180/");
    expect(minionUrl("[fe80::1]:9113", 9181)).toBe("http://[fe80::1]:9181/");
  });
});

