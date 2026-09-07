# kiri 実装計画

最終更新: 2026-09-08

設計の詳細と決定根拠は [design.md](./design.md) を参照。本書は実装の進め方のみを扱う。

## 1. モジュール構成

`lib.rs` と `main.rs` を分離する。統合テストから `use kiri::...` できるようにするためで、追加コストはほぼない。

```
src/
  main.rs              エントリ、サブコマンドのディスパッチ
  lib.rs               公開API（テストから利用）
  cli.rs               clap derive による引数定義
  error.rs             エラー型 → exit code のマッピング
  warning.rs           警告型（code / message / hint / data）。error.rs と同じ契約
  report.rs            JSON 出力の構造体（serde）
  image_io/
    load.rs            JPEG/PNG 読み込み + EXIF Orientation 正規化 + ICC → sRGB
    heif.rs            HEIC/AVIF の判別と、変換手順を添えたエラー
    save.rs            AVIF/PNG/JPEG 書き出し、拡張子からの形式推論
  color/
    lab.rs             sRGB ↔ Lab 変換、知覚的色距離（ΔE）
    icc.rs             埋め込み ICC の解釈と sRGB への変換（行列 + TRC 型のみ）
    synthetic.rs       テスト用の合成 ICC 生成（#[cfg(test)]）
  cutout/
    background.rs      背景色推定、uniformity とテクスチャ（外周の勾配分布）算出
    floodfill.rs       外周シードの連結フラッドフィル
    morphology.rs      連結成分の面積フィルタ / オープニング / クロージング
    edges.rs           Sobel による輪郭強度（堤防）と外周のテクスチャ計測
    refine.rs          境界帯のアルファ再推定と色の復元（既定の経路）
    feather.rs         境界フェザリング（refine のフォールバックと --no-refine 用）
    despill.rs         色かぶり除去（--no-refine 用）
    diagnostics.rs     境界の診断値（halo_ratio, edge_width）
    mask.rs            マスク型と統計（foreground_ratio, bbox, touches_edge）
    subject.rs         主体（商品）の位置の推定。背景推定だけから求める
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
- **ICC 変換**: 合成 ICC をテスト内で組み立てて検証する。実写ファイルを置かずに済み、
  かつ「Display P3 の原色が sRGB でクランプされる」「中性グレーが中性のまま L* を保つ」
  「sRGB では恒等」といった性質を数値で固定できる。**ColorSync（`sips --matchTo`）との
  突き合わせは実写でしか取れないので、数値は PR の記録に残し、テストには持ち込まない**
- **フラッドフィル**: 手書きの小さなビットマップで期待マスクを検証。**白背景×白商品の穴あきケースを必ず含める**（最重要）
- **EXIF Orientation**: 8方向すべて
- **canvas / fill_ratio**: 座標計算

### 3.2 ゴールデンテスト（合成画像）

合成商品画像の生成器をテストフィクスチャとして持つ。難ケースを網羅する。

- 白背景 × 白い商品
- 落ち影あり
- 商品が画像外周に接している（見切れ）
- グラデーション背景（`uniformity` が下がり警告が出ること）

### 3.2b 境界品質の回帰テスト（`tests/edge_quality.rs`）

**真の被覆率が解析的に分かる**合成シーンを作り、切り抜き結果を正解と突き合わせる。
境界の良し悪しは目で見ないと分からないと思われがちだが、正解を持った合成シーンなら
数値で追える。追えなければ「直したつもりで悪化させた」ことに気づけない。

固定している値: アルファ誤差、背景色のまま不透明な縁の割合、商品が削られた割合、
黒地に載せたときのハロー輝度、細部（幅 3px のストラップ）の残存率、孤立ノイズの除去、
輪郭から離れた背景に残るゴミの割合、同一入力での出力バイト列の一致。

**対照実験を必ず添える。** 「既定値で通る」だけを固定すると、その仕組みが死んでも
テストは通り続ける。織り目のある背景（S11）では「既定値なら切れる」と
「`--edge-threshold 8` を明示すると布が残る」を対で固定している。

**片側だけの対照では足りないこともある。** S11 は濃色商品なので、堤防が
自動調整の値でも 0（無効）でも同じ結果になる。引き上げ幅を 10 倍にしても通って
しまうため、織り目の上に淡色商品を置いた S12 を足し、「切ると商品が消える」と
「既定の 8 だと織り目が残る」の両端を同じテストで押さえている。

`--ignored` を付けると、一覧表（`print_the_metrics_table`）と 12MP での
所要時間（`print_the_refine_cost_on_large_inputs`）を出す。既定値の変更前後を
同じ物差しで比べるためのもので、境界処理に手を入れたら必ず前後で取る。

```
cargo test --release --test edge_quality -- --ignored --nocapture
```

所要時間のほうは形状ごとに測る。**角丸矩形だけでは計算量の崩れが見えない。**
境界帯の推定にかかる時間は画像の面積ではなく帯の面積（周長 × 帯幅）で決まるので、
メッシュ・レース・櫛のように構造が帯より細い素材でしか桁が変わらない。実際、
窓を全走査していた頃は角丸矩形が数十 ms のままで、櫛だけが数秒の桁に膨らんだ。
判定は「桁が変わったら落ちる」緩いもの（追加 2 秒未満）しか置いていない。
**絶対値をドキュメントに書き写さないこと。**`--no-refine` 側の実測が倍近く
振れるため、形状どうしの順序すら再現しない。前後で比べるならその場で 2 回回す。

CI で走るのは同じ形の小さい画像を使う `refine_does_not_scale_with_the_area_of_the_window`
で、`--no-refine` との**時間比**に上限を置く。絶対時間は機械によって何倍も違うが、
同じ画像どうしの比なら debug/release の差も含めて安定する。

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
- `cargo +1.85 check --all-targets`（MSRV）

MSRV の検査を CI に入れるのは、**宣言だけ置いても検査しなければ守られない**ためである。実際に一度、`rust-version = "1.85"` を掲げたまま let-chains（安定化は 1.88）が2箇所入り込んでいた。ローカルの新しいツールチェーンでは通ってしまうので、CI でしか検出できない。

1.85 は edition 2024 の下限であり、これ以上下げられない。上げる場合は「その版でしか書けない何か」が必要になったときに限り、理由を添えて上げる。

リリース時は各OS向けバイナリをビルドして GitHub Releases に添付する。pure Rust のためクロスコンパイルは素直に通る。

## 5. 進捗

- [x] Phase 0: 土台
- [x] Phase 1: I/O と `kiri info` / `kiri convert`
- [x] Phase 2: `kiri resize`
- [x] Phase 3: 切り抜きコア
- [x] Phase 4: EC整形
- [x] Phase 5: バッチ
- [ ] Phase 6: 仕上げ
  - [x] 境界品質の改善（境界帯のアルファ再推定、面積フィルタ、診断値と回帰テスト）
  - [x] エッジ堤防の頑健化（フィルの 3 段化、影の専用判定、測地的オープニング、
        非極大抑制による稜線の細線化）
  - [x] 背景のテクスチャ検知と堤防の自動調整（不織布・段ボールのような素材で
        堤防が背景の中で壁になる問題。外周の勾配 p50 で発火を決め、p90 から
        高さを決める）
  - [x] 孤立ノイズの面積を解像度に追従させる（`--cleanup` を長辺 1000px 換算の
        半径として読み替える。20MP の織り目 150px² が既定で消える）
  - [x] 埋め込み ICC（Display P3 / AdobeRGB 等）から sRGB への変換、
        `color_space` / `color_converted` の報告、`--no-color-convert`
  - [x] HEIC 入力を明確に断り、変換手順をヒントで返す
  - [x] 警告の構造化（`code` / `hint` / `data`）。エラーには当初から機械可読な
        `code` があったのに、警告だけが日本語の散文で、エージェントは文字列
        マッチで分岐するしかなかった。同じ道具の中で契約の形が違う理由が無い
  - [x] 主体（商品）の位置の推定と報告（`subject`）。背景推定だけから最大の
        連結成分として求まる。`info` / `cutout` の両方に同じ形で出す。
        信頼度は面積比 0.05 と捕捉率 0.70 で決め、**ΔE は使わない**
        （誤検出でも大きく出るため）。**求めた bbox は自動適用しない**——
        bbox は構図の意思決定であり、`edge_threshold` の自動調整とは性質が違う
  - [x] `SUBJECT_TOUCHES_EDGE` の誤診の解消（`BBOX_RECOMMENDED` への分岐）と、
        `info` 段階での「bbox で救えるか」の判別。合成シーン
        `split_background_scene` で対照実験ごと固定した
  - [x] 外周が汚染された画像で主体を信用しない。商品が外周サンプルの 1 割以上を
        占めると閾値がその商品自身の色差を指し、**残る輪郭の滲みだけで
        `capture_ratio` が 1.0 近くに張り付く**——誤検出を弾くはずの捕捉率が
        誤検出を後押しする向きに反転する。外周 ΔE の二峰性
        （p50 < 5 かつ p90 > 15）を指紋にして `low` へ降格する。合成シーン
        `bleeding_product_scene`（**縮小が走る 600px**）で固定した
  - [x] `BBOX_RECOMMENDED` の条件に `touches_edge` を戻す。ここは「外周接触を
        どう読むか」の分岐であって不均一な背景そのものへ反応する警告ではない。
        `touches_edge: false` の対照を添えた
  - [x] `HALO_REMAINS` に次の一手（`--tolerance` を上げる）を添える。
        これで `info` → `cutout --bbox` → `cutout --bbox --tolerance` の
        3 手で実写が解ける
  - [x] 縮小の入力を借用ビュー（`images::ImageRef`）にして 20MP の複製をやめる。
        `info` のピーク RSS 470MB → 372MB（main 比 +71% → +35%）
  - [ ] `subject` の較正を `--border` から切り離す（現状は既定値前提であることを
        `MIN_CAPTURE_RATIO` のコメントと README に明記するに留めている。
        実測: IMG_0238 が `--border 2` で low、`--border 110` で high へ裏返る）
  - [ ] 輪郭のコントラストが ΔE 9 を下回る素材への対処（現状は限界として文書化）
  - [ ] README の整備、実素材での既定値の再調整
  - [ ] リリース用 CI
