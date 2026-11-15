//! Bounded lossy output adapter for the already-pinned libwebp 1.6.0 binding.
//! WebPMemoryWriter is deliberately not used: every emitted segment is checked
//! before destination growth. Native codec workspace is bounded by input geometry.
#![allow(unsafe_code)]
use crate::{app::error::AppError, capture::geometry::PhysicalRect};
use image::RgbaImage;
use libwebp_sys as native;
use std::mem::MaybeUninit;

struct Picture(native::WebPPicture);
impl Drop for Picture {
    fn drop(&mut self) {
        // SAFETY: this guard owns a successfully initialized picture. libwebp
        // owns the imported pixel allocations and frees them exactly once.
        unsafe {
            native::WebPPictureFree(&mut self.0);
        }
    }
}
struct Output {
    bytes: Vec<u8>,
    limit: usize,
    failed: bool,
}
unsafe extern "C" fn write_segment(
    data: *const u8,
    size: usize,
    picture: *const native::WebPPicture,
) -> i32 {
    // SAFETY: WebPEncode borrows our initialized picture and custom_ptr only
    // for this synchronous call. No native pointer is retained after return.
    let output = unsafe { &mut *((*picture).custom_ptr.cast::<Output>()) };
    let accepted = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        if size > output.limit.saturating_sub(output.bytes.len())
            || (size > 0 && data.is_null())
            || output.bytes.try_reserve_exact(size).is_err()
        {
            return false;
        }
        if size > 0 {
            // SAFETY: the encoder supplies a live segment of exactly size
            // bytes. We checked the bound before forming a slice or allocating.
            output
                .bytes
                .extend_from_slice(unsafe { std::slice::from_raw_parts(data, size) });
        }
        true
    }))
    .unwrap_or(false);
    output.failed |= !accepted;
    i32::from(accepted)
}

pub(super) fn encode_lossy(
    pixels: &RgbaImage,
    quality: u8,
    limit: usize,
) -> Result<Vec<u8>, AppError> {
    PhysicalRect::new(0, 0, pixels.width(), pixels.height())?.rgba_len()?;
    if !(1..=100).contains(&quality)
        || pixels.width() > 16383
        || pixels.height() > 16383
        || limit == 0
        || limit > super::export::MAX_ENCODED_BYTES
    {
        return Err(AppError::InvalidInput(
            "lossy WebP input exceeds its validated bounds",
        ));
    }
    let stride = i32::try_from(
        pixels
            .width()
            .checked_mul(4)
            .ok_or(AppError::Encoder("WebP row size overflow"))?,
    )
    .map_err(|_| AppError::Encoder("WebP row size overflow"))?;
    let mut config = MaybeUninit::<native::WebPConfig>::zeroed();
    let mut picture = MaybeUninit::<native::WebPPicture>::zeroed();
    // SAFETY: use encoder ABI initializers, zeroing reserved/padding fields
    // before libwebp writes documented fields. No Rust value is assumed valid
    // until its initializer succeeds. Quality is a finite validated integer.
    unsafe {
        if native::WebPConfigInitInternal(
            config.as_mut_ptr(),
            native::WebPPreset::WEBP_PRESET_DEFAULT,
            f32::from(quality),
            native::WEBP_ENCODER_ABI_VERSION as i32,
        ) == 0
            || native::WebPPictureInitInternal(
                picture.as_mut_ptr(),
                native::WEBP_ENCODER_ABI_VERSION as i32,
            ) == 0
        {
            return Err(AppError::Encoder("WebP encoder initialization failed"));
        }
        let config = config.assume_init();
        let mut picture = Picture(picture.assume_init());
        if native::WebPValidateConfig(&config) == 0 {
            return Err(AppError::Encoder("WebP encoder configuration rejected"));
        }
        picture.0.width = pixels.width() as i32;
        picture.0.height = pixels.height() as i32;
        // ImportRGBA copies rows into native-owned buffers; libwebp never
        // writes through or retains our immutable prepared-input allocation.
        if native::WebPPictureImportRGBA(&mut picture.0, pixels.as_ptr(), stride) == 0 {
            return Err(AppError::Encoder("WebP input allocation failed"));
        }
        let mut output = Output {
            bytes: Vec::new(),
            limit,
            failed: false,
        };
        picture.0.writer = Some(write_segment);
        picture.0.custom_ptr = (&mut output as *mut Output).cast();
        let success = native::WebPEncode(&config, &mut picture.0) != 0;
        // The picture no longer needs the borrowed writer state. Clear it
        // before either value drops, including on any error path below.
        picture.0.custom_ptr = std::ptr::null_mut();
        picture.0.writer = None;
        if output.failed {
            return Err(AppError::Encoder(
                "WebP output exceeds memory or encoded size budget",
            ));
        }
        if !success || output.bytes.is_empty() {
            return Err(AppError::Encoder("lossy WebP encoding failed"));
        }
        Ok(output.bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::GenericImageView;
    #[test]
    fn native_writer_rejects_budget_exhaustion_and_recovers() {
        let pixels = RgbaImage::from_fn(64, 48, |x, y| {
            image::Rgba([(x * 7) as u8, (y * 11) as u8, (x * y) as u8, 255])
        });
        let original = pixels.clone();
        for limit in [1, 12, 32] {
            assert!(matches!(
                encode_lossy(&pixels, 90, limit),
                Err(AppError::Encoder(_))
            ));
        }
        let encoded = encode_lossy(&pixels, 90, super::super::export::MAX_ENCODED_BYTES).unwrap();
        let decoded = image::load_from_memory(&encoded).unwrap().to_rgba8();
        assert_eq!(decoded.dimensions(), pixels.dimensions());
        assert_eq!(pixels, original);
        // Independent of this adapter: the previously vetted wrapper still
        // encodes the same input successfully with the pinned native library.
        let reference = webpx::Encoder::new_rgba(pixels.as_raw(), 64, 48)
            .quality(90.0)
            .encode_owned(webpx::Unstoppable)
            .unwrap();
        assert_eq!(
            image::load_from_memory(&reference).unwrap().dimensions(),
            (64, 48)
        );
    }
}
