//! 渲染样本转储:把若干字形以 ASCII / PGM 输出,用于无浏览器环境的质量目检。
use std::fs;
use ttengine::engine::{Engine, Face};
use ttengine::raster::write_pgm;

fn ascii_art(e: &mut Engine, ch: char) -> (String, usize) {
    let gid = e.glyph_id(ch as u32).unwrap();
    let r = e.render_glyph(gid).unwrap();
    let w = r.bitmap.width as usize;
    let mut s = format!("--- '{}' {}x{} ---\n", ch, r.bitmap.width, r.bitmap.height);
    for y in 0..r.bitmap.height as usize {
        for x in 0..w {
            let a = r.bitmap.alpha[y * w + x];
            s.push(match a {
                0 => ' ',
                1..=60 => '.',
                61..=140 => 'o',
                141..=220 => 'O',
                _ => '#',
            });
        }
        s.push('\n');
    }
    (s, w)
}

fn main() {
    fs::create_dir_all("test-out").ok();

    // 拉丁 12px(带微调)
    let data = fs::read("C:/Windows/Fonts/arial.ttf").unwrap();
    let face = Face::parse(&data, 0).unwrap();
    let mut eng = Engine::new(face, 12);
    for ch in "Hag".chars() {
        let (art, _) = ascii_art(&mut eng, ch);
        println!("{art}");
    }

    // 汉字 16px 无字形 hint(黑体)
    let data2 = fs::read("C:/Windows/Fonts/simhei.ttf").unwrap();
    let face2 = Face::parse(&data2, 0).unwrap();
    let mut eng2 = Engine::new(face2, 16);
    for ch in "永中".chars() {
        let gid = eng2.glyph_id(ch as u32).unwrap();
        let r = eng2.render_glyph(gid).unwrap();
        write_pgm(&format!("test-out/simhei_{}_16.pgm", ch), &r.bitmap).unwrap();
        let (art, _) = ascii_art(&mut eng2, ch);
        println!("{art}");
    }

    // 12px 小字号汉字(关键验收场景)
    let data3 = fs::read("C:/Windows/Fonts/simhei.ttf").unwrap();
    let face3 = Face::parse(&data3, 0).unwrap();
    let mut eng3 = Engine::new(face3, 12);
    eng3.hinting_enabled = false;
    for ch in "永的一是中国".chars() {
        let (art, _) = ascii_art(&mut eng3, ch);
        println!("{art}");
    }
    println!("PGM 样本已写入 core/test-out/");
}
