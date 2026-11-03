import { isValidElement, type ReactElement, type ReactNode } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import { beforeEach, describe, expect, it, vi } from "vitest";

vi.mock("../api", async (importOriginal) => {
  const real = await importOriginal<typeof import("../api")>();
  return { ...real, call: vi.fn() };
});

import { ApiError, call } from "../api";
import { t } from "../strings";
import { ConfirmDialog, dialogNavHandler } from "../ui/ConfirmDialog";
import {
  errorText,
  keepsDialogOpen,
  HomeView,
  MAX_TASK_ROWS,
  newUsbStick,
  runAsk,
  VolumeCard,
  type Ask,
  type HomeViewProps,
  type Volume,
} from "./Home";
import { TaskRow } from "./TaskRow";
import { cancelJob, type Job } from "./useJobs";

const mockCall = vi.mocked(call);

const job = (over: Partial<Job> = {}): Job => ({
  job: "0123456789abcdef0123456789abcdef",
  kind: "copy",
  state: "running",
  files_done: 1,
  files_total: 4,
  bytes_done: 100,
  bytes_total: 400,
  mine: true,
  ...over,
});

const vol = (path: string, mount_from: string, over: Partial<Volume> = {}): Volume => ({
  path,
  mount_from,
  fs_type: "ufs",
  total_bytes: 1000,
  free_bytes: 250,
  writable: true,
  ...over,
});

/** Every element in `node`'s tree (function components are not expanded). */
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

const byAction = (node: ReactNode, action: string) => {
  const el = elements(node).find((e) => e.props["data-action"] === action);
  if (!el) throw new Error(`no element with data-action=${action}`);
  return el;
};

function viewProps(over: Partial<HomeViewProps> = {}): HomeViewProps {
  return {
    status: null,
    volumes: [],
    volumesError: false,
    jobs: [],
    jobsLoaded: true,
    jobsError: false,
    ask: null,
    busy: false,
    message: null,
    onAsk: vi.fn(),
    onConfirm: vi.fn(),
    onDismiss: vi.fn(),
    onOpenDrive: null,
    ...over,
  };
}

beforeEach(() => {
  mockCall.mockReset();
  mockCall.mockResolvedValue({});
});

describe("Home", () => {
  it("shows_pc_job_as_from_pc", () => {
    expect(t("cw_task_from_pc")).not.toBe("cw_task_from_pc");
    const pc = renderToStaticMarkup(<TaskRow job={job({ mine: false })} onCancel={() => {}} />);
    expect(pc).toContain(t("cw_task_from_pc"));
    expect(pc).not.toContain(t("cw_task_from_console"));
    const mine = renderToStaticMarkup(<TaskRow job={job({ mine: true })} onCancel={() => {}} />);
    expect(mine).toContain(t("cw_task_from_console"));
  });

  it("names what a network-share copy copies", () => {
    const row = renderToStaticMarkup(<TaskRow job={job({ job: "smb:3", name: "PPSA01234" })} onCancel={() => {}} />);
    expect(row).toContain(" · PPSA01234");
    expect(renderToStaticMarkup(<TaskRow job={job({})} onCancel={() => {}} />)).not.toContain("PPSA01234");
  });

  it("cancel_asks_confirm_then_calls_api", async () => {
    const running = job({ mine: false });
    // Cancel on the row only asks.
    const onCancel = vi.fn();
    (byAction(TaskRow({ job: running, onCancel }), "cancel").props.onClick as () => void)();
    expect(onCancel).toHaveBeenCalledWith(running);
    expect(mockCall).not.toHaveBeenCalled();

    // The ask opens the confirm dialog; focus starts on the safe choice.
    const ask: Ask = { kind: "cancel", job: running };
    const props = viewProps({ ask, jobs: [running] });
    const html = renderToStaticMarkup(<HomeView {...props} />);
    expect(html).toContain("data-nav-modal");
    expect(html).toContain(t("cw_home_confirm_cancel_title"));
    const dialog = elements(HomeView(props)).find((e) => e.type === ConfirmDialog);
    expect(dialog).toBeDefined();
    expect(mockCall).not.toHaveBeenCalled();

    // "Keep going" (or ○) closes it with no call.
    (dialog!.props.onCancel as () => void)();
    expect(props.onDismiss).toHaveBeenCalled();
    expect(mockCall).not.toHaveBeenCalled();

    // Confirm runs the ask: POST /api/jobs/cancel {job}.
    (dialog!.props.onConfirm as () => void)();
    expect(props.onConfirm).toHaveBeenCalled();
    await runAsk(ask, { cancel: cancelJob });
    expect(mockCall).toHaveBeenCalledWith("/api/jobs/cancel", {
      job: running.job,
    });
  });

  it("Tasks hides finished PC jobs, shows running PC jobs and finished console jobs", () => {
    const html = (jobs: Job[]) => renderToStaticMarkup(<HomeView {...viewProps({ jobs })} />);
    const n = (h: string) => h.split("cw-task is-").length - 1;
    expect(n(html([job({ state: "done", mine: false }), job({ state: "failed", mine: false })]))).toBe(0);
    expect(n(html([job({ state: "running", mine: false })]))).toBe(1);
    expect(n(html([job({ state: "done", mine: true }), job({ state: "failed", mine: true })]))).toBe(2);
  });

  it("a finished task has no Cancel", () => {
    expect(() => byAction(TaskRow({ job: job({ state: "done" }), onCancel: () => {} }), "cancel")).toThrow();
  });

  it("close game asks, then posts the app id", async () => {
    const props = viewProps({
      status: {
        version: null,
        firmware: null,
        model: null,
        temp_c: null,
        running: { title_id: "PPSA01234", app_id: 77, name: null },
      },
    });
    (byAction(HomeView(props), "close-game").props.onClick as () => void)();
    const ask: Ask = { kind: "close", appId: 77, titleId: "PPSA01234" };
    expect(props.onAsk).toHaveBeenCalledWith(ask);
    expect(mockCall).not.toHaveBeenCalled();
    await runAsk(ask, { cancel: cancelJob });
    expect(mockCall).toHaveBeenCalledWith("/api/app/close", { app_id: 77 });
  });

  it("the running game's Saves opens Saves at that game", () => {
    const onOpenSaves = vi.fn();
    const status = {
      version: null,
      firmware: null,
      model: null,
      temp_c: null,
      running: { title_id: "PPSA01234", app_id: 77, name: "A Game" },
    };
    (byAction(HomeView(viewProps({ status, onOpenSaves })), "open-game-saves").props.onClick as () => void)();
    expect(onOpenSaves).toHaveBeenCalledWith("PPSA01234");
    expect(renderToStaticMarkup(<HomeView {...viewProps({ status })} />)).not.toContain("open-game-saves");
  });

  it("Home has no power actions or Browse USB", () => {
    const html = renderToStaticMarkup(
      <HomeView {...viewProps({ volumes: [vol("/mnt/usb0", "/dev/da0s1")], onOpenDrive: vi.fn() })} />,
    );
    for (const a of ["rest", "restart", "browse-usb"]) expect(html).not.toContain(`data-action="${a}"`);
    expect(html).not.toContain("api/power");
  });

  const running = {
    version: null,
    firmware: null,
    model: null,
    temp_c: null,
    running: { title_id: "PPSA01234", app_id: 77, name: "Some Game" },
  };

  it("running card: name, cover, Close; Cheats shortcut only once the section exists", () => {
    const hidden = renderToStaticMarkup(<HomeView {...viewProps({ status: running, cover: "blob:x" })} />);
    expect(hidden).toContain("Some Game");
    expect(hidden).toContain('src="blob:x"');
    expect(hidden).toContain('data-action="close-game"');
    expect(hidden).not.toContain('data-action="open-cheats"');
    const go = vi.fn();
    (byAction(HomeView(viewProps({ status: running, onOpenCheats: go })), "open-cheats").props.onClick as () => void)();
    expect(go).toHaveBeenCalled();
  });

  it("empty states say what to do next", () => {
    const html = renderToStaticMarkup(<HomeView {...viewProps({ status: { ...running, running: null } })} />);
    expect(html).toContain(t("cw_home_no_game_plain"));
    expect(html).toContain(t("cw_home_no_tasks"));
  });

  it("every running task keeps a Cancel; only finished ones are capped, behind a +N more toggle", () => {
    const id = (i: number) => `${i}`.repeat(32);
    const runningJobs = [1, 2, 3].map((i) => job({ job: id(i), mine: i === 1 }));
    const html3 = renderToStaticMarkup(<HomeView {...viewProps({ jobs: runningJobs, onToggleTasks: vi.fn() })} />);
    expect(html3.split('data-action="cancel"').length - 1).toBe(3);
    expect(html3).not.toContain('data-action="toggle-tasks"');

    const finished = [4, 5, 6].map((i) => job({ job: id(i), state: "done", mine: true }));
    const jobs = [runningJobs[0], ...finished];
    const toggle = vi.fn();
    const collapsed = renderToStaticMarkup(<HomeView {...viewProps({ jobs, onToggleTasks: toggle })} />);
    expect(collapsed.split("cw-task is-").length - 1).toBe(MAX_TASK_ROWS);
    expect(collapsed).toContain(t("cw_home_tasks_more", { n: 2 }));
    (byAction(HomeView(viewProps({ jobs, onToggleTasks: toggle })), "toggle-tasks").props.onClick as () => void)();
    expect(toggle).toHaveBeenCalled();

    const open = renderToStaticMarkup(
      <HomeView {...viewProps({ jobs, onToggleTasks: toggle, tasksExpanded: true })} />,
    );
    expect(open.split("cw-task is-").length - 1).toBe(4);
    expect(open).toContain(t("cw_home_tasks_fewer"));
  });

  it("the no-game line promises Cheats only once the section exists", () => {
    const idle = { ...running, running: null };
    expect(renderToStaticMarkup(<HomeView {...viewProps({ status: idle })} />)).toContain(t("cw_home_no_game_plain"));
    expect(renderToStaticMarkup(<HomeView {...viewProps({ status: idle, onOpenCheats: vi.fn() })} />)).toContain(
      t("cw_home_no_game_next"),
    );
  });

  it("a cover that fails to load is dropped", () => {
    const onCoverError = vi.fn();
    const img = elements(HomeView(viewProps({ status: running, cover: "blob:x", onCoverError }))).find(
      (e) => e.type === "img",
    )!;
    (img.props.onError as () => void)();
    expect(onCoverError).toHaveBeenCalled();
  });

  it("one storage card per drive with free of total", () => {
    const html = renderToStaticMarkup(
      <HomeView
        {...viewProps({
          volumes: [vol("/data", "x"), vol("/mnt/usb0", "/dev/da0s1")],
        })}
      />,
    );
    expect(html).toContain(t("cw_home_vol_internal"));
    expect(html).toContain(t("cw_home_vol_usb"));
    expect(html.split("data-volume=").length - 1).toBe(2);
  });

  it("F3: ✕ on a storage card opens Files at that drive's path", () => {
    const open = vi.fn();
    const cards = elements(
      HomeView(viewProps({ volumes: [vol("/data", "x"), vol("/mnt/usb0", "/dev/da0s1")], onOpenDrive: open })),
    ).filter((e) => e.type === VolumeCard);
    expect(cards).toHaveLength(2);
    // ✕ clicks the focused card (nav.ts confirm): the card (a role=button inside the li) carries the onClick.
    const card = elements(VolumeCard(cards[1].props as Parameters<typeof VolumeCard>[0])).find(
      (e) => e.props["data-volume"] === "/mnt/usb0",
    )!;
    expect(card.props.role).toBe("button");
    expect(card.props.tabIndex).toBe(0);
    (card.props.onClick as () => void)();
    expect(open).toHaveBeenCalledWith("/mnt/usb0");
    // Enter and Space work like a button's (and are marked handled, so nav.ts does not click it again).
    for (const key of ["Enter", " "]) {
      const ev = { key, preventDefault: vi.fn() };
      (card.props.onKeyDown as (e: typeof ev) => void)(ev);
      expect(ev.preventDefault).toHaveBeenCalled();
    }
    expect(open).toHaveBeenCalledTimes(3);
    // Without Files to open, a card is only shown: no button role.
    const plain = elements(VolumeCard({ v: vol("/data", "x"), onOpen: null })).find((e) => e.props["data-volume"])!;
    expect(plain.props.role).toBeUndefined();
  });

  it("a 409 or 503 keeps the dialog open with the error inside it", () => {
    expect(keepsDialogOpen(new ApiError(409, "busy"))).toBe(true);
    expect(keepsDialogOpen(new ApiError(503, "stopping"))).toBe(true);
    expect(keepsDialogOpen(new ApiError(500, "io_error"))).toBe(false);
    expect(keepsDialogOpen(new ApiError(401, "token"))).toBe(false);
    const dialog = elements(
      HomeView(viewProps({ ask: { kind: "close", appId: 1, titleId: "X" }, askError: t("cw_home_err_busy") })),
    ).find((e) => e.type === ConfirmDialog)!;
    expect(dialog.props.error).toBe(t("cw_home_err_busy"));
    const html = renderToStaticMarkup(
      <ConfirmDialog title="x" confirmLabel="y" error={t("cw_home_err_busy")} onConfirm={vi.fn()} onCancel={vi.fn()} />,
    );
    expect(html).toContain(t("cw_home_err_busy"));
    expect(html).toContain('role="alert"');
  });

  it("errors become short translated messages", () => {
    expect(errorText(new ApiError(0, "offline"))).toBe(t("cw_home_err_offline"));
    expect(errorText(new ApiError(409, "busy"))).toBe(t("cw_home_err_busy"));
    expect(errorText(new ApiError(404, "unknown_job"))).toBe(t("cw_home_err_gone"));
    expect(errorText(new ApiError(500, "io_error"))).toBe(t("cw_home_err_failed", { code: "io_error" }));
    expect(errorText(new ApiError(401, "token"))).toBeNull(); // the reload banner says it
    expect(errorText(new Error("x"))).toBe(t("cw_home_err_failed", { code: "x" }));
  });

  it("the shown message renders as an alert", () => {
    const html = renderToStaticMarkup(<HomeView {...viewProps({ message: { tone: "error", text: "Nope" } })} />);
    expect(html).toContain('role="alert"');
    expect(html).toContain("Nope");
  });
});

describe("ConfirmDialog", () => {
  it("is a nav modal whose safe choice is the default focus", () => {
    const html = renderToStaticMarkup(
      <ConfirmDialog title="T" confirmLabel="Do it" onConfirm={() => {}} onCancel={() => {}} />,
    );
    expect(html).toContain("data-nav-modal");
    expect(html).toContain('role="alertdialog"');
    // The cancel button comes first and carries data-nav-default.
    const safe = html.indexOf("data-nav-default");
    expect(safe).toBeGreaterThan(-1);
    expect(safe).toBeLessThan(html.indexOf("Do it"));
    expect(html).toContain(t("cw_dialog_cancel"));
  });

  it("○ cancels; other actions are swallowed while it is open", () => {
    const onCancel = vi.fn();
    const handle = dialogNavHandler(onCancel);
    expect(handle("back")).toBe(true);
    expect(onCancel).toHaveBeenCalledTimes(1);
    expect(handle("nextSection")).toBe(true);
    expect(onCancel).toHaveBeenCalledTimes(1);
  });
});

describe("newUsbStick", () => {
  const v = (path: string): Volume => ({
    path,
    mount_from: "/dev/da0s1",
    fs_type: "exfatfs",
    total_bytes: 64e9,
    free_bytes: 32e9,
    writable: true,
  });
  it("is quiet on the first read: what is plugged in at page open is not news", () => {
    expect(newUsbStick(null, [v("/data"), v("/mnt/usb0")])).toBeNull();
  });
  it("names a USB stick that was not there before, and nothing else", () => {
    const seen = new Set(["/data", "/mnt/usb0"]);
    expect(newUsbStick(seen, [v("/data"), v("/mnt/usb0"), v("/mnt/usb1")])?.path).toBe("/mnt/usb1");
    expect(newUsbStick(seen, [v("/data"), v("/mnt/usb0"), v("/mnt/ext0")])).toBeNull();
  });
});
