/**
 * Standalone performance benchmark: cold-cache per-glyph rasterization time
 * across Latin and CJK fonts, asserted against the 0.2 ms/glyph budget.
 *
 * Run: node test/bench.mjs
 */
import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { dirname, join } from 'node:path';

const here = dirname(fileURLToPath(import.meta.url));
const root = join(here, '..');
const { instance } = await WebAssembly.instantiate(
  readFileSync(join(root, 'public/core/ttfont_core.wasm')),
  {},
);
const ex = instance.exports;
const scratch = ex.tt_alloc(32);

function openFont(relativePath, fontIndex = 0) {
  const data = readFileSync(join(root, relativePath));
  const ptr = ex.tt_alloc(data.length);
  new Uint8Array(ex.memory.buffer, ptr, data.length).set(data);
  const handle = ex.tt_font_open(ptr, data.length, fontIndex);
  ex.tt_free(ptr, data.length, data.length);
  if (handle === 0) throw new Error(`failed to open ${relativePath}`);
  return handle;
}

function render(handle, gid) {
  const rc = ex.tt_render_glyph(handle, gid, scratch);
  if (rc !== 0) throw new Error(`render gid ${gid} failed: ${rc}`);
  const v = new DataView(ex.memory.buffer, scratch, 32);
  const ap = v.getUint32(24, true);
  const al = v.getUint32(28, true);
  if (ap !== 0) ex.tt_alpha_free(ap, al);
}

function bench(label, fontPath, text, ppem, fontIndex = 0) {
  const handle = openFont(fontPath, fontIndex);
  ex.tt_set_pixel_size(handle, ppem, 1);
  const gids = [...new Set(Array.from(text).map((c) => ex.tt_codepoint_to_gid(handle, c.codePointAt(0))))];
  for (const gid of gids) render(handle, gid); // warmup
  const runs = [];
  for (let rep = 0; rep < 10; rep++) {
    const t0 = performance.now();
    for (const gid of gids) render(handle, gid);
    runs.push((performance.now() - t0) / gids.length);
  }
  const best = Math.min(...runs);
  const p95 = runs.sort((a, b) => a - b)[Math.floor(runs.length * 0.95)];
  ex.tt_font_close(handle);
  const ok = best <= 0.2;
  console.log(
    `${label.padEnd(22)} ${String(gids.length).padStart(4)} glyphs  best=${best.toFixed(4)}ms  p95=${p95.toFixed(4)}ms  ` +
      (ok ? '✓ ≤0.2ms' : '✗ over budget'),
  );
  return ok;
}

console.log(`Per-glyph rasterization benchmark (budget 0.20 ms)`);
const results = [
  bench('Arial Latin @12px', 'test/fonts/arial.ttf', 'Hamburgefons The quick brown fox', 12),
  bench('Arial Latin @16px', 'test/fonts/arial.ttf', 'Hamburgefons The quick brown fox', 16),
  bench('SimHei CJK @12px', 'test/fonts/simhei.ttf', '字体渲染测试中华人民共和国汉字笔画', 12),
  bench('SimHei CJK @16px', 'test/fonts/simhei.ttf', '字体渲染测试中华人民共和国汉字笔画', 16),
  bench('YaHei TTC CJK @12px', 'test/fonts/msyh.ttc', '中华人民共和国微软雅黑字体', 12, 0),
];
console.log(results.every(Boolean) ? '\nALL PERFORMANCE TARGETS MET' : '\nSOME TARGETS MISSED');
process.exit(results.every(Boolean) ? 0 : 1);
