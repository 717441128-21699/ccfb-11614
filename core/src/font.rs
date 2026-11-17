//! High-level font engine: owns the parsed tables, the hint interpreter and
//! the rasterizer, and serves glyph bitmaps on demand (no eager rasterization
//! — CJK fonts with tens of thousands of glyphs only pay for what is used).

use crate::cmap::Cmap;
use crate::error::{Error, Result};
use crate::glyph::{GlyfParser, Outline};
use crate::interp::Interpreter;
use crate::raster::Rasterizer;
use crate::reader::Reader;
use crate::tables::{
    find_table, parse_directory, parse_head, parse_hhea, parse_hmtx, parse_maxp, Head, Hhea,
    LongMetric, Maxp, TableRecord,
};

pub const TAG_CMAP: [u8; 4] = *b"cmap";
pub const TAG_GLYF: [u8; 4] = *b"glyf";
pub const TAG_LOCA: [u8; 4] = *b"loca";
pub const TAG_HEAD: [u8; 4] = *b"head";
pub const TAG_HHEA: [u8; 4] = *b"hhea";
pub const TAG_HMTX: [u8; 4] = *b"hmtx";
pub const TAG_MAXP: [u8; 4] = *b"maxp";
pub const TAG_CVT: [u8; 4] = *b"cvt ";
pub const TAG_FPGM: [u8; 4] = *b"fpgm";
pub const TAG_PREP: [u8; 4] = *b"prep";

/// Pixel-space placement of a rendered glyph inside its bitmap and on a line.
#[derive(Clone, Copy, Default, Debug)]
pub struct GlyphMetrics {
    pub width: u32,
    pub height: u32,
    /// Bitmap column of the leftmost covered pixel relative to the pen origin.
    pub left: i32,
    /// Bitmap row of the topmost covered pixel relative to the baseline (y-up).
    pub top: i32,
    /// Pen advance in pixels.
    pub advance: f32,
    /// Horizontal left side bearing in pixels.
    pub lsb: f32,
}

pub struct FontEngine {
    data: Vec<u8>,
    tables: Vec<TableRecord>,
    head: Head,
    maxp: Maxp,
    hhea: Hhea,
    metrics: Vec<LongMetric>,
    cmap: Option<Cmap>,
    cvt: Vec<i16>,
    fpgm: Vec<u8>,
    prep: Vec<u8>,
    interpreter: Option<Interpreter<'static>>,
    rasterizer: Rasterizer,
    ppem: u16,
    hinting: bool,
    // Reusable parse output, kept across calls to avoid reallocation.
    last_metrics: GlyphMetrics,
    last_fault: Option<crate::interp::FaultInfo>,
}

// SAFETY: the interpreter borrows data that outlives it (owned by `data`), but
// Rust cannot express that self-referential borrow. We construct it with a
// 'static lifetime over a stable allocation: `data` is a Vec<u8> that is never
// mutated or reallocated after `open`, so the bytes' address stays valid for
// the engine's lifetime. All access goes through this single owning struct.
unsafe fn extend_lifetime<T>(b: &[T]) -> &'static [T] {
    core::mem::transmute::<&[T], &'static [T]>(b)
}

unsafe fn extend_lifetime_ref<T>(b: &T) -> &'static T {
    core::mem::transmute::<&T, &'static T>(b)
}

impl FontEngine {
    pub fn open(bytes: Vec<u8>, font_index: u32) -> Result<FontEngine> {
        if bytes.len() < 12 {
            return Err(Error::NotTrueType);
        }
        // TTC collections: the leading 'ttcf' header points at per-face
        // table-directory offsets; a plain sfnt starts directly with a tag.
        let is_ttc = &bytes[0..4] == b"ttcf";
        let tables = if is_ttc {
            let mut r = Reader::at(&bytes, 4);
            let _major = r.read_u16()?;
            let _minor = r.read_u16()?;
            let num_fonts = r.read_u32()?;
            if font_index >= num_fonts {
                return Err(Error::InvalidArg);
            }
            // OffsetTable entries follow; skip to the requested face.
            for _ in 0..font_index {
                r.skip(4)?;
            }
            let dir_offset = r.read_u32()? as usize;
            parse_face_directory(&bytes, dir_offset)?
        } else {
            parse_directory(&bytes)?
        };
        let first_tag = tables.first().map(|t| t.tag).unwrap_or([0; 4]);
        if first_tag == [0; 4] {
            return Err(Error::NotTrueType);
        }

        let head_rec = find_table(&tables, &TAG_HEAD)?;
        let head = parse_head(&bytes, head_rec)?;
        let maxp = parse_maxp(&bytes, find_table(&tables, &TAG_MAXP)?)?;
        let hhea = parse_hhea(&bytes, find_table(&tables, &TAG_HHEA)?)?;
        let hmtx = parse_hmtx(
            &bytes,
            find_table(&tables, &TAG_HMTX)?,
            maxp.num_glyphs,
            hhea.number_of_h_metrics,
        )?;

        let cmap = if let Ok(rec) = find_table(&tables, &TAG_CMAP) {
            Cmap::parse(&bytes[rec.offset..rec.offset + rec.length]).ok()
        } else {
            None
        };

        let cvt = read_i16_table(&bytes, &tables, &TAG_CVT)?;
        let fpgm = read_u8_table(&bytes, &tables, &TAG_FPGM)?;
        let prep = read_u8_table(&bytes, &tables, &TAG_PREP)?;

        // Reject CFF/OpenType outlines explicitly: this engine targets glyf.
        if !is_ttc && &bytes[0..4] == b"OTTO" {
            return Err(Error::CffUnsupported);
        }

        let rasterizer = Rasterizer::new(1, 1);

        Ok(FontEngine {
            data: bytes,
            tables,
            head,
            maxp,
            hhea,
            metrics: hmtx,
            cmap,
            cvt,
            fpgm,
            prep,
            interpreter: None,
            rasterizer,
            ppem: 0,
            hinting: true,
            last_metrics: GlyphMetrics::default(),
            last_fault: None,
        })
    }

    pub fn num_glyphs(&self) -> u16 {
        self.maxp.num_glyphs
    }

    pub fn units_per_em(&self) -> u16 {
        self.head.units_per_em
    }

    pub fn codepoint_to_gid(&self, cp: u32) -> u16 {
        self.cmap.as_ref().map_or(0, |c| c.lookup(cp))
    }

    pub fn set_pixel_size(&mut self, ppem: u16) -> Result<()> {
        if ppem == 0 {
            return Err(Error::InvalidArg);
        }
        if ppem == self.ppem && self.interpreter.is_some() {
            return Ok(());
        }
        self.ppem = ppem;
        // Build the interpreter with the 'static view of the stable byte store.
        let cvt_static = unsafe { extend_lifetime(&self.cvt) };
        let fpgm_static = unsafe { extend_lifetime(&self.fpgm) };
        let prep_static = unsafe { extend_lifetime(&self.prep) };
        let maxp_static = unsafe { extend_lifetime_ref(&self.maxp) };
        let mut interp = Interpreter::new(
            cvt_static,
            fpgm_static,
            prep_static,
            maxp_static,
        );
        // Store before running so a fault's trace survives the error.
        let result = interp.set_size(ppem, self.head.units_per_em);
        self.interpreter = Some(interp);
        if let Err(e) = result {
            self.last_fault = self.interpreter.as_ref().and_then(|i| i.last_fault.clone());
            return Err(e);
        }
        Ok(())
    }

    pub fn fault_info(&self) -> Option<crate::interp::FaultInfo> {
        self.interpreter
            .as_ref()
            .and_then(|i| i.last_fault.clone())
            .or_else(|| self.last_fault.clone())
    }

    pub fn interpreter_ref(&self) -> Option<&Interpreter<'static>> {
        self.interpreter.as_ref()
    }

    pub fn trace_depths(&self) -> &[u32] {
        match &self.interpreter {
            Some(i) => i.trace_depth.as_slice(),
            None => &[],
        }
    }

    pub fn set_hinting(&mut self, enabled: bool) {
        self.hinting = enabled;
    }

    pub fn hinting_enabled(&self) -> bool {
        self.hinting
    }

    /// Render one glyph into a freshly sized alpha buffer. Returns
    /// `(alpha bytes, metrics)`. Empty glyphs return a zero-sized buffer.
    pub fn render_glyph(&mut self, gid: u16) -> Result<(Vec<u8>, GlyphMetrics)> {
        if gid >= self.maxp.num_glyphs {
            return Err(Error::BadGlyphIndex);
        }
        let loca = find_table(&self.tables, &TAG_LOCA)?;
        let glyf_rec = find_table(&self.tables, &TAG_GLYF)?;
        let parser = GlyfParser::new(
            &self.data,
            loca,
            glyf_rec,
            &self.head,
            &self.maxp,
        );
        let outline = match parser.parse(gid)? {
            Some(o) => o,
            None => {
                let m = self.unhinted_metrics(gid, &Outline::default());
                return Ok((Vec::new(), m));
            }
        };

        // Linear scale from FUnits to pixels (the 16.16 fixed factor cancels
        // the integer scaling, so we work directly in float here for the
        // unhinted path).
        let funit_to_px = self.ppem as f64 / self.head.units_per_em as f64;
        let pixel_points = if self.hinting {
            let interp = self.interpreter.as_mut().ok_or(Error::InvalidArg)?;
            // Per-glyph bytecode faults are non-fatal: the fpgm/prep programs
            // already ran (the high-value grid/CVT work), so a glyph whose
            // local instructions hit an unsupported edge falls back to
            // unhinted scaled outlines rather than failing the whole render.
            match interp.hint_outline(&outline) {
                Ok(()) => interp.hinted_pixel_points(),
                Err(_) => self.unhinted_points(&outline.points, funit_to_px),
            }
        } else {
            self.unhinted_points(&outline.points, funit_to_px)
        };

        // Tight pixel bounding box with one-cell AA padding on every side.
        let (min_x, min_y, max_x, max_y) = pixel_bounds(&pixel_points);
        let pad = 1.0;
        let left = (min_x.floor() as i32) - 1;
        let top = (max_y.ceil() as i32) + 1;
        let width = ((max_x.ceil() - min_x.floor()) as usize).max(1) + 2;
        let height = ((max_y.ceil() - min_y.floor()) as usize).max(1) + 2;
        let _ = pad;

        if width > 4096 || height > 4096 {
            return Err(Error::Unsupported);
        }
        self.rasterizer = Rasterizer::new(width, height);
        // Shift y-up points into bitmap-local coordinates (origin top-left).
        let shifted: Vec<(f64, f64)> = pixel_points
            .iter()
            .map(|(x, y)| (x - left as f64, *y - (top as f64 - height as f64)))
            .collect();
        let alpha = self.rasterizer.render(&outline, &shifted)?.to_vec();

        let mut metrics = self.unhinted_metrics(gid, &outline);
        metrics.width = width as u32;
        metrics.height = height as u32;
        metrics.left = left;
        metrics.top = top;
        self.last_metrics = metrics;
        Ok((alpha, metrics))
    }

    fn unhinted_points(
        &self,
        points: &[crate::glyph::GlyphPoint],
        funit_to_px: f64,
    ) -> Vec<(f64, f64)> {
        points
            .iter()
            .map(|p| (p.x as f64 * funit_to_px, p.y as f64 * funit_to_px))
            .collect()
    }

    fn unhinted_metrics(&self, gid: u16, _outline: &Outline) -> GlyphMetrics {
        let lm = self.metrics.get(gid as usize).copied().unwrap_or_default();
        let scale_factor = self.ppem as f32 / self.head.units_per_em as f32;
        GlyphMetrics {
            width: 0,
            height: 0,
            left: 0,
            top: 0,
            advance: lm.advance_width as f32 * scale_factor,
            lsb: lm.lsb as f32 * scale_factor,
        }
    }

    pub fn line_metrics(&self) -> (f32, f32, f32) {
        let s = self.ppem as f32 / self.head.units_per_em as f32;
        (
            self.hhea.ascender as f32 * s,
            self.hhea.descender as f32 * s,
            self.hhea.line_gap as f32 * s,
        )
    }
}

fn parse_face_directory(data: &[u8], offset: usize) -> Result<Vec<TableRecord>> {
    // Reuse the standard parser by pointing a temporary sfnt-like reader at
    // the face directory; parse_directory reads from offset 0, so wrap a slice
    // starting there (the offsets inside are still absolute file offsets).
    let mut r = Reader::at(data, offset);
    let _sfnt = r.read_u32()?;
    let num_tables = r.read_u16()? as usize;
    r.skip(6)?;
    let mut tables = Vec::with_capacity(num_tables.min(64));
    for _ in 0..num_tables {
        let tag = r.read_tag()?;
        let _checksum = r.read_u32()?;
        let toff = r.read_u32()? as usize;
        let length = r.read_u32()? as usize;
        if toff.checked_add(length).map_or(true, |end| end > data.len()) {
            return Err(Error::BadOffset);
        }
        tables.push(TableRecord {
            tag,
            offset: toff,
            length,
        });
    }
    Ok(tables)
}

fn read_i16_table(
    data: &[u8],
    tables: &[TableRecord],
    tag: &[u8; 4],
) -> Result<Vec<i16>> {
    let Ok(rec) = find_table(tables, tag) else {
        return Ok(Vec::new());
    };
    let t = &data[rec.offset..rec.offset + rec.length];
    let mut r = Reader::at(t, 0);
    let count = t.len() / 2;
    let mut out = Vec::with_capacity(count);
    for _ in 0..count {
        out.push(r.read_i16()?);
    }
    Ok(out)
}

fn read_u8_table(
    data: &[u8],
    tables: &[TableRecord],
    tag: &[u8; 4],
) -> Result<Vec<u8>> {
    let Ok(rec) = find_table(tables, tag) else {
        return Ok(Vec::new());
    };
    Ok(data[rec.offset..rec.offset + rec.length].to_vec())
}

fn pixel_bounds(pts: &[(f64, f64)]) -> (f64, f64, f64, f64) {
    let mut min_x = f64::INFINITY;
    let mut min_y = f64::INFINITY;
    let mut max_x = f64::NEG_INFINITY;
    let mut max_y = f64::NEG_INFINITY;
    for (x, y) in pts {
        min_x = min_x.min(*x);
        min_y = min_y.min(*y);
        max_x = max_x.max(*x);
        max_y = max_y.max(*y);
    }
    if min_x.is_infinite() {
        return (0.0, 0.0, 0.0, 0.0);
    }
    (min_x, min_y, max_x, max_y)
}
