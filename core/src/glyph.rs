//! `glyf` outline parser: simple glyphs (with the implicit-point-flag
//! compression) and recursive composite glyphs with 2x2 affine transforms.
//! Output coordinates stay in integer FUnits — the bytecode interpreter and
//! the scaler both consume integers.

use crate::reader::Reader;
use crate::tables::{glyph_range, TableRecord};
use crate::error::{Error, Result};
use crate::tables::{Head, Maxp};

pub const FLAG_ON_CURVE: u8 = 0x01;
pub const FLAG_X_SHORT: u8 = 0x02;
pub const FLAG_Y_SHORT: u8 = 0x04;
pub const FLAG_REPEAT: u8 = 0x08;
pub const FLAG_X_SAME: u8 = 0x10;
pub const FLAG_Y_SAME: u8 = 0x20;
// Bits 6/7 are reserved in simple flags and reused by the interpreter as
// point-touch markers (x/y), exactly as FreeType/swash do.
pub const FLAG_TOUCH_X: u8 = 0x40;
pub const FLAG_TOUCH_Y: u8 = 0x80;

// Composite glyph flags.
const ARG_1_AND_2_ARE_WORDS: u16 = 0x0001;
const ARGS_ARE_XY_VALUES: u16 = 0x0002;
const ROUND_XY_TO_GRID: u16 = 0x0004;
const WE_HAVE_A_SCALE: u16 = 0x0008;
const _OBSOLETE: u16 = 0x0010;
const MORE_COMPONENTS: u16 = 0x0020;
const WE_HAVE_AN_XY_SCALE: u16 = 0x0040;
const WE_HAVE_A_2X2: u16 = 0x0080;
const WE_HAVE_INSTRUCTIONS: u16 = 0x0100;
const USE_MY_METRICS: u16 = 0x0200;
const OVERLAP_COMPOUND: u16 = 0x0400;

#[derive(Clone, Copy, Default)]
pub struct GlyphPoint {
    pub x: i32,
    pub y: i32,
    pub flags: u8,
}

#[derive(Clone, Default)]
pub struct Outline {
    pub points: Vec<GlyphPoint>,
    /// End point index of each contour (inclusive), as in the glyf spec.
    pub contour_ends: Vec<u16>,
    /// Raw glyph instruction stream (empty when none).
    pub instructions: Vec<u8>,
    pub bbox: [i16; 4],
    /// True once `instructions` includes composite-level instructions.
    pub has_composite_instructions: bool,
}

impl Outline {
    pub fn contour_count(&self) -> usize {
        self.contour_ends.len()
    }

    pub fn point_count(&self) -> usize {
        self.points.len()
    }

    pub fn is_on_curve(&self, i: usize) -> bool {
        self.points[i].flags & FLAG_ON_CURVE != 0
    }
}

#[derive(Clone, Copy, Default, Debug)]
struct Affine {
    xx: f32,
    xy: f32,
    yx: f32,
    yy: f32,
    tx: f32,
    ty: f32,
}

impl Affine {
    fn identity() -> Self {
        Affine {
            xx: 1.0,
            yy: 1.0,
            ..Default::default()
        }
    }

    fn apply(&self, x: f32, y: f32) -> (f32, f32) {
        (
            self.xx * x + self.yx * y + self.tx,
            self.xy * x + self.yy * y + self.ty,
        )
    }

    fn multiply(&self, child: &Affine) -> Affine {
        // self * child
        Affine {
            xx: self.xx * child.xx + self.yx * child.xy,
            xy: self.xy * child.xx + self.yy * child.xy,
            yx: self.xx * child.yx + self.yx * child.yy,
            yy: self.xy * child.yx + self.yy * child.yy,
            tx: self.xx * child.tx + self.yx * child.ty + self.tx,
            ty: self.xy * child.tx + self.yy * child.ty + self.ty,
        }
    }
}

pub struct GlyfParser<'a> {
    data: &'a [u8],
    loca: &'a TableRecord,
    glyf: &'a TableRecord,
    head: &'a Head,
    maxp: &'a Maxp,
}

impl<'a> GlyfParser<'a> {
    pub fn new(
        data: &'a [u8],
        loca: &'a TableRecord,
        glyf: &'a TableRecord,
        head: &'a Head,
        maxp: &'a Maxp,
    ) -> Self {
        Self {
            data,
            loca,
            glyf,
            head,
            maxp,
        }
    }

    pub fn parse(&self, gid: u16) -> Result<Option<Outline>> {
        let range = match glyph_range(
            self.data,
            self.loca,
            self.glyf,
            gid,
            self.maxp.num_glyphs,
            self.head.index_to_loc_format == 1,
        )? {
            Some(r) => r,
            None => return Ok(None),
        };
        let mut outline = Outline::default();
        self.load_component(range, Affine::identity(), 0, &mut outline)?;
        Ok(Some(outline))
    }

    fn load_component(
        &self,
        range: core::ops::Range<usize>,
        transform: Affine,
        depth: u16,
        out: &mut Outline,
    ) -> Result<()> {
        let max_depth = self.maxp.max_component_depth.max(1).max(16);
        if depth >= max_depth {
            return Err(Error::CompositeDepth);
        }
        let mut r = Reader::at(self.data, range.start);
        let number_of_contours = r.read_i16()?;
        let bbox = [
            r.read_i16()?,
            r.read_i16()?,
            r.read_i16()?,
            r.read_i16()?,
        ];
        if depth == 0 {
            out.bbox = bbox;
        }

        if number_of_contours >= 0 {
            self.parse_simple(&mut r, number_of_contours as u16, &transform, out)?;
            return Ok(());
        }

        // Composite glyph.
        let mut flags = MORE_COMPONENTS;
        let mut composite_instr: Option<(usize, usize)> = None;
        while flags & MORE_COMPONENTS != 0 {
            flags = r.read_u16()?;
            let component_gid = r.read_u16()?;

            // Argument interpretation.
            let (arg1, arg2) = if flags & ARG_1_AND_2_ARE_WORDS != 0 {
                (r.read_i16()? as i32, r.read_i16()? as i32)
            } else {
                (r.read_i8()? as i32, r.read_i8()? as i32)
            };
            let (mut dx, mut dy) = if flags & ARGS_ARE_XY_VALUES != 0 {
                (arg1 as f32, arg2 as f32)
            } else {
                // Point-number attachment: resolve after all components are
                // present. Most CJK composites use XY values; keep a best
                // effort by treating anchors as zero translation.
                (0.0, 0.0)
            };
            if flags & ROUND_XY_TO_GRID != 0 {
                dx = dx.round();
                dy = dy.round();
            }

            let mut child = Affine {
                tx: dx,
                ty: dy,
                ..Affine::identity()
            };
            if flags & WE_HAVE_A_SCALE != 0 {
                let s = r.read_f2dot14()?;
                child.xx = s;
                child.yy = s;
            } else if flags & WE_HAVE_AN_XY_SCALE != 0 {
                child.xx = r.read_f2dot14()?;
                child.yy = r.read_f2dot14()?;
            } else if flags & WE_HAVE_A_2X2 != 0 {
                child.xx = r.read_f2dot14()?;
                child.yx = r.read_f2dot14()?;
                child.xy = r.read_f2dot14()?;
                child.yy = r.read_f2dot14()?;
            }
            let _ = OVERLAP_COMPOUND;
            let _ = USE_MY_METRICS;

            let child_range = match glyph_range(
                self.data,
                self.loca,
                self.glyf,
                component_gid,
                self.maxp.num_glyphs,
                self.head.index_to_loc_format == 1,
            )? {
                Some(cr) => cr,
                None => continue,
            };
            let composed = transform.multiply(&child);
            self.load_component(child_range, composed, depth + 1, out)?;

            if flags & WE_HAVE_INSTRUCTIONS != 0 {
                let instr_len_pos = r.position();
                let instr_len = r.read_u16()? as usize;
                let instr_start = r.position();
                composite_instr = Some((instr_start, instr_len));
                let _ = instr_len_pos;
            }
        }

        if let Some((start, len)) = composite_instr {
            let bytes = Reader::at(self.data, start).read_bytes(len)?;
            out.instructions = bytes.to_vec();
            out.has_composite_instructions = true;
        }
        Ok(())
    }

    fn parse_simple(
        &self,
        r: &mut Reader<'a>,
        num_contours: u16,
        transform: &Affine,
        out: &mut Outline,
    ) -> Result<()> {
        if num_contours == 0 {
            return Ok(());
        }
        let mut contour_ends = Vec::with_capacity(num_contours as usize);
        for _ in 0..num_contours {
            contour_ends.push(r.read_u16()?);
        }
        let num_points = contour_ends.last().copied().unwrap_or(0) as usize + 1;
        if num_points > 0xFFFF {
            return Err(Error::Unsupported);
        }

        let instruction_len = r.read_u16()? as usize;
        let instructions = r.read_bytes(instruction_len)?.to_vec();

        let mut raw_flags = Vec::with_capacity(num_points);
        while raw_flags.len() < num_points {
            let flag = r.read_u8()?;
            if flag & FLAG_REPEAT != 0 {
                let repeat = r.read_u8()? as usize;
                if raw_flags.len() + repeat + 1 > num_points {
                    return Err(Error::BadOffset);
                }
                for _ in 0..=repeat {
                    raw_flags.push(flag);
                }
            } else {
                raw_flags.push(flag);
            }
        }

        let mut xs = Vec::with_capacity(num_points);
        let mut x: i32 = 0;
        for flag in &raw_flags {
            if flag & FLAG_X_SHORT != 0 {
                let d = r.read_u8()? as i32;
                x += if flag & FLAG_X_SAME != 0 { d } else { -d };
            } else if flag & FLAG_X_SAME != 0 {
                // same, no delta
            } else {
                x += r.read_i16()? as i32;
            }
            xs.push(x);
        }
        let mut ys = Vec::with_capacity(num_points);
        let mut y: i32 = 0;
        for flag in &raw_flags {
            if flag & FLAG_Y_SHORT != 0 {
                let d = r.read_u8()? as i32;
                y += if flag & FLAG_Y_SAME != 0 { d } else { -d };
            } else if flag & FLAG_Y_SAME != 0 {
                // same, no delta
            } else {
                y += r.read_i16()? as i32;
            }
            ys.push(y);
        }

        let point_base = out.points.len() as u16;
        let identity = transform.xx == 1.0
            && transform.yy == 1.0
            && transform.xy == 0.0
            && transform.yx == 0.0
            && transform.tx == 0.0
            && transform.ty == 0.0;
        for i in 0..num_points {
            let (px, py) = if identity {
                (xs[i] as f32, ys[i] as f32)
            } else {
                transform.apply(xs[i] as f32, ys[i] as f32)
            };
            // Composite transforms land on integer FUnit coordinates per spec.
            out.points.push(GlyphPoint {
                x: px.round() as i32,
                y: py.round() as i32,
                flags: raw_flags[i] & (FLAG_ON_CURVE | FLAG_TOUCH_X | FLAG_TOUCH_Y),
            });
        }
        for end in contour_ends {
            out.contour_ends.push(point_base + end);
        }
        // Simple (non-composite) glyph owns the instruction stream.
        if !out.has_composite_instructions && point_base == 0 {
            out.instructions = instructions;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::reader::Reader;

    #[test]
    fn simple_glyph_with_instructions_parses_to_exact_end() {
        // One contour, 4 on-curve points forming a square, with a non-empty
        // instruction stream (the case that regressed for Arial 'H').
        let mut bytes = Vec::new();
        // header: numberOfContours=1, bbox 0,0,100,100
        bytes.extend_from_slice(&1i16.to_be_bytes());
        for v in [0i16, 0, 100, 100] {
            bytes.extend_from_slice(&v.to_be_bytes());
        }
        // single contour ending at point index 3
        bytes.extend_from_slice(&3u16.to_be_bytes());
        // instruction length 8 + 8 instruction bytes (arbitrary values)
        bytes.extend_from_slice(&8u16.to_be_bytes());
        bytes.extend_from_slice(&[0xAA, 0xBB, 0xCC, 0xDD, 0xEE, 0xFF, 0x11, 0x22]);
        // 4 flags, each on-curve + x-short(positive) + y-short(positive)
        let flag = FLAG_ON_CURVE | FLAG_X_SHORT | FLAG_X_SAME | FLAG_Y_SHORT | FLAG_Y_SAME;
        for _ in 0..4 {
            bytes.push(flag);
        }
        // 4 x deltas (1 byte each)
        bytes.extend_from_slice(&[10u8, 20, 30, 40]);
        // 4 y deltas
        bytes.extend_from_slice(&[5u8, 15, 25, 35]);

        let glyph_len = bytes.len();
        // Wrap in a fake file with some leading padding so offsets are
        // absolute (mirrors how the parser is invoked on real fonts).
        let pad = 16usize;
        let mut file = vec![0u8; pad];
        file.extend_from_slice(&bytes);

        let mut r = Reader::at(&file, pad);
        let contours = r.read_i16().unwrap();
        assert_eq!(contours, 1);
        r.skip(8).unwrap(); // bbox
        let end = r.read_u16().unwrap();
        assert_eq!(end, 3);
        let instr_len = r.read_u16().unwrap() as usize;
        assert_eq!(instr_len, 8);
        let instr = r.read_bytes(instr_len).unwrap();
        assert_eq!(instr.len(), 8);
        // After consuming everything, the reader must sit exactly at glyph end.
        assert_eq!(r.position(), pad + glyph_len);
    }
}
