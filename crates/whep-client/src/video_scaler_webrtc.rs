//! I420 scaling helpers backed by libwebrtc.

use libwebrtc::video_frame::{I420Buffer, VideoBuffer};

pub fn scale_i420(src: &I420Buffer, target_width: u32, target_height: u32) -> I420Buffer {
    let (stride_y, stride_u, stride_v) = src.strides();
    let mut owned =
        I420Buffer::with_strides(src.width(), src.height(), stride_y, stride_u, stride_v);
    let (src_y, src_u, src_v) = src.data();
    let (dst_y, dst_u, dst_v) = owned.data_mut();
    dst_y.copy_from_slice(src_y);
    dst_u.copy_from_slice(src_u);
    dst_v.copy_from_slice(src_v);

    owned.scale(target_width as i32, target_height as i32)
}
