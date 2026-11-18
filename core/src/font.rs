//! 字体文件装载:SFNT 表目录与 TrueType Collection(`.ttc`)。
//!
//! 不拷贝字体字节,所有表只记录 `(起始偏移, 长度)`,按需借用解析。

use crate::Error;
use crate::Result;

/// 大端二进制读取器,全部带边界检查。
#[derive(Clone)]
pub struct Reader<'a> {
    pub buf: &'a [u8],
    pub pos: usize,
}

impl<'a> Reader<'a> {
    pub fn new(buf: &'a [u8]) -> Self {
        Reader { buf, pos: 0 }
    }

    pub fn at(buf: &'a [u8], pos: usize) -> Self {
        Reader { buf, pos }
    }

    #[inline]
    pub fn remaining(&self) -> usize {
        self.buf.len().saturating_sub(self.pos)
    }

    #[inline]
    pub fn need(&self, n: usize) -> Result<()> {
        if self.pos.checked_add(n).map_or(true, |end| end > self.buf.len()) {
            return Err(Error::oob("read", self.pos));
        }
        Ok(())
    }

    #[inline]
    pub fn u8(&mut self) -> Result<u8> {
        self.need(1)?;
        let v = self.buf[self.pos];
        self.pos += 1;
        Ok(v)
    }

    #[inline]
    pub fn i8(&mut self) -> Result<i8> {
        Ok(self.u8()? as i8)
    }

    #[inline]
    pub fn u16(&mut self) -> Result<u16> {
        self.need(2)?;
        let v = u16::from_be_bytes([self.buf[self.pos], self.buf[self.pos + 1]]);
        self.pos += 2;
        Ok(v)
    }

    #[inline]
    pub fn i16(&mut self) -> Result<i16> {
        Ok(self.u16()? as i16)
    }

    #[inline]
    pub fn u32(&mut self) -> Result<u32> {
        self.need(4)?;
        let v = u32::from_be_bytes([
            self.buf[self.pos],
            self.buf[self.pos + 1],
            self.buf[self.pos + 2],
            self.buf[self.pos + 3],
        ]);
        self.pos += 4;
        Ok(v)
    }

    #[inline]
    pub fn i32(&mut self) -> Result<i32> {
        Ok(self.u32()? as i32)
    }

    /// TrueType 里的 4 字节标签,如 `b'glyf'`。
    #[inline]
    pub fn tag(&mut self) -> Result<[u8; 4]> {
        self.need(4)?;
        let t = [
            self.buf[self.pos],
            self.buf[self.pos + 1],
            self.buf[self.pos + 2],
            self.buf[self.pos + 3],
        ];
        self.pos += 4;
        Ok(t)
    }

    pub fn skip(&mut self, n: usize) -> Result<()> {
        self.need(n)?;
        self.pos += n;
        Ok(())
    }

    pub fn sub(&self, len: usize) -> Result<Reader<'a>> {
        self.need(len)?;
        Ok(Reader {
            buf: &self.buf[self.pos..self.pos + len],
            pos: 0,
        })
    }

    /// 固定位置读取(不推进游标),给随机访问的表用。
    #[inline]
    pub fn peek_u8_at(buf: &[u8], off: usize) -> Result<u8> {
        if off >= buf.len() {
            return Err(Error::oob("peek_u8", off));
        }
        Ok(buf[off])
    }

    #[inline]
    pub fn peek_u16_at(buf: &[u8], off: usize) -> Result<u16> {
        if off + 2 > buf.len() {
            return Err(Error::oob("peek_u16", off));
        }
        Ok(u16::from_be_bytes([buf[off], buf[off + 1]]))
    }

    #[inline]
    pub fn peek_i16_at(buf: &[u8], off: usize) -> Result<i16> {
        Ok(Self::peek_u16_at(buf, off)? as i16)
    }

    #[inline]
    pub fn peek_u32_at(buf: &[u8], off: usize) -> Result<u32> {
        if off + 4 > buf.len() {
            return Err(Error::oob("peek_u32", off));
        }
        Ok(u32::from_be_bytes([
            buf[off],
            buf[off + 1],
            buf[off + 2],
            buf[off + 3],
        ]))
    }
}

/// 一条表目录记录。
#[derive(Clone, Copy, Debug)]
pub struct TableRecord {
    pub tag: [u8; 4],
    pub checksum: u32,
    pub offset: usize,
    pub length: usize,
}

impl TableRecord {
    pub fn tag_eq(&self, name: &[u8; 4]) -> bool {
        &self.tag == name
    }
}

/// 一个 SFNT 字体(可能来自 TTC 中的某一序号)。
pub struct FontFile<'a> {
    /// 整个文件的原始字节(TTC 时所有 face 共享)。
    pub data: &'a [u8],
    /// 本 face 的 sfnt 版本魔数。
    pub sfnt_version: u32,
    pub tables: Vec<TableRecord>,
}

impl<'a> FontFile<'a> {
    /// 解析单个 SFNT(`.ttf`/`.otf`)或 TTC 集合的第 `face_index` 个 face。
    pub fn parse(data: &'a [u8], face_index: u32) -> Result<FontFile<'a>> {
        let mut r = Reader::new(data);
        let magic = r.u32()?;
        let is_ttc = magic == 0x74746366 // 'ttcf'
            || magic == 0x74746370 /* 'ttcp' */;
        if is_ttc {
            // TTC Header
            let _major = r.u16()?;
            let _minor = r.u16()?;
            let num_fonts = r.u32()? as usize;
            if face_index as usize >= num_fonts {
                return Err(Error::BadGlyph(face_index as u16));
            }
            let offsets = (0..num_fonts)
                .map(|_| r.u32().map(|o| o as usize))
                .collect::<Result<Vec<_>>>()?;
            let face_off = offsets[face_index as usize];
            Self::parse_sfnt_at(data, face_off)
        } else {
            if face_index != 0 {
                return Err(Error::Unsupported {
                    what: "face_index for non-TTC",
                });
            }
            Self::parse_sfnt_at(data, 0)
        }
    }

    fn parse_sfnt_at(data: &'a [u8], off: usize) -> Result<FontFile<'a>> {
        let mut r = Reader::at(data, off);
        let sfnt_version = r.u32()?;
        match sfnt_version {
            0x00010000 | 0x74727565 /* 'true' */ => {}
            0x4F54544F /* 'OTTO' */ => {
                return Err(Error::Unsupported {
                    what: "CFF/OpenType outlines (OTTO)",
                })
            }
            other => return Err(Error::BadMagic {
                what: "sfnt version",
                found: other,
            }),
        }
        let num_tables = r.u16()? as usize;
        let _search_range = r.u16()?;
        let _entry_selector = r.u16()?;
        let _range_shift = r.u16()?;

        let mut tables = Vec::with_capacity(num_tables);
        for _ in 0..num_tables {
            let tag = r.tag()?;
            let checksum = r.u32()?;
            let offset = r.u32()? as usize;
            let length = r.u32()? as usize;
            if offset.checked_add(length).map_or(true, |e| e > data.len()) {
                return Err(Error::oob("table record", offset));
            }
            tables.push(TableRecord {
                tag,
                checksum,
                offset,
                length,
            });
        }
        Ok(FontFile {
            data,
            sfnt_version,
            tables,
        })
    }

    pub fn find_table(&self, name: &[u8; 4]) -> Option<TableRecord> {
        self.tables.iter().copied().find(|t| t.tag_eq(name))
    }

    pub fn require_table(&self, name: &[u8; 4]) -> Result<TableRecord> {
        self.find_table(name).ok_or(Error::Unsupported {
            what: "required table missing",
        })
    }

    pub fn table_bytes(&self, name: &[u8; 4]) -> Result<&'a [u8]> {
        let t = self.require_table(name)?;
        Ok(&self.data[t.offset..t.offset + t.length])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 用 Windows 自带字体做烟雾测试(测试机有这些文件)。
    fn arial() -> Vec<u8> {
        std::fs::read("C:/Windows/Fonts/arial.ttf").expect("arial.ttf present")
    }

    #[test]
    fn parses_arial_directory() {
        let data = arial();
        let f = FontFile::parse(&data, 0).unwrap();
        for needed in [b"head", b"hhea", b"maxp", b"cmap", b"loca", b"glyf", b"hmtx"] {
            assert!(f.find_table(needed).is_some(), "missing {}", std::str::from_utf8(needed).unwrap());
        }
    }

    #[test]
    fn parses_ttc_collection() {
        let data = std::fs::read("C:/Windows/Fonts/simsun.ttc").expect("simsun.ttc present");
        let f0 = FontFile::parse(&data, 0).unwrap();
        assert!(f0.find_table(b"glyf").is_some());
        // 第二个 face 也应能独立解析
        let f1 = FontFile::parse(&data, 1).unwrap();
        assert!(f1.find_table(b"cmap").is_some());
    }

    #[test]
    fn rejects_garbage() {
        let junk = [0u8; 32];
        assert!(FontFile::parse(&junk, 0).is_err());
    }
}
