//! 必备 SFNT 表解析:head / maxp / hhea / hmtx / cmap。
//!
//! 全部以借用视图方式工作,解析结果只保存少量标量和偏移,
//! 大表(cmap/hmtx)查询时再回到原始字节随机访问。

use crate::font::{FontFile, Reader};
use crate::Result;

#[derive(Clone, Copy, Debug)]
pub struct Head {
    pub units_per_em: u16,
    pub index_to_loc_format: i16,
    pub bbox: [i16; 4],
}

impl Head {
    pub fn parse(font: &FontFile) -> Result<Head> {
        let raw = font.table_bytes(b"head")?;
        let r = |o: usize| Reader::peek_u16_at(raw, o);
        let units_per_em = r(18)?;
        let bbox = [
            Reader::peek_i16_at(raw, 36)?,
            Reader::peek_i16_at(raw, 38)?,
            Reader::peek_i16_at(raw, 40)?,
            Reader::peek_i16_at(raw, 42)?,
        ];
        let index_to_loc_format = Reader::peek_i16_at(raw, 50)?;
        Ok(Head {
            units_per_em,
            index_to_loc_format,
            bbox,
        })
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Maxp {
    pub num_glyphs: u16,
    pub max_points: u16,
    pub max_contours: u16,
    pub max_composite_points: u16,
    pub max_composite_contours: u16,
    pub max_stack_elements: u16,
    pub max_storage: u16,
    pub max_function_defs: u16,
    pub max_instruction_defs: u16,
    pub max_twilight_points: u16,
}

impl Maxp {
    pub fn parse(font: &FontFile) -> Result<Maxp> {
        let raw = font.table_bytes(b"maxp")?;
        let version = Reader::peek_u32_at(raw, 0)?;
        let num_glyphs = Reader::peek_u16_at(raw, 4)?;
        let mut m = Maxp {
            num_glyphs,
            max_points: 0,
            max_contours: 0,
            max_composite_points: 0,
            max_composite_contours: 0,
            max_stack_elements: 0,
            max_storage: 0,
            max_function_defs: 0,
            max_instruction_defs: 0,
            max_twilight_points: 0,
        };
        // 0.5 版只有 numGlyphs;1.0 版带完整 TrueType 极值(表长恰好 32 字节)
        if version == 0x00010000 && raw.len() >= 32 {
            m.max_points = Reader::peek_u16_at(raw, 6)?;
            m.max_contours = Reader::peek_u16_at(raw, 8)?;
            m.max_composite_points = Reader::peek_u16_at(raw, 10)?;
            m.max_composite_contours = Reader::peek_u16_at(raw, 12)?;
            // 14: maxZones
            m.max_twilight_points = Reader::peek_u16_at(raw, 16)?;
            m.max_storage = Reader::peek_u16_at(raw, 18)?;
            m.max_function_defs = Reader::peek_u16_at(raw, 20)?;
            m.max_instruction_defs = Reader::peek_u16_at(raw, 22)?;
            m.max_stack_elements = Reader::peek_u16_at(raw, 24)?;
            // 26: maxSizeOfInstructions, 28: maxComponentElements, 30: maxComponentDepth
        }
        Ok(m)
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Hhea {
    pub ascent: i16,
    pub descent: i16,
    pub line_gap: i16,
    pub number_of_h_metrics: u16,
}

impl Hhea {
    pub fn parse(font: &FontFile) -> Result<Hhea> {
        let raw = font.table_bytes(b"hhea")?;
        Ok(Hhea {
            ascent: Reader::peek_i16_at(raw, 4)?,
            descent: Reader::peek_i16_at(raw, 6)?,
            line_gap: Reader::peek_i16_at(raw, 8)?,
            number_of_h_metrics: Reader::peek_u16_at(raw, 34)?,
        })
    }
}

/// hmtx 借用视图。
pub struct Hmtx<'a> {
    raw: &'a [u8],
    num_long: usize,
}

impl<'a> Hmtx<'a> {
    pub fn parse(font: &FontFile<'a>, hhea: &Hhea) -> Result<Hmtx<'a>> {
        Ok(Hmtx {
            raw: font.table_bytes(b"hmtx")?,
            num_long: hhea.number_of_h_metrics as usize,
        })
    }

    /// 返回 (advance_width, lsb),字体单位。
    pub fn metrics(&self, glyph: u16) -> (i32, i16) {
        let g = glyph as usize;
        if g < self.num_long {
            let o = g * 4;
            let advance = Reader::peek_u16_at(self.raw, o).unwrap_or(0) as i32;
            let lsb = Reader::peek_i16_at(self.raw, o + 2).unwrap_or(0);
            (advance, lsb)
        } else {
            let last = (self.num_long - 1) * 4;
            let advance = Reader::peek_u16_at(self.raw, last).unwrap_or(0) as i32;
            let lsb_off = self.num_long * 4 + (g - self.num_long) * 2;
            let lsb = Reader::peek_i16_at(self.raw, lsb_off).unwrap_or(0);
            (advance, lsb)
        }
    }
}

/// cmap 子表定位信息(只记录偏移,查询时再解析)。
#[derive(Clone, Copy)]
struct CmapSubtable {
    platform: u16,
    encoding: u16,
    offset: usize,
    format: u16,
}

pub struct Cmap<'a> {
    raw: &'a [u8],
    sub: CmapSubtable,
}

impl<'a> Cmap<'a> {
    pub fn parse(font: &FontFile<'a>) -> Result<Cmap<'a>> {
        let raw = font.table_bytes(b"cmap")?;
        let num = Reader::peek_u16_at(raw, 2)? as usize;
        let mut subs = Vec::new();
        for i in 0..num {
            let rec = 4 + i * 8;
            let platform = Reader::peek_u16_at(raw, rec)?;
            let encoding = Reader::peek_u16_at(raw, rec + 2)?;
            let off = Reader::peek_u32_at(raw, rec + 4)? as usize;
            if off + 2 > raw.len() {
                continue;
            }
            let format = Reader::peek_u16_at(raw, off)?;
            subs.push(CmapSubtable {
                platform,
                encoding,
                offset: off,
                format,
            });
        }
        // 选择优先级:31-bit 全覆盖(format 12)> Windows BMP(format 4)
        // > Unicode BMP(format 4)> 其它任意可用子表。
        let score = |s: &CmapSubtable| -> i32 {
            match (s.format, s.platform, s.encoding) {
                (12, 3, 10) => 100,
                (12, 0, _) => 95,
                (4, 3, 1) => 80,
                (4, 0, _) => 70,
                (6, 3, 1) => 60,
                (6, 0, _) => 55,
                (0, _, _) => 40,
                _ => 10,
            }
        };
        let best = subs
            .iter()
            .copied()
            .max_by_key(score)
            .ok_or(crate::Error::Unsupported { what: "cmap subtable" })?;
        Ok(Cmap { raw, sub: best })
    }

    /// Unicode 码点 -> 字形 ID,缺字返回 0。
    pub fn glyph_id(&self, codepoint: u32) -> Result<u16> {
        let raw = self.raw;
        let base = self.sub.offset;
        Ok(match self.sub.format {
            0 => {
                if codepoint < 256 {
                    let o = base + 6 + codepoint as usize;
                    Reader::peek_u8_at(raw, o)? as u16
                } else {
                    0
                }
            }
            4 => Self::lookup_format4(raw, base, codepoint)?,
            6 => Self::lookup_format6(raw, base, codepoint)?,
            12 => Self::lookup_format12(raw, base, codepoint)?,
            _ => 0,
        })
    }

    fn lookup_format4(raw: &[u8], base: usize, cp: u32) -> Result<u16> {
        if cp > 0xFFFF {
            return Ok(0);
        }
        let c = cp as u16;
        let seg_x2 = Reader::peek_u16_at(raw, base + 6)? as usize;
        let seg_count = seg_x2 / 2;
        let end_off = base + 14;
        let start_off = end_off + seg_x2 + 2;
        let delta_off = start_off + seg_x2;
        let range_off = delta_off + seg_x2;
        // 线性分段扫描(BMP 字体段数通常几十,无需二分也够快)
        for i in 0..seg_count {
            let end = Reader::peek_u16_at(raw, end_off + i * 2)?;
            if c <= end {
                let start = Reader::peek_u16_at(raw, start_off + i * 2)?;
                if c < start {
                    return Ok(0);
                }
                let delta = Reader::peek_i16_at(raw, delta_off + i * 2)?;
                let range = Reader::peek_u16_at(raw, range_off + i * 2)?;
                if range == 0 {
                    return Ok((c as i32 + delta as i32) as u16);
                }
                let gi_addr = range_off + i * 2 + range as usize + (c - start) as usize * 2;
                let g = Reader::peek_u16_at(raw, gi_addr)?;
                if g == 0 {
                    return Ok(0);
                }
                return Ok((g as i32 + delta as i32) as u16);
            }
        }
        Ok(0)
    }

    fn lookup_format6(raw: &[u8], base: usize, cp: u32) -> Result<u16> {
        if cp > 0xFFFF {
            return Ok(0);
        }
        let first = Reader::peek_u16_at(raw, base + 6)? as u32;
        let count = Reader::peek_u16_at(raw, base + 8)? as u32;
        if cp < first || cp >= first + count {
            return Ok(0);
        }
        Reader::peek_u16_at(raw, base + 10 + (cp - first) as usize * 2)
    }

    fn lookup_format12(raw: &[u8], base: usize, cp: u32) -> Result<u16> {
        let num_groups = Reader::peek_u32_at(raw, 12 + base)? as usize;
        let groups = base + 16;
        // 组数量大(中文字体上万组),用二分
        let mut lo = 0usize;
        let mut hi = num_groups;
        while lo < hi {
            let mid = (lo + hi) / 2;
            let g = groups + mid * 12;
            let start = Reader::peek_u32_at(raw, g)?;
            let end = Reader::peek_u32_at(raw, g + 4)?;
            if cp < start {
                hi = mid;
            } else if cp > end {
                lo = mid + 1;
            } else {
                let start_glyph = Reader::peek_u32_at(raw, g + 8)?;
                return Ok((start_glyph + cp - start) as u16);
            }
        }
        Ok(0)
    }
}

/// 字体单位 -> 26.6 像素的线性缩放系数。
///
/// 返回值是"每字体单位对应的 26.6 值",配合
/// [`F26Dot6::scale_font_units`](crate::fixed::F26Dot6::scale_font_units) 使用:
/// 该函数内部做 `units * scale / 64`,因此 `scale = ppem*64/upem`。
#[inline]
pub fn pixel_scale(units_per_em: u16, ppem: u16) -> i32 {
    ((ppem as i64 * 64 * 64) / units_per_em as i64) as i32
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::font::FontFile;

    #[test]
    fn arial_basic_tables() {
        let data = std::fs::read("C:/Windows/Fonts/arial.ttf").unwrap();
        let f = FontFile::parse(&data, 0).unwrap();
        let head = Head::parse(&f).unwrap();
        assert_eq!(head.units_per_em, 2048);
        let maxp = Maxp::parse(&f).unwrap();
        assert!(maxp.num_glyphs > 1000);
        let hhea = Hhea::parse(&f).unwrap();
        let hmtx = Hmtx::parse(&f, &hhea).unwrap();
        let cmap = Cmap::parse(&f).unwrap();
        let gid = cmap.glyph_id(b'A' as u32).unwrap();
        let (adv, _) = hmtx.metrics(gid);
        assert!(adv > 0);
        // hhea 行高字段单位为字体单位,应在合理量级
        assert!(hhea.ascent > 0 && hhea.descent < 0);
    }

    #[test]
    fn cmap_latin_and_cjk() {
        // Arial 含基本拉丁
        let data = std::fs::read("C:/Windows/Fonts/arial.ttf").unwrap();
        let f = FontFile::parse(&data, 0).unwrap();
        let cmap = Cmap::parse(&f).unwrap();
        let g_a = cmap.glyph_id(b'A' as u32).unwrap();
        assert!(g_a > 0);
        assert_eq!(cmap.glyph_id(0).unwrap(), 0);

        // 黑体含汉字(format 4 或 12)
        let data2 = std::fs::read("C:/Windows/Fonts/simhei.ttf").unwrap();
        let f2 = FontFile::parse(&data2, 0).unwrap();
        let cmap2 = Cmap::parse(&f2).unwrap();
        let g_han = cmap2.glyph_id('中' as u32).unwrap();
        assert!(g_han > 0);
        let g_han2 = cmap2.glyph_id('龘' as u32).unwrap();
        assert!(g_han2 > 0);
    }

    #[test]
    fn ttc_cmap() {
        let data = std::fs::read("C:/Windows/Fonts/simsun.ttc").unwrap();
        let f = FontFile::parse(&data, 0).unwrap();
        let cmap = Cmap::parse(&f).unwrap();
        assert!(cmap.glyph_id('国' as u32).unwrap() > 0);
    }
}
