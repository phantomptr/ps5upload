import { describe, expect, it } from "vitest";
import { renderToStaticMarkup } from "react-dom/server";

import { IntervalChoice } from "./AutoBackupCard";
import { intervalChoices } from "../../state/autoSaveBackup";

const noop = () => {};

describe("automatic backups: how often", () => {
  it("offers 5 minutes to a day, with what the engine has in its place", () => {
    expect(intervalChoices(10)).toEqual([5, 10, 30, 60, 360, 1440]);
    // An engine before the choice sends none: it backs up every 10 minutes.
    expect(intervalChoices(undefined)).toEqual([5, 10, 30, 60, 360, 1440]);
    // One set through the API stays listed, in order.
    expect(intervalChoices(45)).toEqual([5, 10, 30, 45, 60, 360, 1440]);
  });

  it("names each choice and selects the engine's", () => {
    const html = renderToStaticMarkup(<IntervalChoice minutes={60} onChange={noop} />);
    expect(html).toContain("Back up every");
    for (const label of ["5 min", "10 min", "30 min", "1 hour", "6 hours", "1 day"]) {
      expect(html).toContain(`>${label}</option>`);
    }
    expect(html).toMatch(/<option value="60" selected="">1 hour<\/option>/);
    // No setting yet: 10 minutes, what the engine does.
    expect(renderToStaticMarkup(<IntervalChoice minutes={undefined} onChange={noop} />)).toMatch(
      /<option value="10" selected="">10 min<\/option>/,
    );
    expect(renderToStaticMarkup(<IntervalChoice minutes={45} onChange={noop} />)).toMatch(
      /<option value="45" selected="">45 min<\/option>/,
    );
  });
});
