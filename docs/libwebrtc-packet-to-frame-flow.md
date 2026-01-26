# libwebrtc パケット受信からフレーム生成までの調査報告

## 概要

libwebrtcでは、ネットワークから受信したRTPパケットが複数の処理段階を経て、最終的にデコードされた映像フレーム（`VideoFrame`）および音声フレーム（`AudioFrame`）に変換される。

---

## 1. 映像パケット処理フロー

### 1.1 データフロー全体像

```
RTPパケット受信
    |
    v
RtpPacketReceived (modules/rtp_rtcp/source/rtp_packet_received.h:28)
    |
    v
RtpVideoStreamReceiver2::OnRtpPacket() (video/rtp_video_stream_receiver2.h:149)
    |
    v
VideoRtpDepacketizer::Parse() - ペイロード解析
    |
    v
PacketBuffer::Packet (modules/video_coding/packet_buffer.h:33)
    |
    v
PacketBuffer::InsertPacket() - フレーム完成検出
    |
    v
RtpFrameObject (フレームオブジェクト生成)
    |
    v
RtpFrameReferenceFinder::ManageFrame() (modules/video_coding/rtp_frame_reference_finder.h:40)
    |
    v
EncodedFrame (api/video/encoded_frame.h:30)
    |
    v
VideoDecoder::Decode() (api/video_codecs/video_decoder.h)
    |
    v
VideoFrame (api/video/video_frame.h:31)
```

### 1.2 各段階のデータ構造

#### Stage 1: RtpPacketReceived (`rtp_packet_received.h:28`)

ネットワークから受信した生のRTPパケットを保持する。

```cpp
class RtpPacketReceived : public RtpPacket {
  webrtc::Timestamp arrival_time_;    // パケット到着時刻
  EcnMarking ecn_;                    // 輻輳通知マーキング
  int payload_type_frequency_;        // ペイロードタイプ周波数
  bool recovered_;                    // RTX/FEC経由で復元されたか
};
```

**含まれる情報:**
- RTPヘッダ（シーケンス番号、タイムスタンプ、SSRC等）
- ペイロードデータ
- 到着時刻メタデータ
- 復元フラグ

#### Stage 2: PacketBuffer::Packet (`packet_buffer.h:33`)

デパケタイズ後のビデオペイロード情報を保持する。

```cpp
struct Packet {
  bool continuous = false;           // 連続性フラグ
  bool marker_bit = false;           // マーカービット
  uint8_t payload_type = 0;          // ペイロードタイプ
  int64_t sequence_number = 0;       // シーケンス番号
  uint32_t timestamp = 0;            // RTPタイムスタンプ
  int times_nacked = -1;             // NACK要求回数
  CopyOnWriteBuffer video_payload;   // ビデオペイロードデータ
  RTPVideoHeader video_header;       // ビデオヘッダ情報
};
```

**含まれる情報:**
- コーデック種別（`video_header.codec`）
- フレーム寸法（`width`, `height`）
- フレーム内位置（`is_first_packet_in_frame`, `is_last_packet_in_frame`）

#### Stage 3: EncodedFrame (`encoded_frame.h:30`)

完全なエンコード済みフレームを表現する。

```cpp
class EncodedFrame : public EncodedImage {
  static const uint8_t kMaxFrameReferences = 5;

  int64_t _renderTimeMs = -1;              // レンダリング時刻
  uint8_t _payloadType = 0;                // ペイロードタイプ
  CodecSpecificInfo _codecSpecificInfo;   // コーデック固有情報
  VideoCodecType _codec;                   // コーデックタイプ

  size_t num_references = 0;               // 参照フレーム数
  int64_t references[kMaxFrameReferences]; // 参照フレームID
  bool is_last_spatial_layer = true;       // 空間レイヤー終端
};
```

**含まれる情報:**
- 受信時刻（`ReceivedTime()`）
- レンダリング時刻
- フレーム間依存関係（`num_references`, `references[]`）
- キーフレーム判定（`is_keyframe()` = `num_references == 0`）

#### Stage 4: VideoFrame (`video_frame.h:31`)

デコード済みの最終映像フレーム。

```cpp
class VideoFrame {
  uint16_t id_;                                    // フレームID
  scoped_refptr<VideoFrameBuffer> video_frame_buffer_; // ピクセルデータ
  uint32_t timestamp_rtp_;                         // RTPタイムスタンプ
  int64_t ntp_time_ms_;                           // NTP時刻
  int64_t timestamp_us_;                          // システム時刻
  std::optional<Timestamp> presentation_timestamp_; // 表示タイムスタンプ
  std::optional<Timestamp> reference_time_;       // キャプチャ時刻
  VideoRotation rotation_;                        // 回転角度
  std::optional<ColorSpace> color_space_;         // 色空間情報
  std::optional<UpdateRect> update_rect_;         // 更新矩形
  RtpPacketInfos packet_infos_;                   // 元パケット情報
  std::optional<ProcessingTime> processing_time_; // 処理時間
};
```

**含まれる情報:**
- 実際のピクセルバッファ（`VideoFrameBuffer`）
- 各種タイムスタンプ（RTP、NTP、システム）
- 回転・色空間情報
- 更新領域情報

---

## 2. 音声パケット処理フロー

### 2.1 データフロー全体像

```
RTPパケット受信
    |
    v
RtpPacketReceived
    |
    v
NetEqImpl::InsertPacket() (modules/audio_coding/neteq/neteq_impl.h:132)
    |
    v
Packet (modules/audio_coding/neteq/packet.h:29)
    |
    v
PacketBuffer (ジッターバッファ)
    |
    v
AudioDecoder::ParsePayload() (api/audio_codecs/audio_decoder.h:94)
    |
    v
EncodedAudioFrame (api/audio_codecs/audio_decoder.h:43)
    |
    v
AudioDecoder::Decode() / EncodedAudioFrame::Decode()
    |
    v
AudioFrame (api/audio/audio_frame.h:56)
```

### 2.2 各段階のデータ構造

#### Stage 1: Packet（NetEq用） (`packet.h:29`)

オーディオRTPパケット情報を保持する。

```cpp
struct Packet {
  struct Priority {
    int codec_level;   // コーデック優先度
    int red_level;     // RED冗長度レベル
  };

  uint32_t timestamp;                              // RTPタイムスタンプ
  uint16_t sequence_number;                        // シーケンス番号
  uint8_t payload_type;                            // ペイロードタイプ
  Buffer payload;                                  // ペイロードデータ
  Priority priority;                               // 優先度
  std::optional<RtpPacketInfo> packet_info;        // パケット情報
  std::unique_ptr<TickTimer::Stopwatch> waiting_time; // 待機時間
  std::unique_ptr<AudioDecoder::EncodedAudioFrame> frame; // パース済みフレーム
};
```

#### Stage 2: AudioDecoder::ParseResult (`audio_decoder.h:69`)

パース結果を保持する構造体。

```cpp
struct ParseResult {
  uint32_t timestamp;                              // サンプル単位タイムスタンプ
  int priority;                                    // 優先度
  std::unique_ptr<EncodedAudioFrame> frame;       // エンコード済みフレーム
};
```

#### Stage 3: EncodedAudioFrame (`audio_decoder.h:43`)

エンコード済み音声フレームのインターフェース。

```cpp
class EncodedAudioFrame {
  struct DecodeResult {
    size_t num_decoded_samples;  // デコード済みサンプル数
    SpeechType speech_type;      // 音声タイプ（speech/comfort noise）
  };

  virtual size_t Duration() const = 0;           // フレーム長（サンプル/チャンネル）
  virtual bool IsDtxPacket() const;              // DTXパケットか
  virtual std::optional<DecodeResult> Decode(ArrayView<int16_t> decoded) const = 0;
};
```

#### Stage 4: AudioFrame (`audio_frame.h:56`)

最終的なデコード済み音声フレーム。

```cpp
class AudioFrame {
  enum : size_t {
    kMaxDataSizeSamples = 7680,  // 最大サンプル数
  };

  enum VADActivity { kVadActive, kVadPassive, kVadUnknown };
  enum SpeechType { kNormalSpeech, kPLC, kCNG, kPLCCNG, kCodecPLC, kUndefined };

  uint32_t timestamp_ = 0;                       // RTPタイムスタンプ
  int64_t elapsed_time_ms_ = -1;                 // 経過時間
  int64_t ntp_time_ms_ = -1;                     // NTP時刻
  size_t samples_per_channel_ = 0;               // チャンネル当たりサンプル数
  int sample_rate_hz_ = 0;                       // サンプルレート
  size_t num_channels_ = 0;                      // チャンネル数
  SpeechType speech_type_ = kUndefined;          // 音声タイプ
  VADActivity vad_activity_ = kVadUnknown;       // VAD状態
  RtpPacketInfos packet_infos_;                  // 元パケット情報

  std::array<int16_t, kMaxDataSizeSamples> data_; // PCMサンプルデータ
  ChannelLayout channel_layout_;                  // チャンネルレイアウト
  std::optional<int64_t> absolute_capture_timestamp_ms_; // キャプチャ時刻
};
```

**含まれる情報:**
- 実際のPCMサンプルデータ（`data_`）
- 10ms〜120msのステレオ32kHzまで対応
- VAD（Voice Activity Detection）状態
- PLC（Packet Loss Concealment）情報

---

## 3. NetEqの役割（音声専用）

NetEq（`neteq_impl.h:64`）は音声処理における重要なコンポーネントで、以下の機能を担う：

1. **ジッターバッファリング**: パケットの到着順序と時間のばらつきを吸収
2. **パケットロス補償（PLC）**: 欠落パケットの音声を推定生成
3. **時間伸縮**: 再生速度の微調整による遅延制御
4. **コンフォートノイズ生成（CNG）**: 無音期間のノイズ生成

```cpp
class NetEqImpl : public NetEq {
  // 主要メソッド
  int InsertPacket(const RTPHeader& rtp_header, ArrayView<const uint8_t> payload);
  int GetAudio(AudioFrame* audio_frame, ...);

  // 内部処理
  int Decode(PacketList* packet_list, ...);
  int DecodeLoop(PacketList* packet_list, ...);
  void DoNormal(...);    // 通常再生
  int DoExpand(...);     // パケットロス時の拡張
  int DoAccelerate(...); // 加速再生
  int DoPreemptiveExpand(...); // 先行拡張
};
```

---

## 4. 映像フレームリファレンス解決

`RtpFrameReferenceFinder`（`rtp_frame_reference_finder.h:25`）は、SVC/シミュラキャスト環境でのフレーム間依存関係を解決する。

```cpp
class RtpFrameReferenceFinder {
  // フレームを管理し、依存関係が解決されたら返却
  ReturnVector ManageFrame(std::unique_ptr<RtpFrameObject> frame);

  // パディング受信通知
  ReturnVector PaddingReceived(uint16_t seq_num);

  // 古いフレームのクリア
  void ClearTo(uint16_t seq_num);
};
```

フレームは以下の条件で解放される：
- 必要な参照情報がすべて到着した場合
- スタッシュフレーム数が上限を超えた場合
- `ClearTo()`でクリアされた場合
- フレームが古くなった場合

---

## 5. 主要ファイルパス一覧

| 処理層 | ファイルパス |
|--------|-------------|
| RTPパケット受信 | `modules/rtp_rtcp/source/rtp_packet_received.h` |
| ビデオRTP受信 | `video/rtp_video_stream_receiver2.h` |
| パケットバッファ（ビデオ） | `modules/video_coding/packet_buffer.h` |
| フレーム参照解決 | `modules/video_coding/rtp_frame_reference_finder.h` |
| エンコード済みフレーム | `api/video/encoded_frame.h` |
| デコード済みビデオフレーム | `api/video/video_frame.h` |
| NetEq | `modules/audio_coding/neteq/neteq_impl.h` |
| オーディオパケット | `modules/audio_coding/neteq/packet.h` |
| オーディオデコーダ | `api/audio_codecs/audio_decoder.h` |
| デコード済みオーディオフレーム | `api/audio/audio_frame.h` |

---

## 6. まとめ

libwebrtcにおけるパケットからフレームへの変換は、以下のように整理できる：

### 映像
1. **受信**: `RtpPacketReceived` - 到着時刻、復元フラグ等のメタデータ付加
2. **デパケタイズ**: `PacketBuffer::Packet` - コーデックヘッダ解析、連続性判定
3. **フレーム組立**: `EncodedFrame` - 複数パケットを1フレームに結合、依存関係付加
4. **デコード**: `VideoFrame` - ピクセルバッファ、タイムスタンプ、表示情報

### 音声
1. **受信**: `RtpPacketReceived` - 到着時刻メタデータ付加
2. **NetEq挿入**: `Packet` - 優先度、待機時間管理
3. **パース**: `EncodedAudioFrame` - デコード可能な単位に分割
4. **デコード**: `AudioFrame` - PCMサンプル、VAD状態、チャンネル情報
