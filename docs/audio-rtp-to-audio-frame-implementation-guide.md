# 音声RTPパケットからAudioFrameまでの実装ガイド

本ドキュメントは、libwebrtcにおける音声RTPパケット受信からデコード済みオーディオフレーム（AudioFrame）生成までの処理フローを詳細に解説し、フルスクラッチ実装に必要な情報を網羅します。

## 1. 全体アーキテクチャ

### 1.1 処理フロー概要

```
RTPパケット受信
      |
      v
+------------------+
| ChannelReceive   |  <- RTP受信・復号化
| (OnRtpPacket)    |
+------------------+
      |
      v
+------------------+
| NetEqImpl        |  <- ジッタバッファ・デコード制御
| (InsertPacket)   |
+------------------+
      |
      v
+------------------+
| PacketBuffer     |  <- パケットバッファリング（タイムスタンプ順ソート）
+------------------+
      |
      v
+------------------+
| DecisionLogic    |  <- 再生判断ロジック（Normal/Expand/Accelerate等）
+------------------+
      |
      v
+------------------+
| AudioDecoder     |  <- コーデックデコード（Opus, G.711等）
+------------------+
      |
      v
+------------------+
| DSP Processing   |  <- 信号処理（Expand/Merge/Accelerate等）
+------------------+
      |
      v
+------------------+
| SyncBuffer       |  <- 出力同期バッファ
+------------------+
      |
      v
+------------------+
| AudioFrame       |  <- 最終出力（10ms単位のPCMデータ）
+------------------+
```

### 1.2 主要コンポーネント

| コンポーネント | 役割 | ファイル |
|--------------|------|---------|
| ChannelReceive | RTPパケット受信・NetEqへの橋渡し | `audio/channel_receive.cc` |
| NetEqImpl | ジッタバッファ制御、デコード調整 | `modules/audio_coding/neteq/neteq_impl.cc` |
| PacketBuffer | パケットの時系列管理 | `modules/audio_coding/neteq/packet_buffer.cc` |
| DecisionLogic | 再生操作の決定 | `modules/audio_coding/neteq/decision_logic.cc` |
| Expand | パケットロス補間（PLC） | `modules/audio_coding/neteq/expand.cc` |
| AudioDecoder | コーデック抽象インターフェース | `api/audio_codecs/audio_decoder.h` |

## 2. データ構造

### 2.1 Packet構造体（音声用）

```cpp
// modules/audio_coding/neteq/packet.h
struct Packet {
  // 優先度（codecレベルとREDレベル）
  struct Priority {
    int codec_level;  // 0=プライマリ, >0=冗長
    int red_level;    // REDパケットの階層
  };

  uint8_t payload_type;           // RTPペイロードタイプ
  uint16_t sequence_number;       // RTPシーケンス番号
  uint32_t timestamp;             // RTPタイムスタンプ
  Priority priority;              // パケット優先度
  Buffer payload;                 // ペイロードデータ
  std::unique_ptr<EncodedAudioFrame> frame;  // パース済みフレーム
  std::optional<RtpPacketInfo> packet_info;  // RTP情報
  std::unique_ptr<Stopwatch> waiting_time;   // 待機時間計測
};
```

### 2.2 AudioFrame構造体

```cpp
// api/audio/audio_frame.h
class AudioFrame {
  // 最大サンプル数: 7680 (ステレオ32kHz 120ms or 8ch 48kHz 20ms)
  static constexpr size_t kMaxDataSizeSamples = 7680;

  enum SpeechType {
    kNormalSpeech = 0,  // 通常音声
    kPLC = 1,           // パケットロス補間
    kCNG = 2,           // コンフォートノイズ
    kPLCCNG = 3,        // PLC + CNG
    kCodecPLC = 5,      // コーデック内蔵PLC
    kUndefined = 4
  };

  uint32_t timestamp_;              // RTPタイムスタンプ
  size_t samples_per_channel_;      // チャンネルあたりサンプル数
  int sample_rate_hz_;              // サンプルレート
  size_t num_channels_;             // チャンネル数
  SpeechType speech_type_;          // 音声タイプ
  int16_t data_[kMaxDataSizeSamples]; // PCMデータ（インターリーブ）
};
```

## 3. 処理フロー詳細

### 3.1 RTPパケット受信（ChannelReceive）

**ファイル**: `audio/channel_receive.cc`

```cpp
// エントリポイント: OnRtpPacket (line 679-717)
void ChannelReceive::OnRtpPacket(const RtpPacketReceived& packet) {
  // 1. パケット統計更新
  rtp_receive_statistics_->OnRtpPacket(packet);

  // 2. ペイロード抽出
  auto payload = rtc::ArrayView<const uint8_t>(
      packet.payload(), packet.payload_size());

  // 3. SRTP復号化（必要な場合）
  // 4. NetEqへの挿入
  ReceivePacket(payload, packet.header(), ...);
}

// ReceivePacket (line 719-776)
void ChannelReceive::ReceivePacket(
    ArrayView<const uint8_t> packet,
    const RTPHeader& header, ...) {

  // SRTP復号化処理
  // ...

  // NetEqへパケット挿入
  OnReceivedPayloadData(payload, header);
}

// OnReceivedPayloadData (line 340-386)
void ChannelReceive::OnReceivedPayloadData(
    ArrayView<const uint8_t> payload,
    const RTPHeader& rtp_header) {

  // NetEq::InsertPacketを呼び出し
  neteq_->InsertPacket(rtp_header, payload, receive_time);
}
```

### 3.2 NetEqパケット挿入

**ファイル**: `modules/audio_coding/neteq/neteq_impl.cc`

```cpp
// InsertPacket (line 182-428)
int NetEqImpl::InsertPacket(
    const RTPHeader& rtp_header,
    ArrayView<const uint8_t> payload,
    const RtpPacketInfo& packet_info) {

  // 1. Packetオブジェクト作成
  PacketList packet_list;
  packet_list.push_back([&] {
    Packet packet;
    packet.payload_type = rtp_header.payloadType;
    packet.sequence_number = rtp_header.sequenceNumber;
    packet.timestamp = rtp_header.timestamp;
    packet.payload.SetData(payload.data(), payload.size());
    return packet;
  }());

  // 2. 初回パケット処理（リセット）
  if (first_packet_) {
    timestamp_scaler_->Reset();
    packet_buffer_->Flush();
    dtmf_buffer_->Flush();
  }

  // 3. REDパケット分割
  if (decoder_database_->IsRed(rtp_header.payloadType)) {
    red_payload_splitter_->SplitRed(&packet_list);
  }

  // 4. タイムスタンプスケーリング
  timestamp_scaler_->ToInternal(&packet_list);

  // 5. DTMFイベント処理
  // ... (DTMFパケットはdtmf_buffer_へ)

  // 6. デコーダによるペイロードパース
  for (Packet& packet : packet_list) {
    const DecoderDatabase::DecoderInfo* info =
        decoder_database_->GetDecoderInfo(packet.payload_type);

    // ParsePayloadでEncodedAudioFrameを生成
    std::vector<AudioDecoder::ParseResult> results =
        info->GetDecoder()->ParsePayload(
            std::move(packet.payload), packet.timestamp);

    for (auto& result : results) {
      Packet new_packet;
      new_packet.timestamp = result.timestamp;
      new_packet.frame = std::move(result.frame);
      parsed_packet_list.push_back(std::move(new_packet));
    }
  }

  // 7. PacketBufferへ挿入
  for (Packet& packet : parsed_packet_list) {
    packet_buffer_->InsertPacket(std::move(packet));
    controller_->PacketArrived(...);
  }

  return kOK;
}
```

### 3.3 PacketBuffer（音声用）

**ファイル**: `modules/audio_coding/neteq/packet_buffer.cc`

音声用PacketBufferは映像用と異なり、**タイムスタンプでソートされたリスト**として実装されています。

```cpp
// 構造: std::list<Packet> buffer_（タイムスタンプ昇順）

// InsertPacket (line 82-127)
int PacketBuffer::InsertPacket(Packet&& packet) {
  // 1. 空パケットチェック
  if (packet.empty()) {
    return kInvalidPacket;
  }

  // 2. 待機時間計測開始
  packet.waiting_time = tick_timer_->GetNewStopwatch();

  // 3. バッファ満杯チェック
  if (buffer_.size() >= max_number_of_packets_) {
    Flush();
    return kFlushed;
  }

  // 4. 挿入位置を後ろから検索（新しいパケットは後ろにある可能性が高い）
  PacketList::reverse_iterator rit = std::find_if(
      buffer_.rbegin(), buffer_.rend(),
      NewTimestampIsLarger(packet));

  // 5. 重複タイムスタンプ処理
  // 同じタイムスタンプで高優先度がある場合は挿入しない
  if (rit != buffer_.rend() && packet.timestamp == rit->timestamp) {
    return return_val;  // 既存パケットを維持
  }

  // 6. リストに挿入
  buffer_.insert(rit.base(), std::move(packet));

  return return_val;
}

// GetNextPacket (line 163-175)
std::optional<Packet> PacketBuffer::GetNextPacket() {
  if (Empty()) return std::nullopt;

  std::optional<Packet> packet(std::move(buffer_.front()));
  buffer_.pop_front();
  return packet;
}
```

### 3.4 GetAudio（デコード済み音声取得）

**ファイル**: `modules/audio_coding/neteq/neteq_impl.cc`

```cpp
// GetAudio (line 430-457)
int NetEqImpl::GetAudio(AudioFrame* audio_frame, ...) {
  // GetAudioInternalを呼び出し
  if (GetAudioInternal(audio_frame, action_override) != kNoError) {
    return kFail;
  }

  // 統計更新
  stats_->IncreaseCounter(output_size_samples_, fs_hz_);
  audio_frame->speech_type_ = ToSpeechType(LastOutputType());

  return kOK;
}

// GetAudioInternal (line 661-895)
int NetEqImpl::GetAudioInternal(AudioFrame* audio_frame, ...) {
  PacketList packet_list;
  Operation operation;

  tick_timer_->Increment();

  // 1. ミュート状態チェック
  if (enable_muted_state_ && expand_->Muted() && packet_buffer_->Empty()) {
    // ミュートフレームを返す
    audio_frame->Mute();
    return 0;
  }

  // 2. 操作決定（DecisionLogic）
  int return_value = GetDecision(&operation, &packet_list, ...);

  // 3. デコード実行
  int decode_return_value = Decode(&packet_list, &operation,
                                    &length, &speech_type);

  // 4. 操作に応じた処理
  algorithm_buffer_->Clear();
  switch (operation) {
    case Operation::kNormal:
      DoNormal(decoded_buffer_.get(), length, speech_type, play_dtmf);
      break;
    case Operation::kMerge:
      DoMerge(decoded_buffer_.get(), length, speech_type, play_dtmf);
      break;
    case Operation::kExpand:
      if (!DoCodecPlc()) {
        DoExpand(play_dtmf);
      }
      break;
    case Operation::kAccelerate:
    case Operation::kFastAccelerate:
      DoAccelerate(decoded_buffer_.get(), length, ...);
      break;
    case Operation::kPreemptiveExpand:
      DoPreemptiveExpand(decoded_buffer_.get(), length, ...);
      break;
    case Operation::kRfc3389Cng:
      DoRfc3389Cng(&packet_list, play_dtmf);
      break;
    // ...
  }

  // 5. SyncBufferへコピー
  sync_buffer_->PushBack(*algorithm_buffer_);

  // 6. 出力フレーム生成
  audio_frame->ResetWithoutMuting();
  InterleavedView<int16_t> view = audio_frame->mutable_data(
      output_size_samples_, sync_buffer_->Channels());
  sync_buffer_->GetNextAudioInterleaved(view);

  // 7. タイムスタンプ設定
  audio_frame->timestamp_ = timestamp_scaler_->ToExternal(playout_timestamp_);

  return return_value;
}
```

### 3.5 DecisionLogic（操作決定）

**ファイル**: `modules/audio_coding/neteq/decision_logic.cc`

```cpp
// GetDecision (line 109-156)
NetEq::Operation DecisionLogic::GetDecision(
    const NetEqStatus& status, bool* reset_decoder) {

  // 1. エラー状態ガード
  if (status.last_mode == NetEq::Mode::kError) {
    return status.next_packet ? Operation::kUndefined : Operation::kExpand;
  }

  // 2. CNGパケット処理
  if (status.next_packet && status.next_packet->is_cng) {
    return CngOperation(status);
  }

  // 3. パケットなしの場合
  if (!status.next_packet) {
    return NoPacket(status);  // Expand or CNG継続
  }

  // 4. デコード延期判定
  if (PostponeDecode(status)) {
    return NoPacket(status);
  }

  // 5. パケット利用可能性に応じた判定
  if (status.target_timestamp == status.next_packet->timestamp) {
    // 期待するパケットが利用可能
    return ExpectedPacketAvailable(status);
  }

  if (!PacketBuffer::IsObsoleteTimestamp(...)) {
    // 将来のパケットが利用可能
    return FuturePacketAvailable(status);
  }

  // 新しいストリーム検出
  return Operation::kUndefined;
}

// ExpectedPacketAvailable (line 264-286)
NetEq::Operation DecisionLogic::ExpectedPacketAvailable(
    NetEqController::NetEqStatus status) {

  if (!disallow_time_stretching_ && ...) {
    const int playout_delay_ms = GetPlayoutDelayMs(status);
    const int64_t low_limit = TargetLevelMs();
    const int64_t high_limit = low_limit +
        packet_arrival_history_->GetMaxDelayMs() +
        kDelayAdjustmentGranularityMs;

    // バッファレベルに応じた操作選択
    if (playout_delay_ms >= high_limit * 4) {
      return Operation::kFastAccelerate;  // 急速加速
    }
    if (playout_delay_ms >= high_limit) {
      return Operation::kAccelerate;      // 加速（サンプル削除）
    }
    if (playout_delay_ms < low_limit) {
      return Operation::kPreemptiveExpand; // 先行拡張（サンプル追加）
    }
  }

  return Operation::kNormal;
}
```

### 3.6 Decode（デコード処理）

**ファイル**: `modules/audio_coding/neteq/neteq_impl.cc`

```cpp
// Decode (line 1182-1287)
int NetEqImpl::Decode(PacketList* packet_list, Operation* operation,
                      int* decoded_length, SpeechType* speech_type) {
  *speech_type = AudioDecoder::kSpeech;

  // アクティブデコーダ取得
  AudioDecoder* decoder = decoder_database_->GetActiveDecoder();

  if (!packet_list->empty()) {
    uint8_t payload_type = packet_list->front().payload_type;
    if (!decoder_database_->IsComfortNoise(payload_type)) {
      decoder = decoder_database_->GetDecoder(payload_type);
      decoder_database_->SetActiveDecoder(payload_type, &decoder_changed);
    }
  }

  // デコーダリセット
  if (reset_decoder_) {
    decoder->Reset();
    reset_decoder_ = false;
  }

  // デコードループ
  if (*operation == Operation::kCodecInternalCng) {
    return DecodeCng(decoder, decoded_length, speech_type);
  } else {
    return DecodeLoop(packet_list, *operation, decoder,
                      decoded_length, speech_type);
  }
}

// DecodeLoop (line 1321-1382)
int NetEqImpl::DecodeLoop(PacketList* packet_list, ...) {
  while (!packet_list->empty() &&
         !decoder_database_->IsComfortNoise(packet_list->front().payload_type)) {

    // EncodedAudioFrame::Decodeを呼び出し
    auto opt_result = packet_list->front().frame->Decode(
        ArrayView<int16_t>(&decoded_buffer_[*decoded_length],
                           decoded_buffer_length_ - *decoded_length));

    packet_list->pop_front();

    if (opt_result) {
      *speech_type = opt_result->speech_type;
      *decoded_length += opt_result->num_decoded_samples;
      decoder_frame_length_ = opt_result->num_decoded_samples / decoder->Channels();
    }
  }
  return 0;
}
```

## 4. 操作タイプ詳細

### 4.1 Operation列挙型

```cpp
enum class Operation {
  kNormal,           // 通常デコード
  kMerge,            // Expand後のマージ（スムージング）
  kExpand,           // パケットロス補間（PLC）
  kAccelerate,       // 加速（バッファ削減）
  kFastAccelerate,   // 高速加速
  kPreemptiveExpand, // 先行拡張（バッファ増加）
  kRfc3389Cng,       // RFC3389コンフォートノイズ
  kRfc3389CngNoPacket, // CNG継続（パケットなし）
  kCodecInternalCng,   // コーデック内蔵CNG
  kDtmf,             // DTMFトーン生成
  kUndefined         // 未定義（リセット要求）
};
```

### 4.2 各操作の処理

#### Normal（通常再生）

```cpp
void NetEqImpl::DoNormal(const int16_t* decoded_buffer,
                         size_t decoded_length, ...) {
  normal_->Process(decoded_buffer, decoded_length,
                   last_mode_, algorithm_buffer_.get());
  last_mode_ = Mode::kNormal;
}
```

#### Expand（パケットロス補間）

パケットロス時に過去の音声から合成信号を生成：

```cpp
int Expand::Process(AudioMultiVector* output) {
  if (first_expand_) {
    // 初回: 信号分析（ピッチ、スペクトル特性）
    AnalyzeSignal(random_vector);
    first_expand_ = false;
  }

  // ラグインデックス更新（ピッチ周期）
  UpdateLagIndex();

  // 有声成分生成（過去信号からの外挿）
  // expand_vector0/1からvoiced_vectorを生成

  // 無声成分生成（ARフィルタリング）
  WebRtcSpl_FilterARFastQ12(scaled_random_vector, unvoiced_vector, ...);

  // 有声/無声のクロスフェード
  DspHelper::CrossFade(voiced_vector, unvoiced_vector, ...);

  // ミューティング適用（長時間expandでフェードアウト）
  DspHelper::MuteSignal(temp_data, parameters.mute_slope, current_lag);

  // 背景ノイズ追加
  background_noise_->GenerateBackgroundNoise(...);

  consecutive_expands_++;
  return 0;
}
```

#### Merge（Expand後のスムージング）

Expand状態から通常デコードへの遷移をスムーズに：

```cpp
void NetEqImpl::DoMerge(int16_t* decoded_buffer, ...) {
  size_t new_length = merge_->Process(decoded_buffer, decoded_length,
                                       algorithm_buffer_.get());
  last_mode_ = Mode::kMerge;
  expand_->Reset();
}
```

#### Accelerate（加速）

バッファ蓄積を減らすためにサンプルを削除：

```cpp
int NetEqImpl::DoAccelerate(int16_t* decoded_buffer, ...) {
  // 最低30ms必要
  const size_t required_samples = 240 * fs_mult_;

  // 加速処理（ピッチ周期に基づくサンプル削除）
  size_t samples_removed = 0;
  accelerate_->Process(decoded_buffer, decoded_length, fast_accelerate,
                       algorithm_buffer_.get(), &samples_removed);

  stats_->AcceleratedSamples(samples_removed);
}
```

#### PreemptiveExpand（先行拡張）

バッファアンダーランを防ぐためにサンプルを追加：

```cpp
int NetEqImpl::DoPreemptiveExpand(int16_t* decoded_buffer, ...) {
  size_t samples_added = 0;
  preemptive_expand_->Process(decoded_buffer, decoded_length,
                              old_borrowed_samples_per_channel,
                              algorithm_buffer_.get(), &samples_added);

  stats_->PreemptiveExpandedSamples(samples_added);
}
```

## 5. AudioDecoderインターフェース

### 5.1 基本インターフェース

```cpp
class AudioDecoder {
 public:
  enum SpeechType { kSpeech = 1, kComfortNoise = 2 };

  // ペイロードをパースしてEncodedAudioFrameを生成
  virtual std::vector<ParseResult> ParsePayload(Buffer&& payload,
                                                 uint32_t timestamp);

  // デコード実行
  int Decode(const uint8_t* encoded, size_t encoded_len,
             int sample_rate_hz, size_t max_decoded_bytes,
             int16_t* decoded, SpeechType* speech_type);

  // PLC機能
  virtual bool HasDecodePlc() const;
  virtual size_t DecodePlc(size_t num_frames, int16_t* decoded);
  virtual void GeneratePlc(size_t requested_samples_per_channel,
                           BufferT<int16_t>* concealment_audio);

  // リセット
  virtual void Reset() = 0;

  // パケット情報
  virtual int PacketDuration(const uint8_t* encoded, size_t encoded_len) const;
  virtual bool PacketHasFec(const uint8_t* encoded, size_t encoded_len) const;

  // サンプルレート・チャンネル数
  virtual int SampleRateHz() const = 0;
  virtual size_t Channels() const = 0;

 protected:
  // 派生クラスで実装
  virtual int DecodeInternal(const uint8_t* encoded, size_t encoded_len,
                             int sample_rate_hz, int16_t* decoded,
                             SpeechType* speech_type) = 0;
};
```

### 5.2 EncodedAudioFrame

```cpp
class EncodedAudioFrame {
 public:
  struct DecodeResult {
    size_t num_decoded_samples;  // 全チャンネル合計サンプル数
    SpeechType speech_type;
  };

  virtual size_t Duration() const = 0;
  virtual bool IsDtxPacket() const;
  virtual std::optional<DecodeResult> Decode(ArrayView<int16_t> decoded) const = 0;
};
```

### 5.3 Opus実装例

```cpp
// modules/audio_coding/codecs/opus/audio_decoder_opus.cc

std::vector<ParseResult> AudioDecoderOpusImpl::ParsePayload(
    Buffer&& payload, uint32_t timestamp) {
  std::vector<ParseResult> results;

  // FEC（前方誤り訂正）チェック
  if (PacketHasFec(payload.data(), payload.size())) {
    const int duration = PacketDurationRedundant(payload.data(), payload.size());
    Buffer payload_copy(payload.data(), payload.size());
    results.emplace_back(
        timestamp - duration,  // FECは1フレーム前のデータ
        1,                     // 優先度1（冗長）
        std::make_unique<OpusFrame>(this, std::move(payload_copy), false));
  }

  // メインペイロード
  results.emplace_back(
      timestamp,
      0,  // 優先度0（プライマリ）
      std::make_unique<OpusFrame>(this, std::move(payload), true));

  return results;
}

int AudioDecoderOpusImpl::DecodeInternal(
    const uint8_t* encoded, size_t encoded_len,
    int sample_rate_hz, int16_t* decoded, SpeechType* speech_type) {

  int16_t temp_type = 1;
  int ret = WebRtcOpus_Decode(dec_state_, encoded, encoded_len,
                               decoded, &temp_type);

  if (ret > 0)
    ret *= channels_;  // 全チャンネル合計サンプル数

  *speech_type = ConvertSpeechType(temp_type);
  return ret;
}

// コーデック内蔵PLC
void AudioDecoderOpusImpl::GeneratePlc(
    size_t requested_samples_per_channel,
    BufferT<int16_t>* concealment_audio) {

  // nullptrを渡すとOpusがPLCを生成
  int ret = WebRtcOpus_Decode(dec_state_, nullptr, 0,
                               decoded.data(), &temp_type);
}
```

## 6. 実装のポイント

### 6.1 タイムスタンプ管理

音声RTPのタイムスタンプはサンプル単位：

```
タイムスタンプ増分 = サンプルレート / (1000 / フレーム長ms)

例: 48kHz、20msフレーム
増分 = 48000 / (1000/20) = 48000 / 50 = 960サンプル
```

### 6.2 ジッタバッファ管理

```cpp
// ターゲットレベル計算（DecisionLogic）
int TargetLevelMs() const {
  return delay_constraints_.Clamp(delay_manager_->TargetDelayMs());
}

// バッファレベルフィルタリング
void FilterBufferLevel(size_t buffer_size_samples) {
  buffer_level_filter_->SetTargetBufferLevel(TargetLevelMs());
  buffer_level_filter_->Update(buffer_size_samples, time_stretched_samples);
}
```

### 6.3 出力サイズ

NetEqは常に**10ms単位**で出力：

```cpp
// 出力サンプル数計算
output_size_samples_ = kOutputSizeMs * 8 * fs_mult_;
// kOutputSizeMs = 10
// fs_mult_ = sample_rate / 8000

// 例: 48kHz
// output_size_samples_ = 10 * 8 * 6 = 480サンプル
```

## 7. フルスクラッチ実装のための擬似コード

### 7.1 基本的なジッタバッファ実装

```python
class AudioJitterBuffer:
    def __init__(self, sample_rate: int, max_packets: int = 200):
        self.sample_rate = sample_rate
        self.packet_buffer = SortedList(key=lambda p: p.timestamp)
        self.max_packets = max_packets
        self.target_delay_ms = 60  # 初期ターゲット遅延
        self.output_size_samples = sample_rate // 100  # 10ms

        self.sync_buffer = deque()  # 出力同期バッファ
        self.last_decoded_timestamp = 0
        self.expand_count = 0

    def insert_packet(self, rtp_header: RTPHeader, payload: bytes):
        """RTPパケットを挿入"""
        # デコーダでパース
        frames = self.decoder.parse_payload(payload, rtp_header.timestamp)

        for frame in frames:
            packet = Packet(
                timestamp=frame.timestamp,
                sequence_number=rtp_header.sequence_number,
                priority=frame.priority,
                frame=frame
            )

            # バッファ満杯チェック
            if len(self.packet_buffer) >= self.max_packets:
                self.flush()

            # タイムスタンプ順で挿入
            self.packet_buffer.add(packet)

        # 遅延推定更新
        self.update_delay_estimate(rtp_header.timestamp)

    def get_audio(self) -> AudioFrame:
        """10msのオーディオフレームを取得"""
        # 操作決定
        operation = self.decide_operation()

        if operation == Operation.NORMAL:
            return self.do_normal()
        elif operation == Operation.EXPAND:
            return self.do_expand()
        elif operation == Operation.MERGE:
            return self.do_merge()
        elif operation == Operation.ACCELERATE:
            return self.do_accelerate()
        elif operation == Operation.PREEMPTIVE_EXPAND:
            return self.do_preemptive_expand()

    def decide_operation(self) -> Operation:
        """次の操作を決定"""
        target_timestamp = self.last_decoded_timestamp + self.output_size_samples

        # パケットがない場合
        if not self.packet_buffer:
            return Operation.EXPAND

        next_packet = self.packet_buffer[0]

        # 期待するパケットが利用可能
        if next_packet.timestamp == target_timestamp:
            buffer_level_ms = self.get_buffer_level_ms()
            target_level_ms = self.target_delay_ms

            if buffer_level_ms >= target_level_ms * 4:
                return Operation.FAST_ACCELERATE
            elif buffer_level_ms >= target_level_ms * 1.5:
                return Operation.ACCELERATE
            elif buffer_level_ms < target_level_ms * 0.5:
                return Operation.PREEMPTIVE_EXPAND
            else:
                return Operation.NORMAL

        # パケットが将来のものの場合
        if next_packet.timestamp > target_timestamp:
            if self.expand_count > 0:
                return Operation.MERGE  # Expandからの復帰
            return Operation.EXPAND

        # パケットが過去のもの（廃棄）
        return Operation.UNDEFINED

    def do_normal(self) -> AudioFrame:
        """通常デコード"""
        packet = self.packet_buffer.pop(0)

        # デコード
        decoded = packet.frame.decode()

        # 出力フレーム生成
        self.sync_buffer.extend(decoded)
        output = self.extract_output_frame()

        self.last_decoded_timestamp = packet.timestamp
        self.expand_count = 0

        return output

    def do_expand(self) -> AudioFrame:
        """パケットロス補間（簡易版）"""
        # 過去の音声から合成
        if self.expand_count == 0:
            self.analyze_for_expand()

        # ピッチベースの外挿
        expanded = self.generate_expanded_audio()

        # ミューティング適用
        mute_factor = max(0, 1.0 - self.expand_count * 0.1)
        expanded = expanded * mute_factor

        self.sync_buffer.extend(expanded)
        output = self.extract_output_frame()
        output.speech_type = SpeechType.PLC

        self.expand_count += 1
        self.last_decoded_timestamp += self.output_size_samples

        return output

    def do_accelerate(self) -> AudioFrame:
        """加速（サンプル削除）"""
        # 複数フレームをデコード
        decoded = self.decode_multiple_frames(30)  # 30ms分

        # ピッチ周期を検出してサンプル削除
        pitch_period = self.detect_pitch(decoded)
        accelerated = self.remove_pitch_period(decoded, pitch_period)

        self.sync_buffer.extend(accelerated)
        return self.extract_output_frame()

    def extract_output_frame(self) -> AudioFrame:
        """SyncBufferから10ms分を抽出"""
        output = AudioFrame()
        output.sample_rate_hz = self.sample_rate
        output.samples_per_channel = self.output_size_samples
        output.num_channels = self.num_channels

        samples_needed = self.output_size_samples * self.num_channels
        output.data = [self.sync_buffer.popleft()
                       for _ in range(min(samples_needed, len(self.sync_buffer)))]

        return output
```

### 7.2 簡易Expand（PLC）実装

```python
class SimpleExpand:
    def __init__(self, sample_rate: int):
        self.sample_rate = sample_rate
        self.history = deque(maxlen=sample_rate)  # 1秒分
        self.pitch_period = 0
        self.ar_coeffs = []

    def analyze(self, audio_history: List[int16]):
        """信号分析"""
        self.history.extend(audio_history[-self.sample_rate:])

        # ピッチ検出（自己相関）
        self.pitch_period = self.detect_pitch()

        # ARモデル係数推定（LPC分析）
        self.ar_coeffs = self.estimate_lpc_coeffs()

    def detect_pitch(self) -> int:
        """自己相関によるピッチ検出"""
        history = list(self.history)
        min_period = self.sample_rate // 400  # 400Hz
        max_period = self.sample_rate // 80   # 80Hz

        best_corr = 0
        best_period = min_period

        for period in range(min_period, max_period):
            corr = sum(history[i] * history[i - period]
                       for i in range(period, len(history)))
            if corr > best_corr:
                best_corr = corr
                best_period = period

        return best_period

    def generate(self, num_samples: int) -> List[int16]:
        """補間音声生成"""
        output = []
        history = list(self.history)

        for i in range(num_samples):
            # ピッチ周期で過去信号を繰り返し
            voiced = history[-(self.pitch_period - (i % self.pitch_period))]

            # ARフィルタでノイズ生成
            noise = self.generate_ar_noise()

            # 有声/無声のミックス
            sample = int(0.7 * voiced + 0.3 * noise)
            output.append(sample)

        return output
```

## 8. 映像処理との主な違い

| 項目 | 音声 | 映像 |
|-----|------|------|
| バッファ構造 | ソート済みリスト | リングバッファ |
| フレーム完成判定 | 1パケット=1フレーム（通常） | is_first/last_packet_in_frame |
| 出力単位 | 固定10ms | 可変（フレームレート依存） |
| PLC | 信号処理ベース（Expand） | フレームコピー/補間 |
| タイムストレッチ | Accelerate/PreemptiveExpand | なし |
| ジッタバッファ | 積極的な遅延制御 | 参照フレーム依存 |

## 9. 参考リソース

### 9.1 RFC

- RFC 3550: RTP: A Transport Protocol for Real-Time Applications
- RFC 3551: RTP Profile for Audio and Video Conferences
- RFC 3389: Real-time Transport Protocol (RTP) Payload for Comfort Noise
- RFC 6716: Definition of the Opus Audio Codec

### 9.2 libwebrtcソースファイル

- `audio/channel_receive.cc` - RTP受信エントリポイント
- `modules/audio_coding/neteq/neteq_impl.cc` - NetEq本体
- `modules/audio_coding/neteq/packet_buffer.cc` - パケットバッファ
- `modules/audio_coding/neteq/decision_logic.cc` - 操作決定ロジック
- `modules/audio_coding/neteq/expand.cc` - PLC実装
- `api/audio_codecs/audio_decoder.h` - デコーダインターフェース
- `api/audio/audio_frame.h` - 出力フレーム構造
