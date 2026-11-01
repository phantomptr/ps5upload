import { describe, expect, it } from "vitest";

import { DOUBLE_MS, TAP_MS, TouchpadGestures } from "./touchpadGestures";

function pad(speed = 1) {
  const log: string[] = [];
  const moves: Array<[number, number]> = [];
  const scrolls: Array<[number, number]> = [];
  const g = new TouchpadGestures(
    {
      move: (dx, dy) => moves.push([dx, dy]),
      button: (b, down) => log.push(`${b}${down ? "↓" : "↑"}`),
      scroll: (dy, dx) => scrolls.push([dy, dx]),
    },
    () => speed,
  );
  return { g, log, moves, scrolls };
}

describe("TouchpadGestures", () => {
  it("one finger drag moves the pointer, times the speed", () => {
    const { g, moves, log } = pad(2);
    g.down(1, 100, 100, 0);
    g.move(1, 105, 100); // inside the tap slop: nothing yet
    g.move(1, 120, 110);
    g.move(1, 130, 100);
    g.up(1, 500);
    expect(moves).toEqual([
      [30, 20],
      [20, -20],
    ]);
    expect(log).toEqual([]); // a drag is not a click
  });

  it("a tap is a left click; a slow press is nothing", () => {
    const { g, log } = pad();
    g.down(1, 50, 50, 0);
    g.up(1, TAP_MS - 10);
    expect(log).toEqual(["0↓", "0↑"]);
    log.length = 0;
    g.down(1, 300, 300, 5000);
    g.up(1, 5000 + TAP_MS + 200);
    expect(log).toEqual([]);
  });

  it("a two-finger tap is a right click", () => {
    const { g, log, moves } = pad();
    g.down(1, 50, 50, 0);
    g.down(2, 90, 50, 30);
    g.up(1, 150);
    g.up(2, 170);
    expect(log).toEqual(["2↓", "2↑"]);
    expect(moves).toEqual([]);
  });

  it("two-finger drag scrolls with the content (wheel the other way), and never moves", () => {
    const { g, scrolls, moves, log } = pad();
    g.down(1, 50, 200, 0);
    g.down(2, 90, 200, 10);
    g.move(1, 50, 180);
    g.move(2, 90, 180);
    g.move(1, 50, 160);
    g.up(1, 400);
    g.move(2, 90, 100); // the last finger lifting is not a move
    g.up(2, 420);
    expect(scrolls.length).toBeGreaterThan(0);
    expect(scrolls.every(([dy]) => dy > 0)).toBe(true); // fingers up = content up = wheel down
    expect(moves).toEqual([]);
    expect(log).toEqual([]);
  });

  it("tap then touch & drag holds the left button until the finger lifts", () => {
    const { g, log, moves } = pad();
    g.down(1, 100, 100, 0);
    g.up(1, 80);
    g.down(1, 104, 102, 80 + DOUBLE_MS - 50);
    expect(log).toEqual(["0↓", "0↑", "0↓"]);
    g.move(1, 150, 140);
    g.up(1, 2000);
    expect(log).toEqual(["0↓", "0↑", "0↓", "0↑"]);
    expect(moves.length).toBe(1);
  });

  it("two quick taps are a double click; a late second tap is two clicks", () => {
    const { g, log } = pad();
    g.down(1, 100, 100, 0);
    g.up(1, 60);
    g.down(1, 100, 100, 150);
    g.up(1, 200);
    expect(log).toEqual(["0↓", "0↑", "0↓", "0↑"]);
    log.length = 0;
    g.down(1, 100, 100, 5000);
    g.up(1, 5050);
    g.down(1, 100, 100, 5050 + DOUBLE_MS + 100);
    g.up(1, 5050 + DOUBLE_MS + 150);
    expect(log).toEqual(["0↓", "0↑", "0↓", "0↑"]);
  });

  it("cancel lets go of a held drag", () => {
    const { g, log } = pad();
    g.down(1, 100, 100, 0);
    g.up(1, 50);
    g.down(1, 100, 100, 100);
    g.cancel();
    expect(log).toEqual(["0↓", "0↑", "0↓", "0↑"]);
  });
});
