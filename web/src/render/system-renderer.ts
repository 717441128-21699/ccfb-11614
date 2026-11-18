/**
 * 系统渲染基线:用同一份 TTF 字节通过 FontFace 注册,
 * 再用 Canvas 2D fillText 绘制。这条路径在 Windows 上走 DirectWrite,
 * 在 macOS 上走 CoreText,代表"系统渲染"的真实效果。
 */

export interface SystemLineResult {
  /** 每个字符的宽度(用于和自研引擎逐字对齐) */
  charWidths: number[];
  width: number;
}

/** 用字体字节注册一个 FontFace,返回字体族名。 */
export async function registerFontFace(bytes: Uint8Array, family: string): Promise<string> {
  const blob = new Blob([bytes.slice().buffer], { type: 'font/ttf' });
  const url = URL.createObjectURL(blob);
  const face = new FontFace(family, `url(${url})`);
  await face.load();
  (document.fonts as FontFaceSet).add(face);
  return family;
}

/**
 * 在给定 2D 上下文上用系统字体绘制一行文字。
 * 返回每个字符的布局宽度,供两侧基线对齐。
 */
export function drawSystemLine(
  ctx: CanvasRenderingContext2D,
  text: string,
  family: string,
  px: number,
  originX: number,
  baselineY: number,
  color = '#111',
): SystemLineResult {
  ctx.save();
  ctx.font = `${px}px "${family}", sans-serif`;
  ctx.textBaseline = 'alphabetic';
  ctx.textAlign = 'left';
  ctx.fillStyle = color;
  ctx.imageSmoothingEnabled = true;

  const charWidths: number[] = [];
  let pen = originX;
  // 逐字符绘制并测量,保证和自研引擎逐字可比
  for (const ch of text) {
    ctx.fillText(ch, pen, baselineY);
    const w = ctx.measureText(ch).width;
    charWidths.push(w);
    pen += w;
  }
  ctx.restore();
  return { charWidths, width: pen - originX };
}

/** 读取系统渲染画布的 ImageData,用于逐像素对比。 */
export function readCanvas(ctx: CanvasRenderingContext2D, w: number, h: number): ImageData {
  return ctx.getImageData(0, 0, w, h);
}
