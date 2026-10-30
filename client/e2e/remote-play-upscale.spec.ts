import { expect, test } from "@playwright/test";

// Upscaling (FSR 1, lib/rpPicture.ts) measured in a real browser's WebGL 2: a sharp picture
// halved, then brought back to full size by the browser's own smooth scaling and by FSR 1. FSR 1
// must come closer to the original and keep edges crisper, the right way up.

test("FSR 1 brings a halved picture back closer to the original than smooth scaling", async ({ page }) => {
  await page.goto("/", { waitUntil: "domcontentloaded" });
  const r = await page.evaluate(async () => {
    const { PicturePass, DEFAULT_PICTURE } = await import("/src/lib/rpPicture.ts");
    const W = 640;
    const H = 360;
    // A picture with what games have: hard and diagonal edges, text, curves, gradients.
    const orig = new OffscreenCanvas(W, H);
    const g = orig.getContext("2d")!;
    const grad = g.createLinearGradient(0, 0, W, H);
    grad.addColorStop(0, "#1b2a49");
    grad.addColorStop(1, "#c96f2d");
    g.fillStyle = grad;
    g.fillRect(0, 0, W, H);
    g.fillStyle = "#f2f2f2";
    g.font = "bold 34px sans-serif";
    g.fillText("PS5 Remote Play 1080p", 24, 60);
    g.font = "18px sans-serif";
    g.fillText("The quick brown fox jumps over the lazy dog", 24, 100);
    g.strokeStyle = "#ffe14d";
    g.lineWidth = 3;
    for (let i = 0; i < 9; i++) {
      g.beginPath();
      g.moveTo(30 + i * 22, 340);
      g.lineTo(150 + i * 40, 140);
      g.stroke();
    }
    g.fillStyle = "#2ec4b6";
    g.beginPath();
    g.arc(480, 240, 80, 0, Math.PI * 2);
    g.fill();
    g.fillStyle = "#000";
    g.fillRect(420, 120, 8, 200);
    // The top-left marker tells which way up a result is.
    g.fillStyle = "#ff0000";
    g.fillRect(0, 0, 12, 12);

    const small = new OffscreenCanvas(W / 2, H / 2);
    const s = small.getContext("2d")!;
    s.imageSmoothingQuality = "high";
    s.drawImage(orig, 0, 0, W / 2, H / 2);

    const pixels = (c: OffscreenCanvas | HTMLCanvasElement) => {
      const t = new OffscreenCanvas(W, H);
      t.getContext("2d")!.drawImage(c, 0, 0);
      return t.getContext("2d")!.getImageData(0, 0, W, H).data;
    };
    const smooth = new OffscreenCanvas(W, H);
    const sm = smooth.getContext("2d")!;
    sm.imageSmoothingQuality = "low"; // bilinear: what the page does without upscaling
    sm.drawImage(small, 0, 0, W, H);

    const pass = new PicturePass();
    const out = pass.render(small, W / 2, H / 2, { ...DEFAULT_PICTURE, upscale: true }, { w: W, h: H });

    const ref = pixels(orig);
    const psnr = (d: Uint8ClampedArray) => {
      let se = 0;
      let n = 0;
      // Away from the border (both treat edges differently).
      for (let y = 4; y < H - 4; y++)
        for (let x = 4; x < W - 4; x++)
          for (let k = 0; k < 3; k++) {
            const i = (y * W + x) * 4 + k;
            se += (d[i] - ref[i]) ** 2;
            n++;
          }
      return 10 * Math.log10((255 * 255) / (se / n));
    };
    const edges = (d: Uint8ClampedArray) => {
      let e = 0;
      for (let y = 1; y < H; y++)
        for (let x = 1; x < W; x++) {
          const i = (y * W + x) * 4 + 1;
          e += Math.abs(d[i] - d[i - 4]) + Math.abs(d[i] - d[i - W * 4]);
        }
      return e;
    };
    const fsr = pixels(pass.canvas as OffscreenCanvas);
    const smoothPx = pixels(smooth);
    return {
      out,
      psnrSmooth: psnr(smoothPx),
      psnrFsr: psnr(fsr),
      edgesRef: edges(ref),
      edgesSmooth: edges(smoothPx),
      edgesFsr: edges(fsr),
      corner: Array.from(fsr.slice(4 * (4 * W + 4), 4 * (4 * W + 4) + 3)),
    };
  });
  console.log(JSON.stringify(r));
  expect(r.out).toEqual({ w: 640, h: 360 });
  // The right way up: the red marker is still top-left.
  expect(r.corner[0]).toBeGreaterThan(150);
  expect(r.corner[1]).toBeLessThan(90);
  // Closer to the original than smooth scaling, and crisper (closer to the original's edges).
  expect(r.psnrFsr).toBeGreaterThan(r.psnrSmooth);
  expect(r.edgesFsr).toBeGreaterThan(r.edgesSmooth);
  expect(r.edgesFsr).toBeLessThan(r.edgesRef * 1.15);
});
