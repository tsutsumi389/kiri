# kiri 実装計画

最終更新: 2026-09-05

設計の詳細と決定根拠は [design.md](./design.md) を参照。本書は実装の進め方のみを扱う。

## 1. モジュール構成

`lib.rs` と `main.rs` を分離する。統合テストから `use kiri::...` できるようにするためで、追加コストはほぼない。

```
src/
  main.rs              エントリ、サブコマンドのディスパッチ
  lib.rs               公開API（テストから利用）
  cli.rs               clap derive による引数定義
  error.rs             エラー型 → exit code のマッピング
  report.rs            JSON 出力の構造体（serde）
  image_io/
    load.rs            JPEG/PNG 読み込み + EXIF Orientation 正規化
    save.rs            AVIF/PNG/JPEG 書き出し、拡張子からの形式推論
  color/
    lab.rs             sRGB ↔ Lab 変換、知覚的色距離（ΔE）
  cutout/
    background.rs      背景色推定、uniformity 算出
    floodfill.rs       外周シードの連結フラッドフィル
    morphology.rs      オープニング / クロージング
    feather.rs         境界フェザリング
    despill.rs         色かぶり除去
    mask.rs            マスク型と統計（foreground_ratio, bbox, touches_edge）
  transform/
    resize.rs          fast_image_resize ラッパ
    canvas.rs          キャンバス配置、fill_ratio
    composite.rs       背景色合成
  batch.rs             spec.json の読み込みと rayon 並列実行
```

## 2. フェーズ分割

| # | 内容 | 完成するもの | 目安 |
|---|---|---|---|
| 0 | 土台：`cargo init`、`[[bin]]` 明示、`.gitignore`、error/exit code、`--json` の stdout/stderr 規約 | — | 0.5日 |
| 1 | I/O：読み込み＋EXIF正規化、書き出し、背景色推定 | `kiri info` / `kiri convert` | 1日 |
| 2 | リサイズ：Lanczos3、fitモード、拡大禁止 | `kiri resize` | 0.5日 |
| 3 | 切り抜きコア：Lab変換、連結フラッドフィル、マスク統計、bbox適用、モルフォロジー、フェザリング、デスパイル | `kiri cutout`（切り抜き部） | 2〜3日 |
| 4 | EC整形：キャンバス配置、fill_ratio、背景色合成 | `kiri cutout`（完成） | 1日 |
| 5 | バッチ：spec.json、rayon並列 | `kiri batch` | 0.5日 |
| 6 | 仕上げ：README、実画像での再計測とデフォルト値調整、GitHub Actions | v0.1.0 | 1日 |

**Phase 1 の `kiri info` を最初に完成させる。** 最小で end-to-end が通り、JSON規約とエラー処理の型がそこで確定する。型が決まれば以降は同じ形で積み上げられる。

## 3. テスト戦略

### 3.1 ユニットテスト

- **Lab変換**: 既知の値で検証（純白 → L=100、純黒 → L=0）
- **フラッドフィル**: 手書きの小さなビットマップで期待マスクを検証。**白背景×白商品の穴あきケースを必ず含める**（最重要）
- **EXIF Orientation**: 8方向すべて
- **canvas / fill_ratio**: 座標計算

### 3.2 ゴールデンテスト（合成画像）

合成商品画像の生成器をテストフィクスチャとして持つ。難ケースを網羅する。

- 白背景 × 白い商品
- 落ち影あり
- 商品が画像外周に接している（見切れ）
- グラデーション背景（`uniformity` が下がり警告が出ること）

### 3.3 CLI統合テスト（`assert_cmd` + `predicates`）

- 各エラーケースの exit code
- **stdout が常に valid JSON であること**（AI前提のCLIとして最も重要）
- stdout にログが混入しないこと
- `--force` なしでの上書き拒否

### 3.4 冪等性テスト

同じ入力に対して同じ出力バイト列が得られること。決定的動作の保証。

## 4. CI

GitHub Actions で以下を実行する。

- `cargo test`
- `cargo clippy -- -D warnings`
- `cargo fmt --check`

リリース時は各OS向けバイナリをビルドして GitHub Releases に添付する。pure Rust のためクロスコンパイルは素直に通る。

## 5. 進捗

- [ ] Phase 0: 土台
- [ ] Phase 1: I/O と `kiri info` / `kiri convert`
- [ ] Phase 2: `kiri resize`
- [ ] Phase 3: 切り抜きコア
- [ ] Phase 4: EC整形
- [ ] Phase 5: バッチ
- [ ] Phase 6: 仕上げ
