//! Full-resolution page decode. Every original pixel is kept: level 0 is the
//! stored image, untouched. Smaller levels (each a 2×2 box average of the
//! previous) exist only so a zoomed-out page is drawn without aliasing; they
//! never replace level 0 when zoomed in.

use std::path::Path;

#[derive(Clone, Debug)]
pub struct Level {
    pub w: u32,
    pub h: u32,
    /// `0x00RRGGBB`, row-major.
    pub px: Vec<u32>,
}

#[derive(Clone, Debug)]
pub struct PageImage {
    pub levels: Vec<Level>,
}

impl PageImage {
    pub fn width(&self) -> u32 {
        self.levels[0].w
    }
    pub fn height(&self) -> u32 {
        self.levels[0].h
    }
    pub fn byte_size(&self) -> usize {
        self.levels.iter().map(|l| l.px.len() * 4).sum()
    }

    pub fn from_level0(level: Level) -> PageImage {
        let mut levels = vec![level];
        while let Some(last) = levels.last() {
            if last.w.max(last.h) <= 160 || last.w < 2 || last.h < 2 {
                break;
            }
            let next = halve(last);
            levels.push(next);
        }
        PageImage { levels }
    }
}

fn halve(src: &Level) -> Level {
    let w = src.w.div_ceil(2);
    let h = src.h.div_ceil(2);
    let mut px = vec![0u32; (w * h) as usize];
    let sw = src.w as usize;
    for y in 0..h as usize {
        let y0 = (2 * y).min(src.h as usize - 1);
        let y1 = (2 * y + 1).min(src.h as usize - 1);
        let r0 = &src.px[y0 * sw..y0 * sw + sw];
        let r1 = &src.px[y1 * sw..y1 * sw + sw];
        let out = &mut px[y * w as usize..(y + 1) * w as usize];
        for (x, o) in out.iter_mut().enumerate() {
            let x0 = 2 * x;
            let x1 = (2 * x + 1).min(sw - 1);
            let (a, b, c, d) = (r0[x0], r0[x1], r1[x0], r1[x1]);
            // Average the four pixels channel-wise with two lanes per add:
            // red+blue in one word, green in another.
            let rb = (a & 0xff00ff) + (b & 0xff00ff) + (c & 0xff00ff) + (d & 0xff00ff) + 0x020002;
            let g = (a & 0x00ff00) + (b & 0x00ff00) + (c & 0x00ff00) + (d & 0x00ff00) + 0x000200;
            *o = ((rb >> 2) & 0xff00ff) | ((g >> 2) & 0x00ff00);
        }
    }
    Level { w, h, px }
}

pub fn decode_page(path: &Path) -> Result<PageImage, String> {
    let bytes = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
    decode_bytes(&bytes).map_err(|e| format!("{}: {e}", path.display()))
}

pub fn decode_bytes(bytes: &[u8]) -> Result<PageImage, String> {
    let img = image::load_from_memory(bytes).map_err(|e| e.to_string())?;
    let rgb = img.into_rgb8();
    let (w, h) = rgb.dimensions();
    if w == 0 || h == 0 {
        return Err("empty image".into());
    }
    let raw = rgb.into_raw();
    let px: Vec<u32> = raw
        .chunks_exact(3)
        .map(|c| ((c[0] as u32) << 16) | ((c[1] as u32) << 8) | c[2] as u32)
        .collect();
    Ok(PageImage::from_level0(Level { w, h, px }))
}

/// A synthetic page (for benchmarks and tests): paper with text-like bars.
pub fn synthetic_page(w: u32, h: u32, seed: u64) -> PageImage {
    let mut px = vec![0x00f4_efe4u32; (w * h) as usize];
    let mut s = seed | 1;
    let mut rnd = || {
        s ^= s << 13;
        s ^= s >> 7;
        s ^= s << 17;
        s
    };
    let line_h = 22;
    let mut y = 90;
    while y + 14 < h.saturating_sub(90) {
        let indent = if rnd() % 7 == 0 { 40 } else { 0 };
        let end = w.saturating_sub(70) - (rnd() % 120) as u32 * u32::from(rnd() % 5 == 0);
        let mut x = 70 + indent;
        while x < end {
            let word = 12 + (rnd() % 60) as u32;
            for yy in y..y + 12 {
                for xx in x..(x + word).min(end) {
                    px[(yy * w + xx) as usize] = 0x0020_1c18;
                }
            }
            x += word + 7;
        }
        y += line_h + if rnd() % 9 == 0 { 18 } else { 0 };
    }
    PageImage::from_level0(Level { w, h, px })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mip_chain_halves_and_averages() {
        let lvl = Level {
            w: 4,
            h: 2,
            px: vec![
                0x000000, 0xffffff, 0x101010, 0x101010, 0x000000, 0xffffff, 0x101010, 0x101010,
            ],
        };
        let h = halve(&lvl);
        assert_eq!((h.w, h.h), (2, 1));
        assert_eq!(h.px[1], 0x101010);
        let c = h.px[0] & 0xff;
        assert!((127..=128).contains(&c), "{c}");
    }

    #[test]
    fn odd_sizes_round_up() {
        let p = PageImage::from_level0(Level {
            w: 801,
            h: 1281,
            px: vec![0x808080; 801 * 1281],
        });
        assert_eq!((p.levels[1].w, p.levels[1].h), (401, 641));
        assert!(p.levels.last().unwrap().w.max(p.levels.last().unwrap().h) <= 160);
    }
}
