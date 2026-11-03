import { expect, test } from "@playwright/test";

// Go live and the OBS feed (10.8.0, #635): the Stream button on the Play bar opens the panel;
// ffmpeg is downloaded on first use; a site is added with its key (which never comes back);
// Go live starts the engine's run and the LIVE chip shows on the picture; Stop ends it. The
// engine is faked here (its own tests stream to a real local RTMP server). Nothing is played:
// no frames are sent, and Playwright runs Chrome with --mute-audio.

test("Stream panel: download ffmpeg, add Twitch, go live, see the chip, stop; OBS link", async ({ page }) => {
  const added: Array<Record<string, unknown>> = [];
  const starts: Array<Record<string, unknown>> = [];
  let stops = 0;
  let ffmpegInstalled = false;
  let live: Record<string, unknown> | null = null;
  const destinations: Array<Record<string, unknown>> = [];
  const session = { host: "192.168.1.100", codec: "h264", width: 1920, height: 1080, fps: 60, bitrate_kbps: 15000 };
  const twitchChecks = [
    { code: "bitrate", blocking: false, have: 15000, limit: 6000, message: "Your Play bitrate 15 Mbps is above Twitch's 6 Mbps." },
  ];

  await page.addInitScript(() => {
    window.localStorage.setItem("ps5upload.host", "192.168.1.100");
    window.localStorage.setItem(
      "ps5upload.roster.v1",
      JSON.stringify({
        active_id: "p1",
        profiles: [
          {
            id: "p1",
            name: "Living room",
            host: "192.168.1.100",
            wake_regist_key: "00112233",
            wake_rp_key: "00000000000000000000000000000001",
          },
        ],
      }),
    );
  });
  await page.route(
    (url) => url.pathname.startsWith("/api/"),
    async (route) => {
      const req = route.request();
      const path = new URL(req.url()).pathname;
      const json = (body: unknown, status = 200) =>
        route.fulfill({ status, contentType: "application/json", body: JSON.stringify(body) });
      if (path === "/api/stream/status") {
        return json({
          ffmpeg: {
            installed: ffmpegInstalled,
            version: "9.0.2",
            supported: true,
            download_bytes: 28_395_699,
            source: "https://ffmpeg.martin-riedl.de/…",
            install: { running: false, step: null, done_bytes: 0, total_bytes: 0, error: null, message: null },
          },
          session,
          presets: [],
          destinations,
          store_error: null,
          live,
        });
      }
      if (path === "/api/stream/tools/ffmpeg/install") {
        ffmpegInstalled = true;
        return json({ ok: true });
      }
      if (path === "/api/stream/destinations" && req.method() === "POST") {
        const b = req.postDataJSON() as Record<string, unknown>;
        added.push(b);
        const d = {
          id: "twitch-0a1b2c",
          preset: b.preset,
          label: "Twitch",
          server: "",
          bandwidth_test: b.bandwidth_test ?? false,
          enabled: true,
          key_hint: "…WXYZ",
          name: "Twitch",
          checks: twitchChecks,
        };
        destinations.push(d);
        return json(d);
      }
      if (path === "/api/stream/start") {
        starts.push(req.postDataJSON() as Record<string, unknown>);
        live = {
          phase: "live",
          error: null,
          started_s: 3,
          live_s: 2,
          reconnects: 0,
          retry_in_s: null,
          kbps: 6100,
          fps: 60,
          speed: 1,
          frames: 120,
          dropped_frames: 0,
          upload_skips: 0,
          slow_upload: false,
          audio_kbps: 160,
          destinations: [
            { id: "twitch-0a1b2c", label: "Twitch", preset: "twitch", name: "Twitch", state: "live", error: null, bandwidth_test: true },
          ],
          session,
        };
        return json({ ok: true, live });
      }
      if (path === "/api/stream/stop") {
        stops += 1;
        if (live) live = { ...live, phase: "ended" };
        return json({ ok: true });
      }
      if (path === "/api/ps5/rp/live/status") {
        return json({ running: true, consumers: 1, codec: "h264", width: 1920, height: 1080, fps: 60 });
      }
      let body: unknown = {};
      if (path === "/api/version") body = { version: "10.8.0", caps: {} };
      if (path === "/api/ps5/rp/session") body = { host: null };
      return json(body);
    },
  );
  await page.routeWebSocket(/\/api\/ps5\/rp\/ws/, (ws) => {
    ws.onMessage((m) => {
      const msg = JSON.parse(String(m)) as Record<string, unknown>;
      if ("host" in msg) ws.send(JSON.stringify({ stage: "first_frame" }));
    });
  });

  await page.goto("/play", { waitUntil: "domcontentloaded" });
  await expect(page.getByRole("heading", { name: "Play", exact: true })).toBeVisible({ timeout: 30_000 });
  await page.getByTestId("rp-start").click();
  await page.getByTestId("rp-toolbar").hover();
  await page.getByTestId("rp-golive").click();
  const panel = page.getByTestId("golive-panel");
  await expect(panel).toBeVisible();

  // First time: the ffmpeg download, with its size.
  await expect(panel).toContainText("ffmpeg, about 28 MB");
  await panel.getByTestId("golive-ffmpeg-download").click();
  await expect(panel.getByTestId("golive-ffmpeg")).toHaveCount(0);

  // Add Twitch: where the key is, test mode on by default, validation, then saved.
  await panel.getByTestId("golive-add-twitch-button").click();
  await expect(panel.getByTestId("golive-key-help")).toContainText("Creator Dashboard");
  await panel.getByTestId("golive-add-save").click();
  await expect(panel).toContainText("Paste the stream key from the site.");
  expect(added).toHaveLength(0);
  await panel.getByTestId("golive-key").fill("live_test_test_test_key");
  await panel.getByTestId("golive-add-save").click();
  await expect.poll(() => added).toEqual([{ preset: "twitch", key: "live_test_test_test_key", bandwidth_test: true }]);
  // Only the key's end ever shows again, with Twitch's bitrate limit and the restart offer.
  const row = panel.getByTestId("golive-dest");
  await expect(row).toContainText("…WXYZ");
  await expect(panel).not.toContainText("live_123456789");
  await expect(panel.getByTestId("golive-check-bitrate")).toContainText("above Twitch's 6 Mbps");
  await expect(panel.getByTestId("golive-fix")).toContainText("Restart Remote Play at 6 Mbps?");

  // Go live: the saved destination by id; the LIVE chip on the picture.
  await panel.getByTestId("golive-start").click();
  await expect.poll(() => starts).toEqual([{ destinations: [{ key_id: "twitch-0a1b2c" }], audio_kbps: 160 }]);
  await expect(panel.getByTestId("golive-live")).toContainText("test, not public");
  await expect(page.getByTestId("golive-chip")).toContainText("LIVE");
  await expect(page.getByTestId("golive-chip")).toContainText("6.1 Mbps");

  await panel.getByTestId("golive-stop").click();
  await expect.poll(() => stops).toBe(1);
  await expect(page.getByTestId("golive-chip")).toHaveCount(0);

  // OBS: the feed's address on this computer, and that OBS reads it.
  await panel.getByTestId("golive-tab-obs").click();
  await expect(panel.getByTestId("rp-obs-url")).toHaveValue(/\/api\/ps5\/rp\/live\.ts$/);
  await expect(panel.getByTestId("rp-obs-state")).toContainText("OBS connected (1)");
  await expect(panel).toContainText("Input format: mpegts");
});
