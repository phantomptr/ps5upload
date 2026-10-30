// Remote Play picture tools: sharpen, brightness, contrast, saturation and a 21:9 crop, done on
// the GPU (WebGL) for each decoded frame, only while a tool is on. The result is drawn into the
// stream's own canvas, so the page, its fit layout, screenshots and clips all see it unchanged.
// With every tool off the stream draws frames exactly as before. "Sharp pixels" is CSS
// (image-rendering) and needs nothing here.

export interface PictureTools {
  /** 0–100: how much the picture is sharpened. */
  sharpen: number;
  /** -50–50 each; 0 = unchanged. */
  brightness: number;
  contrast: number;
  saturation: number;
  /** Keep only the middle 21:9 band (a game letterboxed for ultrawide, on an ultrawide). */
  crop219: boolean;
  /** Scale with hard edges (retro games): CSS only. */
  pixelated: boolean;
}

export const DEFAULT_PICTURE: PictureTools = {
  sharpen: 0,
  brightness: 0,
  contrast: 0,
  saturation: 0,
  crop219: false,
  pixelated: false,
};

/** Whether any tool needs the GPU pass (pixelated is CSS). */
export function needsPass(p: PictureTools): boolean {
  return p.sharpen > 0 || p.brightness !== 0 || p.contrast !== 0 || p.saturation !== 0 || p.crop219;
}

/** The part of a `w`×`h` frame shown: all of it, or the middle 21:9 band. */
export function cropRect(w: number, h: number, crop219: boolean): { y: number; h: number } {
  if (!crop219) return { y: 0, h };
  const band = Math.min(h, Math.round((w * 9) / 21 / 2) * 2);
  return { y: Math.round((h - band) / 2), h: band };
}

/** A stored setting, checked (anything odd is the default). */
export function readPicture(v: unknown): PictureTools {
  const o = (v ?? {}) as Record<string, unknown>;
  const n = (x: unknown, lo: number, hi: number) =>
    typeof x === "number" && Number.isFinite(x) ? Math.min(hi, Math.max(lo, Math.round(x))) : 0;
  return {
    sharpen: n(o.sharpen, 0, 100),
    brightness: n(o.brightness, -50, 50),
    contrast: n(o.contrast, -50, 50),
    saturation: n(o.saturation, -50, 50),
    crop219: o.crop219 === true,
    pixelated: o.pixelated === true,
  };
}

const VS = `#version 300 es
in vec2 p;
out vec2 uv;
uniform vec4 src; // x, y, w, h of the frame shown, in 0-1
void main() {
  uv = src.xy + (p * 0.5 + 0.5) * src.zw;
  uv.y = src.y + src.w - (uv.y - src.y); // frames are top-down
  gl_Position = vec4(p, 0.0, 1.0);
}`;

const FS = `#version 300 es
precision mediump float;
in vec2 uv;
out vec4 o;
uniform sampler2D t;
uniform vec2 px;      // one texel
uniform float sharp;  // 0-1
uniform float bright; // -0.5-0.5
uniform float contr;  // 0.5-1.5
uniform float satur;  // 0-2
void main() {
  vec3 c = texture(t, uv).rgb;
  if (sharp > 0.0) {
    vec3 n = texture(t, uv + vec2(px.x, 0.0)).rgb + texture(t, uv - vec2(px.x, 0.0)).rgb
           + texture(t, uv + vec2(0.0, px.y)).rgb + texture(t, uv - vec2(0.0, px.y)).rgb;
    c = clamp(c + (c - n * 0.25) * sharp * 1.5, 0.0, 1.0);
  }
  c = (c - 0.5) * contr + 0.5 + bright;
  float l = dot(c, vec3(0.2126, 0.7152, 0.0722));
  c = mix(vec3(l), c, satur);
  o = vec4(clamp(c, 0.0, 1.0), 1.0);
}`;

/** The GPU pass: draws a frame through the shader onto its own canvas, which the stream then
 *  draws into the page's canvas. Throws when WebGL 2 is not there. */
export class PicturePass {
  readonly canvas: HTMLCanvasElement | OffscreenCanvas;
  private gl: WebGL2RenderingContext;
  private prog: WebGLProgram;
  private tex: WebGLTexture;
  private u: Record<string, WebGLUniformLocation | null> = {};

  constructor() {
    this.canvas =
      typeof OffscreenCanvas !== "undefined" ? new OffscreenCanvas(16, 16) : document.createElement("canvas");
    const gl = this.canvas.getContext("webgl2", { alpha: false, antialias: false, preserveDrawingBuffer: true }) as
      | WebGL2RenderingContext
      | null;
    if (!gl) throw new Error("no WebGL 2");
    this.gl = gl;
    const sh = (type: number, src: string) => {
      const s = gl.createShader(type)!;
      gl.shaderSource(s, src);
      gl.compileShader(s);
      if (!gl.getShaderParameter(s, gl.COMPILE_STATUS)) throw new Error(gl.getShaderInfoLog(s) ?? "shader");
      return s;
    };
    const prog = gl.createProgram()!;
    gl.attachShader(prog, sh(gl.VERTEX_SHADER, VS));
    gl.attachShader(prog, sh(gl.FRAGMENT_SHADER, FS));
    gl.linkProgram(prog);
    if (!gl.getProgramParameter(prog, gl.LINK_STATUS)) throw new Error(gl.getProgramInfoLog(prog) ?? "program");
    this.prog = prog;
    gl.useProgram(prog);
    const buf = gl.createBuffer();
    gl.bindBuffer(gl.ARRAY_BUFFER, buf);
    gl.bufferData(gl.ARRAY_BUFFER, new Float32Array([-1, -1, 1, -1, -1, 1, 1, 1]), gl.STATIC_DRAW);
    const loc = gl.getAttribLocation(prog, "p");
    gl.enableVertexAttribArray(loc);
    gl.vertexAttribPointer(loc, 2, gl.FLOAT, false, 0, 0);
    this.tex = gl.createTexture()!;
    gl.bindTexture(gl.TEXTURE_2D, this.tex);
    gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MIN_FILTER, gl.LINEAR);
    gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MAG_FILTER, gl.LINEAR);
    gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_WRAP_S, gl.CLAMP_TO_EDGE);
    gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_WRAP_T, gl.CLAMP_TO_EDGE);
    for (const n of ["src", "px", "sharp", "bright", "contr", "satur", "t"]) this.u[n] = gl.getUniformLocation(prog, n);
  }

  /** Draw `frame` (w×h) with `p`; the result is on `canvas`, out.w × out.h. */
  render(frame: TexImageSource, w: number, h: number, p: PictureTools): { w: number; h: number } {
    const gl = this.gl;
    const crop = cropRect(w, h, p.crop219);
    if (this.canvas.width !== w || this.canvas.height !== crop.h) {
      this.canvas.width = w;
      this.canvas.height = crop.h;
    }
    gl.viewport(0, 0, w, crop.h);
    gl.useProgram(this.prog);
    gl.bindTexture(gl.TEXTURE_2D, this.tex);
    gl.texImage2D(gl.TEXTURE_2D, 0, gl.RGBA, gl.RGBA, gl.UNSIGNED_BYTE, frame);
    gl.uniform4f(this.u.src, 0, crop.y / h, 1, crop.h / h);
    gl.uniform2f(this.u.px, 1 / w, 1 / h);
    gl.uniform1f(this.u.sharp, p.sharpen / 100);
    gl.uniform1f(this.u.bright, p.brightness / 100);
    gl.uniform1f(this.u.contr, 1 + p.contrast / 100);
    gl.uniform1f(this.u.satur, 1 + p.saturation / 50);
    gl.uniform1i(this.u.t, 0);
    gl.drawArrays(gl.TRIANGLE_STRIP, 0, 4);
    return { w, h: crop.h };
  }

  dispose(): void {
    this.gl.getExtension("WEBGL_lose_context")?.loseContext();
  }
}
