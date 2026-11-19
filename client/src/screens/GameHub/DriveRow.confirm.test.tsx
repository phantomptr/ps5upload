import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";

import { confirmClosesFor, confirmRunAllowed, ConfirmRow, packedWithin, rowBusyText } from "./DriveRow";

describe("game row: what the progress strip says", () => {
  it("only a download reads as one; Play, Register, Unregister, Heal and Edit have their own words", () => {
    expect(rowBusyText("download")[2]).toBe("Downloading from PS5");
    const others = (["launch", "register", "unregister", "heal", "edit-checkout"] as const).map(
      (b) => rowBusyText(b)[2],
    );
    expect(others).toEqual([
      "Starting…",
      "Registering…",
      "Unregistering",
      "Restoring home-screen files",
      "Getting the files ready to edit",
    ]);
  });
});

type Confirm = Parameters<typeof ConfirmRow>[0]["confirm"];
const confirm = { kind: "delete", entry: { name: "Dead Space", path: "/data/games/ds" } } as unknown as Confirm;
const row = (busy: boolean) =>
  renderToStaticMarkup(
    <ConfirmRow confirm={confirm} tied={false} busy={busy} onCancel={() => undefined} onRun={() => undefined} />,
  );
/** The "Yes, delete" button's tag. */
const yes = (html: string) => {
  const at = html.lastIndexOf("<button");
  return html.slice(at, html.indexOf(">", at));
};
/** The attribute, not the `disabled:` class names every button carries. */
const DISABLED = ' disabled=""';

describe("game row: Yes, delete", () => {
  it("can be pressed when nothing runs on the row", () => {
    expect(row(false)).toContain("Yes, delete");
    expect(yes(row(false))).not.toContain(DISABLED);
  });

  it("is disabled while the row is busy (a delete under way, or a Move)", () => {
    expect(yes(row(true))).toContain(DISABLED);
  });

  it("starts only when nothing runs and it is not already under way", () => {
    expect(confirmRunAllowed(null, false)).toBe(true);
    // The second press of a double press: `busy` has not rendered yet.
    expect(confirmRunAllowed(null, true)).toBe(false);
    expect(confirmRunAllowed("move-copying", false)).toBe(false);
    expect(confirmRunAllowed("delete", false)).toBe(false);
  });

  it("closes when another operation starts, not for its own", () => {
    expect(confirmClosesFor("move-copying")).toBe(true);
    expect(confirmClosesFor("download")).toBe(true);
    expect(confirmClosesFor("delete")).toBe(false);
    expect(confirmClosesFor("chmod")).toBe(false);
    expect(confirmClosesFor(null)).toBe(false);
  });
});

describe("Mount: the packed-image check", () => {
  it("passes on what the engine said", async () => {
    expect(await packedWithin(Promise.resolve("lz4"), 1000)).toBe("lz4");
    expect(await packedWithin(Promise.resolve(null), 1000)).toBeNull();
  });

  it("never holds the mount: a failed or slow check counts as not packed", async () => {
    expect(await packedWithin(Promise.reject(new Error("older engine: no such route")), 1000)).toBeNull();
    expect(await packedWithin(new Promise<string>(() => undefined), 10)).toBeNull();
  });
});
