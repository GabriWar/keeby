// SPDX-License-Identifier: GPL-3.0-only
// Ported from wl-kbptr (https://github.com/moverest/wl-kbptr) by moverest.
//! Target detection for floating mode: wl-kbptr's OpenCV code in
//! `csrc/target_detection.cpp`, built by `build.rs`.

use super::modes::Rect;

#[cfg(feature = "detect")]
mod ffi {
    use super::Rect;

    #[repr(C)]
    #[derive(Clone, Copy)]
    pub struct CRect {
        pub x: i32,
        pub y: i32,
        pub w: i32,
        pub h: i32,
    }

    extern "C" {
        pub fn compute_target_from_img_buffer(
            data: *mut libc::c_void,
            height: u32,
            width: u32,
            stride: u32,
            format: u32,
            transform: u32,
            initial_area: CRect,
            areas: *mut *mut CRect,
        ) -> libc::c_int;
    }

    pub fn detect(data: &mut [u8], width: u32, height: u32, stride: u32, format: u32, transform: u32, area: Rect) -> Vec<Rect> {
        let mut out: *mut CRect = std::ptr::null_mut();
        let initial = CRect { x: area.x, y: area.y, w: area.w, h: area.h };
        // SAFETY: `data` holds `stride * height` bytes in `format`; the C side
        // only reads it and returns a malloc'd array of `n` rects.
        unsafe {
            let n = compute_target_from_img_buffer(
                data.as_mut_ptr().cast(),
                height,
                width,
                stride,
                format,
                transform,
                initial,
                &mut out,
            );
            let rects = if out.is_null() || n <= 0 {
                Vec::new()
            } else {
                std::slice::from_raw_parts(out, n as usize)
                    .iter()
                    .map(|r| Rect { x: r.x, y: r.y, w: r.w, h: r.h })
                    .collect()
            };
            libc::free(out.cast());
            rects
        }
    }
}

pub const AVAILABLE: bool = cfg!(feature = "detect");

/// Finds clickable targets in a captured frame. `area` is the captured region
/// in surface coordinates; rects come back in the same space.
#[allow(unused_variables)]
pub fn detect(data: &mut [u8], width: u32, height: u32, stride: u32, format: u32, transform: u32, area: Rect) -> Vec<Rect> {
    #[cfg(feature = "detect")]
    return ffi::detect(data, width, height, stride, format, transform, area);
    #[cfg(not(feature = "detect"))]
    Vec::new()
}
