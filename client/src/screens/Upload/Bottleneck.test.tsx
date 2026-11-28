import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it, vi } from "vitest";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));

import { BottleneckLine, JobLiveNotes, LinkLine, UnsettledLine } from "./Bottleneck";
import type { BottleneckCause, JobLive } from "../../lib/jobLive";

const live = (over: Partial<JobLive>): JobLive => ({
  skipping: false,
  skipDoneBytes: 0,
  skipTotalBytes: 0,
  bottleneck: null,
  settling: false,
  ...over,
});

describe("bottleneck_line_renders_each_cause", () => {
  const cases: Array<[BottleneckCause, string]> = [
    ["network", "Limited by: network"],
    ["source", "Limited by: reading the source"],
    ["disk", "Limited by: console disk"],
    ["workers", "Limited by: console workers"],
    ["memory", "Limited by: console memory"],
  ];
  it.each(cases)("%s", (cause, text) => {
    expect(renderToStaticMarkup(<BottleneckLine cause={cause} />)).toContain(text);
  });
  it("renders nothing without a cause", () => {
    expect(renderToStaticMarkup(<BottleneckLine cause={null} />)).toBe("");
  });
});

describe("skipping_phase_renders", () => {
  it("says what is being skipped, with both byte counts", () => {
    const html = renderToStaticMarkup(
      <JobLiveNotes
        live={live({ skipping: true, skipDoneBytes: 1024 ** 3, skipTotalBytes: 4 * 1024 ** 3 })}
      />,
    );
    expect(html).toContain("Skipping data the console already has");
    expect(html).toContain("1.00 GiB");
    expect(html).toContain("4.00 GiB");
    expect(html).toContain('data-testid="skipping-line"');
  });

  it("shows nothing when the engine sent no live fields", () => {
    expect(renderToStaticMarkup(<JobLiveNotes live={undefined} />)).toBe("");
  });
});

describe("finishing on the console", () => {
  it("says how many files are left when the engine sends the counts", () => {
    const html = renderToStaticMarkup(
      <JobLiveNotes live={live({ settling: true, settleLeft: 1200, settleTotal: 5000 })} />,
    );
    expect(html).toContain("Finishing on the console…");
    expect(html).toContain('data-testid="settling-count"');
    expect(html).toMatch(/1,?200 of 5,?000 files left/);
  });

  it("shows no count without them (an engine that does not send them)", () => {
    const html = renderToStaticMarkup(<JobLiveNotes live={live({ settling: true })} />);
    expect(html).not.toContain("settling-count");
  });

  it("shows only when settling is set", () => {
    expect(renderToStaticMarkup(<JobLiveNotes live={live({ settling: true })} />)).toContain(
      "Finishing on the console…",
    );
    expect(
      renderToStaticMarkup(<JobLiveNotes live={live({ bottleneck: "network" })} />),
    ).not.toContain("Finishing on the console");
  });
});

describe("a console that has every byte and reports nothing new (#873)", () => {
  it("says for how long, beside what it was last doing", () => {
    const html = renderToStaticMarkup(
      <JobLiveNotes live={live({ verifying: true, quietMs: 14 * 60_000 })} />,
    );
    expect(html).toContain("Verifying on the PS5…");
    expect(html).toContain('data-testid="console-quiet-line"');
    expect(html).toContain("The PS5 has reported no progress for 14 min.");
  });
  it("says how much of the read-back is done when the helper reports it", () => {
    const GiB = 1024 ** 3;
    const html = renderToStaticMarkup(
      <JobLiveNotes
        live={live({ verifying: true, verifyLeft: 100 * GiB, verifyTotal: 156 * GiB })}
      />,
    );
    expect(html).toContain('data-testid="verifying-count"');
    expect(html).toContain("Checking on the PS5: 56.0 GiB of 156 GiB");
    expect(html).not.toContain("Verifying on the PS5…");
    expect(html).not.toContain("console-quiet-line");
  });
  it("says nothing while the console is getting on with it", () => {
    expect(
      renderToStaticMarkup(<JobLiveNotes live={live({ verifying: true })} />),
    ).not.toContain("console-quiet-line");
  });
});

describe("a finished upload the console did not confirm", () => {
  it("is a warning in the result, not a clean success", () => {
    const html = renderToStaticMarkup(<UnsettledLine live={live({ unsettled: true })} />);
    expect(html).toContain('data-testid="unsettled-warning"');
    expect(html).toContain("has not confirmed saving all files");
  });
  it("shows nothing for a clean finish or no notes", () => {
    expect(renderToStaticMarkup(<UnsettledLine live={live({})} />)).toBe("");
    expect(renderToStaticMarkup(<UnsettledLine live={undefined} />)).toBe("");
  });
});

describe("link_line_says_which_network", () => {
  const cases: Array<[number | undefined, string]> = [
    [2500, "Via USB 2.5G adapter"],
    [5000, "Via USB 5G adapter"],
    [10000, "Via USB 10G adapter"],
    [1000, "Via USB 1G adapter"],
    [100, "Via USB 100M adapter"],
    [undefined, "Via USB adapter"],
  ];
  it.each(cases)("usb at %s Mbit/s", (mbps, text) => {
    const html = renderToStaticMarkup(<LinkLine live={live({ link: "usb", linkMbps: mbps })} />);
    expect(html).toContain('data-testid="queue-link"');
    expect(html).toContain(text);
  });
  it("names the console's own network", () => {
    expect(renderToStaticMarkup(<LinkLine live={live({ link: "console" })} />)).toContain("Via console network");
  });
  it("shows nothing for a console without an adapter", () => {
    expect(renderToStaticMarkup(<LinkLine live={live({})} />)).toBe("");
    expect(renderToStaticMarkup(<LinkLine live={undefined} />)).toBe("");
  });
  it("is part of the live notes the queue row and the transfer view show", () => {
    const html = renderToStaticMarkup(
      <JobLiveNotes live={live({ link: "usb", linkMbps: 2500, bottleneck: "network" })} />,
    );
    expect(html).toContain("Via USB 2.5G adapter");
    expect(html).toContain("Limited by: network");
  });
});
