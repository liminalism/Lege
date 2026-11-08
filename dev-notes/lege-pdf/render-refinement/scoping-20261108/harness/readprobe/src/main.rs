//! read-probe <pdf> <cache_capacity> <mode> [width height] [order]
//!   mode = shared : one RenderSession, pages compiled+rendered sequentially on
//!                   this thread (thread-local CpuWorkerContext persists, as on
//!                   a Tokio blocking / rayon worker in lege-process)
//!   mode = fresh  : a new RenderSession per page (new session id => the
//!                   thread-local worker context is reset; new backend)
//!   order = comma-separated page list (default: all pages in order)
//! Prints `page <p> hash <h>` per render (DefaultHasher over the RGB plane).
#[cfg(feature = "mimalloc")]
type Inner = mimalloc::MiMalloc;
#[cfg(feature = "mimalloc")]
const INNER: Inner = mimalloc::MiMalloc;
#[cfg(not(feature = "mimalloc"))]
type Inner = std::alloc::System;
#[cfg(not(feature = "mimalloc"))]
const INNER: Inner = std::alloc::System;

/// Tracing wrapper: records every allocation of exactly TRACE_SIZE bytes
/// (ArcInner<CompiledPage> = 16 + size_of::<CompiledPage>() = 328) into a
/// fixed ring, without allocating. Used to read the CompiledPage address
/// that lege-pdf-read keeps private.
struct Tracer;
const TRACE_SIZE: usize = 328;
static RING: [std::sync::atomic::AtomicUsize; 1 << 16] =
    [const { std::sync::atomic::AtomicUsize::new(0) }; 1 << 16];
static IDX: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
unsafe impl std::alloc::GlobalAlloc for Tracer {
    unsafe fn alloc(&self, l: std::alloc::Layout) -> *mut u8 {
        let p = unsafe { std::alloc::GlobalAlloc::alloc(&INNER, l) };
        if l.size() == TRACE_SIZE {
            let i = IDX.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            RING[i & 0xffff].store(p as usize, std::sync::atomic::Ordering::Relaxed);
        }
        p
    }
    unsafe fn dealloc(&self, p: *mut u8, l: std::alloc::Layout) {
        unsafe { std::alloc::GlobalAlloc::dealloc(&INNER, p, l) }
    }
}
#[global_allocator]
static GLOBAL: Tracer = Tracer;

/// Address of the last TRACE_SIZE allocation made since ring index `since`
/// (single-threaded use: the compile of one page).
fn last_traced(since: usize) -> usize {
    let now = IDX.load(std::sync::atomic::Ordering::Relaxed);
    if now == since { 0 } else { RING[(now - 1) & 0xffff].load(std::sync::atomic::Ordering::Relaxed) }
}
fn trace_mark() -> usize {
    IDX.load(std::sync::atomic::Ordering::Relaxed)
}

use std::hash::{Hash, Hasher};
use std::sync::Arc;

use lege_pdf_read::{RasterPlane, RasterProduct, RenderSession};

fn hash_plane(plane: &RasterPlane) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    match plane {
        RasterPlane::Rgb8(s) => {
            s.width.hash(&mut h);
            s.height.hash(&mut h);
            s.pixels.hash(&mut h);
        }
        RasterPlane::Gray8(s) => {
            s.width.hash(&mut h);
            s.height.hash(&mut h);
            s.pixels.hash(&mut h);
        }
    }
    h.finish()
}

fn main() {
    let a: Vec<String> = std::env::args().collect();
    let path = &a[1];
    let cap: usize = a[2].parse().expect("cap");
    let mode = a[3].as_str();
    let width: u32 = a.get(4).map(|v| v.parse().unwrap()).unwrap_or(1224);
    let height: u32 = a.get(5).map(|v| v.parse().unwrap()).unwrap_or(1584);
    let bytes: Arc<[u8]> = Arc::from(std::fs::read(path).expect("read pdf"));
    let open = || RenderSession::open_with_cache_capacity(Arc::clone(&bytes), None, cap).expect("open");
    let mut session = open();
    let n = session.page_count();
    let order: Vec<u32> = match a.get(6) {
        Some(s) => s.split(',').map(|x| x.parse().unwrap()).collect(),
        None => (0..n).collect(),
    };
    if mode == "evict1" {
        // Single thread: render `a` and drop its handle; compile-only the fill
        // pages (the page cache evicts `a`, freeing it); compile+render `c`.
        let (a_page, fills, c_page) = (order[0], order[1..order.len() - 1].to_vec(), order[order.len() - 1]);
        {
            let m = trace_mark();
            let ha = session.compile(a_page).expect("compile a");
            let addr = last_traced(m);
            let pa = session.render(&ha, &RasterProduct::rgb8(width, height)).expect("render a");
            println!("page {a_page} hash {:016x} ptr {addr:#x}", hash_plane(&pa));
        }
        for f in fills {
            let m = trace_mark();
            let hf = session.compile(f).expect("compile fill");
            println!("fill {f} ptr {:#x}", last_traced(m));
            drop(hf);
        }
        let m = trace_mark();
        let hc = session.compile(c_page).expect("compile c");
        let addr = last_traced(m);
        let pc = session.render(&hc, &RasterProduct::rgb8(width, height)).expect("render c");
        println!("page {c_page} hash {:016x} ptr {addr:#x}", hash_plane(&pc));
        return;
    }
    if mode == "evict" {
        // order = a,f1..fn,c : this thread compiles+renders `a` and keeps the
        // handle; a second thread compiles (and renders if FILL_RENDER=1) the
        // fill pages through the same session, evicting `a` from the page
        // cache; this thread then drops `a`, compiles and renders `c`.
        let session = Arc::new(session);
        let (a_page, fills, c_page) = (order[0], order[1..order.len() - 1].to_vec(), order[order.len() - 1]);
        let m = trace_mark();
        let ha = session.compile(a_page).expect("compile a");
        let addr_a = last_traced(m);
        let pa = session.render(&ha, &RasterProduct::rgb8(width, height)).expect("render a");
        println!("page {a_page} hash {:016x} ptr {addr_a:#x}", hash_plane(&pa));
        drop(pa);
        let s2 = Arc::clone(&session);
        let fill_render = std::env::var("FILL_RENDER").map(|v| v == "1").unwrap_or(false);
        std::thread::spawn(move || {
            for f in fills {
                let hf = s2.compile(f).expect("compile fill");
                if fill_render {
                    let pf = s2.render(&hf, &RasterProduct::rgb8(width, height)).expect("render fill");
                    println!("fill {f} hash {:016x}", hash_plane(&pf));
                }
            }
        })
        .join()
        .unwrap();
        drop(ha);
        let m = trace_mark();
        let hc = session.compile(c_page).expect("compile c");
        let addr_c = last_traced(m);
        let pc = session.render(&hc, &RasterProduct::rgb8(width, height)).expect("render c");
        println!("page {c_page} hash {:016x} ptr {addr_c:#x}", hash_plane(&pc));
        return;
    }
    for p in order {
        if mode == "fresh" {
            session = open();
        }
        let m = trace_mark();
        let page = session.compile(p).expect("compile");
        let addr = last_traced(m);
        let plane = session.render(&page, &RasterProduct::rgb8(width, height)).expect("render");
        let mut h = std::collections::hash_map::DefaultHasher::new();
        match &plane {
            RasterPlane::Rgb8(s) => {
                s.width.hash(&mut h);
                s.height.hash(&mut h);
                s.pixels.hash(&mut h);
            }
            RasterPlane::Gray8(s) => {
                s.width.hash(&mut h);
                s.height.hash(&mut h);
                s.pixels.hash(&mut h);
            }
        }
        println!("page {p} hash {:016x} ptr {addr:#x}", h.finish());
        drop(plane);
        drop(page);
    }
}
