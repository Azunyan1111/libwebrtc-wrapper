# 映像RTPパケット受信からEncodedFrame生成までの実装ガイド

本ドキュメントは、libwebrtcの映像RTPパケット処理パイプラインを詳細に解説し、フルスクラッチで同等機能を実装するための参考資料である。

---

## 目次

1. [全体アーキテクチャ](#1-全体アーキテクチャ)
2. [Step 1: RTPパケット受信](#2-step-1-rtpパケット受信)
3. [Step 2: ペイロードデパケタイズ](#3-step-2-ペイロードデパケタイズ)
4. [Step 3: パケットバッファリング](#4-step-3-パケットバッファリング)
5. [Step 4: フレーム組立](#5-step-4-フレーム組立)
6. [Step 5: フレーム参照解決](#6-step-5-フレーム参照解決)
7. [データ構造定義](#7-データ構造定義)
8. [疑似コード実装例](#8-疑似コード実装例)
9. [C++参考実装の引用](#9-c参考実装の引用)

---

## 1. 全体アーキテクチャ

### 1.1 処理フロー概要

```
+------------------+     +-------------------+     +---------------+
| RTPパケット受信   | --> | デパケタイザ       | --> | パケットバッファ |
| OnRtpPacket()    |     | Parse()           |     | InsertPacket() |
+------------------+     +-------------------+     +---------------+
                                                          |
                                                          v
+------------------+     +-------------------+     +---------------+
| EncodedFrame     | <-- | フレーム参照解決   | <-- | フレーム組立    |
| (出力)           |     | ManageFrame()     |     | OnInsertedPacket()|
+------------------+     +-------------------+     +---------------+
```

### 1.2 主要コンポーネント

| コンポーネント | 責務 | libwebrtcクラス |
|--------------|------|----------------|
| RTPレシーバー | パケット受信・統計・RTCP処理 | `RtpVideoStreamReceiver2` |
| デパケタイザ | コーデック固有ペイロード解析 | `VideoRtpDepacketizer` |
| パケットバッファ | パケット順序整理・フレーム完成検出 | `PacketBuffer` |
| フレーム参照解決 | フレーム間依存関係解決 | `RtpFrameReferenceFinder` |

---

## 2. Step 1: RTPパケット受信

### 2.1 処理概要

RTPパケットを受信し、基本的な検証とルーティングを行う。

### 2.2 処理フロー

```
OnRtpPacket(packet)
    |
    +-- パケットが空か確認 -> 空ならパディング処理へ
    |
    +-- REDパケットか確認 -> REDならFEC処理へ
    |
    +-- ペイロードタイプからデパケタイザを取得
    |
    +-- デパケタイザでパース
    |
    +-- OnReceivedPayloadData()を呼び出し
```

### 2.3 入力データ

```
RTPパケット構造:
+--------+--------+--------+--------+
|  V=2   |P|X| CC |M|  PT   |  Seq   |
+--------+--------+--------+--------+
|           Timestamp (32bit)        |
+--------+--------+--------+--------+
|              SSRC (32bit)          |
+--------+--------+--------+--------+
|      [拡張ヘッダ（オプション）]      |
+--------+--------+--------+--------+
|             Payload                |
+--------+--------+--------+--------+
```

### 2.4 抽出すべき情報

| フィールド | 説明 | 用途 |
|-----------|------|------|
| SequenceNumber | 16bit シーケンス番号 | パケット順序管理 |
| Timestamp | 32bit RTPタイムスタンプ | 同一フレーム判定 |
| Marker | 1bit マーカービット | フレーム終端判定 |
| PayloadType | 7bit ペイロードタイプ | デパケタイザ選択 |
| SSRC | 32bit 送信元識別子 | ストリーム識別 |
| Payload | 可変長 | コーデックデータ |

### 2.5 C++参考実装

**ファイル**: `video/rtp_video_stream_receiver2.cc:760-783`

```cpp
void RtpVideoStreamReceiver2::OnRtpPacket(const RtpPacketReceived& packet) {
  RTC_DCHECK_RUN_ON(&packet_sequence_checker_);

  if (!receiving_)
    return;

  ReceivePacket(packet);

  // 統計更新
  if (!packet.recovered()) {
    rtp_receive_statistics_->OnRtpPacket(packet);
  }
}
```

**ファイル**: `video/rtp_video_stream_receiver2.cc:1172-1239`

```cpp
void RtpVideoStreamReceiver2::ReceivePacket(const RtpPacketReceived& packet) {
  // 空パケット（パディング）処理
  if (packet.payload_size() == 0) {
    NotifyReceiverOfEmptyPacket(packet.SequenceNumber(), ...);
    return;
  }

  // REDパケット処理
  if (packet.PayloadType() == red_payload_type_) {
    ParseAndHandleEncapsulatingHeader(packet);
    return;
  }

  // デパケタイザ取得
  const auto type_it = payload_type_map_.find(packet.PayloadType());
  if (type_it == payload_type_map_.end()) {
    return;
  }

  // パース実行
  std::optional<VideoRtpDepacketizer::ParsedRtpPayload> parsed_payload =
      type_it->second->Parse(packet.PayloadBuffer());

  if (parsed_payload == std::nullopt) {
    return;
  }

  // ペイロードデータ処理へ
  OnReceivedPayloadData(std::move(parsed_payload->video_payload),
                        packet, parsed_payload->video_header, times_nacked);
}
```

---

## 3. Step 2: ペイロードデパケタイズ

### 3.1 処理概要

RTPペイロードからコーデック固有のヘッダを解析し、以下を抽出する：
- フレーム境界フラグ（`is_first_packet_in_frame`, `is_last_packet_in_frame`）
- コーデック固有メタデータ（Picture ID、Temporal Layer等）
- 実際のビデオペイロード

### 3.2 デパケタイザインターフェース

```
入力: CopyOnWriteBuffer rtp_payload
出力: ParsedRtpPayload {
    RTPVideoHeader video_header;  // メタデータ
    CopyOnWriteBuffer video_payload;  // ビデオデータ
}
```

### 3.3 VP8デパケタイズ（RFC 7741）

#### ペイロードディスクリプタ形式

```
       0 1 2 3 4 5 6 7
      +-+-+-+-+-+-+-+-+
      |X|R|N|S|R| PID | (必須)
      +-+-+-+-+-+-+-+-+
 X:   |I|L|T|K| RSV   | (オプション)
      +-+-+-+-+-+-+-+-+
 I:   |M| PictureID   | (オプション)
      +-+-+-+-+-+-+-+-+
      |   PictureID   | (Mビット=1の場合)
      +-+-+-+-+-+-+-+-+
 L:   |   TL0PICIDX   | (オプション)
      +-+-+-+-+-+-+-+-+
 T/K: |TID|Y| KEYIDX  | (オプション)
      +-+-+-+-+-+-+-+-+
```

#### 重要なビット

| ビット | 名前 | 説明 |
|-------|------|------|
| X | Extension | 拡張バイトの有無 |
| N | NonReference | 非参照フレーム |
| S | Start | パーティション開始 |
| PID | PartitionID | パーティションID (0-7) |

#### フレーム開始判定

```
is_first_packet_in_frame = (S == 1) && (PID == 0)
```

**C++参考実装** (`video_rtp_depacketizer_vp8.cc:175-176`):
```cpp
video_header->is_first_packet_in_frame =
    vp8_header.beginningOfPartition && vp8_header.partitionId == 0;
```

### 3.4 VP9デパケタイズ

#### ペイロードディスクリプタ形式

```
       0 1 2 3 4 5 6 7
      +-+-+-+-+-+-+-+-+
      |I|P|L|F|B|E|V|Z| (必須)
      +-+-+-+-+-+-+-+-+
```

#### 重要なビット

| ビット | 名前 | 説明 |
|-------|------|------|
| I | PictureID present | Picture ID存在 |
| P | Inter-picture predicted | インター予測 |
| L | Layer indices present | レイヤーインデックス存在 |
| F | Flexible mode | フレキシブルモード |
| B | Beginning of frame | フレーム開始 |
| E | End of frame | フレーム終了 |
| V | Scalability structure | スケーラビリティ構造存在 |
| Z | Not used for inter-layer | インターレイヤー非使用 |

#### フレーム境界判定

```
is_first_packet_in_frame = B
is_last_packet_in_frame = E
```

**C++参考実装** (`video_rtp_depacketizer_vp9.cc:222-223`):
```cpp
video_header->is_first_packet_in_frame = b_bit;
video_header->is_last_packet_in_frame = e_bit;
```

### 3.5 汎用デパケタイザ実装パターン

```python
# 疑似コード
def parse_vp8_payload(rtp_payload: bytes) -> ParsedPayload:
    if len(rtp_payload) == 0:
        return None

    offset = 0
    first_byte = rtp_payload[0]

    # 必須フィールド解析
    extension = (first_byte & 0x80) != 0
    non_reference = (first_byte & 0x20) != 0
    start_of_partition = (first_byte & 0x10) != 0
    partition_id = first_byte & 0x07
    offset += 1

    picture_id = None
    temporal_idx = None

    # 拡張フィールド解析
    if extension and offset < len(rtp_payload):
        ext_byte = rtp_payload[offset]
        has_picture_id = (ext_byte & 0x80) != 0
        has_tl0_pic_idx = (ext_byte & 0x40) != 0
        has_tid = (ext_byte & 0x20) != 0
        offset += 1

        if has_picture_id and offset < len(rtp_payload):
            if rtp_payload[offset] & 0x80:  # 15-bit picture ID
                picture_id = ((rtp_payload[offset] & 0x7F) << 8) | rtp_payload[offset + 1]
                offset += 2
            else:  # 7-bit picture ID
                picture_id = rtp_payload[offset] & 0x7F
                offset += 1

        # ... 他のオプションフィールド

    return ParsedPayload(
        is_first_packet_in_frame=(start_of_partition and partition_id == 0),
        is_last_packet_in_frame=False,  # マーカービットで判定
        picture_id=picture_id,
        video_payload=rtp_payload[offset:]
    )
```

---

## 4. Step 3: パケットバッファリング

### 4.1 処理概要

受信したパケットをバッファリングし、フレームを構成するすべてのパケットが揃ったら返却する。

### 4.2 主要データ構造

```
PacketBuffer {
    buffer: Array<Packet>  // リングバッファ
    first_seq_num: uint16  // バッファ内最古のシーケンス番号
    missing_packets: Set<uint16>  // 欠落パケット集合
}

Packet {
    sequence_number: int64  // アンラップ済みシーケンス番号
    timestamp: uint32       // RTPタイムスタンプ
    payload_type: uint8
    video_payload: bytes
    video_header: RTPVideoHeader
    continuous: bool        // 連続性フラグ
    marker_bit: bool        // マーカービット
}
```

### 4.3 InsertPacket アルゴリズム

```
InsertPacket(packet):
    1. シーケンス番号からバッファインデックスを計算
       index = seq_num % buffer.size

    2. バッファ衝突チェック
       if buffer[index] != null:
           if 同一シーケンス番号 -> 重複、破棄
           else -> バッファ拡張を試みる

    3. パケットをバッファに格納
       buffer[index] = packet

    4. 欠落パケット集合を更新

    5. フレーム検索を実行
       return FindFrames(seq_num)
```

### 4.4 FindFrames アルゴリズム（フレーム完成検出）

**コア概念**: フレームは`is_last_packet_in_frame`フラグが立ったパケットから逆方向に`is_first_packet_in_frame`フラグが立ったパケットまでの連続したパケット群。

```
FindFrames(seq_num):
    frames = []

    while バッファ内を走査:
        1. 連続性チェック (PotentialNewFrame)
           - パケットが存在するか
           - シーケンス番号が期待値と一致するか
           - フレーム先頭か、または前パケットが連続か

        2. パケットを連続としてマーク
           buffer[index].continuous = true

        3. フレーム終端チェック
           if packet.is_last_packet_in_frame:
               4. フレーム先頭を逆方向に検索
               start_seq = seq_num
               while not buffer[start_seq].is_first_packet_in_frame:
                   start_seq -= 1

               5. フレームのパケット群を収集
               for j in range(start_seq, seq_num + 1):
                   frames.append(buffer[j])
                   buffer[j] = null

    return frames
```

### 4.5 連続性判定 (PotentialNewFrame)

```
PotentialNewFrame(seq_num):
    packet = buffer[seq_num % buffer.size]
    prev_packet = buffer[(seq_num - 1) % buffer.size]

    if packet == null:
        return false

    if packet.seq_num != seq_num:
        return false  # 別のパケットが格納されている

    if packet.is_first_packet_in_frame:
        return true  # フレーム先頭は常に新フレームの可能性

    if prev_packet == null:
        return false  # 前パケットがない

    if prev_packet.seq_num != seq_num - 1:
        return false  # シーケンス番号が不連続

    if prev_packet.timestamp != packet.timestamp:
        return false  # タイムスタンプが異なる（別フレーム）

    return prev_packet.continuous  # 前パケットが連続ならこれも連続
```

### 4.6 C++参考実装

**ファイル**: `modules/video_coding/packet_buffer.cc:66-127`

```cpp
PacketBuffer::InsertResult PacketBuffer::InsertPacket(
    std::unique_ptr<PacketBuffer::Packet> packet) {
  PacketBuffer::InsertResult result;

  uint16_t seq_num = packet->seq_num();
  size_t index = seq_num % buffer_.size();

  // 初回パケット処理
  if (!first_packet_received_) {
    first_seq_num_ = seq_num;
    first_packet_received_ = true;
  }

  // バッファ衝突処理
  if (buffer_[index] != nullptr) {
    if (buffer_[index]->seq_num() == packet->seq_num()) {
      return result;  // 重複パケット
    }
    // バッファ拡張
    while (ExpandBufferSize() && buffer_[seq_num % buffer_.size()] != nullptr) {
    }
  }

  packet->continuous = false;
  buffer_[index] = std::move(packet);
  UpdateMissingPackets(seq_num);

  result.packets = FindFrames(seq_num);
  return result;
}
```

**ファイル**: `modules/video_coding/packet_buffer.cc:239-411`

```cpp
std::vector<std::unique_ptr<PacketBuffer::Packet>> PacketBuffer::FindFrames(
    uint16_t seq_num) {
  std::vector<std::unique_ptr<PacketBuffer::Packet>> found_frames;

  for (size_t i = 0; i < buffer_.size(); ++i) {
    if (!PotentialNewFrame(seq_num)) {
      break;
    }

    buffer_[seq_num % buffer_.size()]->continuous = true;

    if (buffer_[seq_num % buffer_.size()]->is_last_packet_in_frame()) {
      uint16_t start_seq_num = seq_num;
      int start_index = seq_num % buffer_.size();

      // フレーム先頭を検索
      while (true) {
        if (buffer_[start_index]->is_first_packet_in_frame()) {
          break;
        }
        start_index = (start_index > 0) ? start_index - 1 : buffer_.size() - 1;
        --start_seq_num;
      }

      // パケット群を収集
      for (uint16_t j = start_seq_num; j != seq_num + 1; ++j) {
        found_frames.push_back(std::move(buffer_[j % buffer_.size()]));
      }
    }
    ++seq_num;
  }
  return found_frames;
}
```

---

## 5. Step 4: フレーム組立

### 5.1 処理概要

PacketBufferから返されたパケット群からRtpFrameObjectを生成する。

### 5.2 処理フロー

```
OnInsertedPacket(result):
    for packet in result.packets:
        if packet.is_first_packet_in_frame:
            # 新フレーム開始
            first_packet = packet
            payloads.clear()
            packet_infos.clear()

        payloads.append(packet.video_payload)
        packet_infos.append(packet_info)

        if packet.is_last_packet_in_frame:
            # フレーム完成
            bitstream = depacketizer.AssembleFrame(payloads)
            frame = RtpFrameObject(
                first_seq_num=first_packet.seq_num,
                last_seq_num=packet.seq_num,
                timestamp=first_packet.timestamp,
                bitstream=bitstream,
                ...
            )
            OnAssembledFrame(frame)
```

### 5.3 ペイロード結合（AssembleFrame）

複数パケットのペイロードを単純連結する（多くのコーデックの場合）。

```python
def assemble_frame(payloads: List[bytes]) -> bytes:
    total_size = sum(len(p) for p in payloads)
    result = bytearray(total_size)
    offset = 0
    for payload in payloads:
        result[offset:offset + len(payload)] = payload
        offset += len(payload)
    return bytes(result)
```

### 5.4 C++参考実装

**ファイル**: `video/rtp_video_stream_receiver2.cc:818-920`

```cpp
void RtpVideoStreamReceiver2::OnInsertedPacket(
    video_coding::PacketBuffer::InsertResult result) {
  video_coding::PacketBuffer::Packet* first_packet = nullptr;
  std::vector<ArrayView<const uint8_t>> payloads;
  RtpPacketInfos::vector_type packet_infos;

  for (auto& packet : result.packets) {
    if (packet->is_first_packet_in_frame()) {
      payloads.clear();
      packet_infos.clear();
      first_packet = packet.get();
    }

    payloads.emplace_back(packet->video_payload);
    packet_infos.push_back(packet_info);

    if (packet->is_last_packet_in_frame()) {
      // ビットストリーム組立
      scoped_refptr<EncodedImageBuffer> bitstream =
          depacketizer_it->second->AssembleFrame(payloads);

      // RtpFrameObject生成
      OnAssembledFrame(std::make_unique<RtpFrameObject>(
          first_packet->seq_num(),
          last_packet.seq_num(),
          last_packet.marker_bit,
          max_nack_count,
          min_recv_time,
          max_recv_time,
          first_packet->timestamp,
          ntp_time_ms,
          last_packet.video_header.video_timing,
          first_packet->payload_type,
          first_packet->codec(),
          last_packet.video_header.rotation,
          last_packet.video_header.content_type,
          first_packet->video_header,
          last_packet.video_header.color_space,
          RtpPacketInfos(std::move(packet_infos)),
          std::move(bitstream)));
    }
  }
}
```

---

## 6. Step 5: フレーム参照解決

### 6.1 処理概要

組み立てられたフレームのフレーム間依存関係（どのフレームを参照しているか）を解決する。

### 6.2 参照解決器の種類

libwebrtcは以下の参照解決器を使い分ける：

| 解決器 | 使用条件 | 説明 |
|-------|---------|------|
| `RtpGenericFrameRefFinder` | Generic Descriptorあり | 汎用フレーム記述子使用時 |
| `RtpVp8RefFinder` | VP8 + temporal layer | VP8テンポラルレイヤー使用時 |
| `RtpVp9RefFinder` | VP9 + temporal layer | VP9テンポラルレイヤー使用時 |
| `RtpFrameIdOnlyRefFinder` | Picture IDのみ | Picture IDのみ存在時 |
| `RtpSeqNumOnlyRefFinder` | 上記以外 | シーケンス番号のみ使用時 |

### 6.3 解決器選択ロジック

```
ManageFrame(frame):
    video_header = frame.GetRtpVideoHeader()

    if video_header.generic.has_value():
        return RtpGenericFrameRefFinder.ManageFrame(frame)

    switch frame.codec_type():
        case VP8:
            if has_temporal_info:
                return RtpVp8RefFinder.ManageFrame(frame)
            elif has_picture_id:
                return RtpFrameIdOnlyRefFinder.ManageFrame(frame)
            else:
                return RtpSeqNumOnlyRefFinder.ManageFrame(frame)

        case VP9:
            # 同様のロジック

        default:
            return RtpSeqNumOnlyRefFinder.ManageFrame(frame)
```

### 6.4 フレーム依存関係の設定

参照解決器は`EncodedFrame`の以下のフィールドを設定する：

```cpp
size_t num_references;  // 参照フレーム数 (0-5)
int64_t references[5];  // 参照フレームID配列
```

キーフレームの場合：`num_references = 0`
デルタフレームの場合：`num_references >= 1`

### 6.5 C++参考実装

**ファイル**: `modules/video_coding/rtp_frame_reference_finder.cc:56-114`

```cpp
RtpFrameReferenceFinder::ReturnVector RtpFrameReferenceFinderImpl::ManageFrame(
    std::unique_ptr<RtpFrameObject> frame) {
  const RTPVideoHeader& video_header = frame->GetRtpVideoHeader();

  // Generic Descriptorがあればそれを使用
  if (video_header.generic.has_value()) {
    return GetRefFinderAs<RtpGenericFrameRefFinder>().ManageFrame(
        std::move(frame), *video_header.generic);
  }

  // コーデック別処理
  switch (frame->codec_type()) {
    case kVideoCodecVP8: {
      const RTPVideoHeaderVP8& vp8_header = ...;

      if (vp8_header.temporalIdx == kNoTemporalIdx) {
        if (vp8_header.pictureId == kNoPictureId) {
          return GetRefFinderAs<RtpSeqNumOnlyRefFinder>().ManageFrame(
              std::move(frame));
        }
        return GetRefFinderAs<RtpFrameIdOnlyRefFinder>().ManageFrame(
            std::move(frame), vp8_header.pictureId);
      }
      return GetRefFinderAs<RtpVp8RefFinder>().ManageFrame(std::move(frame));
    }
    // ... 他のコーデック
  }
}
```

---

## 7. データ構造定義

### 7.1 RTPVideoHeader

```cpp
struct RTPVideoHeader {
    VideoFrameType frame_type;  // kVideoFrameKey / kVideoFrameDelta
    uint16_t width;
    uint16_t height;
    VideoRotation rotation;
    VideoContentType content_type;
    bool is_first_packet_in_frame;
    bool is_last_packet_in_frame;
    VideoCodecType codec;

    // コーデック固有ヘッダ（VP8/VP9/H264等）
    variant<VP8Header, VP9Header, H264Header, ...> video_type_header;

    // Generic Frame Descriptor（オプション）
    optional<GenericDescriptorInfo> generic;
};
```

### 7.2 PacketBuffer::Packet

```cpp
struct Packet {
    bool continuous;           // 連続性フラグ
    bool marker_bit;           // RTPマーカービット
    uint8_t payload_type;      // ペイロードタイプ
    int64_t sequence_number;   // アンラップ済みシーケンス番号
    uint32_t timestamp;        // RTPタイムスタンプ
    int times_nacked;          // NACK回数

    CopyOnWriteBuffer video_payload;  // ペイロードデータ
    RTPVideoHeader video_header;      // ビデオヘッダ
};
```

### 7.3 RtpFrameObject / EncodedFrame

```cpp
class EncodedFrame {
    int64_t id_;                      // フレームID
    size_t num_references;            // 参照フレーム数
    int64_t references[5];            // 参照フレームID

    VideoFrameType _frameType;        // フレームタイプ
    uint8_t _payloadType;             // ペイロードタイプ
    uint32_t rtp_timestamp_;          // RTPタイムスタンプ
    int64_t ntp_time_ms_;             // NTP時刻
    int64_t _renderTimeMs;            // レンダリング時刻

    EncodedImageBuffer data_;         // エンコードデータ
    uint32_t _encodedWidth;           // 幅
    uint32_t _encodedHeight;          // 高さ
};

class RtpFrameObject : public EncodedFrame {
    uint16_t first_seq_num_;          // 先頭シーケンス番号
    uint16_t last_seq_num_;           // 終端シーケンス番号
    int64_t last_packet_received_time_;  // 最終パケット受信時刻
    int times_nacked_;                // NACK回数

    RTPVideoHeader rtp_video_header_; // RTPビデオヘッダ
    VideoCodecType codec_type_;       // コーデックタイプ
};
```

---

## 8. 疑似コード実装例

### 8.1 完全な処理パイプライン

```python
class VideoRtpReceiver:
    def __init__(self):
        self.packet_buffer = PacketBuffer(start_size=512, max_size=2048)
        self.reference_finder = RtpFrameReferenceFinder()
        self.depacketizers = {}  # payload_type -> depacketizer

    def on_rtp_packet(self, rtp_packet: RtpPacket):
        """RTPパケット受信エントリポイント"""
        # 1. ペイロード空チェック
        if rtp_packet.payload_size == 0:
            self._handle_padding(rtp_packet.sequence_number)
            return

        # 2. デパケタイザ取得
        depacketizer = self.depacketizers.get(rtp_packet.payload_type)
        if not depacketizer:
            return

        # 3. ペイロードパース
        parsed = depacketizer.parse(rtp_packet.payload)
        if not parsed:
            return

        # 4. パケット構造体作成
        packet = Packet(
            sequence_number=self._unwrap_seq_num(rtp_packet.sequence_number),
            timestamp=rtp_packet.timestamp,
            payload_type=rtp_packet.payload_type,
            marker_bit=rtp_packet.marker,
            video_payload=parsed.video_payload,
            video_header=parsed.video_header
        )

        # マーカービットでフレーム終端を更新
        packet.video_header.is_last_packet_in_frame |= rtp_packet.marker

        # 5. パケットバッファに挿入
        result = self.packet_buffer.insert_packet(packet)

        # 6. 完成フレームを処理
        self._on_inserted_packet(result)

    def _on_inserted_packet(self, result: InsertResult):
        """完成パケット群からフレームを組立"""
        first_packet = None
        payloads = []

        for packet in result.packets:
            if packet.is_first_packet_in_frame:
                first_packet = packet
                payloads = []

            payloads.append(packet.video_payload)

            if packet.is_last_packet_in_frame:
                # フレーム組立
                bitstream = self._assemble_frame(payloads)

                # RtpFrameObject作成
                frame = RtpFrameObject(
                    first_seq_num=first_packet.sequence_number,
                    last_seq_num=packet.sequence_number,
                    timestamp=first_packet.timestamp,
                    video_header=first_packet.video_header,
                    bitstream=bitstream
                )

                # フレーム参照解決
                completed_frames = self.reference_finder.manage_frame(frame)

                # 完成フレームをコールバック
                for completed in completed_frames:
                    self._on_complete_frame(completed)

    def _assemble_frame(self, payloads: List[bytes]) -> bytes:
        """複数ペイロードを連結"""
        return b''.join(payloads)


class PacketBuffer:
    def __init__(self, start_size: int, max_size: int):
        self.buffer = [None] * start_size
        self.max_size = max_size
        self.first_seq_num = 0
        self.first_packet_received = False

    def insert_packet(self, packet: Packet) -> InsertResult:
        seq_num = packet.sequence_number & 0xFFFF  # 16bit wrap
        index = seq_num % len(self.buffer)

        # 初回パケット
        if not self.first_packet_received:
            self.first_seq_num = seq_num
            self.first_packet_received = True

        # 衝突チェック
        if self.buffer[index] is not None:
            if self.buffer[index].sequence_number == packet.sequence_number:
                return InsertResult(packets=[])  # 重複
            self._expand_buffer()
            index = seq_num % len(self.buffer)

        packet.continuous = False
        self.buffer[index] = packet

        return InsertResult(packets=self._find_frames(seq_num))

    def _find_frames(self, seq_num: int) -> List[Packet]:
        frames = []

        for _ in range(len(self.buffer)):
            if not self._potential_new_frame(seq_num):
                break

            index = seq_num % len(self.buffer)
            self.buffer[index].continuous = True

            if self.buffer[index].is_last_packet_in_frame:
                # フレーム先頭を検索
                start_seq = seq_num
                while not self.buffer[start_seq % len(self.buffer)].is_first_packet_in_frame:
                    start_seq -= 1

                # パケット収集
                for j in range(start_seq, seq_num + 1):
                    idx = j % len(self.buffer)
                    frames.append(self.buffer[idx])
                    self.buffer[idx] = None

            seq_num += 1

        return frames

    def _potential_new_frame(self, seq_num: int) -> bool:
        index = seq_num % len(self.buffer)
        prev_index = (index - 1) % len(self.buffer)

        packet = self.buffer[index]
        if packet is None or packet.sequence_number != seq_num:
            return False

        if packet.is_first_packet_in_frame:
            return True

        prev = self.buffer[prev_index]
        if prev is None or prev.sequence_number != seq_num - 1:
            return False

        if prev.timestamp != packet.timestamp:
            return False

        return prev.continuous
```

---

## 9. C++参考実装の引用

### 9.1 主要ファイル一覧

| ファイル | 説明 |
|---------|------|
| `video/rtp_video_stream_receiver2.h/cc` | メイン受信クラス |
| `modules/video_coding/packet_buffer.h/cc` | パケットバッファ |
| `modules/video_coding/rtp_frame_reference_finder.h/cc` | フレーム参照解決 |
| `modules/rtp_rtcp/source/video_rtp_depacketizer.h` | デパケタイザインターフェース |
| `modules/rtp_rtcp/source/video_rtp_depacketizer_vp8.cc` | VP8デパケタイザ |
| `modules/rtp_rtcp/source/video_rtp_depacketizer_vp9.cc` | VP9デパケタイザ |
| `modules/rtp_rtcp/source/frame_object.h/cc` | RtpFrameObject |
| `api/video/encoded_frame.h` | EncodedFrame基底クラス |

### 9.2 定数定義

```cpp
// packet_buffer.cc
constexpr int kPacketBufferStartSize = 512;
constexpr int kPacketBufferMaxSize = 2048;

// VP8/VP9 globals
constexpr int kNoPictureId = -1;
constexpr int kNoTemporalIdx = 0xFF;
constexpr int kNoTl0PicIdx = -1;
```

### 9.3 RFC参照

| RFC | 内容 |
|-----|------|
| RFC 3550 | RTP基本仕様 |
| RFC 7741 | VP8 RTP Payload Format |
| draft-ietf-payload-vp9 | VP9 RTP Payload Format |
| draft-ietf-payload-av1 | AV1 RTP Payload Format |

---

## 付録: 実装時の注意点

### A.1 シーケンス番号ラップアラウンド

16bitシーケンス番号は65535を超えると0に戻る。比較・差分計算時は必ずラップアラウンドを考慮すること。

```python
def seq_num_lt(a: int, b: int) -> bool:
    """シーケンス番号a < bの比較（ラップアラウンド対応）"""
    diff = (b - a) & 0xFFFF
    return diff != 0 and diff < 0x8000
```

### A.2 タイムスタンプの扱い

RTPタイムスタンプは90kHz（ビデオの場合）で刻まれる32bitカウンタ。こちらもラップアラウンドする。

### A.3 メモリ管理

パケットバッファは大量のメモリを消費する可能性がある。適切なサイズ制限とクリーンアップ処理が必要。

### A.4 スレッドセーフティ

libwebrtcでは`RTC_DCHECK_RUN_ON`マクロでスレッドチェックを行っている。マルチスレッド環境では適切な同期機構が必要。
