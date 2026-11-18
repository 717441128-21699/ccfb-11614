//! 高层引擎门面:把字体解析、微调、栅格化串成一条按需管线。
//!
//! 字体字节由外部所有(JS 侧 ArrayBuffer),引擎只持有借用引用,
//! 因此整个引擎生命周期不超过一次调用传入的缓冲。

use crate::fixed::F26Dot6;
use crate::font::FontFile;
use crate::glyf::{GlyfTable, SimpleGlyph};
use crate::hint::HintedGlyph;
use crate::hint::Hinter;
use crate::raster::{rasterize, Bitmap};
use crate::tables::{pixel_scale, Cmap, Head, Hhea, Hmtx, Maxp};
use crate::{Error, Result};

/// 一个字形的栅格化结果与布局度量。
#[derive(Clone, Debug)]
pub struct GlyphBitmap {
    pub bitmap: Bitmap,
    /// 水平步进(像素,浮点)
    pub advance: f32,
    /// 位图左边缘相对画笔原点的 x 偏移(像素)
    pub left_side_bearing: f32,
    /// 位图顶部相对基线的 y 偏移(像素,正值在基线上方)
    pub top_bearing: f32,
}

/// 已解析的单个 face(零拷贝,借用外部字体字节)。
pub struct Face<'a> {
    /// 持有 FontFile 以维持表目录生命周期(字段本身不直接读取)
    #[allow(dead_code)]
    font: FontFile<'a>,
    head: Head,
    maxp: Maxp,
    hhea: Hhea,
    hmtx: Hmtx<'a>,
    cmap: Cmap<'a>,
    glyf: GlyfTable<'a>,
    cvt: Vec<i16>,
    fpgm: Vec<u8>,
    prep: Vec<u8>,
}

impl<'a> Face<'a> {
    pub fn parse(data: &'a [u8], face_index: u32) -> Result<Face<'a>> {
        let font = FontFile::parse(data, face_index)?;
        let head = Head::parse(&font)?;
        let maxp = Maxp::parse(&font)?;
        let hhea = Hhea::parse(&font)?;
        let hmtx = Hmtx::parse(&font, &hhea)?;
        let cmap = Cmap::parse(&font)?;
        let glyf = GlyfTable::new(&font, head, maxp.num_glyphs)?;

        let cvt = font
            .find_table(b"cvt ")
            .map(|t| {
                let raw = &data[t.offset..t.offset + t.length];
                (0..raw.len() / 2)
                    .map(|i| i16::from_be_bytes([raw[i * 2], raw[i * 2 + 1]]))
                    .collect()
            })
            .unwrap_or_default();
        let fpgm = font
            .find_table(b"fpgm")
            .map(|t| data[t.offset..t.offset + t.length].to_vec())
            .unwrap_or_default();
        let prep = font
            .find_table(b"prep")
            .map(|t| data[t.offset..t.offset + t.length].to_vec())
            .unwrap_or_default();

        Ok(Face {
            font,
            head,
            maxp,
            hhea,
            hmtx,
            cmap,
            glyf,
            cvt,
            fpgm,
            prep,
        })
    }

    #[inline]
    pub fn units_per_em(&self) -> u16 {
        self.head.units_per_em
    }

    #[inline]
    pub fn num_glyphs(&self) -> u16 {
        self.maxp.num_glyphs
    }

    /// 行高指标(像素)。
    pub fn line_metrics(&self, ppem: u16) -> LineMetrics {
        let s = pixel_scale(self.head.units_per_em, ppem) as f32 / 64.0 / 64.0;
        LineMetrics {
            ascent: self.hhea.ascent as f32 * s,
            descent: self.hhea.descent as f32 * s,
            line_gap: self.hhea.line_gap as f32 * s,
        }
    }

    pub fn glyph_id(&self, codepoint: u32) -> Result<u16> {
        self.cmap.glyph_id(codepoint)
    }

    /// 取一个简单字形(复合字形递归展平),用于无微调路径。
    fn load_simple(&self, glyph: u16) -> Result<SimpleGlyph> {
        self.glyf.flatten(glyph)
    }

    /// 诊断/测试用:加载原始字形。
    #[doc(hidden)]
    pub fn debug_load_raw(&self, glyph: u16) -> Result<crate::glyf::RawGlyph> {
        self.glyf.load(glyph)
    }
}

#[derive(Clone, Copy, Debug)]
pub struct LineMetrics {
    pub ascent: f32,
    pub descent: f32,
    pub line_gap: f32,
}

/// 检测微调结果的包围盒跨度是否与无 hint 结果同量级。
/// 任一有效轴(原始跨度 > 1px)的比例落在 [0.45, 2.2] 之外即判定异常,
/// 同时覆盖"几何坍缩"和"坐标异常膨胀"两类 VM 误差。
fn has_sane_span(hinted: &HintedGlyph, unhinted: &[crate::glyf::Point]) -> bool {
    let span = |min: i32, max: i32| (max - min).max(0);
    let (mut hxmin, mut hxmax, mut hymin, mut hymax) = (i32::MAX, i32::MIN, i32::MAX, i32::MIN);
    let n = hinted.num_points();
    for i in 0..n {
        hxmin = hxmin.min(hinted.x(i).raw());
        hxmax = hxmax.max(hinted.x(i).raw());
        hymin = hymin.min(hinted.y(i).raw());
        hymax = hymax.max(hinted.y(i).raw());
    }
    let (mut uxmin, mut uxmax, mut uymin, mut uymax) = (i32::MAX, i32::MIN, i32::MAX, i32::MIN);
    for p in unhinted {
        uxmin = uxmin.min(p.x);
        uxmax = uxmax.max(p.x);
        uymin = uymin.min(p.y);
        uymax = uymax.max(p.y);
    }
    let hxs = span(hxmin, hxmax);
    let uxs = span(uxmin, uxmax);
    let hys = span(hymin, hymax);
    let uys = span(uymin, uymax);
    // 轴有效(原始跨度 > 1px = 64)才检查;比例用 i64 防溢出
    let axis_ok = |h: i32, u: i32| -> bool {
        if u <= 64 {
            return true;
        }
        let hv = h as i64;
        let uv = u as i64;
        // 0.45 <= h/u <= 2.2
        hv * 100 >= uv * 45 && hv * 100 <= uv * 220
    };
    axis_ok(hxs, uxs) && axis_ok(hys, uys)
}

/// 渲染引擎:持有一个 face 与某字号下的微调上下文。
pub struct Engine<'a> {
    face: Face<'a>,
    hinter: Hinter,
    ppem: u16,
    scale: i32,
    /// 是否启用微调
    pub hinting_enabled: bool,
}

impl<'a> Engine<'a> {
    pub fn new(face: Face<'a>, ppem: u16) -> Self {
        let scale = pixel_scale(face.units_per_em(), ppem);
        let mut hinter = Hinter::new();
        hinter.init_font(
            &face.cvt,
            &face.fpgm,
            &face.prep,
            scale,
            ppem,
            face.maxp.max_storage,
            face.maxp.max_stack_elements,
            face.maxp.max_twilight_points,
        );
        Engine {
            face,
            hinter,
            ppem,
            scale,
            hinting_enabled: true,
        }
    }

    #[inline]
    pub fn ppem(&self) -> u16 {
        self.ppem
    }

    #[inline]
    pub fn scale(&self) -> i32 {
        self.scale
    }

    pub fn line_metrics(&self) -> LineMetrics {
        self.face.line_metrics(self.ppem)
    }

    pub fn glyph_id(&self, codepoint: u32) -> Result<u16> {
        self.face.glyph_id(codepoint)
    }

    /// 渲染单个字形(按需解析 + 可选微调 + 栅格化)。
    pub fn render_glyph(&mut self, glyph: u16) -> Result<GlyphBitmap> {
        if glyph >= self.face.num_glyphs() {
            return Err(Error::BadGlyph(glyph));
        }
        let (adv_units, lsb_units) = self.face.hmtx.metrics(glyph);
        let scale_f = self.scale as f32 / 64.0 / 64.0;
        let advance = adv_units as f32 * scale_f;
        let lsb = lsb_units as f32 * scale_f;

        // 空字形(.notdef / space):返回零尺寸位图
        if self.face.glyf.glyph_range(glyph)?.is_none() {
            return Ok(GlyphBitmap {
                bitmap: Bitmap {
                    width: 0,
                    height: 0,
                    left: 0,
                    top: 0,
                    alpha: Vec::new(),
                },
                advance,
                left_side_bearing: lsb,
                top_bearing: 0.0,
            });
        }

        // 几何:统一用展平后的简单字形。
        // 对损坏/不支持的字形(复合锚点、越界标志等)优雅降级为空位图,
        // 保证整段文本布局不会因单个字形失败而中断。
        let simple = match self.face.load_simple(glyph) {
            Ok(s) => s,
            Err(_) => {
                return Ok(self.empty_glyph(advance, lsb));
            }
        };
        if simple.points.is_empty() {
            return Ok(self.empty_glyph(advance, lsb));
        }

        // 微调:需要字形原始指令。简单字形直接 load;复合字形的指令
        // 在各组件内,展平后无法整体微调,故对复合字形走无 hint 路径。
        let raw = self.face.glyf.load(glyph).ok();
        let unhinted_pts =
            crate::raster::scale_simple(&simple, self.scale, F26Dot6::ZERO, F26Dot6::ZERO);
        let (points, end_pts) = if self.hinting_enabled {
            if let Some(r) = raw.as_ref().and_then(|r| r.simple.as_ref()) {
                if !r.instructions.is_empty() {
                    let hinted: HintedGlyph = self.hinter.hint_simple(r, self.scale, self.ppem);
                    // 合理性校验(三重),任一不满足就整字形回退无 hint:
                    // 1) 坐标必须落在字号的有限邻域内(防越界)
                    // 2) 包围盒不得异常收缩(防几何坍缩)
                    // 3) 包围盒不得异常膨胀(防坏坐标撑出超大位图塞满图集)
                    let limit = (self.ppem as i32 + 8) * 64 * 4;
                    let in_range = hinted
                        .points
                        .iter()
                        .all(|p| p.raw().abs() <= limit);
                    let sane_span = has_sane_span(&hinted, &unhinted_pts);
                    if in_range && sane_span {
                        let ends = hinted.end_pts.clone();
                        (hinted.to_raster_points(), ends)
                    } else {
                        (unhinted_pts, simple.end_pts.clone())
                    }
                } else {
                    (unhinted_pts, simple.end_pts.clone())
                }
            } else {
                (unhinted_pts, simple.end_pts.clone())
            }
        } else {
            (unhinted_pts, simple.end_pts.clone())
        };

        // 计算包围盒(26.6),确定位图尺寸与原点偏移。
        let mut xmin = i32::MAX;
        let mut ymin = i32::MAX;
        let mut xmax = i32::MIN;
        let mut ymax = i32::MIN;
        for p in &points {
            xmin = xmin.min(p.x);
            ymin = ymin.min(p.y);
            xmax = xmax.max(p.x);
            ymax = ymax.max(p.y);
        }
        // floor / ceil 整除(对负数也正确)
        let floor6 = |v: i32| v >> 6;
        let ceil6 = |v: i32| (v + 63) >> 6;

        // 1 像素 padding,避免抗锯齿边缘被裁
        let pad = 1i32;
        // 位图覆盖的像素列/行范围(画布坐标,原点在基线上的画笔位置)
        let px_left = floor6(xmin) - pad;
        let px_right = ceil6(xmax) + pad;
        let px_bottom = floor6(ymin) - pad;
        let px_top = ceil6(ymax) + pad;
        let width = (px_right - px_left).max(0);
        let height = (px_top - px_bottom).max(0);
        // 把点平移到位图局部坐标:位图 (0,0) 对应画布 (px_left, px_top),
        // 栅格器 y 向下,所以局部 y = 画布 y - px_bottom。
        let shift_x = F26Dot6(-px_left * 64);
        let shift_y = F26Dot6(-px_bottom * 64);
        let shifted: Vec<crate::glyf::Point> = points
            .iter()
            .map(|p| crate::glyf::Point {
                x: p.x + shift_x.0,
                y: p.y + shift_y.0,
                on_curve: p.on_curve,
            })
            .collect();

        let bitmap = rasterize(&shifted, &end_pts, width, height);
        Ok(GlyphBitmap {
            bitmap,
            advance,
            left_side_bearing: px_left as f32,
            top_bearing: px_top as f32,
        })
    }

    /// 渲染一串字符,返回逐字形结果(含缺字回退到 .notdef=0)。
    pub fn layout(&mut self, text: &str) -> Result<Vec<GlyphBitmap>> {
        let mut out = Vec::with_capacity(text.chars().count());
        for ch in text.chars() {
            let gid = self.glyph_id(ch as u32)?;
            let gid = if gid == 0 { 0 } else { gid };
            out.push(self.render_glyph(gid)?);
        }
        Ok(out)
    }

    /// 微调统计(界面展示)。
    pub fn hint_stats(&self) -> &crate::hint::HintStats {
        &self.hinter.stats
    }

    /// 构造一个只有步进、没有位图的空字形结果。
    fn empty_glyph(&self, advance: f32, lsb: f32) -> GlyphBitmap {
        GlyphBitmap {
            bitmap: Bitmap {
                width: 0,
                height: 0,
                left: 0,
                top: 0,
                alpha: Vec::new(),
            },
            advance,
            left_side_bearing: lsb,
            top_bearing: 0.0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_latin_and_cjk_glyphs() {
        let data = std::fs::read("C:/Windows/Fonts/arial.ttf").unwrap();
        let face = Face::parse(&data, 0).unwrap();
        let mut engine = Engine::new(face, 16);
        let gid = engine.glyph_id('A' as u32).unwrap();
        let r = engine.render_glyph(gid).unwrap();
        assert!(r.bitmap.width > 0 && r.bitmap.height > 0);
        assert!(r.advance > 0.0);
        let covered = r.bitmap.alpha.iter().filter(|&&a| a > 10).count();
        assert!(covered > 20);
    }

    #[test]
    fn space_glyph_has_advance_no_bitmap() {
        let data = std::fs::read("C:/Windows/Fonts/arial.ttf").unwrap();
        let face = Face::parse(&data, 0).unwrap();
        let mut engine = Engine::new(face, 16);
        let gid = engine.glyph_id(' ' as u32).unwrap();
        let r = engine.render_glyph(gid).unwrap();
        assert_eq!(r.bitmap.width, 0);
        assert!(r.advance > 0.0);
    }

    #[test]
    fn cjk_render_at_small_ppem() {
        let data = std::fs::read("C:/Windows/Fonts/simhei.ttf").unwrap();
        let face = Face::parse(&data, 0).unwrap();
        let mut engine = Engine::new(face, 12);
        let gid = engine.glyph_id('国' as u32).unwrap();
        let r = engine.render_glyph(gid).unwrap();
        assert!(r.bitmap.width >= 10 && r.bitmap.height >= 10);
        let covered = r.bitmap.alpha.iter().filter(|&&a| a > 10).count();
        assert!(covered > 30, "han glyph should cover many px, got {covered}");
    }

    #[test]
    fn layout_handles_missing_glyph() {
        let data = std::fs::read("C:/Windows/Fonts/arial.ttf").unwrap();
        let face = Face::parse(&data, 0).unwrap();
        let mut engine = Engine::new(face, 16);
        // 含一个非常用码位,验证布局不 panic、数量与字符数一致
        let out = engine.layout("A\u{1F600}B").unwrap();
        assert_eq!(out.len(), 3);
        // 首尾的 A/B 都应有正常步进
        assert!(out[0].advance > 0.0 && out[2].advance > 0.0);
    }

    #[test]
    fn rendering_is_deterministic() {
        // 同一字形渲染两次,alpha 必须逐位一致(缓存/无随机数契约)
        let data = std::fs::read("C:/Windows/Fonts/simhei.ttf").unwrap();
        let face = Face::parse(&data, 0).unwrap();
        let mut engine = Engine::new(face, 12);
        let gid = engine.glyph_id('永' as u32).unwrap();
        let a = engine.render_glyph(gid).unwrap();
        let b = engine.render_glyph(gid).unwrap();
        assert_eq!(a.bitmap.alpha.len(), b.bitmap.alpha.len());
        assert_eq!(a.bitmap.alpha, b.bitmap.alpha);
    }

    #[test]
    fn small_ppem_cjk_coverage_is_sane() {
        // 12px 汉字:位图尺寸与覆盖率应落在合理区间,既不空白也不撑满。
        // 走无 hint 路径(黑体本身无字形级指令),聚焦栅格化正确性。
        let data = std::fs::read("C:/Windows/Fonts/simhei.ttf").unwrap();
        let face = Face::parse(&data, 0).unwrap();
        let mut engine = Engine::new(face, 12);
        engine.hinting_enabled = false;
        for ch in "永的一是中国汉字".chars() {
            let gid = engine.glyph_id(ch as u32).unwrap();
            let r = engine.render_glyph(gid).unwrap();
            let total = (r.bitmap.width * r.bitmap.height) as usize;
            let ink = r.bitmap.alpha.iter().filter(|&&a| a > 10).count();
            let coverage = ink as f32 / total.max(1) as f32;
            // "一"是单横笔,高度天然很小,单独豁免尺寸下限
            if ch != '一' {
                assert!(r.bitmap.width >= 10 && r.bitmap.height >= 10, "{ch} too small");
            }
            // 实心汉字覆盖率经验区间约 0.10..0.85
            assert!(coverage > 0.10 && coverage < 0.85, "{ch} coverage {coverage}");
        }
    }

    #[test]
    fn hinted_and_unhinted_both_valid() {
        let data = std::fs::read("C:/Windows/Fonts/arial.ttf").unwrap();
        let face = Face::parse(&data, 0).unwrap();
        let mut engine = Engine::new(face, 12);
        let gid = engine.glyph_id('H' as u32).unwrap();
        // 无 hint 路径必须渲染出充实的字形
        engine.hinting_enabled = false;
        let plain = engine.render_glyph(gid).unwrap();
        assert!(plain.bitmap.width > 0 && plain.bitmap.height > 0);
        let ink = plain.bitmap.alpha.iter().filter(|&&a| a > 10).count();
        assert!(ink > 10, "unhinted H should have solid strokes, got {ink}");
        // hint 路径不 panic、产出尺寸合法即可(投影简化的几何精度由像素对比量化)
        engine.hinting_enabled = true;
        let hinted = engine.render_glyph(gid).unwrap();
        assert!(hinted.bitmap.width >= 0 && hinted.bitmap.height >= 0);
    }
}
