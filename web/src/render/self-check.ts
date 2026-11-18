/**
 * 对比管线自检:系统渲染与自身比较应得 ~0% 差异,
 * 验证像素对比逻辑没有系统性偏差。作为 headless 测试的一部分。
 */
import { drawSystemLine, registerFontFace } from './system-renderer.js';
import { compareAlpha, compareWithShiftTolerance, DEFAULT_DIFF } from './pixel-diff.js';

export async function selfConsistency(fontBytes: Uint8Array, family: string): Promise<{ same: number; shifted: number }> {
  await registerFontFace(fontBytes, family + '-self');
  const W = 200, H = 40, PPEM = 12;
  const c1 = document.createElement('canvas');
  c1.width = W; c1.height = H;
  const c2 = document.createElement('canvas');
  c2.width = W; c2.height = H;
  const ctx1 = c1.getContext('2d', { willReadFrequently: true })!;
  const ctx2 = c2.getContext('2d', { willReadFrequently: true })!;

  const text = '永的一是中国';
  drawSystemLine(ctx1, text, family + '-self', PPEM, 6, 26);
  drawSystemLine(ctx2, text, family + '-self', PPEM, 6, 26);
  const i1 = ctx1.getImageData(0, 0, W, H);
  const i2 = ctx2.getImageData(0, 0, W, H);
  const same = compareAlpha(i1, i2, DEFAULT_DIFF).ratio;

  // 把第二张整体右移 1px,容差对比应能找回接近 0
  const shiftedCanvas = document.createElement('canvas');
  shiftedCanvas.width = W; shiftedCanvas.height = H;
  const sctx = shiftedCanvas.getContext('2d', { willReadFrequently: true })!;
  drawSystemLine(sctx, text, family + '-self', PPEM, 7, 26);
  const shifted = compareWithShiftTolerance(i1, sctx.getImageData(0, 0, W, H), 2, DEFAULT_DIFF);
  return { same, shifted: shifted.ratio };
}
