# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project Overview

libwebrtc-wrapperは、LiveKitの公式`libwebrtc` crateを使用したWHEPクライアントです。WHEPプロトコルでWebRTCストリームを受信し、MKV形式でstdoutに出力するCLIツールを提供します。

## Build Commands

```bash
# WebRTCライブラリのダウンロード（初回のみ必須）
make download              # 全プラットフォーム
make download-mac-arm64    # macOS ARM64のみ
make download-linux-x64    # Linux x64のみ

# ビルド
make build                 # デバッグビルド
make release               # リリースビルド

# テスト
make test

# チェック（コンパイルのみ）
make check

# クリーン
make clean

# Docker（Linux x64向けクロスビルド）
make docker-release-ubuntu
```

## macOS固有の設定

ビルド時に以下の環境変数が自動設定されます（Makefile参照）：
```bash
export SDKROOT=/Library/Developer/CommandLineTools/SDKs/MacOSX15.sdk
export DEVELOPER_DIR=/Library/Developer/CommandLineTools
```

## Architecture

### Crate構成

```
crates/
  whep-client/              # WHEPクライアント（MKV出力）
    src/
      main.rs               # エントリーポイント、引数パース、Ctrl+Cハンドリング
      whep.rs               # WHEP HTTP通信、PeerConnection管理、フレームコールバック
      mkv_writer.rs         # EBML/Matroska形式でVideo/Audioをストリーム出力
```

### 主要な外部依存

- `libwebrtc = "0.3"`: LiveKit公式のlibwebrtc Rustバインディング
- WebRTCライブラリ: LiveKit rust-sdks release `webrtc-0001d84-2`（`deps/webrtc/`に展開）

### データフロー
```
WHEP Endpoint -> PeerConnection -> NativeVideoStream / NativeAudioStream
                                          |
                                          v
                                    MkvWriter -> stdout (MKV stream)
```

## Key Implementation Details

### スレッドモデル
libwebrtcの生ポインタはスレッドセーフではないため、`#[tokio::main(flavor = "current_thread")]`でシングルスレッド実行を強制（`main.rs:92`）。

### MKV出力形式
- Video: V_UNCOMPRESSED (I420 YUV)
- Audio: A_PCM/INT/LIT (PCM S16LE, 48kHz stereo)
- SegmentとClusterは不明サイズエンコーディングでストリーミング出力
- Cluster切り替えは5秒間隔またはキーフレーム（`mkv_writer.rs:71`）

### WHEP実装
- Trickle ICE対応（`whep.rs:654`）
- RFC 8840準拠のend-of-candidates送信（`whep.rs:724`）
- LinkヘッダーからICEサーバー情報をパース（`whep.rs:859`）

### フレーム処理
- `NativeVideoStream`/`NativeAudioStream`でfutures::streamベースの非同期処理（`whep.rs:327`）
- Video: I420フォーマット、stride処理あり（`whep.rs:519-542`）
- Audio: PCM S16LE、サンプル数ベースのタイムスタンプ計算（`whep.rs:595-598`）

## Usage Example

```bash
# WHEPエンドポイントからストリーム受信してffplayで再生
./target/debug/whep-client https://example.com/whep | ffplay -f matroska -i -

# ファイルに保存（リダイレクト）
./target/debug/whep-client https://example.com/whep > output.mkv

# デバッグモード（詳細ログ出力）
./target/debug/whep-client -d https://example.com/whep > output.mkv
```
