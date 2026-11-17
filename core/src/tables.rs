//! SFNT table directory and the small mandatory tables (head, maxp, hhea, hmtx).

use crate::reader::Reader;
use crate::error::{Error, Result};

#[derive(Clone, Copy)]
pub struct TableRecord {
    pub tag: [u8; 4],
    pub offset: usize,
    pub length: usize,
}

impl TableRecord {
    pub fn tag_eq(&self, tag: &[u8; 4]) -> bool {
        &self.tag == tag
    }
}

#[derive(Clone, Copy, Default)]
pub struct Head {
    pub units_per_em: u16,
    pub x_min: i16,
    pub y_min: i16,
    pub x_max: i16,
    pub y_max: i16,
    /// 0 = short (u16/2) loca entries, 1 = long (i32) entries.
    pub index_to_loc_format: i16,
}

#[derive(Clone, Copy, Default)]
pub struct Maxp {
    pub num_glyphs: u16,
    pub max_twilight_points: u16,
    pub max_storage: u16,
    pub max_stack_elements: u16,
    pub max_function_defs: u16,
    pub max_instruction_defs: u16,
    pub max_component_elements: u16,
    pub max_component_depth: u16,
    pub max_points: u16,
    pub max_contours: u16,
}

#[derive(Clone, Copy, Default)]
pub struct Hhea {
    pub ascender: i16,
    pub descender: i16,
    pub line_gap: i16,
    pub number_of_h_metrics: u16,
}

/// One long horizontal metric; remaining glyphs reuse the last advance width.
#[derive(Clone, Copy, Default, Debug)]
pub struct LongMetric {
    pub advance_width: u16,
    pub lsb: i16,
}

pub fn parse_directory(data: &[u8]) -> Result<Vec<TableRecord>> {
    let mut r = Reader::new(data);
    let sfnt_version = r.read_u32()?;
    match sfnt_version {
        0x00010000 | 0x74727565 /* 'true' */ | 0x4F54544F /* 'OTTO' */ | 0x74746366 /* 'ttcf' */ => {}
        _ => return Err(Error::NotTrueType),
    }
    let num_tables = r.read_u16()? as usize;
    // searchRange, entrySelector, rangeShift — not needed.
    r.skip(6)?;
    let mut tables = Vec::with_capacity(num_tables.min(64));
    for _ in 0..num_tables {
        let tag = r.read_tag()?;
        let _checksum = r.read_u32()?;
        let offset = r.read_u32()? as usize;
        let length = r.read_u32()? as usize;
        if offset.checked_add(length).map_or(true, |end| end > data.len()) {
            return Err(Error::BadOffset);
        }
        tables.push(TableRecord { tag, offset, length });
    }
    Ok(tables)
}

pub fn find_table<'a>(tables: &'a [TableRecord], tag: &[u8; 4]) -> Result<&'a TableRecord> {
    tables
        .iter()
        .find(|t| t.tag_eq(tag))
        .ok_or(Error::MissingTable)
}

pub fn table_bytes<'a>(data: &'a [u8], rec: &TableRecord) -> &'a [u8] {
    &data[rec.offset..rec.offset + rec.length]
}

pub fn parse_head(data: &[u8], rec: &TableRecord) -> Result<Head> {
    let t = table_bytes(data, rec);
    let mut r = Reader::at(t, 0);
    let version = r.read_u32()?;
    if version != 0x00010000 && version != 0x00020000 {
        return Err(Error::Unsupported);
    }
    r.skip(14)?; // fontRevision(4) checksumAdjusted(4) magic(4) flags(2)
    let units_per_em = r.read_u16()?;
    r.skip(16)?; // created/modified longdatetime
    let x_min = r.read_i16()?;
    let y_min = r.read_i16()?;
    let x_max = r.read_i16()?;
    let y_max = r.read_i16()?;
    // bbox ends at head offset 44; then macStyle(2), lowestRecPPEM(2),
    // fontDirectionHint(2) = 6 bytes, and indexToLocFormat is at offset 50.
    r.skip(6)?;
    let index_to_loc_format = r.read_i16()?;
    if index_to_loc_format != 0 && index_to_loc_format != 1 {
        return Err(Error::Unsupported);
    }
    Ok(Head {
        units_per_em,
        x_min,
        y_min,
        x_max,
        y_max,
        index_to_loc_format,
    })
}

pub fn parse_maxp(data: &[u8], rec: &TableRecord) -> Result<Maxp> {
    let t = table_bytes(data, rec);
    let mut r = Reader::at(t, 0);
    let version = r.read_u32()?;
    let num_glyphs = r.read_u16()?;
    let mut m = Maxp {
        num_glyphs,
        ..Default::default()
    };
    if version == 0x00010000 {
        // Layout after numGlyphs (all u16):
        // maxPoints, maxContours, maxCompositePoints, maxCompositeContours,
        // maxZones, maxTwilightPoints, maxStorage, maxFunctionDefs,
        // maxInstructionDefs, maxStackElements, maxSizeOfInstructions,
        // maxComponentElements, maxComponentDepth
        m.max_points = r.read_u16()?;
        m.max_contours = r.read_u16()?;
        let _max_composite_points = r.read_u16()?;
        let _max_composite_contours = r.read_u16()?;
        let _max_zones = r.read_u16()?;
        m.max_twilight_points = r.read_u16()?;
        m.max_storage = r.read_u16()?;
        m.max_function_defs = r.read_u16()?;
        m.max_instruction_defs = r.read_u16()?;
        m.max_stack_elements = r.read_u16()?;
        let _max_size_of_instructions = r.read_u16()?;
        m.max_component_elements = r.read_u16()?;
        m.max_component_depth = r.read_u16()?;
    }
    Ok(m)
}

pub fn parse_hhea(data: &[u8], rec: &TableRecord) -> Result<Hhea> {
    let t = table_bytes(data, rec);
    let mut r = Reader::at(t, 0);
    r.skip(4)?; // version
    let ascender = r.read_i16()?;
    let descender = r.read_i16()?;
    let line_gap = r.read_i16()?;
    r.skip(34 - 10)?; // advanceWidthMax .. caretOffset + reserved10 + metricDataFormat
    let number_of_h_metrics = r.read_u16()?;
    Ok(Hhea {
        ascender,
        descender,
        line_gap,
        number_of_h_metrics,
    })
}

pub fn parse_hmtx(
    data: &[u8],
    rec: &TableRecord,
    num_glyphs: u16,
    num_long: u16,
) -> Result<Vec<LongMetric>> {
    let t = table_bytes(data, rec);
    let mut r = Reader::at(t, 0);
    let num_long = (num_long as usize).min(num_glyphs as usize);
    let mut metrics = Vec::with_capacity(num_glyphs as usize);
    for _ in 0..num_long {
        let advance_width = r.read_u16()?;
        let lsb = r.read_i16()?;
        metrics.push(LongMetric {
            advance_width,
            lsb,
        });
    }
    // Remaining glyphs inherit the last advance width; their lsbs follow.
    let last_advance = metrics.last().copied().unwrap_or_default().advance_width;
    for _ in num_long..num_glyphs as usize {
        let lsb = r.read_i16()?;
        metrics.push(LongMetric {
            advance_width: last_advance,
            lsb,
        });
    }
    Ok(metrics)
}

/// Returns the byte range of glyph `gid` inside the glyf table.
pub fn glyph_range(
    data: &[u8],
    loca_rec: &TableRecord,
    glyf_rec: &TableRecord,
    gid: u16,
    num_glyphs: u16,
    long_loca: bool,
) -> Result<Option<core::ops::Range<usize>>> {
    if gid >= num_glyphs {
        return Err(Error::BadGlyphIndex);
    }
    let loca = table_bytes(data, loca_rec);
    let (start, end) = if long_loca {
        let s = gid as usize * 4;
        let mut rs = Reader::at(loca, s);
        let start = rs.read_i32()? as usize;
        let end = rs.read_i32()? as usize;
        (start, end)
    } else {
        let s = gid as usize * 2;
        let mut rs = Reader::at(loca, s);
        let start = rs.read_u16()? as usize * 2;
        let end = rs.read_u16()? as usize * 2;
        (start, end)
    };
    if start == end {
        return Ok(None); // empty glyph (space, etc.)
    }
    if end < start || glyf_rec.length < end {
        return Err(Error::BadOffset);
    }
    Ok(Some(glyf_rec.offset + start..glyf_rec.offset + end))
}
