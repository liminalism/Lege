//! What the desktop shell knows about a file: its icon, and dragging it out.
//!
//! On Windows the icon is the one Explorer shows for that file — the PDF
//! reader's icon for a `.pdf`, the folder icon for a folder — rendered by the
//! shell at exactly the size asked for, so a tile's icon is drawn one shell
//! pixel per screen pixel at any display scale. Dragging hands the shell's own
//! data object for the file to `SHDoDragDrop`, which supplies the drag image
//! and speaks every format a drop target can ask for (file list, shell item,
//! file contents), so the file can be dropped on Explorer, the desktop, a mail
//! or chat window, or any other application.
//!
//! Elsewhere there is no icon (the tile draws a generic page or folder) and no
//! drag-out.

use std::path::Path;

use liminalkit_raster::Bitmap;

/// The shell's icon for `path`, `size × size` physical pixels, straight
/// alpha (as `PreparedImage::from_bitmap` wants it). `None` when the shell
/// has none, or on platforms without one.
pub fn file_icon(path: &Path, size: u32) -> Option<Bitmap> {
    #[cfg(windows)]
    {
        windows_impl::file_icon(path, size)
    }
    #[cfg(not(windows))]
    {
        let _ = (path, size);
        None
    }
}

/// How a drag out of the window ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DragOutcome {
    /// A target accepted the file (it was copied there).
    Dropped,
    /// Released over nothing that takes files, or cancelled with Escape.
    Cancelled,
    /// Not available on this platform, or the shell refused.
    Unavailable,
}

/// Drag `path` out of the window `hwnd`, offering a copy. Blocks in the
/// shell's modal drag loop until the button is released; window messages keep
/// being pumped meanwhile, so the window stays responsive.
pub fn drag_out(hwnd: isize, path: &Path) -> DragOutcome {
    #[cfg(windows)]
    {
        windows_impl::drag_out(hwnd, path)
    }
    #[cfg(not(windows))]
    {
        let _ = (hwnd, path);
        DragOutcome::Unavailable
    }
}

#[cfg(windows)]
mod windows_impl {
    use std::os::windows::ffi::OsStrExt;
    use std::path::Path;

    use liminalkit_raster::Bitmap;
    use windows::Win32::Foundation::{HWND, SIZE};
    use windows::Win32::Graphics::Gdi::{
        BI_RGB, BITMAP, BITMAPINFO, BITMAPINFOHEADER, CreateCompatibleDC, DIB_RGB_COLORS, DeleteDC,
        DeleteObject, GetDIBits, GetObjectW, HBITMAP, HGDIOBJ,
    };
    use windows::Win32::System::Com::{COINIT_APARTMENTTHREADED, CoInitializeEx, IDataObject};
    use windows::Win32::System::Ole::DROPEFFECT_COPY;
    use windows::Win32::UI::Shell::{
        BHID_DataObject, IShellItem, IShellItemImageFactory, SHCreateItemFromParsingName,
        SHDoDragDrop, SIIGBF_BIGGERSIZEOK, SIIGBF_ICONONLY,
    };
    use windows::core::PCWSTR;

    use super::DragOutcome;

    /// The shell objects need COM on the calling thread. winit has already
    /// initialised OLE on the UI thread, which makes this a cheap `S_FALSE`;
    /// on any other thread (a test, say) it sets up an apartment. A thread
    /// already in the multithreaded apartment answers `RPC_E_CHANGED_MODE`,
    /// which is fine too: the shell objects work there.
    fn ensure_com() {
        thread_local! {
            static INITIALISED: () = {
                // SAFETY: no reserved pointer; the apartment, once entered, is
                // left in place for the thread's life, as COM expects.
                let _ = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) };
            };
        }
        INITIALISED.with(|_| {});
    }

    /// The shell's parsing name for `path`: no `\\?\` verbatim prefix, which
    /// `SHCreateItemFromParsingName` does not accept.
    fn wide(path: &Path) -> Vec<u16> {
        let text = path.as_os_str().to_string_lossy();
        let plain = text
            .strip_prefix(r"\\?\UNC\")
            .map(|rest| format!(r"\\{rest}"))
            .unwrap_or_else(|| text.strip_prefix(r"\\?\").unwrap_or(&text).to_owned());
        std::ffi::OsStr::new(&plain)
            .encode_wide()
            .chain(std::iter::once(0))
            .collect()
    }

    fn shell_item(path: &Path) -> windows::core::Result<IShellItem> {
        ensure_com();
        let name = wide(path);
        // SAFETY: `name` is NUL-terminated and outlives the call.
        unsafe { SHCreateItemFromParsingName(PCWSTR(name.as_ptr()), None) }
    }

    pub(super) fn file_icon(path: &Path, size: u32) -> Option<Bitmap> {
        let item = shell_item(path).ok()?;
        let factory: IShellItemImageFactory = windows::core::Interface::cast(&item).ok()?;
        let side = i32::try_from(size.clamp(1, 1024)).ok()?;
        // The file type's icon, never a thumbnail of the content: the tile
        // shows what kind of file this is, the way Explorer's icon view does.
        // SAFETY: plain COM call on a live interface.
        let bitmap = unsafe {
            factory.GetImage(
                SIZE { cx: side, cy: side },
                SIIGBF_ICONONLY | SIIGBF_BIGGERSIZEOK,
            )
        }
        .ok()?;
        let pixels = read_bitmap(bitmap);
        // SAFETY: the shell handed ownership of the bitmap to us.
        unsafe {
            let _ = DeleteObject(HGDIOBJ(bitmap.0));
        }
        let (width, height, mut pixels) = pixels?;
        straighten(&mut pixels);
        Some(Bitmap {
            width,
            height,
            pixels,
        })
    }

    /// The bitmap's pixels as `0xAARRGGBB` words, top row first.
    fn read_bitmap(bitmap: HBITMAP) -> Option<(u32, u32, Vec<u32>)> {
        let mut info = BITMAP::default();
        // SAFETY: `info` is a BITMAP and the size passed is its own.
        let read = unsafe {
            GetObjectW(
                HGDIOBJ(bitmap.0),
                std::mem::size_of::<BITMAP>() as i32,
                Some(std::ptr::from_mut(&mut info).cast()),
            )
        };
        if read == 0 || info.bmWidth <= 0 || info.bmHeight == 0 {
            return None;
        }
        let (width, height) = (info.bmWidth, info.bmHeight.abs());
        let mut header = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: width,
                // Negative: top-down rows, the order a raster wants.
                biHeight: -height,
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut pixels = vec![0u32; width as usize * height as usize];
        // SAFETY: a memory DC for the conversion, released below; `pixels`
        // holds exactly `width × height` 32-bit words.
        let lines = unsafe {
            let dc = CreateCompatibleDC(None);
            let lines = GetDIBits(
                dc,
                bitmap,
                0,
                height as u32,
                Some(pixels.as_mut_ptr().cast()),
                &mut header,
                DIB_RGB_COLORS,
            );
            let _ = DeleteDC(dc);
            lines
        };
        // Little-endian BGRA bytes are already `0xAARRGGBB` words.
        (lines == height).then_some((width as u32, height as u32, pixels))
    }

    /// Bring the shell's pixels to the straight alpha `PreparedImage`
    /// resamples from. The shell answers in either form: a thumbnail-style
    /// bitmap is premultiplied, but an icon is often returned with straight
    /// alpha (measured: a PDF handler's icon at 56, 84 and 112 px has hundreds
    /// of pixels whose colour exceeds their alpha). A channel above its alpha
    /// cannot occur premultiplied, so its presence marks straight data, which
    /// is left alone; dividing it again would whiten the icon's faint edge
    /// pixels into a visible plate. An icon without an alpha channel (every
    /// alpha zero, from an old-style icon) is opaque.
    fn straighten(pixels: &mut [u32]) {
        if pixels.iter().all(|p| p >> 24 == 0) {
            for p in pixels.iter_mut() {
                *p |= 0xFF00_0000;
            }
            return;
        }
        let channel_max = |p: u32| ((p >> 16) & 255).max((p >> 8) & 255).max(p & 255);
        if pixels.iter().any(|&p| channel_max(p) > p >> 24) {
            return;
        }
        for p in pixels.iter_mut() {
            let a = *p >> 24;
            if a == 0 || a == 255 {
                continue;
            }
            let channel = |shift: u32| (((*p >> shift) & 255) * 255 + a / 2) / a;
            let (r, g, b) = (
                channel(16).min(255),
                channel(8).min(255),
                channel(0).min(255),
            );
            *p = (a << 24) | (r << 16) | (g << 8) | b;
        }
    }

    pub(super) fn drag_out(hwnd: isize, path: &Path) -> DragOutcome {
        let Ok(item) = shell_item(path) else {
            return DragOutcome::Unavailable;
        };
        // The shell's own data object for the file: CF_HDROP, the shell ID
        // list and the file's contents, whatever the target asks for.
        // SAFETY: plain COM calls on live interfaces; a null drop source
        // makes the shell supply its own, and the drag image with it.
        let data: windows::core::Result<IDataObject> =
            unsafe { item.BindToHandler(None, &BHID_DataObject) };
        let Ok(data) = data else {
            return DragOutcome::Unavailable;
        };
        let window = HWND(hwnd as *mut core::ffi::c_void);
        // Copy only: a drop on Explorer must never move the file away from
        // where the queue and the output list say it is.
        match unsafe { SHDoDragDrop(Some(window), &data, None, DROPEFFECT_COPY) } {
            Ok(effect) if effect.0 != 0 => DragOutcome::Dropped,
            Ok(_) => DragOutcome::Cancelled,
            Err(_) => DragOutcome::Unavailable,
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        fn temp(name: &str) -> std::path::PathBuf {
            let dir = std::env::temp_dir().join(format!("lege-shell-{}", std::process::id()));
            std::fs::create_dir_all(&dir).unwrap();
            dir.join(name)
        }

        #[test]
        fn the_shell_gives_file_and_folder_icons_at_the_size_asked_for() {
            let pdf = temp("tile test.pdf");
            std::fs::write(&pdf, b"%PDF-1.4\n%%EOF\n").unwrap();
            for size in [48, 96, 144] {
                let icon = file_icon(&pdf, size).expect("a .pdf has an icon");
                assert_eq!((icon.width, icon.height), (size, size));
                assert!(
                    icon.pixels.iter().any(|p| p >> 24 == 255),
                    "some opaque pixels"
                );
                assert!(
                    icon.pixels.iter().any(|p| p >> 24 == 0),
                    "a transparent margin"
                );
            }
            let folder = pdf.parent().unwrap();
            let icon = file_icon(folder, 64).expect("a folder has an icon");
            assert_eq!((icon.width, icon.height), (64, 64));
        }

        #[test]
        fn the_shell_data_object_offers_the_file_for_a_drop() {
            use windows::Win32::System::Com::{DVASPECT_CONTENT, FORMATETC, TYMED_HGLOBAL};
            use windows::Win32::System::Ole::CF_HDROP;
            let pdf = temp("drag test.pdf");
            std::fs::write(&pdf, b"%PDF-1.4\n%%EOF\n").unwrap();
            let item = shell_item(&pdf).unwrap();
            let data: IDataObject = unsafe { item.BindToHandler(None, &BHID_DataObject) }.unwrap();
            let format = FORMATETC {
                cfFormat: CF_HDROP.0,
                ptd: std::ptr::null_mut(),
                dwAspect: DVASPECT_CONTENT.0,
                lindex: -1,
                tymed: TYMED_HGLOBAL.0 as u32,
            };
            // S_OK: a drop target asking for a file list gets one.
            assert!(unsafe { data.QueryGetData(&format) }.is_ok());
        }

        #[test]
        fn premultiplied_pixels_are_straightened_and_straight_ones_kept() {
            let mut pixels = [0x8040_2010, 0xFF11_2233, 0];
            straighten(&mut pixels);
            assert_eq!(pixels[0], 0x8080_4020);
            assert_eq!(pixels[1], 0xFF11_2233);
            assert_eq!(pixels[2], 0);
            // A white pixel at alpha 2 is only possible with straight alpha:
            // the whole bitmap is then left as it came.
            let mut pixels = [0x02FF_FFFF, 0x8040_2010];
            straighten(&mut pixels);
            assert_eq!(pixels, [0x02FF_FFFF, 0x8040_2010]);
        }
    }
}
