# WHEP/WHIP クライアント設計書

## 1. 概要

本ドキュメントは、libwebrtcの静的ビルド済みライブラリを使用したWHEP/WHIPプロトコルクライアントの設計を定義する。

### 1.1 目標

- **whep-cli**: WHEPプロトコルでWebRTCストリームを受信し、デコード済みrawvideo + Opus音声をMKV形式でstdoutに出力
- **whip-cli**: stdinからMKVを読み込み、VP8にエンコードしてWHIPで送信

### 1.2 設計方針

- **UNIXパイプ哲学**: stdin/stdoutを介したデータフローにより、既存ツールとの組み合わせが容易
- **メモリ安全性**: Rustの所有権システムによりメモリリーク・ダングリングポインタを防止
- **シンプルさ**: 必要最小限の機能に絞り、メンテナンス性を重視

### 1.3 対応環境

- **初期対応**: macOS ARM64 (Apple Silicon)
- **将来対応**: Linux x86_64, Linux ARM64

### 1.4 対応コーデック

| 種別 | コーデック |
|------|-----------|
| ビデオ | VP8, VP9 |
| オーディオ | Opus |

---

## 2. 言語選択

### 2.1 Rustを選択した理由

| 観点 | 評価 |
|------|------|
| libwebrtc連携 | FFI経由で可能。bindgenでC++ヘッダから自動バインディング生成 |
| メモリ管理 | 所有権システムによる自動管理。手動管理不要で安全 |
| 実装難易度 | 中程度。FFI部分は注意が必要だが、安全なラッパーで隠蔽可能 |
| メンテナンス性 | 高い。コンパイル時エラーチェックが充実 |
| パフォーマンス | C++と同等 |
| クロスプラットフォーム | macOS/Linuxで同一コードベース |

### 2.2 代替案を採用しなかった理由

- **C++**: libwebrtcと直接連携できるが、メモリ管理の複雑さがメンテナンスコストを上げる
- **Go + CGO**: CGO境界でのメモリ管理が煩雑、パフォーマンスオーバーヘッドあり

---

## 3. アーキテクチャ

### 3.1 全体構成

```
+-------------------------------------------------------------------+
|                     whep-cli / whip-cli                            |
+-------------------------------------------------------------------+
|  +---------------+  +---------------+  +------------------------+  |
|  | WHEP/WHIP     |  | MKV           |  | libwebrtc-rs           |  |
|  | HTTP Client   |  | Writer/Reader |  | (FFI Binding)          |  |
|  +-------+-------+  +-------+-------+  +-----------+------------+  |
|          |                  |                      |               |
|          +------------------+----------------------+               |
|                             |                                      |
+-----------------------------+--------------------------------------+
                              |
+-----------------------------v--------------------------------------+
|                   libwebrtc (M144.7559.2.2)                        |
|  +---------------+ +---------------+ +---------------------------+ |
|  |PeerConnection | | VP8/VP9       | | Opus Decoder/Encoder      | |
|  |Factory        | | Decoder       | |                           | |
|  +---------------+ +---------------+ +---------------------------+ |
+--------------------------------------------------------------------+
```

### 3.2 データフロー

#### whep-cli（受信）

```
WHEP Endpoint
     |
     | HTTP POST (SDP Offer/Answer)
     v
PeerConnection
     |
     | RTP Packets
     v
+----+----+
|         |
v         v
VideoTrack  AudioTrack
|           |
| libwebrtc | libwebrtc
| Decoder   | Decoder
v           v
VideoFrame  AudioFrame
(I420)      (PCM S16LE)
|           |
v           v
MKV Writer (rawvideo + PCM)
     |
     v
stdout
```

#### whip-cli（送信）

```
stdin
     |
     v
MKV Reader
     |
     +----+----+
     |         |
     v         v
Video Block  Audio Block
(rawvideo)   (PCM/Opus)
     |         |
     | VP8     | Opus
     | Encoder | Encoder
     v         v
VideoTrack  AudioTrack
     |         |
     +----+----+
          |
          v
   PeerConnection
          |
          | RTP Packets
          v
   WHIP Endpoint
```

---

## 4. ディレクトリ構造

> **注記**: 以下は設計時点の構造です。現在の実装状況については各項目の注釈を参照してください。

```
libwebrtc-wrapper/
|-- Makefile                      # libwebrtc download
|-- Cargo.toml                    # Workspace definition
|-- deps/
|   `-- webrtc/
|       `-- macos_arm64/          # libwebrtc static library (libwebrtc.a)
|           |-- include/          # C++ headers
|           `-- lib/              # Static library
|-- docs/
|   |-- libwebrtc-packet-to-frame-flow.md
|   |-- audio-rtp-to-audio-frame-implementation-guide.md
|   |-- video-rtp-to-encoded-frame-implementation-guide.md
|   `-- whep-whip-client-design.md  # This document
`-- crates/
    |-- libwebrtc-sys/            # Low-level FFI bindings [実装済み]
    |   |-- Cargo.toml
    |   |-- build.rs              # cc crate, linker settings
    |   |-- src/
    |   |   `-- lib.rs            # Manual FFI bindings
    |   `-- wrapper/
    |       |-- wrapper.h         # C-compatible wrapper header
    |       |-- wrapper.cpp       # C-compatible wrapper impl
    |       `-- include/
    |           `-- __config_site # libc++ ABI override for __Cr namespace
    |-- libwebrtc-rs/             # Safe Rust wrapper [未実装 - 将来対応]
    |   `-- ...
    |-- whep-client/              # WHEP client [実装済み - 基本機能]
    |   |-- Cargo.toml
    |   `-- src/
    |       |-- main.rs
    |       `-- whep.rs           # WHEP HTTP client + PeerConnection
    |       # mkv_writer.rs       # [未実装 - TODO]
    `-- whip-cli/                 # WHIP client [未実装 - 将来対応]
        `-- ...
```

### 4.1 現在の実装状況

| コンポーネント | 状況 | 備考 |
|--------------|------|------|
| libwebrtc-sys | 実装済み | C++ラッパー、FFIバインディング |
| libwebrtc-rs | 未実装 | 現在はlibwebrtc-sysを直接使用 |
| whep-client | 基本機能済み | WHEP接続、ICE確立 |
| MKV Writer | 未実装 | フレーム取得から実装予定 |
| whip-cli | 未実装 | whep-cli完成後に着手予定 |

---

## 5. コンポーネント詳細設計

### 5.1 libwebrtc-sys（低レベルFFIバインディング）

#### 5.1.1 役割

libwebrtcのC++ APIをCインターフェース経由でRustから呼び出すための最低限のバインディングを提供する。

#### 5.1.2 C++ラッパー（wrapper.h / wrapper.cpp）

libwebrtcはC++ APIのため、C互換のラッパー関数を作成する。

```cpp
// wrapper.h
#ifndef LIBWEBRTC_WRAPPER_H
#define LIBWEBRTC_WRAPPER_H

#include <stdint.h>
#include <stddef.h>

#ifdef __cplusplus
extern "C" {
#endif

// Opaque handles
typedef struct WebrtcPeerConnectionFactory WebrtcPeerConnectionFactory;
typedef struct WebrtcPeerConnection WebrtcPeerConnection;
typedef struct WebrtcVideoTrack WebrtcVideoTrack;
typedef struct WebrtcAudioTrack WebrtcAudioTrack;

// Callbacks
typedef void (*VideoFrameCallback)(
    void* user_data,
    int32_t width,
    int32_t height,
    int64_t timestamp_us,
    const uint8_t* y_data, int32_t y_stride,
    const uint8_t* u_data, int32_t u_stride,
    const uint8_t* v_data, int32_t v_stride
);

typedef void (*AudioFrameCallback)(
    void* user_data,
    int32_t sample_rate,
    size_t num_channels,
    size_t samples_per_channel,
    const int16_t* data
);

typedef void (*OnTrackCallback)(
    void* user_data,
    const char* track_id,
    int is_video  // 1 = video, 0 = audio
);

// Factory
WebrtcPeerConnectionFactory* webrtc_factory_create(void);
void webrtc_factory_destroy(WebrtcPeerConnectionFactory* factory);

// PeerConnection
WebrtcPeerConnection* webrtc_pc_create(
    WebrtcPeerConnectionFactory* factory,
    const char* ice_servers_json
);
void webrtc_pc_destroy(WebrtcPeerConnection* pc);

char* webrtc_pc_create_offer(WebrtcPeerConnection* pc);
char* webrtc_pc_create_answer(WebrtcPeerConnection* pc);
int webrtc_pc_set_local_description(WebrtcPeerConnection* pc, const char* sdp, const char* type);
int webrtc_pc_set_remote_description(WebrtcPeerConnection* pc, const char* sdp, const char* type);

void webrtc_pc_set_on_track_callback(
    WebrtcPeerConnection* pc,
    OnTrackCallback callback,
    void* user_data
);

// Video Track
void webrtc_video_track_set_frame_callback(
    WebrtcVideoTrack* track,
    VideoFrameCallback callback,
    void* user_data
);

// Audio Track
void webrtc_audio_track_set_frame_callback(
    WebrtcAudioTrack* track,
    AudioFrameCallback callback,
    void* user_data
);

// Utility
void webrtc_free_string(char* str);

#ifdef __cplusplus
}
#endif

#endif // LIBWEBRTC_WRAPPER_H
```

#### 5.1.3 build.rs

> **注記**: 以下は実際の実装を反映した内容です。静的ライブラリ(libwebrtc.a)を使用し、
> libc++ ABIの互換性問題（`std::__Cr::` vs `std::__1::`）を解決するために
> カスタム`__config_site`を使用しています。

```rust
// crates/libwebrtc-sys/build.rs
use std::env;
use std::path::PathBuf;

fn main() {
    let manifest_dir = env::var("CARGO_MANIFEST_DIR").unwrap();
    let manifest_path = PathBuf::from(&manifest_dir);
    let webrtc_dir = manifest_path
        .join("../../deps/webrtc/macos_arm64")
        .canonicalize()
        .expect("WebRTC directory not found");

    let include_dir = webrtc_dir.join("include");
    let lib_dir = webrtc_dir.join("lib");

    // Custom include directory with __config_site override
    let custom_include = manifest_path.join("wrapper/include");

    // Compile wrapper.cpp
    // libwebrtc.a uses libc++ with __Cr namespace (Chromium ABI).
    // Our custom __config_site must come FIRST to override the system one.
    cc::Build::new()
        .cpp(true)
        .file("wrapper/wrapper.cpp")
        .flag(&format!("-isystem{}", custom_include.display()))
        .include(&include_dir)
        .include(include_dir.join("third_party/abseil-cpp"))
        .flag("-std=c++17")
        .flag("-stdlib=libc++")
        .flag("-fno-rtti")
        .define("WEBRTC_MAC", None)
        .define("WEBRTC_POSIX", None)
        .compile("webrtc_wrapper");

    // Link libwebrtc.a static library
    println!("cargo:rustc-link-search=native={}", lib_dir.display());
    println!("cargo:rustc-link-lib=static=webrtc");

    // System frameworks required by WebRTC
    println!("cargo:rustc-link-lib=framework=Foundation");
    println!("cargo:rustc-link-lib=framework=CoreFoundation");
    println!("cargo:rustc-link-lib=framework=CoreMedia");
    println!("cargo:rustc-link-lib=framework=CoreVideo");
    println!("cargo:rustc-link-lib=framework=CoreAudio");
    println!("cargo:rustc-link-lib=framework=AudioToolbox");
    println!("cargo:rustc-link-lib=framework=VideoToolbox");
    // ... additional frameworks ...
    println!("cargo:rustc-link-lib=c++");
}
```

**libc++ ABI互換性の解決**:
libwebrtc.aはChromiumのlibc++でビルドされており、`std::__Cr::`名前空間を使用します。
システムのlibc++は`std::__1::`を使用するため、リンク時に未解決シンボルエラーが発生します。
この問題は`wrapper/include/__config_site`でABI名前空間を上書きすることで解決しています。

### 5.2 libwebrtc-rs（安全なRustラッパー）

#### 5.2.1 役割

libwebrtc-sysの低レベルFFIを安全なRust APIでラップし、メモリ安全性を保証する。

#### 5.2.2 主要構造体

```rust
// crates/libwebrtc-rs/src/video_frame.rs

/// デコード済みビデオフレーム（I420形式）
pub struct VideoFrame {
    pub width: u32,
    pub height: u32,
    pub timestamp_us: i64,
    pub y_plane: Vec<u8>,
    pub u_plane: Vec<u8>,
    pub v_plane: Vec<u8>,
    pub y_stride: u32,
    pub u_stride: u32,
    pub v_stride: u32,
}

impl VideoFrame {
    /// I420からRGB24に変換
    pub fn to_rgb24(&self) -> Vec<u8> {
        let mut rgb = vec![0u8; (self.width * self.height * 3) as usize];
        // YUV to RGB conversion
        // ...
        rgb
    }

    /// I420からBGRA32に変換
    pub fn to_bgra32(&self) -> Vec<u8> {
        let mut bgra = vec![0u8; (self.width * self.height * 4) as usize];
        // YUV to BGRA conversion
        // ...
        bgra
    }

    /// rawvideo形式（I420プレーナー）のバイト列を取得
    pub fn to_rawvideo(&self) -> Vec<u8> {
        let y_size = (self.y_stride * self.height) as usize;
        let uv_height = (self.height + 1) / 2;
        let u_size = (self.u_stride * uv_height) as usize;
        let v_size = (self.v_stride * uv_height) as usize;

        let mut raw = Vec::with_capacity(y_size + u_size + v_size);
        raw.extend_from_slice(&self.y_plane[..y_size]);
        raw.extend_from_slice(&self.u_plane[..u_size]);
        raw.extend_from_slice(&self.v_plane[..v_size]);
        raw
    }
}
```

```rust
// crates/libwebrtc-rs/src/audio_frame.rs

/// デコード済みオーディオフレーム（PCM S16LE）
pub struct AudioFrame {
    pub sample_rate: u32,
    pub num_channels: u32,
    pub samples_per_channel: usize,
    pub data: Vec<i16>,
}

impl AudioFrame {
    /// PCMデータをバイト列として取得（S16LE、インターリーブ）
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut bytes = Vec::with_capacity(self.data.len() * 2);
        for sample in &self.data {
            bytes.extend_from_slice(&sample.to_le_bytes());
        }
        bytes
    }
}
```

```rust
// crates/libwebrtc-rs/src/peer_connection.rs

use std::sync::Arc;
use crate::error::Error;

pub struct PeerConnectionConfig {
    pub ice_servers: Vec<IceServer>,
}

pub struct IceServer {
    pub urls: Vec<String>,
    pub username: Option<String>,
    pub credential: Option<String>,
}

pub struct PeerConnection {
    inner: *mut libwebrtc_sys::WebrtcPeerConnection,
    _prevent_send_sync: std::marker::PhantomData<*const ()>,
}

impl PeerConnection {
    pub fn new(
        factory: &PeerConnectionFactory,
        config: PeerConnectionConfig,
    ) -> Result<Self, Error> {
        // ...
    }

    pub fn create_offer(&self) -> Result<SessionDescription, Error> {
        // ...
    }

    pub fn create_answer(&self) -> Result<SessionDescription, Error> {
        // ...
    }

    pub fn set_local_description(&self, sdp: &SessionDescription) -> Result<(), Error> {
        // ...
    }

    pub fn set_remote_description(&self, sdp: &SessionDescription) -> Result<(), Error> {
        // ...
    }

    pub fn on_track<F>(&self, callback: F)
    where
        F: Fn(Track) + Send + 'static,
    {
        // ...
    }
}

impl Drop for PeerConnection {
    fn drop(&mut self) {
        unsafe {
            libwebrtc_sys::webrtc_pc_destroy(self.inner);
        }
    }
}

pub struct SessionDescription {
    pub sdp: String,
    pub sdp_type: SdpType,
}

pub enum SdpType {
    Offer,
    Answer,
}

pub enum Track {
    Video(VideoTrack),
    Audio(AudioTrack),
}
```

### 5.3 whep-cli

#### 5.3.1 WHEP HTTPクライアント

```rust
// crates/whep-cli/src/whep.rs

use anyhow::Result;
use reqwest::Client;

pub struct WhepClient {
    endpoint: String,
    auth_token: Option<String>,
    client: Client,
}

pub struct WhepSession {
    pub answer_sdp: String,
    pub resource_url: Option<String>,
}

impl WhepClient {
    pub fn new(endpoint: &str, auth_token: Option<String>) -> Self {
        Self {
            endpoint: endpoint.to_string(),
            auth_token,
            client: Client::new(),
        }
    }

    /// WHEPリソースを作成（SDP Offer送信、Answer受信）
    pub async fn connect(&self, offer_sdp: &str) -> Result<WhepSession> {
        let mut request = self.client
            .post(&self.endpoint)
            .header("Content-Type", "application/sdp")
            .header("Accept", "application/sdp")
            .body(offer_sdp.to_string());

        if let Some(ref token) = self.auth_token {
            request = request.header("Authorization", format!("Bearer {}", token));
        }

        let response = request.send().await?;

        if !response.status().is_success() {
            anyhow::bail!(
                "WHEP request failed: {} {}",
                response.status(),
                response.text().await.unwrap_or_default()
            );
        }

        let resource_url = response
            .headers()
            .get("Location")
            .and_then(|v| v.to_str().ok())
            .map(|s| s.to_string());

        let answer_sdp = response.text().await?;

        Ok(WhepSession {
            answer_sdp,
            resource_url,
        })
    }

    /// WHEPセッションを終了
    pub async fn disconnect(&self, resource_url: &str) -> Result<()> {
        let mut request = self.client.delete(resource_url);

        if let Some(ref token) = self.auth_token {
            request = request.header("Authorization", format!("Bearer {}", token));
        }

        request.send().await?;
        Ok(())
    }
}
```

#### 5.3.2 MKV Writer

MKVフォーマットでrawvideo（V_UNCOMPRESSED）とPCM音声（A_PCM/INT/LIT）を出力する。

**根拠**: Matroska仕様 - https://www.matroska.org/technical/elements.html

```rust
// crates/whep-cli/src/mkv_writer.rs

use std::io::Write;
use anyhow::Result;

/// MKVトラック設定
pub struct MkvConfig {
    pub video_width: u32,
    pub video_height: u32,
    pub video_fps: f64,
    pub audio_sample_rate: u32,
    pub audio_channels: u32,
}

/// MKVストリームライター
pub struct MkvWriter<W: Write> {
    writer: W,
    config: MkvConfig,
    video_track_num: u8,
    audio_track_num: u8,
    timecode_scale: u64,  // nanoseconds per tick (default: 1_000_000 = 1ms)
    cluster_start_time: Option<i64>,
    cluster_position: Option<u64>,
}

impl<W: Write> MkvWriter<W> {
    pub fn new(writer: W, config: MkvConfig) -> Result<Self> {
        let mut mkv = Self {
            writer,
            config,
            video_track_num: 1,
            audio_track_num: 2,
            timecode_scale: 1_000_000,
            cluster_start_time: None,
            cluster_position: None,
        };

        mkv.write_ebml_header()?;
        mkv.write_segment_header()?;
        mkv.write_segment_info()?;
        mkv.write_tracks()?;

        Ok(mkv)
    }

    /// EBMLヘッダを書き込み
    fn write_ebml_header(&mut self) -> Result<()> {
        // EBML Header
        // DocType: "matroska"
        // DocTypeVersion: 4
        // DocTypeReadVersion: 2
        // ...
        Ok(())
    }

    /// Segmentヘッダを書き込み（サイズ未知）
    fn write_segment_header(&mut self) -> Result<()> {
        // Segment ID: 0x18538067
        // Size: unknown (0x01FFFFFFFFFFFFFF)
        // ...
        Ok(())
    }

    /// SegmentInfoを書き込み
    fn write_segment_info(&mut self) -> Result<()> {
        // TimecodeScale: 1000000 (1ms)
        // MuxingApp: "whep-cli"
        // WritingApp: "whep-cli"
        // ...
        Ok(())
    }

    /// Tracksを書き込み
    fn write_tracks(&mut self) -> Result<()> {
        // Track 1: Video
        //   TrackType: 1 (video)
        //   CodecID: "V_UNCOMPRESSED"
        //   Video:
        //     PixelWidth: config.video_width
        //     PixelHeight: config.video_height
        //     ColourSpace: "I420" (FourCC)
        //
        // Track 2: Audio
        //   TrackType: 2 (audio)
        //   CodecID: "A_PCM/INT/LIT"
        //   Audio:
        //     SamplingFrequency: config.audio_sample_rate
        //     Channels: config.audio_channels
        //     BitDepth: 16
        // ...
        Ok(())
    }

    /// ビデオフレームを書き込み
    pub fn write_video_frame(
        &mut self,
        frame: &[u8],
        timestamp_ms: i64,
        is_keyframe: bool,
    ) -> Result<()> {
        self.ensure_cluster(timestamp_ms, is_keyframe)?;
        self.write_simple_block(
            self.video_track_num,
            timestamp_ms,
            frame,
            is_keyframe,
        )?;
        Ok(())
    }

    /// オーディオフレームを書き込み
    pub fn write_audio_frame(
        &mut self,
        frame: &[u8],
        timestamp_ms: i64,
    ) -> Result<()> {
        self.ensure_cluster(timestamp_ms, false)?;
        self.write_simple_block(
            self.audio_track_num,
            timestamp_ms,
            frame,
            true,  // Audio is always keyframe
        )?;
        Ok(())
    }

    /// 必要に応じて新しいClusterを開始
    fn ensure_cluster(&mut self, timestamp_ms: i64, is_video_keyframe: bool) -> Result<()> {
        let should_start = match self.cluster_start_time {
            None => true,
            Some(start) => {
                // 5秒経過、またはビデオキーフレーム
                (timestamp_ms - start) >= 5000 || is_video_keyframe
            }
        };

        if should_start {
            self.end_cluster()?;
            self.start_cluster(timestamp_ms)?;
        }
        Ok(())
    }

    fn start_cluster(&mut self, timestamp_ms: i64) -> Result<()> {
        // Cluster ID: 0x1F43B675
        // Timecode: timestamp_ms
        self.cluster_start_time = Some(timestamp_ms);
        // ...
        Ok(())
    }

    fn end_cluster(&mut self) -> Result<()> {
        // Close previous cluster if exists
        // ...
        Ok(())
    }

    fn write_simple_block(
        &mut self,
        track_num: u8,
        timestamp_ms: i64,
        data: &[u8],
        is_keyframe: bool,
    ) -> Result<()> {
        // SimpleBlock ID: 0xA3
        // Track number (EBML coded)
        // Timecode (relative to cluster, 2 bytes signed)
        // Flags: keyframe, invisible, lacing
        // Data
        // ...
        Ok(())
    }
}
```

#### 5.3.3 メインパイプライン

```rust
// crates/whep-cli/src/main.rs

use anyhow::Result;
use clap::Parser;
use std::io::{self, Write};
use std::sync::mpsc;
use tracing::{info, debug};

use libwebrtc_rs::{
    PeerConnection, PeerConnectionConfig, PeerConnectionFactory,
    VideoFrame, AudioFrame, Track,
};

mod whep;
mod mkv_writer;
mod pipeline;

use whep::WhepClient;
use mkv_writer::{MkvWriter, MkvConfig};

#[derive(Parser)]
#[command(name = "whep-cli")]
#[command(about = "WHEP client - receive WebRTC stream and output as MKV")]
struct Args {
    /// WHEP endpoint URL
    url: String,

    /// Enable debug output (SDP, etc.)
    #[arg(short, long)]
    debug: bool,

    /// Bearer token for authentication
    #[arg(short, long)]
    token: Option<String>,

    /// Preferred video codec (vp8, vp9)
    #[arg(short, long, default_value = "vp8")]
    codec: String,
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();

    // Setup logging
    if args.debug {
        tracing_subscriber::fmt()
            .with_env_filter("debug")
            .with_writer(io::stderr)
            .init();
    } else {
        tracing_subscriber::fmt()
            .with_env_filter("warn")
            .with_writer(io::stderr)
            .init();
    }

    // Create PeerConnection
    let factory = PeerConnectionFactory::new()?;
    let pc = PeerConnection::new(&factory, PeerConnectionConfig {
        ice_servers: vec![],
    })?;

    // Setup media channels
    let (video_tx, video_rx) = mpsc::channel::<VideoFrame>();
    let (audio_tx, audio_rx) = mpsc::channel::<AudioFrame>();

    // Set track callback
    pc.on_track(move |track| {
        match track {
            Track::Video(video_track) => {
                let tx = video_tx.clone();
                video_track.on_frame(move |frame| {
                    let _ = tx.send(frame);
                });
            }
            Track::Audio(audio_track) => {
                let tx = audio_tx.clone();
                audio_track.on_frame(move |frame| {
                    let _ = tx.send(frame);
                });
            }
        }
    });

    // Create SDP offer (recvonly)
    let offer = pc.create_offer()?;
    debug!("SDP Offer:\n{}", offer.sdp);

    // WHEP signaling
    let client = WhepClient::new(&args.url, args.token);
    let session = client.connect(&offer.sdp).await?;
    debug!("SDP Answer:\n{}", session.answer_sdp);

    // Set remote description
    pc.set_remote_description(&libwebrtc_rs::SessionDescription {
        sdp: session.answer_sdp,
        sdp_type: libwebrtc_rs::SdpType::Answer,
    })?;

    // Wait for first frame to determine resolution
    info!("Waiting for first video frame...");
    let first_frame = video_rx.recv()?;

    // Create MKV writer
    let config = MkvConfig {
        video_width: first_frame.width,
        video_height: first_frame.height,
        video_fps: 30.0,  // Will be determined from timestamps
        audio_sample_rate: 48000,
        audio_channels: 2,
    };

    let stdout = io::stdout().lock();
    let mut mkv = MkvWriter::new(stdout, config)?;

    // Process first frame
    let rawvideo = first_frame.to_rawvideo();
    let timestamp_ms = first_frame.timestamp_us / 1000;
    mkv.write_video_frame(&rawvideo, timestamp_ms, true)?;

    // Main processing loop
    loop {
        // Non-blocking receive from both channels
        if let Ok(frame) = video_rx.try_recv() {
            let rawvideo = frame.to_rawvideo();
            let timestamp_ms = frame.timestamp_us / 1000;
            // Note: keyframe detection should be based on codec info
            mkv.write_video_frame(&rawvideo, timestamp_ms, false)?;
        }

        if let Ok(frame) = audio_rx.try_recv() {
            let pcm = frame.to_bytes();
            // Audio timestamp calculation
            let timestamp_ms = 0; // Calculate from sample count
            mkv.write_audio_frame(&pcm, timestamp_ms)?;
        }

        // Small sleep to prevent busy loop
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
}
```

### 5.4 whip-cli

#### 5.4.1 WHIP HTTPクライアント

```rust
// crates/whip-cli/src/whip.rs

use anyhow::Result;
use reqwest::Client;

pub struct WhipClient {
    endpoint: String,
    auth_token: Option<String>,
    client: Client,
}

pub struct WhipSession {
    pub answer_sdp: String,
    pub resource_url: Option<String>,
}

impl WhipClient {
    pub fn new(endpoint: &str, auth_token: Option<String>) -> Self {
        Self {
            endpoint: endpoint.to_string(),
            auth_token,
            client: Client::new(),
        }
    }

    /// WHIPリソースを作成（SDP Offer送信、Answer受信）
    pub async fn publish(&self, offer_sdp: &str) -> Result<WhipSession> {
        let mut request = self.client
            .post(&self.endpoint)
            .header("Content-Type", "application/sdp")
            .header("Accept", "application/sdp")
            .body(offer_sdp.to_string());

        if let Some(ref token) = self.auth_token {
            request = request.header("Authorization", format!("Bearer {}", token));
        }

        let response = request.send().await?;

        if !response.status().is_success() {
            anyhow::bail!(
                "WHIP request failed: {} {}",
                response.status(),
                response.text().await.unwrap_or_default()
            );
        }

        let resource_url = response
            .headers()
            .get("Location")
            .and_then(|v| v.to_str().ok())
            .map(|s| s.to_string());

        let answer_sdp = response.text().await?;

        Ok(WhipSession {
            answer_sdp,
            resource_url,
        })
    }

    /// WHIPセッションを終了
    pub async fn unpublish(&self, resource_url: &str) -> Result<()> {
        let mut request = self.client.delete(resource_url);

        if let Some(ref token) = self.auth_token {
            request = request.header("Authorization", format!("Bearer {}", token));
        }

        request.send().await?;
        Ok(())
    }
}
```

#### 5.4.2 MKV Reader

```rust
// crates/whip-cli/src/mkv_reader.rs

use std::io::Read;
use anyhow::Result;

pub enum MkvBlock {
    Video {
        data: Vec<u8>,
        timestamp_ms: i64,
        is_keyframe: bool,
        width: u32,
        height: u32,
    },
    Audio {
        data: Vec<u8>,
        timestamp_ms: i64,
        sample_rate: u32,
        channels: u32,
    },
}

pub struct MkvReader<R: Read> {
    reader: R,
    video_track_num: Option<u8>,
    audio_track_num: Option<u8>,
    video_width: u32,
    video_height: u32,
    audio_sample_rate: u32,
    audio_channels: u32,
}

impl<R: Read> MkvReader<R> {
    pub fn new(reader: R) -> Result<Self> {
        let mut mkv = Self {
            reader,
            video_track_num: None,
            audio_track_num: None,
            video_width: 0,
            video_height: 0,
            audio_sample_rate: 0,
            audio_channels: 0,
        };

        mkv.parse_header()?;
        Ok(mkv)
    }

    fn parse_header(&mut self) -> Result<()> {
        // Parse EBML header
        // Parse Segment header
        // Parse Tracks
        // ...
        Ok(())
    }

    /// 次のブロックを読み込み
    pub fn read_block(&mut self) -> Result<Option<MkvBlock>> {
        // Parse next SimpleBlock or BlockGroup
        // ...
        Ok(None)
    }

    pub fn video_info(&self) -> (u32, u32) {
        (self.video_width, self.video_height)
    }

    pub fn audio_info(&self) -> (u32, u32) {
        (self.audio_sample_rate, self.audio_channels)
    }
}
```

#### 5.4.3 メインパイプライン

```rust
// crates/whip-cli/src/main.rs

use anyhow::Result;
use clap::Parser;
use std::io::{self, Read};
use tracing::{info, debug};

use libwebrtc_rs::{
    PeerConnection, PeerConnectionConfig, PeerConnectionFactory,
    VideoEncoder, VideoEncoderConfig,
};

mod whip;
mod mkv_reader;
mod pipeline;

use whip::WhipClient;
use mkv_reader::{MkvReader, MkvBlock};

#[derive(Parser)]
#[command(name = "whip-cli")]
#[command(about = "WHIP client - read MKV and send via WebRTC")]
struct Args {
    /// WHIP endpoint URL
    url: String,

    /// Enable debug output (SDP, etc.)
    #[arg(short, long)]
    debug: bool,

    /// Bearer token for authentication
    #[arg(short, long)]
    token: Option<String>,
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();

    // Setup logging
    if args.debug {
        tracing_subscriber::fmt()
            .with_env_filter("debug")
            .with_writer(io::stderr)
            .init();
    } else {
        tracing_subscriber::fmt()
            .with_env_filter("warn")
            .with_writer(io::stderr)
            .init();
    }

    // Create MKV reader from stdin
    let stdin = io::stdin().lock();
    let mut mkv = MkvReader::new(stdin)?;

    let (video_width, video_height) = mkv.video_info();
    let (audio_sample_rate, audio_channels) = mkv.audio_info();

    info!(
        "Input: {}x{} video, {}Hz {}ch audio",
        video_width, video_height, audio_sample_rate, audio_channels
    );

    // Create PeerConnection
    let factory = PeerConnectionFactory::new()?;
    let pc = PeerConnection::new(&factory, PeerConnectionConfig {
        ice_servers: vec![],
    })?;

    // Create VP8 encoder
    let encoder = VideoEncoder::new(VideoEncoderConfig {
        codec: "VP8",
        width: video_width,
        height: video_height,
        fps: 30,
        bitrate_kbps: 2000,
    })?;

    // Add tracks
    let video_track = pc.add_video_track("video0")?;
    let audio_track = pc.add_audio_track("audio0")?;

    // Create SDP offer (sendonly)
    let offer = pc.create_offer()?;
    debug!("SDP Offer:\n{}", offer.sdp);

    // WHIP signaling
    let client = WhipClient::new(&args.url, args.token);
    let session = client.publish(&offer.sdp).await?;
    debug!("SDP Answer:\n{}", session.answer_sdp);

    // Set remote description
    pc.set_remote_description(&libwebrtc_rs::SessionDescription {
        sdp: session.answer_sdp,
        sdp_type: libwebrtc_rs::SdpType::Answer,
    })?;

    // Main processing loop
    while let Some(block) = mkv.read_block()? {
        match block {
            MkvBlock::Video { data, timestamp_ms, width, height, .. } => {
                // Encode rawvideo to VP8
                let encoded = encoder.encode(&data, timestamp_ms)?;

                // Send via WebRTC
                video_track.send_frame(&encoded, timestamp_ms)?;
            }
            MkvBlock::Audio { data, timestamp_ms, .. } => {
                // Send audio (Opus encode if needed)
                audio_track.send_frame(&data, timestamp_ms)?;
            }
        }
    }

    // Cleanup
    if let Some(ref url) = session.resource_url {
        client.unpublish(url).await?;
    }

    Ok(())
}
```

---

## 6. 依存クレート

### 6.1 Cargo.toml（ワークスペース）

```toml
[workspace]
resolver = "2"
members = [
    "crates/libwebrtc-sys",
    "crates/libwebrtc-rs",
    "crates/whep-cli",
    "crates/whip-cli",
]

[workspace.package]
version = "0.1.0"
edition = "2021"
license = "MIT"
repository = "https://github.com/Azunyan1111/libwebrtc-wrapper"

[workspace.dependencies]
# Async runtime
tokio = { version = "1", features = ["full"] }

# HTTP client
reqwest = { version = "0.12", features = ["json"] }

# Error handling
anyhow = "1"
thiserror = "1"

# Logging
tracing = "0.1"
tracing-subscriber = { version = "0.3", features = ["env-filter"] }

# CLI
clap = { version = "4", features = ["derive"] }

# FFI
bindgen = "0.69"
cc = "1"
```

### 6.2 libwebrtc-sys/Cargo.toml

```toml
[package]
name = "libwebrtc-sys"
version.workspace = true
edition.workspace = true

[build-dependencies]
bindgen = { workspace = true }
cc = { workspace = true }

[dependencies]
```

### 6.3 libwebrtc-rs/Cargo.toml

```toml
[package]
name = "libwebrtc-rs"
version.workspace = true
edition.workspace = true

[dependencies]
libwebrtc-sys = { path = "../libwebrtc-sys" }
thiserror = { workspace = true }
tracing = { workspace = true }
```

### 6.4 whep-cli/Cargo.toml

```toml
[package]
name = "whep-cli"
version.workspace = true
edition.workspace = true

[[bin]]
name = "whep-cli"
path = "src/main.rs"

[dependencies]
libwebrtc-rs = { path = "../libwebrtc-rs" }
tokio = { workspace = true }
reqwest = { workspace = true }
anyhow = { workspace = true }
clap = { workspace = true }
tracing = { workspace = true }
tracing-subscriber = { workspace = true }
```

### 6.5 whip-cli/Cargo.toml

```toml
[package]
name = "whip-cli"
version.workspace = true
edition.workspace = true

[[bin]]
name = "whip-cli"
path = "src/main.rs"

[dependencies]
libwebrtc-rs = { path = "../libwebrtc-rs" }
tokio = { workspace = true }
reqwest = { workspace = true }
anyhow = { workspace = true }
clap = { workspace = true }
tracing = { workspace = true }
tracing-subscriber = { workspace = true }
```

---

## 7. CLIインターフェース

### 7.1 whep-cli

```
USAGE:
    whep-cli [OPTIONS] <URL>

ARGS:
    <URL>    WHEP endpoint URL

OPTIONS:
    -d, --debug              Enable debug output (SDP, etc.)
    -t, --token <TOKEN>      Bearer token for authentication
    -c, --codec <CODEC>      Preferred video codec (vp8, vp9) [default: vp8]
    -h, --help               Print help information
    -V, --version            Print version information

EXAMPLES:
    # Receive and play with ffplay
    whep-cli https://example.com/whep | ffplay -f matroska -i -

    # Receive and save to file
    whep-cli https://example.com/whep > output.mkv

    # Debug mode
    whep-cli -d https://example.com/whep | ffplay -f matroska -i -
```

### 7.2 whip-cli

```
USAGE:
    whip-cli [OPTIONS] <URL>

ARGS:
    <URL>    WHIP endpoint URL

OPTIONS:
    -d, --debug              Enable debug output (SDP, etc.)
    -t, --token <TOKEN>      Bearer token for authentication
    -h, --help               Print help information
    -V, --version            Print version information

EXAMPLES:
    # Send from file
    cat input.mkv | whip-cli https://example.com/whip

    # Relay stream (WHEP -> WHIP)
    whep-cli https://source/whep | whip-cli https://dest/whip

    # Send from ffmpeg
    ffmpeg -i input.mp4 -c:v rawvideo -pix_fmt yuv420p -c:a pcm_s16le -f matroska - | \
        whip-cli https://example.com/whip
```

---

## 8. 実装フェーズ

### Phase 1: libwebrtc-sys（FFI基盤）
1. C++ラッパー作成（wrapper.h, wrapper.cpp）
2. build.rs設定（コンパイル、リンク）
3. bindgenによるRustバインディング生成
4. 基本的な動作確認

### Phase 2: libwebrtc-rs（安全ラッパー）
1. Error型の定義
2. PeerConnectionFactory, PeerConnectionのラップ
3. VideoTrack, AudioTrackのラップ
4. VideoFrame, AudioFrame構造体
5. コールバック機構の実装
6. ユニットテスト

### Phase 3: whep-cli（受信側）
1. WHEP HTTPクライアント
2. MKV Writerの実装（V_UNCOMPRESSED, A_PCM/INT/LIT）
3. メインパイプラインの統合
4. 動作確認（ffplayとの連携）

### Phase 4: whip-cli（送信側）
1. WHIP HTTPクライアント
2. MKV Readerの実装
3. VP8エンコーダー統合
4. メインパイプラインの統合
5. 動作確認（リレーテスト）

---

## 9. 技術的考慮事項

### 9.1 libwebrtcリンク方法（macOS）

macOSでは`WebRTC.xcframework`を使用する。

```
deps/webrtc/macos_arm64/
  Frameworks/
    WebRTC.xcframework/
      macos-arm64/
        WebRTC.framework/
          WebRTC        <- バイナリ
          Headers/      <- Objective-Cヘッダ
  include/              <- C++ヘッダ
```

リンク時に必要なシステムフレームワーク:
- CoreFoundation
- CoreMedia
- CoreVideo
- CoreAudio
- AudioToolbox
- VideoToolbox
- Foundation

### 9.2 スレッドモデル

libwebrtcは内部で複数スレッドを使用する:

| スレッド | 役割 |
|---------|------|
| Signaling Thread | SDP操作、PeerConnection API呼び出し |
| Worker Thread | メディア処理 |
| Network Thread | RTP/RTCP送受信 |

Rust側ではこれらを意識し、適切な同期機構を使用する。コールバックは異なるスレッドから呼び出される可能性があるため、`Send + 'static`境界が必要。

### 9.3 メモリ管理

libwebrtcのオブジェクトは参照カウント（`rtc::scoped_refptr`）で管理されている。C++ラッパーで参照カウントを適切に管理し、Rustの`Drop`トレイトで解放する。

```cpp
// wrapper.cpp
void webrtc_pc_destroy(WebrtcPeerConnection* pc) {
    // Release reference
    pc->Release();
}
```

```rust
// peer_connection.rs
impl Drop for PeerConnection {
    fn drop(&mut self) {
        unsafe {
            libwebrtc_sys::webrtc_pc_destroy(self.inner);
        }
    }
}
```

### 9.4 MKV rawvideo形式

MKVでrawvideoを格納する際の仕様:

| 項目 | 値 |
|------|-----|
| CodecID | V_UNCOMPRESSED |
| ColourSpace | I420 (FourCC: 0x30323449) |
| PixelWidth | フレーム幅 |
| PixelHeight | フレーム高さ |

I420フォーマット:
- Y平面: width x height bytes
- U平面: ((width + 1) / 2) x ((height + 1) / 2) bytes
- V平面: ((width + 1) / 2) x ((height + 1) / 2) bytes
- 合計: width x height + 2 * ((width + 1) / 2) * ((height + 1) / 2) bytes per frame

**帯域幅計算例（1080p30fps）:**
- 1920 x 1080 x 1.5 = 3,110,400 bytes/frame
- 3,110,400 x 30 = 93,312,000 bytes/sec = 約 89 MB/sec = 約 711 Mbps

### 9.5 エラーハンドリング

すべてのエラーは`thiserror`で定義した型を使用し、`anyhow`で伝播する。

```rust
// libwebrtc-rs/src/error.rs
use thiserror::Error;

#[derive(Error, Debug)]
pub enum Error {
    #[error("Failed to create PeerConnection: {0}")]
    PeerConnectionCreation(String),

    #[error("Failed to create SDP: {0}")]
    SdpCreation(String),

    #[error("Failed to set SDP: {0}")]
    SdpSet(String),

    #[error("FFI error: {0}")]
    Ffi(String),
}
```

---

## 10. テスト計画

### 10.1 ユニットテスト

- libwebrtc-rs: 各構造体の変換処理
- MKV Writer/Reader: EBML要素の読み書き

### 10.2 統合テスト

1. **ローカルループバック**
   - whep-cli -> whip-cli でローカル中継
   - フレームの整合性確認

2. **ffplayとの連携**
   ```bash
   whep-cli https://example.com/whep | ffplay -f matroska -i -
   ```

3. **ffmpegとの連携**
   ```bash
   ffmpeg -i input.mp4 -c:v rawvideo -pix_fmt yuv420p -c:a pcm_s16le -f matroska - | \
       whip-cli https://example.com/whip
   ```

---

## 11. 参考資料

### 11.1 仕様書

- [WHEP Draft](https://www.ietf.org/archive/id/draft-ietf-wish-whep-01.html)
- [WHIP Draft](https://www.ietf.org/archive/id/draft-ietf-wish-whip-01.html)
- [Matroska Specification](https://www.matroska.org/technical/elements.html)
- [RFC 7741 - RTP Payload Format for VP8](https://tools.ietf.org/html/rfc7741)

### 11.2 既存ドキュメント

- `docs/libwebrtc-packet-to-frame-flow.md` - パケット処理フロー
- `docs/audio-rtp-to-audio-frame-implementation-guide.md` - 音声処理ガイド
- `docs/video-rtp-to-encoded-frame-implementation-guide.md` - 映像処理ガイド

### 11.3 libwebrtcバージョン

- Version: M144.7559.2.2
- Source: https://github.com/shiguredo-webrtc-build/webrtc-build
