import { renderToStaticMarkup } from "react-dom/server";
import { beforeEach, describe, expect, it } from "vitest";

import { STALL_MS } from "../../lib/transferRate";
import { t } from "../strings";
import { RateLine, _resetRates, rateText, trackJob } from "./rate";

const MiB = 2 ** 20;

beforeEach(() => _resetRates());

describe("rateText (#981)", () => {
  it("speed, elapsed time and time left, in that order", () => {
    const text = rateText({ bytesPerSec: 12 * MiB, etaSeconds: 185, stalled: false, elapsedSeconds: 65 }, 65);
    expect(text).toBe(
      ["12.0 MiB/s", t("cw_rate_elapsed", { time: "1m 5s" }), t("cw_rate_left", { time: "3m" })].join(" · "),
    );
  });

  it("stalled replaces the speed and the time left; elapsed stays", () => {
    const text = rateText({ bytesPerSec: 0, etaSeconds: null, stalled: true, elapsedSeconds: 300 }, 300);
    expect(text).toBe(`${t("cw_rate_stalled")} · ${t("cw_rate_elapsed", { time: "5m 0s" })}`);
  });

  it("no elapsed when the start is not known; nothing at all before there is anything", () => {
    expect(rateText({ bytesPerSec: MiB, etaSeconds: null, stalled: false, elapsedSeconds: 9 })).toBe("1.00 MiB/s");
    expect(rateText(null)).toBe("");
    expect(rateText({ bytesPerSec: 0, etaSeconds: null, stalled: false, elapsedSeconds: 0 }, 0)).toBe("");
  });
});

describe("trackJob", () => {
  it("keeps a job's window between reads (a section opened again keeps its speed)", () => {
    trackJob("j", 0, 0, 100 * MiB);
    for (let s = 1; s <= 4; s++) trackJob("j", s * 1000, s * 2 * MiB, 100 * MiB);
    const v = trackJob("j", 5000, 10 * MiB, 100 * MiB);
    expect(v.bytesPerSec).toBeCloseTo(2 * MiB);
    expect(v.etaSeconds).toBeCloseTo(45);
    expect(trackJob("j", 5000 + STALL_MS, 10 * MiB, 100 * MiB).stalled).toBe(true);
  });

  it("an unseen job is forgotten after a while", () => {
    trackJob("old", 0, 0, 10 * MiB);
    trackJob("old", 1000, MiB, 10 * MiB);
    // Another job read much later sweeps it; "old" starts again from scratch.
    trackJob("new", 11 * 60_000, 0, 10 * MiB);
    expect(trackJob("old", 11 * 60_000 + 1000, 2 * MiB, 10 * MiB).bytesPerSec).toBe(0);
  });
});

describe("RateLine", () => {
  it("draws nothing before its first read, and an elapsed time the helper gave", () => {
    expect(renderToStaticMarkup(<RateLine id="x" done={0} total={0} />)).toBe("");
    // The first render has no speed yet; the elapsed time is the helper's.
    expect(renderToStaticMarkup(<RateLine id="y" done={0} total={10} elapsedS={125} prefix=" · " />)).toContain(
      ` · ${t("cw_rate_elapsed", { time: "2m 5s" })}`,
    );
  });
});
