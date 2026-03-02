//! MKV (Matroska) writer for streaming video/audio to stdout
//!
//! Supports:
//! - V_UNCOMPRESSED (I420 YUV) video
//! - A_PCM/INT/LIT (PCM S16LE) audio
//!
//! Uses unknown-size encoding for Segment and Cluster to enable streaming output.

use std::io::{Result, Write};

/// EBML Element IDs
mod ebml_ids {
    // EBML Header
    pub const EBML: &[u8] = &[0x1A, 0x45, 0xDF, 0xA3];
    pub const EBML_VERSION: &[u8] = &[0x42, 0x86];
    pub const EBML_READ_VERSION: &[u8] = &[0x42, 0xF7];
    pub const EBML_MAX_ID_LENGTH: &[u8] = &[0x42, 0xF2];
    pub const EBML_MAX_SIZE_LENGTH: &[u8] = &[0x42, 0xF3];
    pub const DOC_TYPE: &[u8] = &[0x42, 0x82];
    pub const DOC_TYPE_VERSION: &[u8] = &[0x42, 0x87];
    pub const DOC_TYPE_READ_VERSION: &[u8] = &[0x42, 0x85];

    // Segment
    pub const SEGMENT: &[u8] = &[0x18, 0x53, 0x80, 0x67];

    // Info
    pub const INFO: &[u8] = &[0x15, 0x49, 0xA9, 0x66];
    pub const TIMESTAMP_SCALE: &[u8] = &[0x2A, 0xD7, 0xB1];
    pub const MUXING_APP: &[u8] = &[0x4D, 0x80];
    pub const WRITING_APP: &[u8] = &[0x57, 0x41];

    // Tracks
    pub const TRACKS: &[u8] = &[0x16, 0x54, 0xAE, 0x6B];
    pub const TRACK_ENTRY: &[u8] = &[0xAE];
    pub const TRACK_NUMBER: &[u8] = &[0xD7];
    pub const TRACK_UID: &[u8] = &[0x73, 0xC5];
    pub const TRACK_TYPE: &[u8] = &[0x83];
    pub const CODEC_ID: &[u8] = &[0x86];
    pub const CODEC_PRIVATE: &[u8] = &[0x63, 0xA2];

    // Video track
    pub const VIDEO: &[u8] = &[0xE0];
    pub const PIXEL_WIDTH: &[u8] = &[0xB0];
    pub const PIXEL_HEIGHT: &[u8] = &[0xBA];
    pub const COLOUR_SPACE: &[u8] = &[0x2E, 0xB5, 0x24];

    // Audio track
    pub const AUDIO: &[u8] = &[0xE1];
    pub const SAMPLING_FREQUENCY: &[u8] = &[0xB5];
    pub const CHANNELS: &[u8] = &[0x9F];
    pub const BIT_DEPTH: &[u8] = &[0x62, 0x64];

    // Cluster
    pub const CLUSTER: &[u8] = &[0x1F, 0x43, 0xB6, 0x75];
    pub const TIMESTAMP: &[u8] = &[0xE7];
    pub const SIMPLE_BLOCK: &[u8] = &[0xA3];
}

/// Track types
const TRACK_TYPE_VIDEO: u8 = 1;
const TRACK_TYPE_AUDIO: u8 = 2;

/// Track numbers
const VIDEO_TRACK_NUMBER: u8 = 1;
const AUDIO_TRACK_NUMBER: u8 = 2;

/// Unknown size marker (8 bytes)
const UNKNOWN_SIZE: &[u8] = &[0x01, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF];

/// Cluster duration threshold in milliseconds (5 seconds)
const CLUSTER_DURATION_MS: i64 = 5000;

/// MKV configuration
#[derive(Clone, Debug)]
pub struct MkvConfig {
    pub video_width: u32,
    pub video_height: u32,
    pub audio_sample_rate: u32,
    pub audio_channels: u32,
}

/// MKV Writer for streaming output
pub struct MkvWriter<W: Write> {
    writer: W,
    config: MkvConfig,
    cluster_start_time: Option<i64>,
    header_written: bool,
}

impl<W: Write> MkvWriter<W> {
    /// Create a new MKV writer
    pub fn new(writer: W, config: MkvConfig) -> Result<Self> {
        let mut mkv = Self {
            writer,
            config,
            cluster_start_time: None,
            header_written: false,
        };
        mkv.write_header()?;
        Ok(mkv)
    }

    /// Write the MKV header (EBML header, Segment, Info, Tracks)
    fn write_header(&mut self) -> Result<()> {
        self.write_ebml_header()?;
        self.write_segment_start()?;
        self.write_info()?;
        self.write_tracks()?;
        self.header_written = true;
        self.writer.flush()?;
        Ok(())
    }

    /// Write EBML header
    fn write_ebml_header(&mut self) -> Result<()> {
        let mut header_data = Vec::new();

        // EBMLVersion = 1
        write_ebml_element(&mut header_data, ebml_ids::EBML_VERSION, &encode_uint(1))?;
        // EBMLReadVersion = 1
        write_ebml_element(&mut header_data, ebml_ids::EBML_READ_VERSION, &encode_uint(1))?;
        // EBMLMaxIDLength = 4
        write_ebml_element(&mut header_data, ebml_ids::EBML_MAX_ID_LENGTH, &encode_uint(4))?;
        // EBMLMaxSizeLength = 8
        write_ebml_element(&mut header_data, ebml_ids::EBML_MAX_SIZE_LENGTH, &encode_uint(8))?;
        // DocType = "matroska"
        write_ebml_element(&mut header_data, ebml_ids::DOC_TYPE, b"matroska")?;
        // DocTypeVersion = 4
        write_ebml_element(&mut header_data, ebml_ids::DOC_TYPE_VERSION, &encode_uint(4))?;
        // DocTypeReadVersion = 2
        write_ebml_element(&mut header_data, ebml_ids::DOC_TYPE_READ_VERSION, &encode_uint(2))?;

        write_ebml_element(&mut self.writer, ebml_ids::EBML, &header_data)?;
        Ok(())
    }

    /// Write Segment start with unknown size
    fn write_segment_start(&mut self) -> Result<()> {
        self.writer.write_all(ebml_ids::SEGMENT)?;
        self.writer.write_all(UNKNOWN_SIZE)?;
        Ok(())
    }

    /// Write Info element
    fn write_info(&mut self) -> Result<()> {
        let mut info_data = Vec::new();

        // TimestampScale = 1,000,000 (1ms precision)
        write_ebml_element(&mut info_data, ebml_ids::TIMESTAMP_SCALE, &encode_uint(1_000_000))?;
        // MuxingApp
        write_ebml_element(&mut info_data, ebml_ids::MUXING_APP, b"whep-client")?;
        // WritingApp
        write_ebml_element(&mut info_data, ebml_ids::WRITING_APP, b"whep-client")?;

        write_ebml_element(&mut self.writer, ebml_ids::INFO, &info_data)?;
        Ok(())
    }

    /// Write Tracks element (Video + Audio)
    fn write_tracks(&mut self) -> Result<()> {
        let mut tracks_data = Vec::new();

        // Video track
        let video_track = self.build_video_track()?;
        write_ebml_element(&mut tracks_data, ebml_ids::TRACK_ENTRY, &video_track)?;

        // Audio track
        let audio_track = self.build_audio_track()?;
        write_ebml_element(&mut tracks_data, ebml_ids::TRACK_ENTRY, &audio_track)?;

        write_ebml_element(&mut self.writer, ebml_ids::TRACKS, &tracks_data)?;
        Ok(())
    }

    /// Build video track entry
    fn build_video_track(&self) -> Result<Vec<u8>> {
        let mut track = Vec::new();

        // TrackNumber = 1
        write_ebml_element(&mut track, ebml_ids::TRACK_NUMBER, &encode_uint(VIDEO_TRACK_NUMBER as u64))?;
        // TrackUID = 1
        write_ebml_element(&mut track, ebml_ids::TRACK_UID, &encode_uint(1))?;
        // TrackType = 1 (video)
        write_ebml_element(&mut track, ebml_ids::TRACK_TYPE, &encode_uint(TRACK_TYPE_VIDEO as u64))?;
        // CodecID = V_UNCOMPRESSED
        write_ebml_element(&mut track, ebml_ids::CODEC_ID, b"V_UNCOMPRESSED")?;

        // CodecPrivate: FourCC for I420 (0x30323449 = "I420")
        let fourcc: [u8; 4] = [0x49, 0x34, 0x32, 0x30]; // "I420" in little-endian
        write_ebml_element(&mut track, ebml_ids::CODEC_PRIVATE, &fourcc)?;

        // Video element
        let mut video = Vec::new();
        write_ebml_element(&mut video, ebml_ids::PIXEL_WIDTH, &encode_uint(self.config.video_width as u64))?;
        write_ebml_element(&mut video, ebml_ids::PIXEL_HEIGHT, &encode_uint(self.config.video_height as u64))?;
        // ColourSpace: I420 FourCC
        write_ebml_element(&mut video, ebml_ids::COLOUR_SPACE, &fourcc)?;
        write_ebml_element(&mut track, ebml_ids::VIDEO, &video)?;

        Ok(track)
    }

    /// Build audio track entry
    fn build_audio_track(&self) -> Result<Vec<u8>> {
        let mut track = Vec::new();

        // TrackNumber = 2
        write_ebml_element(&mut track, ebml_ids::TRACK_NUMBER, &encode_uint(AUDIO_TRACK_NUMBER as u64))?;
        // TrackUID = 2
        write_ebml_element(&mut track, ebml_ids::TRACK_UID, &encode_uint(2))?;
        // TrackType = 2 (audio)
        write_ebml_element(&mut track, ebml_ids::TRACK_TYPE, &encode_uint(TRACK_TYPE_AUDIO as u64))?;
        // CodecID = A_PCM/INT/LIT (signed integer, little-endian)
        write_ebml_element(&mut track, ebml_ids::CODEC_ID, b"A_PCM/INT/LIT")?;

        // Audio element
        let mut audio = Vec::new();
        write_ebml_element(&mut audio, ebml_ids::SAMPLING_FREQUENCY, &encode_float64(self.config.audio_sample_rate as f64))?;
        write_ebml_element(&mut audio, ebml_ids::CHANNELS, &encode_uint(self.config.audio_channels as u64))?;
        // BitDepth = 16
        write_ebml_element(&mut audio, ebml_ids::BIT_DEPTH, &encode_uint(16))?;
        write_ebml_element(&mut track, ebml_ids::AUDIO, &audio)?;

        Ok(track)
    }

    /// Start a new cluster
    fn start_cluster(&mut self, timestamp_ms: i64) -> Result<()> {
        // Write Cluster element with unknown size
        self.writer.write_all(ebml_ids::CLUSTER)?;
        self.writer.write_all(UNKNOWN_SIZE)?;

        // Write Timestamp
        write_ebml_element(&mut self.writer, ebml_ids::TIMESTAMP, &encode_uint(timestamp_ms as u64))?;

        self.cluster_start_time = Some(timestamp_ms);
        Ok(())
    }

    /// Check if we need to start a new cluster
    fn maybe_start_new_cluster(&mut self, timestamp_ms: i64, _is_keyframe: bool) -> Result<()> {
        let need_new_cluster = match self.cluster_start_time {
            None => true, // First frame
            Some(start) => {
                let elapsed = timestamp_ms - start;
                // SimpleBlock timestamp is signed int16 relative to Cluster Timestamp.
                // Start a new cluster before it overflows.
                elapsed >= CLUSTER_DURATION_MS
                    || elapsed < i16::MIN as i64
                    || elapsed > i16::MAX as i64
            }
        };

        if need_new_cluster {
            self.start_cluster(timestamp_ms)?;
        }

        Ok(())
    }

    /// Write a video frame
    #[allow(dead_code)]
    pub fn write_video_frame(&mut self, frame: &[u8], timestamp_ms: i64, is_keyframe: bool) -> Result<()> {
        self.maybe_start_new_cluster(timestamp_ms, is_keyframe)?;

        let relative_ts = self.relative_timestamp(timestamp_ms);
        self.write_simple_block(VIDEO_TRACK_NUMBER, relative_ts, is_keyframe, frame)?;
        self.writer.flush()?;
        Ok(())
    }

    /// Write a video frame from I420 planes without repacking into a contiguous buffer
    pub fn write_video_frame_i420(
        &mut self,
        width: u32,
        height: u32,
        y_stride: u32,
        u_stride: u32,
        v_stride: u32,
        y_data: &[u8],
        u_data: &[u8],
        v_data: &[u8],
        timestamp_ms: i64,
        is_keyframe: bool,
    ) -> Result<()> {
        self.maybe_start_new_cluster(timestamp_ms, is_keyframe)?;

        let relative_ts = self.relative_timestamp(timestamp_ms);
        self.write_simple_block_i420(
            VIDEO_TRACK_NUMBER,
            relative_ts,
            is_keyframe,
            width,
            height,
            y_stride,
            u_stride,
            v_stride,
            y_data,
            u_data,
            v_data,
        )?;
        self.writer.flush()?;
        Ok(())
    }

    /// Write an audio frame
    pub fn write_audio_frame(&mut self, frame: &[u8], timestamp_ms: i64) -> Result<()> {
        // Audio is never a keyframe in the cluster sense
        self.maybe_start_new_cluster(timestamp_ms, false)?;

        let relative_ts = self.relative_timestamp(timestamp_ms);
        // Audio frames are always "keyframes" (independently decodable for PCM)
        self.write_simple_block(AUDIO_TRACK_NUMBER, relative_ts, true, frame)?;
        self.writer.flush()?;
        Ok(())
    }

    /// Calculate relative timestamp within current cluster
    fn relative_timestamp(&self, timestamp_ms: i64) -> i16 {
        let cluster_start = self.cluster_start_time.unwrap_or(0);
        let relative = timestamp_ms - cluster_start;
        // Clamp to i16 range
        relative.clamp(i16::MIN as i64, i16::MAX as i64) as i16
    }

    /// Write a SimpleBlock
    fn write_simple_block(&mut self, track_number: u8, relative_ts: i16, is_keyframe: bool, data: &[u8]) -> Result<()> {
        // SimpleBlock structure:
        // [Track Number (VINT)] [Relative Timestamp (int16 BE)] [Flags] [Data]
        let track_vint = encode_vint_value(track_number as u64);
        let flags: u8 = if is_keyframe { 0x80 } else { 0x00 };

        let header_len = track_vint.len() + 2 + 1;
        let block_size = header_len + data.len();

        self.writer.write_all(ebml_ids::SIMPLE_BLOCK)?;
        let size = encode_vint_size(block_size as u64);
        self.writer.write_all(&size)?;
        self.writer.write_all(&track_vint)?;
        self.writer.write_all(&relative_ts.to_be_bytes())?;
        self.writer.write_all(&[flags])?;
        self.writer.write_all(data)?;
        Ok(())
    }

    /// Write a SimpleBlock with I420 planes without repacking
    fn write_simple_block_i420(
        &mut self,
        track_number: u8,
        relative_ts: i16,
        is_keyframe: bool,
        width: u32,
        height: u32,
        y_stride: u32,
        u_stride: u32,
        v_stride: u32,
        y_data: &[u8],
        u_data: &[u8],
        v_data: &[u8],
    ) -> Result<()> {
        let w = width as usize;
        let h = height as usize;
        let y_stride = y_stride as usize;
        let u_stride = u_stride as usize;
        let v_stride = v_stride as usize;

        let uv_w = (w + 1) / 2;
        let uv_h = (h + 1) / 2;

        let y_required = required_plane_size(y_stride, w, h)
            .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidData, "Y plane size overflow"))?;
        let u_required = required_plane_size(u_stride, uv_w, uv_h)
            .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidData, "U plane size overflow"))?;
        let v_required = required_plane_size(v_stride, uv_w, uv_h)
            .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidData, "V plane size overflow"))?;

        if y_required > y_data.len() || u_required > u_data.len() || v_required > v_data.len() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "I420 plane data is smaller than expected",
            ));
        }

        let data_len = w
            .checked_mul(h)
            .and_then(|y| uv_w.checked_mul(uv_h).and_then(|uv| y.checked_add(uv.checked_mul(2)?)))
            .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidData, "I420 data length overflow"))?;

        let track_vint = encode_vint_value(track_number as u64);
        let header_len = track_vint.len() + 2 + 1;
        let block_size = header_len + data_len;

        self.writer.write_all(ebml_ids::SIMPLE_BLOCK)?;
        let size = encode_vint_size(block_size as u64);
        self.writer.write_all(&size)?;
        self.writer.write_all(&track_vint)?;
        self.writer.write_all(&relative_ts.to_be_bytes())?;
        let flags: u8 = if is_keyframe { 0x80 } else { 0x00 };
        self.writer.write_all(&[flags])?;

        // Write Y plane
        for row in 0..h {
            let start = row * y_stride;
            let end = start + w;
            self.writer.write_all(&y_data[start..end])?;
        }

        // Write U plane
        for row in 0..uv_h {
            let start = row * u_stride;
            let end = start + uv_w;
            self.writer.write_all(&u_data[start..end])?;
        }

        // Write V plane
        for row in 0..uv_h {
            let start = row * v_stride;
            let end = start + uv_w;
            self.writer.write_all(&v_data[start..end])?;
        }

        Ok(())
    }

    /// Flush the writer
    pub fn flush(&mut self) -> Result<()> {
        self.writer.flush()
    }
}

// EBML utility functions

/// Encode a value as VINT (variable-length integer for element content)
fn encode_vint_value(value: u64) -> Vec<u8> {
    // VINT encoding: the first byte indicates the length
    // 1xxxxxxx = 1 byte (values 0-127)
    // 01xxxxxx xxxxxxxx = 2 bytes
    // etc.
    if value < 0x80 {
        vec![0x80 | value as u8]
    } else if value < 0x4000 {
        vec![0x40 | (value >> 8) as u8, value as u8]
    } else if value < 0x20_0000 {
        vec![
            0x20 | (value >> 16) as u8,
            (value >> 8) as u8,
            value as u8,
        ]
    } else if value < 0x1000_0000 {
        vec![
            0x10 | (value >> 24) as u8,
            (value >> 16) as u8,
            (value >> 8) as u8,
            value as u8,
        ]
    } else {
        // Larger values need more bytes
        let mut bytes = Vec::new();
        let mut v = value;
        while v > 0 {
            bytes.push(v as u8);
            v >>= 8;
        }
        bytes.reverse();
        // Prepend marker based on length
        let len = bytes.len();
        if len <= 8 {
            let marker = 1u8 << (8 - len);
            if bytes.is_empty() || (bytes[0] & marker) == 0 {
                bytes.insert(0, marker);
            } else {
                bytes.insert(0, 0);
                bytes[0] = marker >> 1;
            }
        }
        bytes
    }
}

/// Encode a size value as VINT for element size field
fn encode_vint_size(value: u64) -> Vec<u8> {
    // Size field uses the same encoding as VINT but represents the size
    encode_vint_value(value)
}

/// Encode an unsigned integer using minimal bytes
fn encode_uint(value: u64) -> Vec<u8> {
    if value == 0 {
        return vec![0];
    }

    let mut bytes = Vec::new();
    let mut v = value;
    while v > 0 {
        bytes.push(v as u8);
        v >>= 8;
    }
    bytes.reverse();
    bytes
}

/// Encode a 64-bit float (IEEE 754 big-endian)
fn encode_float64(value: f64) -> Vec<u8> {
    value.to_be_bytes().to_vec()
}

/// Write an EBML element (ID + Size + Data)
fn write_ebml_element<W: Write>(writer: &mut W, id: &[u8], data: &[u8]) -> Result<()> {
    writer.write_all(id)?;
    let size = encode_vint_size(data.len() as u64);
    writer.write_all(&size)?;
    writer.write_all(data)?;
    Ok(())
}

fn required_plane_size(stride: usize, row_width: usize, rows: usize) -> Option<usize> {
    if rows == 0 {
        return Some(0);
    }
    let last_row = rows.checked_sub(1)?;
    last_row.checked_mul(stride)?.checked_add(row_width)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_encode_uint() {
        assert_eq!(encode_uint(0), vec![0]);
        assert_eq!(encode_uint(1), vec![1]);
        assert_eq!(encode_uint(127), vec![127]);
        assert_eq!(encode_uint(128), vec![128]);
        assert_eq!(encode_uint(255), vec![255]);
        assert_eq!(encode_uint(256), vec![1, 0]);
        assert_eq!(encode_uint(1_000_000), vec![0x0F, 0x42, 0x40]);
    }

    #[test]
    fn test_encode_vint_value() {
        // Track number 1 should encode as 0x81
        assert_eq!(encode_vint_value(1), vec![0x81]);
        // Track number 2 should encode as 0x82
        assert_eq!(encode_vint_value(2), vec![0x82]);
    }

    #[test]
    fn test_encode_float64() {
        let bytes = encode_float64(48000.0);
        assert_eq!(bytes.len(), 8);
        // Verify it's big-endian IEEE 754
        let restored = f64::from_be_bytes(bytes.try_into().unwrap());
        assert_eq!(restored, 48000.0);
    }

    // --- encode_vint_value boundary ---

    #[test]
    fn test_encode_vint_value_boundary() {
        // 1バイト最大値: 127 (0x7F) -> 0xFF
        assert_eq!(encode_vint_value(0x7F), vec![0xFF]);
        // 2バイト最小値: 128 (0x80) -> 0x40, 0x80
        assert_eq!(encode_vint_value(0x80), vec![0x40, 0x80]);
        // 2バイト最大値: 0x3FFF -> 0x7F, 0xFF
        assert_eq!(encode_vint_value(0x3FFF), vec![0x7F, 0xFF]);
    }

    // --- encode_vint_size ---

    #[test]
    fn test_encode_vint_size() {
        // encode_vint_sizeはencode_vint_valueと等価
        assert_eq!(encode_vint_size(1), encode_vint_value(1));
        assert_eq!(encode_vint_size(0x80), encode_vint_value(0x80));
        assert_eq!(encode_vint_size(0x3FFF), encode_vint_value(0x3FFF));
    }

    // --- write_ebml_element ---

    #[test]
    fn test_write_ebml_element() {
        let mut buf = Vec::new();
        let id = &[0x42, 0x86]; // EBML_VERSION
        let data = &[0x01];
        write_ebml_element(&mut buf, id, data).unwrap();
        // ID(2) + Size VINT(1: 0x81) + Data(1)
        assert_eq!(buf, vec![0x42, 0x86, 0x81, 0x01]);
    }

    // --- MkvWriter::new ---

    #[test]
    fn test_mkv_writer_new_writes_header() {
        let buf = Vec::new();
        let config = MkvConfig {
            video_width: 1920,
            video_height: 1080,
            audio_sample_rate: 48000,
            audio_channels: 2,
        };
        let writer = MkvWriter::new(buf, config).unwrap();
        let output = writer.writer;
        // EBML header先頭4バイト: 0x1A 0x45 0xDF 0xA3
        assert!(output.len() >= 4);
        assert_eq!(&output[0..4], ebml_ids::EBML);
        // header_writtenフラグ確認
        assert!(writer.header_written);
    }

    // --- write_video_frame ---

    #[test]
    fn test_mkv_writer_write_video_frame() {
        let buf = Vec::new();
        let config = MkvConfig {
            video_width: 2,
            video_height: 2,
            audio_sample_rate: 48000,
            audio_channels: 2,
        };
        let mut writer = MkvWriter::new(buf, config).unwrap();
        let header_len = writer.writer.len();

        let frame_data = vec![0u8; 6]; // 2x2 I420: Y=4 + U=1 + V=1
        writer.write_video_frame(&frame_data, 0, true).unwrap();

        // クラスタが開始され、データが書き込まれたことを確認
        assert!(writer.writer.len() > header_len);
        assert_eq!(writer.cluster_start_time, Some(0));
    }

    // --- write_audio_frame ---

    #[test]
    fn test_mkv_writer_write_audio_frame() {
        let buf = Vec::new();
        let config = MkvConfig {
            video_width: 2,
            video_height: 2,
            audio_sample_rate: 48000,
            audio_channels: 2,
        };
        let mut writer = MkvWriter::new(buf, config).unwrap();
        let header_len = writer.writer.len();

        let audio_data = vec![0u8; 192]; // 48 samples * 2ch * 2bytes
        writer.write_audio_frame(&audio_data, 0).unwrap();

        assert!(writer.writer.len() > header_len);
    }

    // --- maybe_start_new_cluster ---

    #[test]
    fn test_mkv_writer_cluster_transition() {
        let buf = Vec::new();
        let config = MkvConfig {
            video_width: 2,
            video_height: 2,
            audio_sample_rate: 48000,
            audio_channels: 2,
        };
        let mut writer = MkvWriter::new(buf, config).unwrap();

        // 最初のフレームでクラスタ開始
        writer.write_video_frame(&[0u8; 6], 0, true).unwrap();
        assert_eq!(writer.cluster_start_time, Some(0));

        // 5000ms未満: 同じクラスタ
        writer.write_video_frame(&[0u8; 6], 4999, false).unwrap();
        assert_eq!(writer.cluster_start_time, Some(0));

        // 5000ms: CLUSTER_DURATION_MS閾値で新クラスタ
        writer.write_video_frame(&[0u8; 6], 5000, false).unwrap();
        assert_eq!(writer.cluster_start_time, Some(5000));
    }

    #[test]
    fn test_mkv_writer_keyframe_does_not_force_new_cluster() {
        let buf = Vec::new();
        let config = MkvConfig {
            video_width: 2,
            video_height: 2,
            audio_sample_rate: 48000,
            audio_channels: 2,
        };
        let mut writer = MkvWriter::new(buf, config).unwrap();

        writer.write_video_frame(&[0u8; 6], 0, true).unwrap();
        assert_eq!(writer.cluster_start_time, Some(0));

        // keyframe=true でも、閾値内なら同じクラスタを維持する
        writer.write_video_frame(&[0u8; 6], 1, true).unwrap();
        assert_eq!(writer.cluster_start_time, Some(0));
    }

    // --- relative_timestamp ---

    #[test]
    fn test_relative_timestamp_basic() {
        let buf = Vec::new();
        let config = MkvConfig {
            video_width: 2,
            video_height: 2,
            audio_sample_rate: 48000,
            audio_channels: 2,
        };
        let mut writer = MkvWriter::new(buf, config).unwrap();
        writer.cluster_start_time = Some(1000);

        assert_eq!(writer.relative_timestamp(1000), 0);
        assert_eq!(writer.relative_timestamp(1500), 500);
        assert_eq!(writer.relative_timestamp(1033), 33);
    }

    #[test]
    fn test_relative_timestamp_clamp() {
        let buf = Vec::new();
        let config = MkvConfig {
            video_width: 2,
            video_height: 2,
            audio_sample_rate: 48000,
            audio_channels: 2,
        };
        let mut writer = MkvWriter::new(buf, config).unwrap();
        writer.cluster_start_time = Some(0);

        // i16::MAX = 32767
        assert_eq!(writer.relative_timestamp(32767), 32767);
        assert_eq!(writer.relative_timestamp(40000), 32767); // クランプ

        // 負の相対値
        writer.cluster_start_time = Some(50000);
        assert_eq!(writer.relative_timestamp(0), -32768); // クランプ
    }

    // --- build_video_track ---

    #[test]
    fn test_build_video_track() {
        let buf = Vec::new();
        let config = MkvConfig {
            video_width: 1920,
            video_height: 1080,
            audio_sample_rate: 48000,
            audio_channels: 2,
        };
        let writer = MkvWriter::new(buf, config).unwrap();
        let track_data = writer.build_video_track().unwrap();

        // V_UNCOMPRESSEDコーデックIDが含まれていること
        let codec_id = b"V_UNCOMPRESSED";
        assert!(
            track_data
                .windows(codec_id.len())
                .any(|w| w == codec_id),
            "V_UNCOMPRESSED codec ID not found in video track"
        );
    }

    // --- build_audio_track ---

    #[test]
    fn test_build_audio_track() {
        let buf = Vec::new();
        let config = MkvConfig {
            video_width: 1920,
            video_height: 1080,
            audio_sample_rate: 48000,
            audio_channels: 2,
        };
        let writer = MkvWriter::new(buf, config).unwrap();
        let track_data = writer.build_audio_track().unwrap();

        // A_PCM/INT/LITコーデックIDが含まれていること
        let codec_id = b"A_PCM/INT/LIT";
        assert!(
            track_data
                .windows(codec_id.len())
                .any(|w| w == codec_id),
            "A_PCM/INT/LIT codec ID not found in audio track"
        );
    }
}
