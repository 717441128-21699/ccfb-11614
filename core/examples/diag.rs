fn main() {
    let data = std::fs::read("C:/Windows/Fonts/arial.ttf").unwrap();
    let num = u16::from_be_bytes([data[4], data[5]]) as usize;
    for i in 0..num {
        let o = 12 + i * 16;
        let tag = &data[o..o + 4];
        let len = u32::from_be_bytes([data[o + 12], data[o + 13], data[o + 14], data[o + 15]]);
        let off = u32::from_be_bytes([data[o + 8], data[o + 9], data[o + 10], data[o + 11]]);
        if tag == b"hhea" {
            println!("hhea offset={} length={}", off, len);
            let base = off as usize;
            println!("last 6 bytes: {:?}", &data[base + 30..base + 36]);
            println!("numLongHorMetrics = {}", u16::from_be_bytes([data[base + 34], data[base + 35]]));
        }
        if tag == b"hmtx" {
            println!("hmtx offset={} length={}", off, len);
        }
    }
}
