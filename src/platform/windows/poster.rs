//! Native first-frame poster extraction. Only finalized local copies are passed in.
#![allow(unsafe_code)]
use std::{
    io::{Read, Write},
    os::windows::ffi::OsStrExt,
};
use windows::{
    Win32::{
        Foundation::SIZE,
        Graphics::Gdi::*,
        System::Com::{COINIT_APARTMENTTHREADED, CoInitializeEx, CoUninitialize},
        UI::Shell::{IShellItemImageFactory, SHCreateItemFromParsingName, SIIGBF_THUMBNAILONLY},
    },
    core::PCWSTR,
};
struct Com;
impl Drop for Com {
    fn drop(&mut self) {
        unsafe {
            CoUninitialize();
        }
    }
}
struct Bitmap(HBITMAP);
impl Drop for Bitmap {
    fn drop(&mut self) {
        unsafe {
            let _ = DeleteObject(HGDIOBJ(self.0.0));
        }
    }
}
struct Dc(HDC);
impl Drop for Dc {
    fn drop(&mut self) {
        unsafe {
            let _ = DeleteDC(self.0);
        }
    }
}
pub fn mp4_poster(mut source: impl Read) -> Option<eframe::egui::ColorImage> {
    // A private, bounded finalized copy supplies the extension needed by Windows'
    // thumbnail provider. It is removed on every exit; no original is modified.
    let directory = tempfile::tempdir().ok()?;
    let path = directory.path().join("poster.mp4");
    let mut target = std::fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&path)
        .ok()?;
    let copied = std::io::copy(
        &mut Read::by_ref(&mut source).take(64 * 1024 * 1024 + 1),
        &mut target,
    )
    .ok()?;
    if copied > 64 * 1024 * 1024 {
        return None;
    }
    target.flush().ok()?;
    drop(target);
    let wide: Vec<_> = path.as_os_str().encode_wide().chain(Some(0)).collect();
    unsafe {
        CoInitializeEx(None, COINIT_APARTMENTTHREADED).ok().ok()?;
        let _com = Com;
        let item: IShellItemImageFactory =
            SHCreateItemFromParsingName(PCWSTR(wide.as_ptr()), None).ok()?;
        let bitmap = Bitmap(
            item.GetImage(SIZE { cx: 256, cy: 256 }, SIIGBF_THUMBNAILONLY)
                .ok()?,
        );
        let mut object = BITMAP::default();
        if GetObjectW(
            HGDIOBJ(bitmap.0.0),
            size_of::<BITMAP>() as i32,
            Some((&mut object as *mut BITMAP).cast()),
        ) == 0
        {
            return None;
        }
        let (width, height) = (object.bmWidth, object.bmHeight);
        if !(1..=512).contains(&width) || !(1..=512).contains(&height) {
            return None;
        }
        let dc = Dc(CreateCompatibleDC(None));
        if dc.0.0.is_null() {
            return None;
        }
        let mut info = BITMAPINFO::default();
        info.bmiHeader.biSize = size_of::<BITMAPINFOHEADER>() as u32;
        info.bmiHeader.biWidth = width;
        info.bmiHeader.biHeight = -height;
        info.bmiHeader.biPlanes = 1;
        info.bmiHeader.biBitCount = 32;
        info.bmiHeader.biCompression = BI_RGB.0;
        let mut pixels = vec![0u8; width as usize * height as usize * 4];
        if GetDIBits(
            dc.0,
            bitmap.0,
            0,
            height as u32,
            Some(pixels.as_mut_ptr().cast()),
            &mut info,
            DIB_RGB_COLORS,
        ) != height
        {
            return None;
        }
        for pixel in pixels.as_chunks_mut::<4>().0 {
            pixel.swap(0, 2);
            pixel[3] = 255;
        }
        Some(eframe::egui::ColorImage::from_rgba_unmultiplied(
            [width as usize, height as usize],
            &pixels,
        ))
    }
}
