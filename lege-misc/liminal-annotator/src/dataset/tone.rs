//! Paper tone: how yellowed a book's paper is, measured from its pages.
//!
//! For each sampled page, the paper is taken to be its brightest pixels (the
//! top fifth by luma, read on a 4-pixel grid), and the page's paper colour is
//! their per-channel median. A book's tone is the median over its sampled
//! pages, reported as CIE L* (lightness) and b* (blue–yellow: about 0 for
//! white or grey paper, 10–25 for yellowed or tinted paper). Pictures and
//! dark pages pull the bright fifth down a little; the median over pages
//! keeps one plate or a black end paper from deciding the book.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, channel};

use super::database::{self, Tone};

/// Pages sampled per book.
pub const SAMPLES: usize = 5;

fn median(v: &mut [u8]) -> u8 {
    if v.is_empty() {
        return 0;
    }
    let mid = v.len() / 2;
    *v.select_nth_unstable(mid).1
}

/// The paper colour of one page image.
pub fn page_paper(path: &Path) -> Option<[u8; 3]> {
    let img = image::open(path).ok()?.into_rgb8();
    let (w, h) = img.dimensions();
    let mut px: Vec<([u8; 3], u16)> = Vec::with_capacity((w as usize / 4) * (h as usize / 4));
    for y in (0..h).step_by(4) {
        for x in (0..w).step_by(4) {
            let p = img.get_pixel(x, y).0;
            let luma = 2 * p[0] as u16 + 5 * p[1] as u16 + p[2] as u16;
            px.push((p, luma));
        }
    }
    if px.is_empty() {
        return None;
    }
    let k = (px.len() * 4 / 5).min(px.len() - 1);
    px.select_nth_unstable_by_key(k, |p| p.1);
    let bright = &px[k..];
    let mut out = [0u8; 3];
    for (ch, o) in out.iter_mut().enumerate() {
        let mut v: Vec<u8> = bright.iter().map(|p| p.0[ch]).collect();
        *o = median(&mut v);
    }
    Some(out)
}

fn srgb_to_linear(c: u8) -> f64 {
    let c = c as f64 / 255.0;
    if c <= 0.04045 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

/// sRGB → CIE (L*, b*) under D65.
pub fn lab_l_b(rgb: [u8; 3]) -> (f64, f64) {
    let (r, g, b) = (
        srgb_to_linear(rgb[0]),
        srgb_to_linear(rgb[1]),
        srgb_to_linear(rgb[2]),
    );
    let y = 0.2126 * r + 0.7152 * g + 0.0722 * b;
    let z = (0.0193 * r + 0.1192 * g + 0.9505 * b) / 1.089;
    let f = |t: f64| {
        if t > 216.0 / 24389.0 {
            t.cbrt()
        } else {
            (24389.0 / 27.0 * t + 16.0) / 116.0
        }
    };
    (116.0 * f(y) - 16.0, 200.0 * (f(y) - f(z)))
}

/// The tone of a book from its sampled pages (unreadable pages are skipped).
pub fn book_tone(pages: &[PathBuf]) -> Option<Tone> {
    let mut papers: Vec<[u8; 3]> = pages.iter().filter_map(|p| page_paper(p)).collect();
    if papers.is_empty() {
        return None;
    }
    let mut rgb = [0u8; 3];
    for (ch, o) in rgb.iter_mut().enumerate() {
        let mut v: Vec<u8> = papers.iter().map(|p| p[ch]).collect();
        *o = median(&mut v);
    }
    papers.clear();
    let (lightness, yellowness) = lab_l_b(rgb);
    Some(Tone {
        rgb,
        lightness,
        yellowness,
        pages: pages.len() as i64,
    })
}

/// A background measurement of every book that has no tone yet. Results are
/// written to the project as they come and also sent on `results`.
pub struct ToneJob {
    pub results: Receiver<(String, Tone)>,
    pub total: usize,
    done: Arc<AtomicUsize>,
    stop: Arc<AtomicBool>,
    writer: Option<std::thread::JoinHandle<()>>,
}

impl ToneJob {
    /// `None` when every book already has a tone.
    pub fn start(
        project: &Path,
        threads: usize,
        wake: Arc<dyn Fn() + Send + Sync>,
    ) -> Result<Option<ToneJob>, String> {
        let conn = database::open(project)?;
        let books = database::tone_samples(&conn, SAMPLES)?;
        drop(conn);
        if books.is_empty() {
            return Ok(None);
        }
        let total = books.len();
        let (tx, results) = channel();
        let done = Arc::new(AtomicUsize::new(0));
        let stop = Arc::new(AtomicBool::new(false));
        let queue = Arc::new(std::sync::Mutex::new(books));
        let (wtx, wrx) = channel::<(String, Tone)>();
        // One writer, so the measuring threads never wait on SQLite.
        let project_w = project.to_path_buf();
        let writer = std::thread::Builder::new()
            .name("annotator-tone-writer".into())
            .spawn(move || {
                let Ok(conn) = database::open(&project_w) else {
                    return;
                };
                while let Ok((book, t)) = wrx.recv() {
                    let _ = database::set_book_tone(&conn, &book, &t);
                }
            })
            .map_err(|e| e.to_string())?;
        for i in 0..threads.max(1) {
            let (queue, tx, wtx, done, stop, wake) = (
                queue.clone(),
                tx.clone(),
                wtx.clone(),
                done.clone(),
                stop.clone(),
                wake.clone(),
            );
            std::thread::Builder::new()
                .name(format!("annotator-tone-{i}"))
                .spawn(move || {
                    loop {
                        if stop.load(Ordering::Relaxed) {
                            break;
                        }
                        let job = queue.lock().ok().and_then(|mut q| q.pop());
                        let Some((book, pages)) = job else { break };
                        if let Some(t) = book_tone(&pages) {
                            let _ = wtx.send((book.clone(), t));
                            let _ = tx.send((book, t));
                        }
                        done.fetch_add(1, Ordering::Relaxed);
                        wake();
                    }
                })
                .map_err(|e| e.to_string())?;
        }
        Ok(Some(ToneJob {
            results,
            total,
            done,
            stop,
            writer: Some(writer),
        }))
    }

    pub fn done(&self) -> usize {
        self.done.load(Ordering::Relaxed)
    }

    pub fn finished(&self) -> bool {
        self.done() >= self.total
    }

    /// Wait until every measured tone is in the project (the writer ends
    /// when the measuring threads have).
    pub fn wait(mut self) {
        if let Some(w) = self.writer.take() {
            let _ = w.join();
        }
    }
}

impl Drop for ToneJob {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn white_is_neutral_and_cream_is_yellow() {
        let (l, b) = lab_l_b([255, 255, 255]);
        assert!((l - 100.0).abs() < 0.5 && b.abs() < 0.5, "{l} {b}");
        let (_, b) = lab_l_b([200, 200, 200]);
        assert!(b.abs() < 0.5);
        let (_, b) = lab_l_b([235, 220, 180]);
        assert!(b > 15.0, "{b}");
    }

    #[test]
    fn the_paper_is_the_bright_fifth_not_the_ink() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("p.png");
        // Cream paper with a black block over a third of it.
        let mut img = image::RgbImage::from_pixel(200, 300, image::Rgb([236, 222, 186]));
        for y in 0..100 {
            for x in 0..200 {
                img.put_pixel(x, y, image::Rgb([20, 20, 20]));
            }
        }
        img.save(&path).unwrap();
        assert_eq!(page_paper(&path), Some([236, 222, 186]));
        let t = book_tone(&[path.clone(), path]).unwrap();
        assert!(t.yellowness > 15.0, "{t:?}");
    }
}
