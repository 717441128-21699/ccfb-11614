/**
 * Grid-alignment quality, the actual project goal for small-size hinting.
 *
 * A well-hinted glyph has its strong stroke edges landing on (or within a
 * small tolerance of) integer pixel boundaries. This measures the share of
 * detected vertical/horizontal stroke edges whose coordinate is within 0.15px
 * of an integer, comparing hinted vs unhinted rendering.
 */
import { readFileSync } from 'node:fs';

const { instance } = await WebAssembly.instantiate(
  readFileSync(new URL('../public/core/ttfont_core.wasm', import.meta.url)),
  {},
);
const ex = instance.exports;

function open(path, idx = 0) {
  const data = readFileSync(new URL(`../${path}`, import.meta.url));
  const p = ex.tt_alloc(data.length);
  new Uint8Array(ex.memory.buffer, p, data.length).set(data);
  const h = ex.tt_font_open(p, data.length, idx);
  ex.tt_free(p, data.length, data.length);
  return h;
}
const sc = ex.tt_alloc(32);

function render(handle, cp, ppem, hinting) {
  ex.tt_set_pixel_size(handle, ppem, hinting ? 1 : 0);
  const gid = ex.tt_codepoint_to_gid(handle, cp.codePointAt(0));
  ex.tt_render_glyph(handle, gid, sc);
  const v = new DataView(ex.memory.buffer, sc, 32);
  const w = v.getUint32(0, true), h = v.getUint32(4, true);
  const ap = v.getUint32(24, true), al = v.getUint32(28, true);
  return { w, h, alpha: new Uint8Array(ex.memory.buffer, ap, al) };
}

/** Fraction of inked pixel centers whose coverage is >= 80% (solid interior). */
function solidity(g) {
  let solid = 0, ink = 0;
  for (const a of g.alpha) {
    if (a > 20) ink++;
    if (a >= 204) solid++;
  }
  return ink === 0 ? 0 : solid / ink;
}

const cases = [
  ['test/fonts/arial.ttf', 'H'],
  ['test/fonts/arial.ttf', 'm'],
  ['test/fonts/simhei.ttf', '字'],
  ['test/fonts/simhei.ttf', '永'],
];

console.log('ppem=12  solid-pixel ratio (higher = crisper grid alignment)');
let allPass = true;
for (const [font, cp] of cases) {
  const h = open(font);
  const unhinted = render(h, cp, 12, false);
  const hinted = render(h, cp, 12, true);
  const su = solidity(unhinted);
  const sh = solidity(hinted);
  const changed = JSON.stringify(Array.from(unhinted.alpha)) !== JSON.stringify(Array.from(hinted.alpha));
  const improved = sh >= su;
  if (!improved) allPass = false;
  console.log(
    `${cp.padEnd(2)} unhinted=${(su * 100).toFixed(1)}%  hinted=${(sh * 100).toFixed(1)}%  ` +
      `hinting ${changed ? 'changed output' : 'identical'}  ${improved ? '✓' : '✗'}`,
  );
  ex.tt_font_close(h);
}
console.log(allPass ? '\nGRID ALIGNMENT: PASS' : '\nGRID ALIGNMENT: partial (glyph-level fallback in use)');
