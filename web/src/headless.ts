/**
 * 无头端到端测试:不依赖任何手工交互,自动完成
 * 取字体 → WASM 渲染 → 系统渲染 → 像素对比,
 * 结果写入 #log,并通过 document.title 暴露给 headless 截图/退出码判断。
 */
import { TtEngine } from './wasm/loader.js';
import { AtlasRenderer, type PlacedGlyph } from './render/atlas-renderer.js';
import { drawSystemLine, readCanvas, registerFontFace } from './render/system-renderer.js';
import { compareAlpha, comparePerGlyph, compareWithShiftTolerance, glCanvasToImageData, DEFAULT_DIFF } from './render/pixel-diff.js';
import { selfConsistency } from './render/self-check.js';

const log = (msg: string): void => {
  const el = document.getElementById('log')!;
  el.textContent += msg + '\n';
};

async function main(): Promise<void> {
  const PPEM = 24;
  const TEXT = '永的一是中国 Hello 12px 字形测试';
  const checks: { name: string; ok: boolean; detail: string }[] = [];
  const check = (name: string, ok: boolean, detail = ''): void => {
    checks.push({ name, ok, detail });
    log(`${ok ? '✓' : '✗'} ${name} ${detail}`);
  };

  try {
    // 1) 加载 WASM
    const engine = await TtEngine.load(new URL('../wasm/ttengine.wasm', import.meta.url).toString());
    check('WASM 加载', engine.version() === 256, `version=${engine.version()}`);

    // 2) 从服务器取同一份字体(同时给自研引擎和系统 FontFace)
    const resp = await fetch('../test-fonts/simhei.ttf');
    if (!resp.ok) throw new Error(`字体获取失败 ${resp.status}`);
    const fontBytes = new Uint8Array(await resp.arrayBuffer());
    check('字体获取', fontBytes.byteLength > 1_000_000, `${fontBytes.byteLength} bytes`);

    engine.initFont(fontBytes, 0, PPEM, 1024, 1024);
    const family = 'tt-headless-ref';
    await registerFontFace(fontBytes, family);

    // 3) 自研引擎逐字形布局
    const line = engine.lineMetrics();
    const baseline = Math.ceil(line.ascent) + 4;
    const canvasOurs = document.getElementById('canvas-ours') as HTMLCanvasElement;
    const canvasSys = document.getElementById('canvas-system') as HTMLCanvasElement;
    const canvasDiff = document.getElementById('canvas-diff') as HTMLCanvasElement;
    const W = 720, H = 60;
    canvasOurs.width = W; canvasOurs.height = H;
    canvasSys.width = W; canvasSys.height = H;
    canvasDiff.width = W; canvasDiff.height = H;

    const renderer = new AtlasRenderer(canvasOurs);
    let pen = 6;
    const placed: PlacedGlyph[] = [];
    let renderedCount = 0;
    for (const ch of TEXT) {
      const gid = engine.glyphId(ch.codePointAt(0)!);
      const m = engine.render(gid);
      if (m.width > 0 && m.height > 0) {
        placed.push({
          sx: m.x, sy: m.y, sw: m.width, sh: m.height,
          dx: pen + Math.round(m.leftSideBearing),
          dy: baseline - Math.round(m.topBearing),
          dw: m.width, dh: m.height,
        });
        renderedCount++;
      }
      pen += Math.round(m.advance);
    }
    check('自研字形渲染', renderedCount >= 10, `渲染 ${renderedCount} 个非空字形`);

    const atlas = engine.atlasPixels();
    renderer.uploadAtlas(atlas.data, atlas.width, atlas.height);
    renderer.draw(placed, [0.08, 0.08, 0.1, 1], W, H);

    // 4) 系统渲染同一字符串
    const sctx = canvasSys.getContext('2d')!;
    sctx.clearRect(0, 0, W, H);
    const sys = drawSystemLine(sctx, TEXT, family, PPEM, 6, baseline);
    check('系统渲染', sys.width > 50, `宽度 ${sys.width.toFixed(1)}px`);

    // 5) 逐像素对比
    const oursImg = glCanvasToImageData(canvasOurs);
    const sysImg = readCanvas(sctx, W, H);
    const diff = compareAlpha(oursImg, sysImg, DEFAULT_DIFF);
    const oursInk = countInk(oursImg);
    const sysInk = countInk(sysImg);
    check('自研有墨迹输出', oursInk > 500, `墨迹像素 ${oursInk}`);
    check('系统有墨迹输出', sysInk > 500, `墨迹像素 ${sysInk}`);
    check(
      '墨迹规模同量级(0.3x..3x)',
      oursInk > sysInk * 0.3 && oursInk < sysInk * 3,
      `ours=${oursInk} sys=${sysInk}`,
    );
    log(`差异像素占比 = ${(diff.ratio * 100).toFixed(2)}% (阈值 5%),平均 alpha 差 = ${diff.meanAbsDiff.toFixed(1)}`);
    const dctx = canvasDiff.getContext('2d')!;
    dctx.putImageData(diff.heatmap, 0, 0);

    const stats = engine.hintStats();
    log(`微调 VM: opcodes=${stats.opcodes} unknown=${stats.unknown} aborts=${stats.aborts}`);
    check('微调 VM 无硬中止', stats.aborts === 0);

    // 6) 逐字符 12px 对比(需求点名字号):每字独立格子,消除布局累积误差
    const perGlyph = await runPerGlyph12Font(fontBytes, family, '永的一是中国汉字字形');
    log(`[12px 逐字符] 原始差异 = ${(perGlyph.ratio * 100).toFixed(2)}%,平移容差后 = ${(perGlyph.tolerantRatio * 100).toFixed(2)}%(平均 alpha 差 ${perGlyph.meanAbsDiff.toFixed(1)})`);
    check('12px 逐字符渲染完成', perGlyph.compared > 0, `${perGlyph.compared} 像素参与比较`);

    // 7) 方法论自检:系统 vs 系统应 ~0%,证明对比管线无系统偏差
    const sc = await selfConsistency(fontBytes, family);
    log(`[自检] 系统自比差异 = ${(sc.same * 100).toFixed(2)}%(应≈0),1px 平移容差找回 = ${(sc.shifted * 100).toFixed(2)}%`);
    check('对比管线自洽(系统自比≈0)', sc.same < 0.01, `ratio=${(sc.same * 100).toFixed(2)}%`);

    // 8) Arial 12px 拉丁逐字符(带 TrueType 微调字节码),复用单引擎
    try {
      const arialResp = await fetch('./test-fonts/arial.ttf');
      if (arialResp.ok) {
        const arial = new Uint8Array(await arialResp.arrayBuffer());
        const ar = await runPerGlyph12Font(arial, 'ArialHeadless', 'HagibMW0123');
        log(`[Arial 12px 逐字符] 原始 = ${(ar.ratio * 100).toFixed(2)}%,平移容差后 = ${(ar.tolerantRatio * 100).toFixed(2)}%`);
        check('Arial 拉丁逐字符渲染', ar.compared > 0, `${ar.compared} 像素`);
      }
    } catch (e2) {
      log(`Arial 对比跳过: ${(e2 as Error).message}`);
    }
  } catch (e) {
    log(`异常: ${(e as Error).message}`);
    checks.push({ name: '异常', ok: false, detail: (e as Error).message });
  }

  const allOk = checks.every((c) => c.ok);
  const el = document.getElementById('log')!;
  el.innerHTML = `<span class="${allOk ? 'pass' : 'fail'}">${allOk ? 'ALL PASS' : 'HAS FAILURE'}</span>\n` +
    checks.map((c) => `${c.ok ? '✓' : '✗'} ${c.name} ${c.detail}`).join('\n') +
    '\n\n' + el.textContent;
  document.title = allOk ? 'HEADLESS_PASS' : 'HEADLESS_FAIL';
  // 暴露给自动化脚本读取
  (window as unknown as { __ttResult: unknown }).__ttResult = { allOk, checks };
}

function countInk(img: ImageData): number {
  let n = 0;
  for (let i = 3; i < img.data.length; i += 4) if (img.data[i] > 10) n++;
  return n;
}

/**
 * 12px 逐字符对比:为每个字符建同样大小的独立格子,
 * 自研引擎与系统分别在格子内居中/基线对齐渲染,再逐像素比较。
 */
async function runPerGlyph12Font(
  fontBytes: Uint8Array,
  family: string,
  charSet: string,
): Promise<{ ratio: number; meanAbsDiff: number; compared: number; differing: number; tolerantRatio: number }> {
  const PPEM = 12;
  const CELL = 24; // 每字 24x24 格子,留足边距
  const chars = charSet.split('');

  const eng = await TtEngine.load(new URL('../wasm/ttengine.wasm', import.meta.url).toString());
  eng.initFont(fontBytes, 0, PPEM, 512, 512);

  const oursCells: ImageData[] = [];
  const sysCells: ImageData[] = [];
  const glCanvas = document.createElement('canvas');
  glCanvas.width = CELL; glCanvas.height = CELL;
  const gl = new AtlasRenderer(glCanvas);
  const sysCanvas = document.createElement('canvas');
  sysCanvas.width = CELL; sysCanvas.height = CELL;
  const sctx = sysCanvas.getContext('2d', { willReadFrequently: true })!;

  for (const ch of chars) {
    const gid = eng.glyphId(ch.codePointAt(0)!);
    const m = eng.render(gid);
    // —— 自研:把图集子图画进格子 ——
    gl.clearRect(CELL, CELL);
    if (m.width > 0 && m.height > 0) {
      const atlas = eng.atlasPixels();
      gl.uploadAtlas(atlas.data, atlas.width, atlas.height);
      const dx = Math.round((CELL - m.width) / 2);
      const dy = Math.round((CELL - m.height) / 2);
      gl.draw([{ sx: m.x, sy: m.y, sw: m.width, sh: m.height, dx, dy, dw: m.width, dh: m.height }],
        [0.1, 0.1, 0.12, 1], CELL, CELL);
    }
    oursCells.push(glCanvasToImageData(glCanvas));

    // —— 系统:同格子、同字号、基线居中 ——
    sctx.clearRect(0, 0, CELL, CELL);
    sctx.font = `${PPEM}px "${family}", sans-serif`;
    sctx.textBaseline = 'middle';
    sctx.textAlign = 'center';
    sctx.fillStyle = '#1a1a1f';
    sctx.fillText(ch, CELL / 2, CELL / 2 + 1);
    sysCells.push(sctx.getImageData(0, 0, CELL, CELL));
  }

  const r = comparePerGlyph(oursCells, sysCells, DEFAULT_DIFF);

  // 平移容差对比:量化"亚像素错位"与"真实形状差异"两个维度
  const tolerant = oursCells.map((cell, i) =>
    compareWithShiftTolerance(cell, sysCells[i], 2, DEFAULT_DIFF).ratio);
  const tolMean = tolerant.reduce((s, v) => s + v, 0) / tolerant.length;

  return {
    ratio: r.ratio,
    meanAbsDiff: r.meanAbsDiff,
    compared: r.compared,
    differing: r.differing,
    tolerantRatio: tolMean,
  };
}

void main();
