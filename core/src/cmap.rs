//! `cmap` parsing. Supports the three subtables that matter in practice:
//! format 0 (byte encoding table), format 4 (UCS-2 segmented mapping) and
//! format 12 (UCS-4 grouped 32-bit mapping). Subtable selection follows the
//! usual platform/encoding preference (Windows UCS-4 > Windows Unicode >
//! Mac Roman/Unicode).

use crate::reader::Reader;
use crate::error::{Error, Result};

#[derive(Clone)]
enum Mapping {
    Format0([u8; 256]),
    Format4 {
        end_code: Vec<u16>,
        start_code: Vec<u16>,
        id_delta: Vec<i16>,
        /// (idRangeOffset value, absolute position of that field in the cmap
        /// table slice). The address of the mapped glyph id is
        /// `field_pos + ro + 2 * (c - startCode)`.
        range_offset: Vec<(u16, usize)>,
    },
    Format12 {
        groups: Vec<(u32, u32, u32)>, // startChar, endChar, startGlyphId
    },
}

#[derive(Clone)]
pub struct Cmap {
    /// Full cmap table bytes; format-4 glyphIdArray reads index back into it.
    table: Vec<u8>,
    mapping: Mapping,
}

impl Cmap {
    pub fn parse(table: &[u8]) -> Result<Cmap> {
        let mut r = Reader::at(table, 0);
        let _version = r.read_u16()?;
        let num_records = r.read_u16()? as usize;

        let mut best: Option<(i32, usize)> = None;
        for _ in 0..num_records {
            let platform_id = r.read_u16()?;
            let encoding_id = r.read_u16()?;
            let subtable_offset = r.read_u32()? as usize;
            let score = subtable_score(platform_id, encoding_id);
            if score < 0 || subtable_offset >= table.len() {
                continue;
            }
            if best.map_or(true, |(s, _)| score > s) {
                best = Some((score, subtable_offset));
            }
        }
        let (_, offset) = best.ok_or(Error::Unsupported)?;
        let mapping = parse_subtable(table, offset)?;
        Ok(Cmap {
            table: table.to_vec(),
            mapping,
        })
    }

    pub fn lookup(&self, codepoint: u32) -> u16 {
        match &self.mapping {
            Mapping::Format0(arr) => {
                if codepoint <= 0xFF {
                    arr[codepoint as usize] as u16
                } else {
                    0
                }
            }
            Mapping::Format4 {
                end_code,
                start_code,
                id_delta,
                range_offset,
            } => {
                if codepoint > u16::MAX as u32 {
                    return 0;
                }
                let c = codepoint as u16;
                // Segments are sorted by endCode; linear scan is fine for the
                // typical 100-400 segments and keeps the code branch-light.
                for i in 0..end_code.len() {
                    if c > end_code[i] {
                        continue;
                    }
                    if c < start_code[i] {
                        return 0;
                    }
                    let (ro, field_pos) = range_offset[i];
                    if ro == 0 {
                        let gid = c as i32 + id_delta[i] as i32;
                        return (gid & 0xFFFF) as u16;
                    }
                    let addr = field_pos + ro as usize + (c as usize - start_code[i] as usize) * 2;
                    let g = Reader::at(&self.table, addr).read_u16().unwrap_or(0);
                    if g == 0 {
                        return 0;
                    }
                    return (g as i32 + id_delta[i] as i32) as u16;
                }
                0
            }
            Mapping::Format12 { groups } => {
                for (start, end, gid_start) in groups {
                    if codepoint < *start {
                        return 0;
                    }
                    if codepoint <= *end {
                        return (gid_start + (codepoint - start)) as u16;
                    }
                }
                0
            }
        }
    }
}

fn subtable_score(platform: u16, encoding: u16) -> i32 {
    match (platform, encoding) {
        (3, 10) => 100, // Windows, UCS-4
        (3, 1) => 90,   // Windows, UCS-2
        (0, _) => 80,   // Unicode, any encoding
        (3, 0) => 70,   // Windows, symbol
        (1, 0) => 60,   // Mac Roman
        _ => -1,
    }
}

fn parse_subtable(table: &[u8], offset: usize) -> Result<Mapping> {
    let mut r = Reader::at(table, offset);
    let format = r.read_u16()?;
    match format {
        0 => {
            let _length = r.read_u16()?;
            let _language = r.read_u16()?;
            let bytes = r.read_bytes(256)?;
            let mut arr = [0u8; 256];
            arr.copy_from_slice(bytes);
            Ok(Mapping::Format0(arr))
        }
        4 => {
            let length = r.read_u16()? as usize;
            let _language = r.read_u16()?;
            let seg_count_x2 = r.read_u16()? as usize;
            if seg_count_x2 == 0 || seg_count_x2 % 2 != 0 {
                return Err(Error::Unsupported);
            }
            let seg_count = seg_count_x2 / 2;
            r.skip(6)?; // searchRange, entrySelector, rangeShift
            let mut end_code = Vec::with_capacity(seg_count);
            for _ in 0..seg_count {
                end_code.push(r.read_u16()?);
            }
            r.skip(2)?; // reservedPad
            let mut start_code = Vec::with_capacity(seg_count);
            for _ in 0..seg_count {
                start_code.push(r.read_u16()?);
            }
            let mut id_delta = Vec::with_capacity(seg_count);
            for _ in 0..seg_count {
                id_delta.push(r.read_i16()?);
            }
            let ro_array_pos = r.position();
            let mut range_offset = Vec::with_capacity(seg_count);
            for _ in 0..seg_count {
                let ro = r.read_u16()?;
                range_offset.push((ro, ro_array_pos));
            }
            // Sanity: whole subtable must fit in its declared length.
            if offset + length > table.len() {
                return Err(Error::BadOffset);
            }
            Ok(Mapping::Format4 {
                end_code,
                start_code,
                id_delta,
                range_offset,
            })
        }
        12 => {
            // format(2) + reserved(2) + length(4) + language(4) + numGroups(4)
            r.skip(2)?; // reserved
            let _length = r.read_u32()?;
            r.skip(4)?; // language
            let num_groups = r.read_u32()? as usize;
            let mut groups = Vec::with_capacity(num_groups.min(100_000));
            for _ in 0..num_groups {
                let start = r.read_u32()?;
                let end = r.read_u32()?;
                let gid = r.read_u32()?;
                if end < start || gid > 0xFFFF {
                    return Err(Error::Unsupported);
                }
                groups.push((start, end, gid));
            }
            Ok(Mapping::Format12 { groups })
        }
        _ => Err(Error::Unsupported),
    }
}
