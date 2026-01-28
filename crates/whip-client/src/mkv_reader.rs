//! MKV Reader - webm-iterableを使用したMKVストリームパーサー
//!
//! stdinからMKVストリームを読み込み、Video/Audioフレームを抽出する。

use anyhow::{anyhow, Result};
use std::io::Read;
use webm_iterable::matroska_spec::{Master, MatroskaSpec};
use webm_iterable::WebmIterator;

/// MKVトラック情報
#[derive(Debug, Clone, Default)]
pub struct MkvTrackInfo {
    pub video_width: u32,
    pub video_height: u32,
    pub video_codec_id: String,
    pub audio_sample_rate: u32,
    pub audio_channels: u32,
    pub audio_codec_id: String,
    pub timecode_scale: u64,
}

/// MKVフレーム（Video or Audio）
#[derive(Debug, Clone)]
pub enum MkvFrame {
    Video {
        data: Vec<u8>,
        timestamp_ms: i64,
        is_keyframe: bool,
    },
    Audio {
        data: Vec<u8>,
        timestamp_ms: i64,
    },
}

/// MKVストリームリーダー
pub struct MkvReader<R: Read> {
    iterator: WebmIterator<R>,
    track_info: MkvTrackInfo,
    video_track_num: Option<u64>,
    audio_track_num: Option<u64>,
    current_cluster_timestamp: i64,
    header_parsed: bool,
}

impl<R: Read> MkvReader<R> {
    /// 新しいMkvReaderを作成し、ヘッダーをパースする
    pub fn new(reader: R) -> Result<Self> {
        let iterator = WebmIterator::new(reader, &[MatroskaSpec::TrackEntry(Master::Start)]);

        let mut mkv_reader = Self {
            iterator,
            track_info: MkvTrackInfo::default(),
            video_track_num: None,
            audio_track_num: None,
            current_cluster_timestamp: 0,
            header_parsed: false,
        };

        mkv_reader.parse_header()?;

        Ok(mkv_reader)
    }

    /// トラック情報を取得
    pub fn track_info(&self) -> &MkvTrackInfo {
        &self.track_info
    }

    /// ヘッダーをパースしてトラック情報を取得
    fn parse_header(&mut self) -> Result<()> {
        // TimecodeScaleのデフォルト値（1ms = 1,000,000ns）
        self.track_info.timecode_scale = 1_000_000;

        while let Some(tag) = self.iterator.next() {
            let tag = tag.map_err(|e| anyhow!("Failed to parse MKV tag: {:?}", e))?;

            match tag {
                MatroskaSpec::TimestampScale(scale) => {
                    self.track_info.timecode_scale = scale;
                }
                MatroskaSpec::TrackEntry(Master::Full(children)) => {
                    self.parse_track_entry(&children)?;
                }
                MatroskaSpec::Cluster(Master::Start) => {
                    // Clusterに到達したらヘッダーパース終了
                    self.header_parsed = true;
                    break;
                }
                _ => {}
            }
        }

        if !self.header_parsed {
            return Err(anyhow!("Failed to parse MKV header: no Cluster found"));
        }

        Ok(())
    }

    /// TrackEntryをパースしてトラック情報を更新
    fn parse_track_entry(&mut self, children: &[MatroskaSpec]) -> Result<()> {
        let mut track_number: Option<u64> = None;
        let mut track_type: Option<u64> = None;
        let mut codec_id: Option<String> = None;
        let mut video_width: Option<u32> = None;
        let mut video_height: Option<u32> = None;
        let mut audio_sample_rate: Option<f64> = None;
        let mut audio_channels: Option<u64> = None;

        for child in children {
            match child {
                MatroskaSpec::TrackNumber(num) => {
                    track_number = Some(*num);
                }
                MatroskaSpec::TrackType(t) => {
                    track_type = Some(*t);
                }
                MatroskaSpec::CodecID(id) => {
                    codec_id = Some(id.clone());
                }
                MatroskaSpec::Video(Master::Full(video_children)) => {
                    for vc in video_children {
                        match vc {
                            MatroskaSpec::PixelWidth(w) => {
                                video_width = Some(*w as u32);
                            }
                            MatroskaSpec::PixelHeight(h) => {
                                video_height = Some(*h as u32);
                            }
                            _ => {}
                        }
                    }
                }
                MatroskaSpec::Audio(Master::Full(audio_children)) => {
                    for ac in audio_children {
                        match ac {
                            MatroskaSpec::SamplingFrequency(rate) => {
                                audio_sample_rate = Some(*rate);
                            }
                            MatroskaSpec::Channels(ch) => {
                                audio_channels = Some(*ch);
                            }
                            _ => {}
                        }
                    }
                }
                _ => {}
            }
        }

        let track_num = track_number.ok_or_else(|| anyhow!("TrackEntry missing TrackNumber"))?;
        let track_t = track_type.ok_or_else(|| anyhow!("TrackEntry missing TrackType"))?;

        // TrackType: 1 = video, 2 = audio
        match track_t {
            1 => {
                // Video track
                self.video_track_num = Some(track_num);
                self.track_info.video_codec_id = codec_id.unwrap_or_default();
                self.track_info.video_width = video_width.unwrap_or(0);
                self.track_info.video_height = video_height.unwrap_or(0);
            }
            2 => {
                // Audio track
                self.audio_track_num = Some(track_num);
                self.track_info.audio_codec_id = codec_id.unwrap_or_default();
                self.track_info.audio_sample_rate = audio_sample_rate.unwrap_or(48000.0) as u32;
                self.track_info.audio_channels = audio_channels.unwrap_or(2) as u32;
            }
            _ => {}
        }

        Ok(())
    }

    /// 次のフレームを読み込む
    pub fn read_frame(&mut self) -> Result<Option<MkvFrame>> {
        while let Some(tag) = self.iterator.next() {
            let tag = tag.map_err(|e| anyhow!("Failed to parse MKV tag: {:?}", e))?;

            match tag {
                MatroskaSpec::Timestamp(ts) => {
                    // Cluster timestamp (in timecode scale units)
                    // timecode_scale is in nanoseconds, convert to milliseconds
                    self.current_cluster_timestamp =
                        (ts as i64 * self.track_info.timecode_scale as i64) / 1_000_000;
                }
                MatroskaSpec::SimpleBlock(block_data) => {
                    return self.parse_simple_block(&block_data);
                }
                _ => {}
            }
        }

        Ok(None)
    }

    /// SimpleBlockをパースしてMkvFrameを返す
    fn parse_simple_block(&self, data: &[u8]) -> Result<Option<MkvFrame>> {
        if data.len() < 4 {
            return Err(anyhow!("SimpleBlock too short: {} bytes", data.len()));
        }

        // Parse track number (VINT encoded)
        let (track_num, vint_size) = parse_vint(data)?;

        // Relative timestamp (2 bytes, signed big-endian)
        let relative_ts = i16::from_be_bytes([data[vint_size], data[vint_size + 1]]) as i64;

        // Flags (1 byte)
        let flags = data[vint_size + 2];
        let is_keyframe = (flags & 0x80) != 0;

        // Frame data
        let frame_data = data[vint_size + 3..].to_vec();

        // Calculate absolute timestamp
        let timestamp_ms = self.current_cluster_timestamp + relative_ts;

        // Determine if this is video or audio
        if Some(track_num) == self.video_track_num {
            return Ok(Some(MkvFrame::Video {
                data: frame_data,
                timestamp_ms,
                is_keyframe,
            }));
        } else if Some(track_num) == self.audio_track_num {
            return Ok(Some(MkvFrame::Audio {
                data: frame_data,
                timestamp_ms,
            }));
        }

        // Unknown track, skip
        Ok(None)
    }
}

/// VINT（可変長整数）をパース
fn parse_vint(data: &[u8]) -> Result<(u64, usize)> {
    if data.is_empty() {
        return Err(anyhow!("Empty VINT"));
    }

    let first = data[0];
    let len = first.leading_zeros() as usize + 1;

    if len > 8 || data.len() < len {
        return Err(anyhow!("Invalid VINT length"));
    }

    let mask = (1u8 << (8 - len)) - 1;
    let mut value = (first & mask) as u64;

    for i in 1..len {
        value = (value << 8) | data[i] as u64;
    }

    Ok((value, len))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// テスト用のMKVヘッダーを生成するヘルパー
    #[allow(dead_code)]
    fn create_test_mkv_header(_video_width: u32, _video_height: u32) -> Vec<u8> {
        // 最小限のMKVヘッダー（EBML Header + Segment + Info + Tracks + Cluster）
        // 実際のテストでは、ffmpegで生成したテストファイルを使用することを推奨
        Vec::new()
    }

    #[test]
    fn test_parse_vint_single_byte() {
        // 0x81 = VINT value 1
        let data = [0x81];
        let (value, len) = parse_vint(&data).unwrap();
        assert_eq!(value, 1);
        assert_eq!(len, 1);
    }

    #[test]
    fn test_parse_vint_two_bytes() {
        // 0x40 0x01 = VINT value 1 (2-byte encoding)
        let data = [0x40, 0x01];
        let (value, len) = parse_vint(&data).unwrap();
        assert_eq!(value, 1);
        assert_eq!(len, 2);
    }

    #[test]
    fn test_parse_vint_track_number() {
        // Track number 1 encoded as VINT
        let data = [0x81]; // 0x81 = track 1
        let (value, len) = parse_vint(&data).unwrap();
        assert_eq!(value, 1);
        assert_eq!(len, 1);

        // Track number 2 encoded as VINT
        let data = [0x82]; // 0x82 = track 2
        let (value, len) = parse_vint(&data).unwrap();
        assert_eq!(value, 2);
        assert_eq!(len, 1);
    }

    #[test]
    fn test_parse_vint_empty() {
        let data: [u8; 0] = [];
        assert!(parse_vint(&data).is_err());
    }

    #[test]
    fn test_mkv_track_info_default() {
        let info = MkvTrackInfo::default();
        assert_eq!(info.video_width, 0);
        assert_eq!(info.video_height, 0);
        assert_eq!(info.video_codec_id, "");
        assert_eq!(info.audio_sample_rate, 0);
        assert_eq!(info.audio_channels, 0);
        assert_eq!(info.audio_codec_id, "");
        assert_eq!(info.timecode_scale, 0);
    }

    #[test]
    fn test_mkv_frame_video() {
        let frame = MkvFrame::Video {
            data: vec![0x01, 0x02, 0x03],
            timestamp_ms: 1000,
            is_keyframe: true,
        };

        if let MkvFrame::Video {
            data,
            timestamp_ms,
            is_keyframe,
        } = frame
        {
            assert_eq!(data, vec![0x01, 0x02, 0x03]);
            assert_eq!(timestamp_ms, 1000);
            assert!(is_keyframe);
        } else {
            panic!("Expected Video frame");
        }
    }

    #[test]
    fn test_mkv_frame_audio() {
        let frame = MkvFrame::Audio {
            data: vec![0x04, 0x05, 0x06],
            timestamp_ms: 2000,
        };

        if let MkvFrame::Audio { data, timestamp_ms } = frame {
            assert_eq!(data, vec![0x04, 0x05, 0x06]);
            assert_eq!(timestamp_ms, 2000);
        } else {
            panic!("Expected Audio frame");
        }
    }

    // 以下のテストは実際のMKVファイルが必要なため、統合テストとして実装予定
    // #[test]
    // fn test_parse_track_info_video_only() {}
    // #[test]
    // fn test_parse_track_info_audio_only() {}
    // #[test]
    // fn test_parse_track_info_video_and_audio() {}
    // #[test]
    // fn test_read_video_frame_keyframe() {}
    // #[test]
    // fn test_read_video_frame_interframe() {}
    // #[test]
    // fn test_read_audio_frame() {}
    // #[test]
    // fn test_timestamp_calculation() {}
    // #[test]
    // fn test_frame_iterator_eof() {}
}
