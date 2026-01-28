# Repository Guidelines

## プロジェクト構成
- `crates/libwebrtc-sys/` は WebRTC の Rust FFI バインディング。`wrapper/` に C++ の ラッパー 実装、`src/lib.rs` に Rust 側 を置きます。
- `crates/whep-client/` は `whep-client` バイナリ。`src/main.rs` と 補助モジュール を ここで 管理します。
- `deps/webrtc/` は Makefile で 取得する 事前ビルド 依存。`libwebrtc/` は 既存成果物 の 配置先 です。
- `docs/` は ドキュメント、`test_output/` と `test_verify/` は MKV 出力の 参考 データ です。

## ビルド・テスト・開発コマンド
- `make download` : WebRTC ライブラリ を 取得。ネットワーク と `curl`/`unzip` が 必要です。
- `make download-mac-arm64` : macOS ARM64 向け の WebRTC ライブラリ を 取得します。
- `make download-linux-x64` : Linux x64 向け の WebRTC ライブラリ を 取得します。
- `make build` / `make release` : ワークスペース 全体 を デバッグ/リリース ビルド。
- `make test` / `make check` : 主要テスト と 型チェック を 実行。
- `make clean` : ビルド 成果物 を 削除します。
- `make docker-release-ubuntu` : Ubuntu 向け バイナリ を Docker で 生成。
- `cargo run -p whep-client -- <args>` : ローカル 実行例。必要に応じて 引数 を 渡します。

## コーディングスタイル & 命名規則
- Rust は 4 スペース インデント、`rustfmt` の 既定設定 を 想定。
- 命名は `snake_case` (関数/変数)、`CamelCase` (型)、`SCREAMING_SNAKE_CASE` (定数)。
- C++ ラッパー では 既存の `wrapper.*` と 同じ スタイル を 維持します。

## テスト方針
- 単体テスト は `#[test]` を 使用し、`crates/libwebrtc-sys/src/lib.rs` と `crates/whep-client/src/mkv_writer.rs` に あります。
- `cargo test` で ワークスペース 全体、`cargo test -p libwebrtc-sys` で 個別実行。
- メディア出力 の 確認が 必要な場合、`test_output/` と `test_verify/` を 比較します。

## コミット & PR ガイドライン
- `.git/logs/HEAD` の 履歴 では `commit: <短い説明>` 形式が 多く、`commit (initial): ...` も 見られます。
- 新規コミット は 上記形式 を 踏襲し、変更点 が 一文で 伝わる 説明 を 付けます。
- PR では 目的・変更点・動作確認 (`make test` など) を 明記し、必要なら 生成物 や ログ を 添付。

## 設定と依存の注意
- macOS ビルド は Makefile の `SDKROOT` と `DEVELOPER_DIR` を 使用。環境に合わせて 上書き可能です。
- `deps/webrtc/` は 生成物 なので、手動編集は 避けて Makefile で 更新します。
