use std::collections::BTreeMap;

fn table(data: &[u8], want: &[u8; 4]) -> Option<(usize, usize)> {
    let num = u16::from_be_bytes([data[4], data[5]]) as usize;
    for i in 0..num {
        let o = 12 + i * 16;
        if &data[o..o + 4] == want {
            let off = u32::from_be_bytes([data[o + 8], data[o + 9], data[o + 10], data[o + 11]]) as usize;
            let len = u32::from_be_bytes([data[o + 12], data[o + 13], data[o + 14], data[o + 15]]) as usize;
            return Some((off, len));
        }
    }
    None
}

fn histogram(name: &str, data: &[u8], tag: &[u8; 4]) {
    let Some((off, len)) = table(data, tag) else {
        println!("{name}: no {tag:?}");
        return;
    };
    let bytes = &data[off..off + len];
    let mut h = BTreeMap::new();
    for &b in bytes {
        *h.entry(b).or_insert(0u32) += 1;
    }
    println!("\n{name} {tag:?} ({} bytes) opcode histogram:", len);
    for (op, cnt) in &h {
        print!("{:02X}:{} ", op, cnt);
    }
    println!();
}

fn glyph_opcodes(name: &str, data: &[u8]) {
    // 收集 glyf 表内所有指令块的操作码(粗略:只统计频次)
    let Some((goff, glen)) = table(data, b"glyf") else { return };
    let glyf = &data[goff..goff + glen];
    let Some((loff, llen)) = table(data, b"loca") else { return };
    let loca = &data[loff..loff + llen];
    let head_off = table(data, b"head").unwrap().0;
    let fmt = i16::from_be_bytes([data[head_off + 50], data[head_off + 51]]);
    let n = (llen / if fmt == 0 { 2 } else { 4 }) - 1;
    let mut h = BTreeMap::new();
    for g in 0..n {
        let (s, e) = if fmt == 0 {
            (u16::from_be_bytes([loca[g*2], loca[g*2+1]]) as usize * 2,
             u16::from_be_bytes([loca[g*2+2], loca[g*2+3]]) as usize * 2)
        } else {
            let s = u32::from_be_bytes([loca[g*4], loca[g*4+1], loca[g*4+2], loca[g*4+3]]) as usize;
            let e = u32::from_be_bytes([loca[g*4+4], loca[g*4+5], loca[g*4+6], loca[g*4+7]]) as usize;
            (s, e)
        };
        if s == e || e > glyf.len() { continue; }
        let nc = i16::from_be_bytes([glyf[s], glyf[s+1]]);
        if nc <= 0 { continue; }
        let mut p = s + 10;
        for _ in 0..nc { p += 2; }
        if p + 2 > glyf.len() { continue; }
        let ilen = u16::from_be_bytes([glyf[p], glyf[p+1]]) as usize;
        p += 2;
        for k in 0..ilen {
            if p + k < glyf.len() {
                *h.entry(glyf[p+k]).or_insert(0u32) += 1;
            }
        }
    }
    println!("\n{name} glyf per-glyph instruction opcode histogram:");
    for (op, cnt) in &h { print!("{:02X}:{} ", op, cnt); }
    println!();
}

fn main() {
    for (name, path) in [
        ("arial", "C:/Windows/Fonts/arial.ttf"),
        ("simhei", "C:/Windows/Fonts/simhei.ttf"),
        ("segoeui", "C:/Windows/Fonts/segoeui.ttf"),
    ] {
        let data = std::fs::read(path).unwrap();
        histogram(name, &data, b"prep");
        glyph_opcodes(name, &data);
    }
}
