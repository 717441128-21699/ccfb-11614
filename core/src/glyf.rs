//! `loca` / `glyf` 表:字形轮廓解析。
//!
//! 支持简单字形与复合字形;复合字形保留组件结构(供微调解释器使用),
//! 同时提供递归展平(供无微调快速栅格化)。

use crate::fixed::F2Dot14;
use crate::font::{FontFile, Reader};
use crate::tables::Head;
use crate::{Error, Result};

// ---- 复合字形标志位(Apple/OpenType 规范) ----
pub const ARG_1_AND_2_ARE_WORDS: u16 = 0x0001;
pub const ARGS_ARE_XY_VALUES: u16 = 0x0002;
pub const WE_HAVE_A_SCALE: u16 = 0x0008;
pub const MORE_COMPONENTS: u16 = 0x0020;
pub const WE_HAVE_AN_X_AND_Y_SCALE: u16 = 0x0040;
pub const WE_HAVE_A_TWO_BY_TWO: u16 = 0x0080;
pub const WE_HAVE_INSTRUCTIONS: u16 = 0x0100;
pub const USE_MY_METRICS: u16 = 0x0200;

#[derive(Clone, Copy, Debug)]
pub struct Point {
    pub x: i32,
    pub y: i32,
    pub on_curve: bool,
}

/// 一个简单字形(字体单位坐标 + 轮廓端点索引 + 微调指令)。
#[derive(Clone, Debug, Default)]
pub struct SimpleGlyph {
    pub points: Vec<Point>,
    /// 每条轮廓最后一个点在 points 中的索引。
    pub end_pts: Vec<u16>,
    pub instructions: Vec<u8>,
    pub bbox: [i16; 4],
}

/// 复合字形的一个组件引用。
#[derive(Clone, Debug)]
pub struct Component {
    pub glyph_index: u16,
    pub flags: u16,
    /// 平移(字体单位,XY 值形式;点形式在解析期已转换为近似偏移 0,微调阶段再处理)
    pub dx: i32,
    pub dy: i32,
    /// 2x2 变换(2.14)
    pub m00: F2Dot14,
    pub m01: F2Dot14,
    pub m10: F2Dot14,
    pub m11: F2Dot14,
}

#[derive(Clone, Debug, Default)]
pub struct RawGlyph {
    pub bbox: [i16; 4],
    pub simple: Option<SimpleGlyph>,
    pub components: Vec<Component>,
    /// 复合字形自身的指令(组件指令保存在各组件简单字形内)。
    pub composite_instructions: Vec<u8>,
    pub use_my_metrics: bool,
}

impl RawGlyph {
    pub fn is_empty(&self) -> bool {
        self.simple.is_none() && self.components.is_empty()
    }
    pub fn is_composite(&self) -> bool {
        !self.components.is_empty()
    }
}

/// glyf + loca 借用视图。
pub struct GlyfTable<'a> {
    glyf: &'a [u8],
    loca: &'a [u8],
    head: Head,
    num_glyphs: u16,
}

impl<'a> GlyfTable<'a> {
    pub fn new(font: &FontFile<'a>, head: Head, num_glyphs: u16) -> Result<GlyfTable<'a>> {
        Ok(GlyfTable {
            glyf: font.table_bytes(b"glyf")?,
            loca: font.table_bytes(b"loca")?,
            head,
            num_glyphs,
        })
    }

    /// 取 glyph 在 glyf 中的字节范围;空字形返回 None。
    pub fn glyph_range(&self, glyph: u16) -> Result<Option<(usize, usize)>> {
        if glyph >= self.num_glyphs {
            return Err(Error::BadGlyph(glyph));
        }
        let (start, end) = if self.head.index_to_loc_format == 0 {
            let s = Reader::peek_u16_at(self.loca, glyph as usize * 2)? as usize * 2;
            let e = Reader::peek_u16_at(self.loca, glyph as usize * 2 + 2)? as usize * 2;
            (s, e)
        } else {
            let s = Reader::peek_u32_at(self.loca, glyph as usize * 4)? as usize;
            let e = Reader::peek_u32_at(self.loca, glyph as usize * 4 + 4)? as usize;
            (s, e)
        };
        if start == end {
            Ok(None)
        } else if end > self.glyf.len() || start > end {
            Err(Error::oob("glyf range", start))
        } else {
            Ok(Some((start, end)))
        }
    }

    /// 解析原始字形(不展平)。
    pub fn load(&self, glyph: u16) -> Result<RawGlyph> {
        let Some((start, end)) = self.glyph_range(glyph)? else {
            return Ok(RawGlyph::default());
        };
        let mut r = Reader::at(self.glyf, start);
        let number_of_contours = r.i16()?;
        let bbox = [r.i16()?, r.i16()?, r.i16()?, r.i16()?];

        if number_of_contours == 0 {
            return Ok(RawGlyph {
                bbox,
                ..Default::default()
            });
        }
        if number_of_contours > 0 {
            let simple = self.parse_simple(&mut r, number_of_contours as u16, bbox, end)?;
            Ok(RawGlyph {
                bbox,
                simple: Some(simple),
                ..Default::default()
            })
        } else {
            self.parse_composite(&mut r, bbox, end)
        }
    }

    fn parse_simple(
        &self,
        r: &mut Reader,
        num_contours: u16,
        bbox: [i16; 4],
        end: usize,
    ) -> Result<SimpleGlyph> {
        let nc = num_contours as usize;
        let mut end_pts = Vec::with_capacity(nc);
        for _ in 0..nc {
            end_pts.push(r.u16()?);
        }
        let num_points = end_pts.last().copied().map(|v| v as usize + 1).unwrap_or(0);
        let instr_len = r.u16()? as usize;
        let mut instructions = Vec::with_capacity(instr_len);
        for _ in 0..instr_len {
            instructions.push(r.u8()?);
        }
        // ---- flags ----
        let mut flags = Vec::with_capacity(num_points);
        while flags.len() < num_points {
            let f = r.u8()?;
            let repeat = if f & 0x08 != 0 { r.u8()? as usize } else { 1 };
            for _ in 0..repeat {
                flags.push(f);
                if flags.len() == num_points {
                    break;
                }
            }
        }
        // ---- x ----
        let mut xs = vec![0i32; num_points];
        let mut last: i32 = 0;
        for i in 0..num_points {
            let f = flags[i];
            let dx = if f & 0x02 != 0 {
                // x-short: 1 字节
                let v = r.u8()? as i32;
                if f & 0x10 != 0 { v } else { -v }
            } else if f & 0x10 != 0 {
                0
            } else {
                r.i16()? as i32
            };
            last += dx;
            xs[i] = last;
        }
        // ---- y ----
        let mut ys = vec![0i32; num_points];
        last = 0;
        for i in 0..num_points {
            let f = flags[i];
            let dy = if f & 0x04 != 0 {
                let v = r.u8()? as i32;
                if f & 0x20 != 0 { v } else { -v }
            } else if f & 0x20 != 0 {
                0
            } else {
                r.i16()? as i32
            };
            last += dy;
            ys[i] = last;
        }
        let points = (0..num_points)
            .map(|i| Point {
                x: xs[i],
                y: ys[i],
                on_curve: flags[i] & 0x01 != 0,
            })
            .collect();
        // 防御:畸形字体可能声明的数据超过 glyph 范围
        if r.pos > end {
            return Err(Error::oob("simple glyph overrun", r.pos));
        }
        Ok(SimpleGlyph {
            points,
            end_pts,
            instructions,
            bbox,
        })
    }

    fn parse_composite(
        &self,
        r: &mut Reader,
        bbox: [i16; 4],
        end: usize,
    ) -> Result<RawGlyph> {
        let mut components = Vec::new();
        let mut flags = MORE_COMPONENTS;
        let mut have_instr = false;
        let mut use_my_metrics = false;

        while flags & MORE_COMPONENTS != 0 {
            flags = r.u16()?;
            let glyph_index = r.u16()?;
            let (dx, dy) = if flags & ARG_1_AND_2_ARE_WORDS != 0 {
                if flags & ARGS_ARE_XY_VALUES != 0 {
                    (r.i16()? as i32, r.i16()? as i32)
                } else {
                    // 锚点形式:栅格化展平暂不支持精确锚点对齐,记录原始值并在 hint 模块处理
                    let _a = r.u16()?;
                    let _b = r.u16()?;
                    (0, 0)
                }
            } else {
                if flags & ARGS_ARE_XY_VALUES != 0 {
                    (r.i8()? as i32, r.i8()? as i32)
                } else {
                    let _a = r.u8()?;
                    let _b = r.u8()?;
                    (0, 0)
                }
            };
            let (m00, m01, m10, m11) = if flags & WE_HAVE_A_SCALE != 0 {
                let s = F2Dot14(r.i16()?);
                (s, F2Dot14(0), F2Dot14(0), s)
            } else if flags & WE_HAVE_AN_X_AND_Y_SCALE != 0 {
                let sx = F2Dot14(r.i16()?);
                let sy = F2Dot14(r.i16()?);
                (sx, F2Dot14(0), F2Dot14(0), sy)
            } else if flags & WE_HAVE_A_TWO_BY_TWO != 0 {
                (
                    F2Dot14(r.i16()?),
                    F2Dot14(r.i16()?),
                    F2Dot14(r.i16()?),
                    F2Dot14(r.i16()?),
                )
            } else {
                (
                    F2Dot14(0x4000),
                    F2Dot14(0),
                    F2Dot14(0),
                    F2Dot14(0x4000),
                )
            };
            if flags & WE_HAVE_INSTRUCTIONS != 0 {
                have_instr = true;
            }
            if flags & USE_MY_METRICS != 0 {
                use_my_metrics = true;
            }
            components.push(Component {
                glyph_index,
                flags,
                dx,
                dy,
                m00,
                m01,
                m10,
                m11,
            });
            if r.pos > end {
                return Err(Error::oob("component overrun", r.pos));
            }
        }

        let composite_instructions = if have_instr {
            let len = r.u16()? as usize;
            let mut v = Vec::with_capacity(len);
            for _ in 0..len {
                v.push(r.u8()?);
            }
            v
        } else {
            Vec::new()
        };

        Ok(RawGlyph {
            bbox,
            simple: None,
            components,
            composite_instructions,
            use_my_metrics,
        })
    }

    /// 递归展平复合字形为单一简单字形(字体单位)。
    pub fn flatten(&self, glyph: u16) -> Result<SimpleGlyph> {
        let mut out = SimpleGlyph::default();
        self.flatten_into(glyph, 1.0, 0.0, 0.0, 1.0, 0, &mut out)?;
        if out.points.is_empty() {
            return Ok(out);
        }
        // 重新计算 bbox
        let mut xmin = i16::MAX;
        let mut ymin = i16::MAX;
        let mut xmax = i16::MIN;
        let mut ymax = i16::MIN;
        for p in &out.points {
            xmin = xmin.min(p.x as i16);
            ymin = ymin.min(p.y as i16);
            xmax = xmax.max(p.x as i16);
            ymax = ymax.max(p.y as i16);
        }
        out.bbox = [xmin, ymin, xmax, ymax];
        Ok(out)
    }

    fn flatten_into(
        &self,
        glyph: u16,
        a: f32,
        b: f32,
        c: f32,
        d: f32,
        depth: u32,
        out: &mut SimpleGlyph,
    ) -> Result<()> {
        const MAX_DEPTH: u32 = 16;
        if depth > MAX_DEPTH {
            return Err(Error::Unsupported {
                what: "composite nesting too deep",
            });
        }
        let raw = self.load(glyph)?;
        if let Some(simple) = raw.simple {
            let base = out.points.len() as u16;
            for p in &simple.points {
                let x = a * p.x as f32 + c * p.y as f32;
                let y = b * p.x as f32 + d * p.y as f32;
                out.points.push(Point {
                    x: x.round() as i32,
                    y: y.round() as i32,
                    on_curve: p.on_curve,
                });
            }
            for e in simple.end_pts {
                out.end_pts.push(base + e);
            }
            if out.instructions.is_empty() {
                out.instructions = simple.instructions;
            }
            return Ok(());
        }
        for comp in &raw.components {
            let ca = comp.m00.to_f32();
            let cb = comp.m01.to_f32();
            let cc = comp.m10.to_f32();
            let cd = comp.m11.to_f32();
            // 子变换 = 父变换 × 组件变换;平移在父坐标系中累加
            let na = a * ca + c * cc;
            let nb = b * ca + d * cc;
            let nc = a * cb + c * cd;
            let nd = b * cb + d * cd;
            let tx = a * comp.dx as f32 + c * comp.dy as f32;
            let ty = b * comp.dx as f32 + d * comp.dy as f32;
            let before = out.points.len();
            self.flatten_into(comp.glyph_index, na, nb, nc, nd, depth + 1, out)?;
            for p in &mut out.points[before..] {
                p.x += tx.round() as i32;
                p.y += ty.round() as i32;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tables::{Cmap, Head, Hhea, Hmtx, Maxp};

    struct Face {
        data: Vec<u8>,
    }

    impl Face {
        fn open(path: &str) -> Self {
            Face {
                data: std::fs::read(path).unwrap(),
            }
        }
        fn glyf(&self) -> (FontFile<'_>, Head, Maxp, GlyfTable<'_>, Cmap<'_>) {
            let f = FontFile::parse(&self.data, 0).unwrap();
            let head = Head::parse(&f).unwrap();
            let maxp = Maxp::parse(&f).unwrap();
            let g = GlyfTable::new(&f, head, maxp.num_glyphs).unwrap();
            let cmap = Cmap::parse(&f).unwrap();
            (f, head, maxp, g, cmap)
        }
    }

    #[test]
    fn loads_simple_latin_glyph() {
        let face = Face::open("C:/Windows/Fonts/arial.ttf");
        let (_f, _h, _m, glyf, cmap) = face.glyf();
        let g = cmap.glyph_id('A' as u32).unwrap();
        let raw = glyf.load(g).unwrap();
        let s = raw.simple.expect("A is simple");
        assert!(s.points.len() > 5);
        assert!(!s.end_pts.is_empty());
        // A 一般有 2~3 条轮廓(外轮廓 + 三角孔)
        assert!(s.end_pts.len() >= 2);
    }

    #[test]
    fn empty_glyph_for_space() {
        let face = Face::open("C:/Windows/Fonts/arial.ttf");
        let (_f, _h, _m, glyf, cmap) = face.glyf();
        let g = cmap.glyph_id(' ' as u32).unwrap();
        assert!(glyf.glyph_range(g).unwrap().is_none());
        assert!(glyf.load(g).unwrap().is_empty());
    }

    #[test]
    fn composite_glyph_flattens() {
        // Arial 的 Å(U+00C5)通常是复合字形(A + 环)
        let face = Face::open("C:/Windows/Fonts/arial.ttf");
        let (_f, _h, _m, glyf, cmap) = face.glyf();
        let g = cmap.glyph_id(0xC5).unwrap();
        let raw = glyf.load(g).unwrap();
        assert!(raw.is_composite(), "Å expected composite");
        assert!(raw.components.len() >= 2);
        let flat = glyf.flatten(g).unwrap();
        assert!(flat.points.len() > 5);
        assert!(!flat.end_pts.is_empty());
    }

    #[test]
    fn cjk_glyph_has_many_contours() {
        let face = Face::open("C:/Windows/Fonts/simhei.ttf");
        let (_f, _h, _m, glyf, cmap) = face.glyf();
        let g = cmap.glyph_id('永' as u32).unwrap();
        let raw = glyf.load(g).unwrap();
        let s = raw.simple.expect("han glyph simple");
        assert!(s.end_pts.len() >= 3, "永 should have several contours");
    }

    #[test]
    fn metrics_are_sane() {
        let face = Face::open("C:/Windows/Fonts/arial.ttf");
        let (f, _h, _m, _g, cmap) = face.glyf();
        let hhea = Hhea::parse(&f).unwrap();
        let hmtx = Hmtx::parse(&f, &hhea).unwrap();
        let ga = cmap.glyph_id('A' as u32).unwrap();
        let (adv_a, _) = hmtx.metrics(ga);
        let gi = cmap.glyph_id('i' as u32).unwrap();
        let (adv_i, _) = hmtx.metrics(gi);
        assert!(adv_a > adv_i);
    }
}
