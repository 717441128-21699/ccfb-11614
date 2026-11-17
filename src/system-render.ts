/**
 * Reference renderer built on the platform's native text stack (Canvas 2D),
 * plus pixel-level comparison against the WASM renderer. Both render black
 * coverage on an opaque white background so a grayscale comparison is
 * meaningful regardless of subpixel/ClearType settings.
 */

export interface SystemRenderOptions {
  fontFamily: string;
  ppem: number;
  text: string;
}

export interface RenderedPlane {
  canvas: HTMLCanvasElement;
  /** Grayscale coverage per pixel, row-major, 0=black ink on white. */
  gray: Uint8ClampedArray;
  width: number;
  height: number;
  /** Baseline offset from the canvas top, used to align the two renderers. */
  baselineY: number;
}

/**
 * Rasterize `text` with the browser's native font pipeline. The font must be
 * registered with `FontFace`/installed system fonts; we force grayscale AA by
 * drawing on a 2D context (no subpixel alpha) and measuring the tight layout
 * with `measureText` so both engines share an identical canvas size.
 */
export function renderSystemText(opts: SystemRenderOptions): RenderedPlane {
  const { fontFamily, ppem, text } = opts;
  const measure = document.createElement('canvas').getContext('2d')!;
  measure.font = `${ppem}px "${fontFamily}"`;
  const metrics = measure.measureText(text);
  const ascent =
    metrics.actualBoundingBoxAscent ??
    metrics.fontBoundingBoxAscent ??
    ppem * 0.8;
  const descent =
    metrics.actualBoundingBoxDescent ??
    metrics.fontBoundingBoxDescent ??
    ppem * 0.2;
  const width = Math.ceil(metrics.width) + 4;
  const height = Math.ceil(ascent + descent) + 4;
  const baselineY = Math.ceil(ascent) + 2;

  const canvas = document.createElement('canvas');
  canvas.width = width;
  canvas.height = height;
  const ctx = canvas.getContext('2d', { willReadFrequently: true })!;
  ctx.fillStyle = '#ffffff';
  ctx.fillRect(0, 0, width, height);
  ctx.fillStyle = '#000000';
  ctx.font = `${ppem}px "${fontFamily}"`;
  ctx.textBaseline = 'alphabetic';
  ctx.textAlign = 'left';
  ctx.fillText(text, 2, baselineY);

  const image = ctx.getImageData(0, 0, width, height);
  const gray = new Uint8ClampedArray(width * height);
  for (let i = 0; i < gray.length; i++) {
    const r = image.data[i * 4];
    const g = image.data[i * 4 + 1];
    const b = image.data[i * 4 + 2];
    // ITU-R BT.601 luma; white=255, ink approaches 0.
    gray[i] = Math.round(0.299 * r + 0.587 * g + 0.114 * b);
  }
  return { canvas, gray, width, height, baselineY };
}

export interface DiffResult {
  width: number;
  height: number;
  totalPixels: number;
  differingPixels: number;
  differenceRatio: number;
  /** Mean absolute grayscale error across all pixels, 0..255. */
  meanAbsError: number;
  /** Visualization canvas: white background, red marks differing pixels. */
  heatmap: HTMLCanvasElement;
}

export interface DiffOptions {
  /** Pixel counts as different only when |grayA-grayB| exceeds this. */
  grayscaleThreshold?: number;
}

/**
 * Compare two equal-sized grayscale planes. The 5% acceptance criterion in the
 * project brief maps to `differenceRatio`: the share of pixels whose luma
 * delta exceeds `grayscaleThreshold` (default 32 — an edge AA step).
 */
export function diffPlanes(
  ours: Uint8ClampedArray | Uint8Array,
  reference: Uint8ClampedArray | Uint8Array,
  width: number,
  height: number,
  options: DiffOptions = {},
): DiffResult {
  const threshold = options.grayscaleThreshold ?? 32;
  const total = width * height;
  let differing = 0;
  let absSum = 0;

  const heatmap = document.createElement('canvas');
  heatmap.width = width;
  heatmap.height = height;
  const hctx = heatmap.getContext('2d')!;
  const heat = hctx.createImageData(width, height);

  for (let i = 0; i < total; i++) {
    const a = ours[i];
    const b = reference[i];
    const delta = Math.abs(a - b);
    absSum += delta;
    const isDifferent = delta > threshold;
    if (isDifferent) differing += 1;
    const di = i * 4;
    if (isDifferent) {
      // Red overlay; intensity scales with the magnitude of the error.
      heat.data[di] = Math.min(255, 180 + delta / 3);
      heat.data[di + 1] = 40;
      heat.data[di + 2] = 40;
      heat.data[di + 3] = 255;
    } else {
      const v = 255 - Math.round(delta / 2);
      heat.data[di] = v;
      heat.data[di + 1] = v;
      heat.data[di + 2] = v;
      heat.data[di + 3] = 255;
    }
  }
  hctx.putImageData(heat, 0, 0);

  return {
    width,
    height,
    totalPixels: total,
    differingPixels: differing,
    differenceRatio: differing / total,
    meanAbsError: absSum / total,
    heatmap,
  };
}

/** Rasterize our alpha glyph runs into a grayscale plane matching the system
 *  canvas dimensions and baseline, for like-for-like comparison. */
export function composeOurPlane(
  positioned: {
    glyph: { alpha: Uint8Array; width: number; height: number; left: number; top: number };
    x: number;
    baselineY: number;
  }[],
  width: number,
  height: number,
  baselineY: number,
): Uint8ClampedArray {
  const gray = new Uint8ClampedArray(width * height).fill(255);
  for (const { glyph, x } of positioned) {
    const originX = Math.round(x + glyph.left);
    const originY = Math.round(baselineY - glyph.top);
    for (let row = 0; row < glyph.height; row++) {
      for (let col = 0; col < glyph.width; col++) {
        const coverage = glyph.alpha[row * glyph.width + col];
        if (coverage === 0) continue;
        const px = originX + col;
        const py = originY + row;
        if (px < 0 || py < 0 || px >= width || py >= height) continue;
        // Alpha 0..255 over white: resulting gray = 255 - coverage.
        gray[py * width + px] = 255 - coverage;
      }
    }
  }
  return gray;
}
