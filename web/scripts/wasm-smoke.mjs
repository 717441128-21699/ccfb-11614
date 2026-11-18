// Node 端 WASM 冒烟测试:加载真实 .wasm,喂入字体,
// 验证 init / glyph_id / render / 图集 / 行高 / 微调统计全链路。
// 运行:node scripts/wasm-smoke.mjs [字体路径]
import { readFile } from 'node:fs/promises';
import { fileURLToPath } from 'node:url';
import { dirname, join } from 'node:path';

const here = dirname(fileURLToPath(import.meta.url));
const wasmPath = join(here, '..', 'wasm', 'ttengine.wasm');
const fontPath = process.argv[2] || 'C:/Windows/Fonts/arial.ttf';

const bytes = await readFile(wasmPath);
const { instance } = await WebAssembly.instantiate(bytes, {});
const ex = instance.exports;
const mem = ex.memory;
const u32 = () => new Uint32Array(mem.buffer);
const f32 = () => new Float32Array(mem.buffer);
const u64 = () => new BigUint64Array(mem.buffer);

console.log('tt_version =', ex.tt_version());

// 1) 用引擎分配的输入缓冲写入字体(避免宿主硬编码地址越界)
const font = new Uint8Array(await readFile(fontPath));
const capSlot = 8;
const inputPtr = Number(ex.tt_alloc_input(font.byteLength, capSlot));
const cap = u32()[capSlot >>> 2];
if (inputPtr === 0 || cap < font.byteLength) throw new Error('alloc_input failed');
new Uint8Array(mem.buffer, inputPtr, font.byteLength).set(font);

// 2) init(ppem=24, 图集 1024x1024,足够装下整组样本)
let rc = ex.tt_init(inputPtr, font.byteLength, 0, 24, 1024, 1024);
console.log('tt_init =', rc);
if (rc !== 0) throw new Error('init failed');

// 3) glyph id
const gidA = ex.tt_glyph_id('A'.codePointAt(0));
const gidSpace = ex.tt_glyph_id(' '.codePointAt(0));
console.log("gid('A') =", gidA, " gid(' ') =", gidSpace);
if (gidA <= 0) throw new Error('cmap lookup failed');

// 4) 引擎出参暂存区
const scratch = Number(ex.tt_scratch());
rc = ex.tt_render(gidA, scratch, scratch + 4);
console.log('tt_render(A) =', rc);
if (rc !== 0) throw new Error('render failed');
console.log('  atlas x/y =', u32()[scratch >>> 2], u32()[(scratch + 4) >>> 2]);

// 5) 度量
const m = scratch + 32;
rc = ex.tt_entry_metrics(gidA, m, m + 4, m + 8, m + 12, m + 16);
console.log('  metrics w/h =', u32()[m >>> 2], u32()[(m + 4) >>> 2],
  'advance =', f32()[(m + 8) >>> 2].toFixed(2));
if (u32()[m >>> 2] <= 0) throw new Error('empty bitmap');

// 6) 图集墨迹检查(注意 init 用的是 1024)
const atlasPtr = ex.tt_atlas_ptr();
const aw = scratch + 64, ah = scratch + 68;
ex.tt_atlas_size(aw, ah);
const W = u32()[aw >>> 2], H = u32()[ah >>> 2];
const atlas = new Uint8Array(mem.buffer, atlasPtr, W * H);
let ink = 0;
for (let i = 0; i < atlas.length; i += 97) if (atlas[i] > 10) ink++;
console.log(`  atlas ${W}x${H}, 抽样墨迹点 =`, ink);
if (ink === 0) throw new Error('atlas has no ink');

// 7) 行高
ex.tt_line_metrics(m, m + 4, m + 8);
console.log('  line ascent/descent =', f32()[m >>> 2].toFixed(2), f32()[(m + 4) >>> 2].toFixed(2));

// 8) 微调统计
ex.tt_hint_stats(m, m + 8, m + 16);
console.log('  hint opcodes =', u64()[m >>> 3].toString(),
  'unknown =', u32()[(m + 8) >>> 2], 'aborts =', u32()[(m + 16) >>> 2]);

// 9) 拉丁批量渲染稳定性(本字体是 Arial,只测它确实覆盖的字符)
const sample = 'ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789';
let failures = 0;
const seen = new Set();
for (const ch of sample) {
  const g = ex.tt_glyph_id(ch.codePointAt(0));
  if (seen.has(g)) continue;
  seen.add(g);
  const rc2 = ex.tt_render(g, scratch, scratch + 4);
  if (rc2 !== 0) {
    failures++;
    console.log(`  失败字形 '${ch}' gid=${g} 错误码=${rc2}`);
  }
}
console.log(`批量渲染 ${seen.size} 个唯一拉丁字形,失败数 =`, failures);
if (failures > 0) throw new Error('latin glyph failures');

ex.tt_shutdown();
console.log('\n✅ WASM 端到端冒烟测试全部通过');
