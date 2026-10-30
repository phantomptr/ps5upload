import { readFile } from "node:fs/promises";
import { expect, test, type Page } from "@playwright/test";

// Instant replay (9.8): the engine keeps the last seconds of the stream as the console sent them
// (rp_replay.rs); Save replay turns them into an MP4 that plays. Here the "console's" stream is
// real H.264 and Opus, encoded in the page, in the engine's snapshot format.

/** Two seconds of 320x240 H.264 (keyframe every second) and Opus, as a replay snapshot. */
async function makeSnapshot(page: Page): Promise<number[]> {
  return page.evaluate(async () => {
    const W = 320;
    const H = 240;
    const FPS = 30;
    const video: Array<{ t: number; key: boolean; data: Uint8Array }> = [];
    const venc = new VideoEncoder({
      output: (c) => {
        const d = new Uint8Array(c.byteLength);
        c.copyTo(d);
        video.push({ t: c.timestamp, key: c.type === "key", data: d });
      },
      error: (e) => {
        throw e;
      },
    });
    venc.configure({ codec: "avc1.42001f", width: W, height: H, framerate: FPS, avc: { format: "annexb" } });
    const cv = new OffscreenCanvas(W, H);
    const g = cv.getContext("2d")!;
    for (let i = 0; i < FPS * 2; i++) {
      g.fillStyle = `hsl(${i * 6}, 80%, 50%)`;
      g.fillRect(0, 0, W, H);
      const f = new VideoFrame(cv, { timestamp: Math.round((i * 1e6) / FPS) });
      venc.encode(f, { keyFrame: i % FPS === 0 });
      f.close();
    }
    await venc.flush();

    const audio: Array<{ t: number; data: Uint8Array }> = [];
    const aenc = new AudioEncoder({
      output: (c) => {
        const d = new Uint8Array(c.byteLength);
        c.copyTo(d);
        audio.push({ t: c.timestamp, data: d });
      },
      error: (e) => {
        throw e;
      },
    });
    aenc.configure({ codec: "opus", sampleRate: 48000, numberOfChannels: 2, bitrate: 64000 });
    for (let i = 0; i < 200; i++) {
      const n = 480;
      const pcm = new Float32Array(n * 2);
      for (let k = 0; k < n; k++) pcm[k] = pcm[n + k] = Math.sin(((i * n + k) / 48000) * 2 * Math.PI * 440) * 0.2;
      const ad = new AudioData({ format: "f32-planar", sampleRate: 48000, numberOfFrames: n, numberOfChannels: 2, timestamp: i * 10_000, data: pcm });
      aenc.encode(ad);
      ad.close();
    }
    await aenc.flush();

    // The engine sends the parameter sets apart (its video header): split them off the first
    // keyframe, as the console's stream comes.
    const first = video[0].data;
    let idr = 0;
    for (let i = 0; i + 3 < first.length; i++) {
      if (first[i] === 0 && first[i + 1] === 0 && first[i + 2] === 1 && (first[i + 3] & 0x1f) === 5) {
        idr = i > 0 && first[i - 1] === 0 ? i - 1 : i;
        break;
      }
    }
    const header = first.subarray(0, idr);
    video[0].data = first.subarray(idr);

    const parts: number[] = [];
    const u8 = (n: number) => parts.push(n & 0xff);
    const u32 = (n: number) => [24, 16, 8, 0].forEach((s) => u8(n >>> s));
    const u64 = (n: number) => {
      u32(Math.floor(n / 2 ** 32));
      u32(n >>> 0);
    };
    const rec = (kind: number, t: number, data: Uint8Array, key?: boolean) => {
      u8(kind);
      u64(t);
      u32(data.length);
      if (key !== undefined) u8(key ? 1 : 0);
      for (const b of data) parts.push(b);
    };
    for (const c of "PSRP1") parts.push(c.charCodeAt(0));
    rec(0, 0, header);
    const ah = new Uint8Array(10);
    ah[0] = 2;
    new DataView(ah.buffer).setUint32(2, 48000);
    new DataView(ah.buffer).setUint32(6, 480);
    rec(3, 0, ah);
    // Interleaved by time, as the session receives them.
    const all = [
      ...video.map((v) => ({ kind: 1, t: v.t, key: v.key, data: v.data })),
      ...audio.map((a) => ({ kind: 4, t: a.t, key: undefined as boolean | undefined, data: a.data })),
    ].sort((a, b) => a.t - b.t);
    for (const f of all) rec(f.kind, f.t + 5_000_000, f.data, f.kind === 1 ? f.key : undefined);
    return parts;
  });
}

test("Save replay turns the engine's last seconds into an MP4 that plays", async ({ page }) => {
  let snapshot: Buffer | null = null;
  const starts: Array<Record<string, unknown>> = [];
  const changes: unknown[] = [];
  await page.addInitScript(() => {
    window.localStorage.setItem("ps5upload.host", "192.168.1.100");
    window.localStorage.setItem("ps5upload.rp_prefs.v1", JSON.stringify({ replayS: 30 }));
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
      const path = new URL(route.request().url()).pathname;
      if (path === "/api/ps5/rp/replay") {
        if (!snapshot) return route.fulfill({ status: 404, contentType: "application/json", body: "{}" });
        return route.fulfill({ contentType: "application/octet-stream", body: snapshot });
      }
      let body: unknown = {};
      if (path === "/api/version") body = { version: "9.8.0", caps: {} };
      if (path === "/api/ps5/rp/session") body = { host: null };
      await route.fulfill({ contentType: "application/json", body: JSON.stringify(body) });
    },
  );
  await page.routeWebSocket(/\/api\/ps5\/rp\/ws/, (ws) => {
    ws.onMessage((m) => {
      const msg = JSON.parse(String(m)) as Record<string, unknown>;
      if ("host" in msg) {
        starts.push(msg);
        ws.send(JSON.stringify({ stage: "first_frame" }));
      } else if ("replay_s" in msg) changes.push(msg.replay_s);
    });
  });
  await page.goto("/play", { waitUntil: "domcontentloaded" });
  await expect(page.getByRole("heading", { name: "Remote Play" })).toBeVisible({ timeout: 30_000 });
  snapshot = Buffer.from(await makeSnapshot(page));
  await page.getByTestId("rp-start").click();
  // The session is asked to keep the last 30 seconds.
  await expect.poll(() => starts[0]?.replay_s).toBe(30);

  await page.getByTestId("rp-toolbar").hover();
  const download = page.waitForEvent("download");
  await page.getByTestId("rp-replay-save").first().click();
  const file = await download;
  expect(file.suggestedFilename()).toMatch(/^PS5 replay Living room .+\.mp4$/);
  const mp4 = await readFile((await file.path())!);
  expect(mp4.subarray(4, 8).toString()).toBe("ftyp");

  // It plays: the browser reads its picture and length.
  const meta = await page.evaluate(async (b64) => {
    const bytes = Uint8Array.from(atob(b64), (c) => c.charCodeAt(0));
    const v = document.createElement("video");
    v.muted = true;
    v.src = URL.createObjectURL(new Blob([bytes], { type: "video/mp4" }));
    await new Promise<void>((ok, bad) => {
      v.onloadedmetadata = () => ok();
      v.onerror = () => bad(new Error(`video error ${v.error?.code}`));
    });
    return { w: v.videoWidth, h: v.videoHeight, d: v.duration };
  }, mp4.toString("base64"));
  expect(meta.w).toBe(320);
  expect(meta.h).toBe(240);
  expect(meta.d).toBeGreaterThan(1.5);

  // Turning it off mid-stream tells the session.
  await page.getByTestId("rp-toolbar").hover();
  await page.getByTestId("rp-toolbar").getByRole("button", { name: "Settings" }).click();
  await page.getByTestId("rp-replay-setting").selectOption("0");
  await expect.poll(() => changes).toEqual([0]);
  await expect(page.getByTestId("rp-replay-save")).toHaveCount(0);
});
