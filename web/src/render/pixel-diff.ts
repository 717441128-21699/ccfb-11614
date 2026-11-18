/**
 * 逐像素对比:把自研渲染与系统渲染对齐到相同网格后,
 * 统计 alpha 通道差异超过阈值的像素占比。
 *
 * 两侧的渲染管线、抗锯齿模型不同(ClearType vs 解析覆盖率),
 * 不可能逐位一致,因此用"差异像素占比"作为可量化的相似度指标。
 */

export interface DiffOptions {
  /** alpha 差超过该值(0-255)计为差异像素 */
  alphaThreshold: number;
  /** 只比较两侧都可能有墨迹的包围区域,忽略纯空白边距 */
  trimEmpty: boolean;
}

export const DEFAULT_DIFF: DiffOptions = {
  alphaThreshold: 40,
  trimEmpty: true,
};

export interface DiffResult {
  /** 参与比较的像素总数 */
  compared: number;
  /** 差异像素数 */
  differing: number;
  /** 差异占比(0-1) */
  ratio: number;
  /** 平均绝对 alpha 差(0-255) */
  meanAbsDiff: number;
  /** 差异热力图(RGBA),红=自研独有,蓝=系统独有 */
  heatmap: ImageData;
  width: number;
  height: number;
}

/** 从 RGBA ImageData 提取 alpha 平面。 */
function alphaPlane(img: ImageData): Uint8ClampedArray {
  const out = new Uint8ClampedArray(img.width * img.height);
  for (let i = 0; i < out.length; i++) out[i] = img.data[i * 4 + 3];
  return out;
}

/** 计算墨迹包围盒(alpha > 0 的范围)。 */
function inkBounds(a: Uint8ClampedArray, b: Uint8ClampedArray, w: number, h: number) {
  let x0 = w, y0 = h, x1 = 0, y1 = 0;
  for (let y = 0; y < h; y++) {
    for (let x = 0; x < w; x++) {
      const i = y * w + x;
      if (a[i] > 0 || b[i] > 0) {
        if (x < x0) x0 = x;
        if (x > x1) x1 = x;
        if (y < y0) y0 = y;
        if (y > y1) y1 = y;
      }
    }
  }
  if (x1 < x0) return { x0: 0, y0: 0, x1: w - 1, y1: h - 1 };
  return { x0, y0, x1, y1 };
}

export function compareAlpha(
  ours: ImageData,
  system: ImageData,
  opts: DiffOptions = DEFAULT_DIFF,
): DiffResult {
  const width = Math.min(ours.width, system.width);
  const height = Math.min(ours.height, system.height);
  const a = alphaPlane(ours);
  const b = alphaPlane(system);

  const bounds = opts.trimEmpty
    ? inkBounds(a, b, width, height)
    : { x0: 0, y0: 0, x1: width - 1, y1: height - 1 };

  const heatmap = new ImageData(width, height);
  let differing = 0;
  let compared = 0;
  let absSum = 0;

  for (let y = bounds.y0; y <= bounds.y1; y++) {
    for (let x = bounds.x0; x <= bounds.x1; x++) {
      const i = y * width + x;
      const va = a[i];
      const vb = b[i];
      compared++;
      const d = Math.abs(va - vb);
      absSum += d;
      const hi = i * 4;
      if (d > opts.alphaThreshold) {
        differing++;
        // 红:自研有墨、系统无;蓝:系统有墨、自研无
        if (va > vb) {
          heatmap.data[hi] = 230; heatmap.data[hi + 1] = 60; heatmap.data[hi + 2] = 60;
        } else {
          heatmap.data[hi] = 60; heatmap.data[hi + 1] = 110; heatmap.data[hi + 2] = 230;
        }
        heatmap.data[hi + 3] = 200;
      } else if (d > 8) {
        // 轻微差异:黄色提示
        heatmap.data[hi] = 220; heatmap.data[hi + 1] = 200; heatmap.data[hi + 2] = 60;
        heatmap.data[hi + 3] = 120;
      }
    }
  }

  return {
    compared,
    differing,
    ratio: compared > 0 ? differing / compared : 0,
    meanAbsDiff: compared > 0 ? absSum / compared : 0,
    heatmap,
    width,
    height,
  };
}

/** 把 WebGL 画布内容读成 ImageData(强制立即绘制)。 */
export function glCanvasToImageData(canvas: HTMLCanvasElement): ImageData {
  // 用 2D 中转:先把 webgl 画布 drawImage 到临时 2D 画布
  const tmp = document.createElement('canvas');
  tmp.width = canvas.width;
  tmp.height = canvas.height;
  const ctx = tmp.getContext('2d')!;
  ctx.drawImage(canvas, 0, 0);
  return ctx.getImageData(0, 0, canvas.width, canvas.height);
}

/**
 * 逐字符对齐对比:把每个字符分别在独立的、同样大小的格子里渲染,
 * 消除整行布局(advance 累积、取整)带来的错位,只衡量字形栅格化本身。
 *
 * 入参是两个长度相同的 ImageData 数组(每个元素一个字符的格子)。
 * 返回所有格子汇总的差异占比。
 */
export function comparePerGlyph(
  oursCells: ImageData[],
  systemCells: ImageData[],
  opts: DiffOptions = DEFAULT_DIFF,
): { ratio: number; meanAbsDiff: number; compared: number; differing: number; perChar: number[] } {
  let compared = 0;
  let differing = 0;
  let absSum = 0;
  const perChar: number[] = [];

  const n = Math.min(oursCells.length, systemCells.length);
  for (let c = 0; c < n; c++) {
    const r = compareAlpha(oursCells[c], systemCells[c], opts);
    compared += r.compared;
    differing += r.differing;
    absSum += r.meanAbsDiff * r.compared;
    perChar.push(r.ratio);
  }
  return {
    compared,
    differing,
    ratio: compared > 0 ? differing / compared : 0,
    meanAbsDiff: compared > 0 ? absSum / compared : 0,
    perChar,
  };
}

/**
 * 带平移容差的对比:在 ±search 像素内对系统图做二维平移,
 * 取差异最小的对齐。用于区分"字形形状错误"与"亚像素定位偏差"——
 * 后者通过 1px 级平移即可消除,前者无法靠平移修复。
 *
 * 返回最佳对齐下的差异占比与所用平移量。
 */
export function compareWithShiftTolerance(
  ours: ImageData,
  system: ImageData,
  search: number,
  opts: DiffOptions = DEFAULT_DIFF,
): { ratio: number; bestDx: number; bestDy: number; baseRatio: number } {
  const base = compareAlpha(ours, system, opts);
  let best = base.ratio;
  let bestDx = 0;
  let bestDy = 0;
  const w = Math.min(ours.width, system.width);
  const h = Math.min(ours.height, system.height);
  const a = alphaPlane(ours);
  const b = alphaPlane(system);

  for (let dy = -search; dy <= search; dy++) {
    for (let dx = -search; dx <= search; dx++) {
      if (dx === 0 && dy === 0) continue;
      let differing = 0;
      let compared = 0;
      for (let y = 0; y < h; y++) {
        const sy = y + dy;
        if (sy < 0 || sy >= h) continue;
        for (let x = 0; x < w; x++) {
          const sx = x + dx;
          if (sx < 0 || sx >= w) continue;
          compared++;
          if (Math.abs(a[y * w + x] - b[sy * w + sx]) > opts.alphaThreshold) differing++;
        }
      }
      const r = compared > 0 ? differing / compared : 1;
      if (r < best) {
        best = r;
        bestDx = dx;
        bestDy = dy;
      }
    }
  }
  return { ratio: best, bestDx, bestDy, baseRatio: base.ratio };
}
