//! 动态纹理图集:按需栅格化的字形位图缓存进一张 R8 大图。
//!
//! 采用 shelf(货架)装箱算法:每条货架高度固定,字形依次横向排列,
//! 当前货架放不下就新开一条;全部货架放不下时返回 [`Error::AtlasFull`],
//! 由调用方决定扩容或清缓存。
//!
//! 缓存键 = (glyph_id, ppem),支持同一图集内混合字号(按行隔离)。

use crate::engine::{Engine, GlyphBitmap};
use crate::{Error, Result};
use std::collections::HashMap;

/// 图集条目:字形在图集中的位置。
#[derive(Clone, Copy, Debug)]
pub struct AtlasEntry {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
    pub advance: f32,
    pub left_side_bearing: f32,
    pub top_bearing: f32,
}

#[derive(Clone, Copy)]
struct Shelf {
    y: u32,
    height: u32,
    cursor: u32,
}

/// 缓存键。
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct GlyphKey {
    pub glyph_id: u16,
    pub ppem: u16,
}

/// R8 纹理图集。
pub struct Atlas<'a> {
    width: u32,
    height: u32,
    pixels: Vec<u8>,
    shelves: Vec<Shelf>,
    cache: HashMap<GlyphKey, AtlasEntry>,
    /// 触底后是否允许自动清空重建
    pub auto_grow: bool,
    _engine_lifetime: core::marker::PhantomData<&'a Engine<'a>>,
}

impl<'a> Atlas<'a> {
    pub fn new(width: u32, height: u32) -> Self {
        Atlas {
            width,
            height,
            pixels: vec![0u8; (width as usize) * (height as usize)],
            shelves: Vec::new(),
            cache: HashMap::new(),
            auto_grow: true,
            _engine_lifetime: core::marker::PhantomData,
        }
    }

    #[inline]
    pub fn width(&self) -> u32 {
        self.width
    }
    #[inline]
    pub fn height(&self) -> u32 {
        self.height
    }

    /// 图集原始 R8 像素(上传 WebGL 纹理用)。
    #[inline]
    pub fn pixels(&self) -> &[u8] {
        &self.pixels
    }

    #[inline]
    pub fn len(&self) -> usize {
        self.cache.len()
    }

    #[inline]
    pub fn is_empty(&self) -> bool {
        self.cache.is_empty()
    }

    /// 查询已缓存条目(不触发栅格化)。
    pub fn get(&self, key: GlyphKey) -> Option<&AtlasEntry> {
        self.cache.get(&key)
    }

    /// 取一个字形:命中缓存直接返回,否则栅格化并装箱进图集。
    pub fn ensure(&mut self, engine: &mut Engine, glyph_id: u16) -> Result<AtlasEntry> {
        let key = GlyphKey {
            glyph_id,
            ppem: engine.ppem(),
        };
        if let Some(e) = self.cache.get(&key) {
            return Ok(*e);
        }
        let rendered: GlyphBitmap = engine.render_glyph(glyph_id)?;
        let entry = self.pack(key, &rendered)?;
        Ok(entry)
    }

    /// 把一张字形位图装箱并写入像素缓冲。
    fn pack(&mut self, key: GlyphKey, g: &GlyphBitmap) -> Result<AtlasEntry> {
        let gw = g.bitmap.width as u32;
        let gh = g.bitmap.height as u32;
        // 零尺寸字形(空格等):缓存一个空条目,不占像素
        if gw == 0 || gh == 0 {
            let entry = AtlasEntry {
                x: 0,
                y: 0,
                width: 0,
                height: 0,
                advance: g.advance,
                left_side_bearing: g.left_side_bearing,
                top_bearing: g.top_bearing,
            };
            self.cache.insert(key, entry);
            return Ok(entry);
        }
        if gw > self.width || gh > self.height {
            return Err(Error::AtlasFull);
        }

        // 在现有货架里找能放下的:货架高度按 4px 档位取整,
        // 高度落在同一档位的字形共享货架,避免每个字形都开新行。
        let bucket = |h: u32| (h + 3) & !3;
        let gh_bucket = bucket(gh);
        let mut placed: Option<(u32, u32)> = None;
        for shelf in self.shelves.iter_mut() {
            if shelf.height >= gh_bucket && shelf.cursor + gw <= self.width {
                let pos = (shelf.cursor, shelf.y);
                shelf.cursor += gw;
                placed = Some(pos);
                break;
            }
        }
        let (px, py) = if let Some(p) = placed {
            p
        } else {
            // 开新货架,高度取整到档位以容纳同档字形
            let shelf_h = gh_bucket;
            let y = self.shelves.last().map(|s| s.y + s.height).unwrap_or(0);
            if y + shelf_h > self.height {
                return Err(Error::AtlasFull);
            }
            self.shelves.push(Shelf {
                y,
                height: shelf_h,
                cursor: gw,
            });
            (0, y)
        };

        self.blit(px, py, &g.bitmap.alpha, gw as usize, gh as usize);
        let entry = AtlasEntry {
            x: px,
            y: py,
            width: gw,
            height: gh,
            advance: g.advance,
            left_side_bearing: g.left_side_bearing,
            top_bearing: g.top_bearing,
        };
        self.cache.insert(key, entry);
        Ok(entry)
    }

    /// 把字形 alpha 子图写入图集(逐行拷贝)。
    fn blit(&mut self, x: u32, y: u32, src: &[u8], gw: usize, gh: usize) {
        let w = self.width as usize;
        for row in 0..gh {
            let dst_start = (y as usize + row) * w + x as usize;
            let src_start = row * gw;
            if dst_start + gw <= self.pixels.len() && src_start + gw <= src.len() {
                self.pixels[dst_start..dst_start + gw]
                    .copy_from_slice(&src[src_start..src_start + gw]);
            }
        }
    }

    /// 清空图集(LRU 满时的回收手段),保留缓冲容量。
    pub fn clear(&mut self) {
        for p in self.pixels.iter_mut() {
            *p = 0;
        }
        self.shelves.clear();
        self.cache.clear();
    }

    /// 当前已用货架高度(便于界面显示图集占用率)。
    pub fn used_height(&self) -> u32 {
        self.shelves.last().map(|s| s.y + s.height).unwrap_or(0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::Face;

    #[test]
    fn caches_glyphs_and_packs_without_overlap() {
        let data = std::fs::read("C:/Windows/Fonts/simhei.ttf").unwrap();
        let face = Face::parse(&data, 0).unwrap();
        let mut engine = Engine::new(face, 24);
        let mut atlas = Atlas::new(256, 256);

        let text = "汉字字形缓存测试";
        let unique: std::collections::HashSet<char> = text.chars().collect();
        let mut entries = Vec::new();
        for ch in text.chars() {
            let gid = engine.glyph_id(ch as u32).unwrap();
            let e = atlas.ensure(&mut engine, gid).unwrap();
            entries.push(e);
        }
        assert_eq!(atlas.len(), unique.len());

        // 任意两个不同字形的非空条目矩形不得重叠
        // (重复字符命中同一缓存条目,先按位置去重)
        let mut seen: std::collections::HashSet<(u32, u32)> = std::collections::HashSet::new();
        let nonzero: Vec<&AtlasEntry> = entries
            .iter()
            .filter(|e| e.width > 0 && seen.insert((e.x, e.y)))
            .collect();
        for i in 0..nonzero.len() {
            for j in (i + 1)..nonzero.len() {
                let a = nonzero[i];
                let b = nonzero[j];
                let overlap_x = a.x < b.x + b.width && b.x < a.x + a.width;
                let overlap_y = a.y < b.y + b.height && b.y < a.y + a.height;
                assert!(!(overlap_x && overlap_y), "atlas entries overlap: {a:?} {b:?}");
            }
        }
    }

    #[test]
    fn repeated_lookup_is_cached() {
        let data = std::fs::read("C:/Windows/Fonts/arial.ttf").unwrap();
        let face = Face::parse(&data, 0).unwrap();
        let mut engine = Engine::new(face, 16);
        let mut atlas = Atlas::new(256, 256);
        let gid = engine.glyph_id('X' as u32).unwrap();
        let e1 = atlas.ensure(&mut engine, gid).unwrap();
        let n1 = atlas.len();
        let e2 = atlas.ensure(&mut engine, gid).unwrap();
        assert_eq!(atlas.len(), n1, "second lookup must not add entry");
        assert_eq!(e1.x, e2.x);
        assert_eq!(e1.y, e2.y);
    }

    #[test]
    fn space_occupies_no_pixels() {
        let data = std::fs::read("C:/Windows/Fonts/arial.ttf").unwrap();
        let face = Face::parse(&data, 0).unwrap();
        let mut engine = Engine::new(face, 16);
        let mut atlas = Atlas::new(256, 256);
        let gid = engine.glyph_id(' ' as u32).unwrap();
        let e = atlas.ensure(&mut engine, gid).unwrap();
        assert_eq!(e.width, 0);
        assert_eq!(atlas.used_height(), 0);
    }
}
