import { describe, expect, it } from "vitest";
import { bindingForKey, createPadReader, DEFAULT_INPUT_MAP, nextFocus, textareaKeepsKey, type InputMap } from "./nav";

// Vitest runs in node here (no jsdom): rects are plain objects and the
// "elements" are tagged stand-ins, which is all nextFocus looks at.
function rect(left: number, top: number, width: number, height: number): DOMRect {
  return {
    left,
    top,
    width,
    height,
    right: left + width,
    bottom: top + height,
    x: left,
    y: top,
    toJSON: () => ({}),
  } as DOMRect;
}
const el = (name: string) => ({ name }) as unknown as HTMLElement;

describe("nextFocus", () => {
  it("nextFocus_prefers_aligned_neighbour", () => {
    const from = rect(100, 100, 200, 50);
    const aligned = el("aligned"); // straight below, further away
    const diagonal = el("diagonal"); // closer, but off to the right
    const above = el("above");
    const got = nextFocus(
      from,
      [
        { el: diagonal, rect: rect(320, 160, 200, 50) },
        { el: aligned, rect: rect(100, 240, 200, 50) },
        { el: above, rect: rect(100, 20, 200, 50) },
      ],
      "down",
    );
    expect(got).toBe(aligned);
  });

  it("nextFocus_none_beyond_edge", () => {
    const from = rect(100, 100, 200, 50);
    const got = nextFocus(
      from,
      [
        { el: el("left"), rect: rect(0, 100, 80, 50) },
        { el: el("above"), rect: rect(100, 20, 200, 50) },
      ],
      "right",
    );
    expect(got).toBeNull();
  });

  it("falls back to the nearest off-axis candidate when nothing is aligned", () => {
    const from = rect(400, 500, 100, 40); // a row below the last rail item
    const near = el("near");
    const got = nextFocus(
      from,
      [
        { el: el("far"), rect: rect(10, 10, 80, 80) },
        { el: near, rect: rect(10, 300, 80, 80) },
      ],
      "left",
    );
    expect(got).toBe(near);
  });
});

describe("gamepad reader", () => {
  const press = (...idx: number[]) => {
    const b: boolean[] = new Array(17).fill(false);
    for (const i of idx) b[i] = true;
    return b;
  };

  it("fires a face button once per press (edge-triggered)", () => {
    const read = createPadReader(DEFAULT_INPUT_MAP);
    expect(read(press(0), 0)).toEqual(["confirm"]);
    expect(read(press(0), 16)).toEqual([]);
    expect(read(press(0), 900)).toEqual([]);
    expect(read(press(), 916)).toEqual([]);
    expect(read(press(0), 932)).toEqual(["confirm"]);
  });

  it("repeats a held D-pad direction after the delay, then at the repeat rate", () => {
    const read = createPadReader(DEFAULT_INPUT_MAP);
    expect(read(press(13), 0)).toEqual(["down"]);
    expect(read(press(13), 399)).toEqual([]);
    expect(read(press(13), 400)).toEqual(["down"]);
    expect(read(press(13), 519)).toEqual([]);
    expect(read(press(13), 520)).toEqual(["down"]);
    expect(read(press(), 600)).toEqual([]);
  });

  it("ignores buttons the map does not name", () => {
    const map: InputMap = [{ button: "circle", glyph: "○", action: "back", keys: [], keyCodes: [], pad: 1, verified: false }];
    const read = createPadReader(map);
    expect(read(press(0, 1), 0)).toEqual(["back"]);
  });
});

describe("default input map", () => {
  const entry = (b: string) => DEFAULT_INPUT_MAP.find((x) => x.button === b);

  it("covers every DualSense button, each unverified until measured", () => {
    const want = [
      "cross", "circle", "triangle", "square", "up", "down", "left", "right", "lstick", "rstick",
      "l1", "r1", "l2", "r2", "l3", "r3", "options", "create", "touchpad", "ps", "mute",
    ];
    expect(DEFAULT_INPUT_MAP.map((x) => x.button).sort()).toEqual([...want].sort());
    expect(DEFAULT_INPUT_MAP.every((x) => x.verified === false)).toBe(true);
  });

  it("wires each button to its action", () => {
    const actions: Record<string, string> = {
      cross: "confirm", circle: "back", triangle: "actions", square: "select",
      up: "up", down: "down", left: "left", right: "right", lstick: "pointer", rstick: "scroll",
      l1: "prevSection", r1: "nextSection", l2: "pageUp", r2: "pageDown", l3: "top", r3: "rail",
      options: "help", create: "refresh", touchpad: "fullscreen", ps: "system", mute: "system",
    };
    for (const [b, a] of Object.entries(actions)) expect(entry(b)?.action, b).toBe(a);
  });

  it("seeds the standard keys: Enter, Escape/Backspace, arrows, PageUp/PageDown, Home, ContextMenu, F5", () => {
    expect(entry("cross")?.keys).toContain("Enter");
    expect(entry("circle")?.keys).toEqual(expect.arrayContaining(["Escape", "Backspace"]));
    expect(entry("up")?.keys).toContain("ArrowUp");
    expect(entry("l2")?.keys).toContain("PageUp");
    expect(entry("r2")?.keys).toContain("PageDown");
    expect(entry("l3")?.keys).toContain("Home");
    expect(entry("triangle")?.keys).toContain("ContextMenu");
    expect(entry("create")?.keys).toContain("F5");
    expect(entry("cross")?.keyCodes).toContain(13);
    // The PS button and Mute never reach the page; nothing listens for them.
    expect(entry("ps")?.keys).toEqual([]);
    expect(entry("mute")?.keys).toEqual([]);
  });

  it("bindingForKey matches by key, then by keyCode when the key is not named", () => {
    expect(bindingForKey(DEFAULT_INPUT_MAP, "Enter", 0)?.action).toBe("confirm");
    expect(bindingForKey(DEFAULT_INPUT_MAP, "Unidentified", 27)?.action).toBe("back");
    expect(bindingForKey(DEFAULT_INPUT_MAP, "x", 88)).toBeNull();
  });

  it("R18: unverified seeded keys are inert, the always-live set is not", () => {
    for (const k of ["ArrowUp", "ArrowDown", "ArrowLeft", "ArrowRight", "Enter", "Escape", "Backspace"]) {
      expect(bindingForKey(DEFAULT_INPUT_MAP, k, 0), k).not.toBeNull();
    }
    for (const [k, code] of [["F5", 116], ["Home", 36], ["PageUp", 33], ["PageDown", 34], ["ContextMenu", 93], ["F1", 112], ["F11", 122]] as const) {
      expect(bindingForKey(DEFAULT_INPUT_MAP, k, code), k).toBeNull();
      expect(bindingForKey(DEFAULT_INPUT_MAP, "Unidentified", code), `${k} by keyCode`).toBeNull();
    }
    // Once measured, the entry acts.
    const map = DEFAULT_INPUT_MAP.map((b) => (b.button === "create" ? { ...b, verified: true } : b));
    expect(bindingForKey(map, "F5", 116)?.action).toBe("refresh");
  });

  it("R18: unverified gamepad buttons outside the old set are inert", () => {
    const b: boolean[] = new Array(17).fill(false);
    b[6] = b[7] = b[8] = b[9] = b[10] = b[11] = true;
    expect(createPadReader(DEFAULT_INPUT_MAP)(b, 0)).toEqual([]);
  });
});

describe("text editor keys", () => {
  const box = (value: string, at: number) => ({ tagName: "TEXTAREA", value, selectionStart: at, selectionEnd: at });
  it("keeps up/down inside a multi-line box until the first/last line", () => {
    expect(textareaKeepsKey(box("a\nb\nc", 0), "up")).toBe(false);
    expect(textareaKeepsKey(box("\nb", 0), "up")).toBe(false);
    expect(textareaKeepsKey(box("a\nb\nc", 2), "up")).toBe(true);
    expect(textareaKeepsKey(box("a\nb\nc", 2), "down")).toBe(true);
    expect(textareaKeepsKey(box("a\nb\nc", 4), "down")).toBe(false);
    expect(textareaKeepsKey(box("one line", 3), "down")).toBe(false);
    expect(textareaKeepsKey({ tagName: "INPUT", value: "a\nb", selectionStart: 2, selectionEnd: 2 }, "up")).toBe(false);
    expect(textareaKeepsKey(box("a\nb", 2), "left")).toBe(false);
  });
});
