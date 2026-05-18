//! Video scaling helpers for I420 frames.

use crate::video_scaler_webrtc;
use libwebrtc::video_frame::I420Buffer;
use libwebrtc::video_frame::VideoBuffer;

pub const TARGET_WIDTH: u32 = 1600;
pub const TARGET_HEIGHT: u32 = 900;

pub struct VideoScaler {
    target_width: u32,
    target_height: u32,
}

impl VideoScaler {
    pub fn new() -> Self {
        Self::new_with_target(TARGET_WIDTH, TARGET_HEIGHT)
    }

    pub fn new_with_target(target_width: u32, target_height: u32) -> Self {
        Self {
            target_width,
            target_height,
        }
    }

    pub fn scale_if_needed(&self, src: &I420Buffer) -> Option<I420Buffer> {
        let src_width = src.width();
        let src_height = src.height();

        if !self.needs_upscale(src_width, src_height) {
            return None;
        }

        Some(self.scale(src))
    }

    pub fn scale(&self, src: &I420Buffer) -> I420Buffer {
        video_scaler_webrtc::scale_i420(src, self.target_width, self.target_height)
    }

    fn needs_upscale(&self, width: u32, height: u32) -> bool {
        width < self.target_width || height < self.target_height
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_scale_if_needed_skips_target_size_frame() {
        let scaler = VideoScaler::new();
        let src = I420Buffer::new(TARGET_WIDTH, TARGET_HEIGHT);

        let scaled = scaler.scale_if_needed(&src);

        assert!(scaled.is_none());
    }

    #[test]
    fn test_scale_if_needed_scales_small_frame() {
        let scaler = VideoScaler::new();
        let mut src = I420Buffer::new(640, 360);
        let (y, u, v) = src.data_mut();
        y.fill(16);
        u.fill(128);
        v.fill(128);

        let scaled = scaler.scale_if_needed(&src).unwrap();

        assert_eq!(scaled.width(), TARGET_WIDTH);
        assert_eq!(scaled.height(), TARGET_HEIGHT);
    }
}
