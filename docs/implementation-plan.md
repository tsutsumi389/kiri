# kiri 実装計画

最終更新: 2026-09-30

設計の詳細と決定根拠は [design.md](./design.md) を参照。本書は実装の進め方のみを扱う。

## 1. モジュール構成

`lib.rs` と `main.rs` を分離する。統合テストから `use kiri::...` できるようにするためで、追加コストはほぼない。

```
src/
  main.rs              エントリ、サブコマンドのディスパッチ
  lib.rs               公開API（テストから利用）
  cli.rs               clap derive による引数定義
  error.rs             エラー型とカタログ（code → exit code / 意味）。kind は code から引く
  warning.rs           警告型（code / message / hint / data）とカタログ。error.rs と同じ契約
  report.rs            JSON 出力の構造体（serde）
  image_io/
    load.rs            JPEG/PNG 読み込み + EXIF Orientation 正規化 + ICC → sRGB
    heif.rs            HEIC/AVIF の判別と、変換手順を添えたエラー
    save.rs            AVIF/PNG/JPEG 書き出し、拡張子からの形式推論、sRGB の名乗り（IccPolicy）
    derive.rs          最終画像 1 枚 → 書き出す派生（Derivation）のエンコードと書き込み
  color/
    lab.rs             sRGB ↔ Lab 変換、知覚的色距離（ΔE）
    icc.rs             埋め込み ICC の解釈と sRGB への変換（行列 + TRC 型のみ）
    srgb_profile.rs    出力へ埋める sRGB の ICC（v2、516B）を自前で組む
    synthetic.rs       テスト用の合成 ICC 生成（#[cfg(test)]）
  cutout/
    background.rs      背景色推定、uniformity とテクスチャ（外周の勾配分布）算出
    floodfill.rs       外周シードの連結フラッドフィル
    morphology.rs      連結成分の面積フィルタ / オープニング / クロージング
    edges.rs           Sobel による輪郭強度（堤防）と外周のテクスチャ計測
    refine.rs          境界帯のアルファ再推定と色の復元（既定の経路）
    feather.rs         境界フェザリング（refine のフォールバックと --no-refine 用）
    despill.rs         色かぶり除去（--no-refine 用）
    constraints.rs     空間的な指示（トライマップ / マスク / 多角形）の画素表現
    diagnostics.rs     境界の診断値（halo_ratio, edge_width）
    mask.rs            マスク型と統計（foreground_ratio, bbox, touches_edge）
    subject.rs         主体（商品）の位置の推定。背景推定だけから求める
  segment/             セグメンテーションモデル（feature `segment`、既定は無効）
    mod.rs             3 値の指定、前処理、確率マップ、トライマップ化。**モデルが無くても検査できる**
    model.rs           既知のモデルの素性（URL / ダイジェスト / ライセンス）と置き場所の解決
    isnet.rs           tract-onnx による推論。**このファイルだけが tract を知る**
    sha256.rs          モデル検証のためだけの SHA-256（自前、依存を足さない）
  transform/
    resize.rs          fast_image_resize ラッパ
    rotate.rs          回転。90 度単位は画素の入れ替え、それ以外は Catmull-Rom
    canvas.rs          キャンバス配置、fill_ratio、1 画素の straight alpha 合成
    shadow.rs          アルファから落ち影を合成する（箱型 3 回のガウス近似）
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
| 7 | 回転：90度単位は無劣化、任意角は Catmull-Rom で外接矩形へ拡張 | `kiri rotate` | 0.5日 |
| 8 | 自己記述：code をカタログ化、パーサから契約を組み立て、書かずに試せるようにする | `kiri schema` / `--dry-run` | 0.5日 |
| 9 | 値の読み方：しきい値を定数へ寄せ、`fields[]` として配る | `kiri schema` の `fields[]` | 0.5日 |
| 10 | 境界の欠陥を測れるようにする：実写背景のベンチと、輪郭の粗さ・縁の汚染の診断値 | `mask.contour_roughness` / `mask.rim_contamination` / `tests/real_backgrounds.rs` | 1日 |
| 11 | 空間的な指示：トライマップ / マスク画像 / 多角形を入口として受け、画素ごとの制約へ畳む | `--trimap` / `--fg-mask` / `--bg-mask` / `--fg-polygon` / `--bg-polygon` / `constraints` ブロック | 1日 |
| 12 | 境界を matting として解く：帯の中の二値画素を色で塗り直し、色の門つきメディアンで均し、guided filter でアルファを解く | `--matting` / `--smooth-contour` / `--no-reclassify` / `settings.band_min_radius` | 1日 |
| 13 | 背景を 1 色ではなく照明場 B(x, y) として持つ | `--background-model` / `background.field_range` / `background.residual` | 1日 |
| 14 | 意味の事前知識：pure Rust 推論でセグメンテーションモデルを粗マスクの供給源にする | `--segment` / `kiri model list` / `segment` ブロック | 1日 |
| 15 | 探索を kiri 側に持たせ、影を合成する | `--optimize` / `--shadow synth` | 1日 |
| 17 | リリース用 CI：検査のワークフローを置き、feature の有無で 2 系統回す | GitHub Actions | 0.5日 |
| 18 | 出力を 1 本の派生パイプラインへ畳み、sRGB の ICC を埋める | `Derivation` / `render()` / `outputs[].icc` | 1.5日 |
| 19 | 目標バイト数へ品質を自動探索する | `--max-bytes` / `outputs[].quality_used` | 1日 |
| 20 | 1 枚から複数サイズ・複数形式を書き、書いたものを列挙する | `--derive` / `--sizes` / `--formats` / `--manifest` | 2日 |
| 21 | 品質指標を合否に畳み、exit code で仕分けられるようにする | `--fail-on` / exit 5 / `BatchReport.rejected` | 1日 |
| 22 | モール規格をプリセットとして持ち、既存画像を検査する（**済**。§5 の Phase 22） | `--profile` / `kiri lint` / `schema.profiles[]` | 2日 |
| 23 | 主体の傾きを畳み、セット内で大きさと余白を揃える（**済**。§5 の Phase 23） | `--rotate auto` / batch の `set` | 1.5日 |
| 24 | 背景が中性だという前提と照明場から白点と露出を直す（**済**。§5 の Phase 24） | `--white-balance` / `--exposure` | 2日 |
| 25 | 反射を合成する（**済**。§5 の Phase 25） | `--reflect` | 1日 |
| 26 | 出力の既定を実写の実測から引き直す：`--quality` を 40 dB に届く 90 へ、profile の canvas を固定値から入力依存の段へ（**済**。§5 の Phase 26） | `--quality` の既定 90 / `profile::CANVAS_LADDER` / `Profile::canvas_for` | 1日 |
| 27 | 境界のアルファを帯だけの closed-form matting で解く（**済**。§8。**狙った的には当たらず、織り目のある実写背景に当たった**——受け入れ条件は §8.6 で差し替えた） | `--matting closed-form` / `MATTING_NOT_CONVERGED` | 1〜2日 |
| 28 | 帯を遷移と厚みから引き直し、柔らかい輪郭と細い構造を取り戻す（**中止**。§9.9。**第一歩の計測で §8 の診断が誤りだと分かった**——帯は真の遷移を既に覆っていて、的は帯幅ではなかった。実装はしていない） | 計測の通り道（`CutoutResult.band_width_histogram`。JSON には出さない） | 0.5日（計測で中止） |
| 29 | 切り抜いた素材と文字を spec から 1 枚へ組む（**済**。§10.10。**組版だけを `resvg` へ出し、契約と測りは kiri が持つ**。計画と違えた 6 点と費用の実測は §10.10） | `kiri compose` / spec の `layers[].role` / `TEXT_OVERFLOW` ほか指標 4 つ | L |

**Phase 17〜26 は EC 特化のロードマップ**で、狙いと順序の根拠は
[7. EC 特化のロードマップ](phases/07-roadmap-ec.md)に置く。

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

#### 実写背景のベンチ（`tests/real_backgrounds.rs`）

**合成背景では実写の布を再現できない。** `weave`（縦横の正弦の積）は周期も振幅も
一定で、照明ムラ・しわ・繊維の向きを持たない。崩れが出るかどうかが周期の選び方に
依存してしまう（4.3 も「周期 8px では再現しない」と認めている）。

そこで**背景だけを実写にする**。`tests/fixtures/backgrounds/` に置いた 3 枚
（白い不織布 2 枚と暗い机、いずれも等倍の切り出し）の上に、正解の被覆率が
解析的に分かる合成商品を**線形 RGB で**載せる。テクスチャは本物のまま、輪郭には
解析的な正解が残るので、診断値を正解由来の誤差と突き合わせられる。

固定している値: 新しい 2 つの診断値が欠陥を見ること（きれいなシーンの 3 倍以上）、
きれいなシーンで誤警報しないこと（しきい値の半分以下）、正解由来の指標との
順位相関（Spearman ρ ≥ 0.7、27 点。壊滅ケースを除いた 20 点でも ρ ≥ 0.5）、
しきい値が**正解だけで分けた** 2 群のあいだに入っていること、実写背景シーンの
決定性、`diagnose()` の追加コストが切り抜き本体の 35% を超えないこと。

**既存の S シーンは 1 ビットも動かさない。** 角丸矩形の SDF と被覆率の規則は
`ProductShape` として切り出して両方から呼ぶが、合成の色は S シーンが sRGB のまま、
R シーンだけ線形 RGB で混ぜる。前者は回帰の基準であり、画素を動かせば固定してきた
数値が全部動く。後者は `refine` が線形で解く前提に正解側を合わせたものである。

手持ちの正解つき実写は `KIRI_BENCH_DIR` で差し込む（`tests/fixtures/README.md`）。
実写に正解アルファを付ける作業は人にしかできず、素材ごとに権利も違うので、
リポジトリには置かない。

### 3.3 CLI統合テスト（`assert_cmd` + `predicates`）

- 各エラーケースの exit code
- **stdout が常に valid JSON であること**（AI前提のCLIとして最も重要）
- stdout にログが混入しないこと
- `--force` なしでの上書き拒否

### 3.4 冪等性テスト

同じ入力に対して同じ出力バイト列が得られること。決定的動作の保証。

## 4. CI

GitHub Actions で以下を実行する。

- `cargo test --profile ci-test`
- `cargo clippy --profile ci-test -- -D warnings`
- `cargo fmt --check`
- `cargo +1.85 check --all-targets`（MSRV）

**テストは `ci-test` プロファイルで回す。** debug のままだと `tests/real_backgrounds.rs` の較正ベンチだけで手元 861 秒・標準ランナー 36 分かかり、`test` ジョブ全体が 55 分になっていた。画像処理を最適化なしで回していたためで、最適化すると手元の実行は 996 秒 → 51 秒（19.4 倍）になる。**テストの数を減らして得た速さではない**——19 本はそれぞれ別の Phase の受け入れ基準を持っており、計測用の 7 本はもともと `#[ignore]` である。

素の `--release` を使わないのは、`debug_assert!` と整数のオーバーフロー検査が落ちるからである。不変条件の一部はそこに預けてある。`ci-test` は `release` を継承したうえで両方を戻し、`lto` と `codegen-units` は外す——テストの実行時間には効かないのに、ビルドだけが伸びる。clippy も同じプロファイルを指すのは、中間生成物を共有するためである。

MSRV の検査を CI に入れるのは、**宣言だけ置いても検査しなければ守られない**ためである。実際に一度、`rust-version = "1.85"` を掲げたまま let-chains（安定化は 1.88）が2箇所入り込んでいた。ローカルの新しいツールチェーンでは通ってしまうので、CI でしか検出できない。

1.85 は edition 2024 の下限であり、これ以上下げられない。上げる場合は「その版でしか書けない何か」が必要になったときに限り、理由を添えて上げる。

リリース時は各OS向けバイナリをビルドして GitHub Releases に添付する。pure Rust のためクロスコンパイルは素直に通る。**ただしリリース用のワークフローは製品化を決めるまで置かない**——検査のワークフローだけを先に入れる。

public リポジトリなので GitHub 製の標準ランナーは分数無制限で無料である（larger runner は public でも課金される）。キャッシュはリポジトリあたり 10GB なので、`target` をまるごとではなく依存だけを載せる。**ISNet の 176MB は CI では取得しない**——モデルが無ければ推論のテストは黙って飛ぶ設計なので、コンパイルと非推論部分だけを守る。

**x86_64 では NASM が要る。** `ravif` が既定で引く `rav1e` の `asm` feature が x86 のアセンブリを持っており、標準ランナーには nasm が入っていない。手元の aarch64 macOS では要らないので、**CI に載せて初めて出た**。asm を切る道は採らない——AVIF のエンコードが遅くなるうえ、出力バイト列が変わらない保証が無い（Phase 18 は「ICC を除けば 1 バイトも変わらない」を md5 で固定している）。これは**ビルド時のツール**であって、生成物が C ライブラリを引くわけではないので、`cargo tree -e normal` に `*-sys` が現れないという Cargo.toml の基準はそのまま保たれている。

## 5. 進捗

記号は 3 つある。

- `[x]` — 実装した
- `[ ]` — **残件**。着手の順は [6. 残件の優先順位](#6-残件の優先順位)
- `※` — **決定と限界**。作業ではない。「やらないと決めた」か「素材側の限界として
  測り切った」もので、**残件として数えない**。理由ごと残すのは、同じ道を 2 度
  検討しないためである

記録が長くなったので Phase ごとに `phases/` へ分けた。**節番号は分けても
変えていない**——`§5 の Phase 22` のような参照が他の節・src / tests から
150 箇所以上張られていて、番号が識別子として働いているからである。

| Phase | 記録 |
|---|---|
| Phase 0〜7 | [phases/05-progress-00-07.md](phases/05-progress-00-07.md) |
| Phase 10〜15 | [phases/05-progress-10-15.md](phases/05-progress-10-15.md) |
| Phase 17〜21 | [phases/05-progress-17-21.md](phases/05-progress-17-21.md) |
| Phase 22〜23 | [phases/05-progress-22-23.md](phases/05-progress-22-23.md) |
| Phase 24〜26 | [phases/05-progress-24-26.md](phases/05-progress-24-26.md) |

**着手の順は [6. 残件の優先順位](#6-残件の優先順位)。** 計画中・中止した段は
§7〜§10 にある（下の索引）。

## 6. 残件の優先順位

§5 の `[ ]` は 1 件だけになった（`※` は決定とその理由の記録であって作業では
ないので数えない。**件数は書かない**——手で書き写した数は必ず実数から離れ、
実際この行は Phase 17 の時点で既に 1 つずれていた）。

ここに並ぶのは**いまの kiri を完成させるための残件**である。EC 特化のために
**足す**ものは [7. EC 特化のロードマップ](phases/07-roadmap-ec.md)
に分けた。**2 つの表の交点だった P0 の「リリース用 CI」= Phase 17 は片付いた**ので、
残るのは P2 の 1 件である。それは Phase 24 と同じ「効果が測れる保証が薄い」群なので、
着手前にベンチで見積もる。

片付いたものが 3 群ある。かつての P1「契約の穴」6 件（`cutout --rotate` と spec の
`rotate`、`--model-path` の 2 件、`kiri schema` の `segment_available`、
`to_probability` / `Probability::new`、`nearest`）と、**P1「性能」9 件**である。
後者の中身は §5 の Phase 14 / 15 / 16 にあり、要点は 3 つ:

- **推論を f16 へ畳んだ**。`cutout --segment isnet` が 24.5MP で
  6.20 秒 / 1642MB → 5.61 秒 / 1300MB、`info --segment isnet` が
  1.88 秒 / 1456MB → 1.97 秒 / 938MB。前景比率は 0.1745 → 0.1746 しか動かない
- **見立てを持ち回るようにした**（`BackgroundSeen`）。`--optimize` の探索段で
  21 回走っていた `analyse_background` が 1 回になり、`auto` の門と `info` の
  二度測りも消えた
- **`batch` が `--segment` を受ける**。計画をプロセスで 1 つ持ち、推論を 1 本ずつ
  通すので、`--jobs` を上げてもモデルのぶんのメモリは増えない

3 群目は **P0「リリース用 CI」1 件**で、中身は §5 の Phase 17 にある。clippy / fmt /
test / MSRV を GitHub Actions へ移した。**新規のコードは無い**——手元でしか回して
いなかったものを機械に移しただけで、`--features segment` の有無で 2 系統、MSRV には
付けない、という §4 の条件はそのまま写してある。

### P2 — 品質と機能（残る 1 件）

**効果が測れる保証が薄い。** 着手前に必ずベンチで見積もる——P0/P1 の 10 倍の
時間を使って数値が動かないことがありうる。

- ~~帯だけを切り出した closed-form matting（Phase 12）~~ **済（Phase 27）。**
  §6 が P2 へ課した「P0/P1 の 10 倍の時間を使って数値が動かないことがありうる」は
  **半分だけ当たった**——狙った的（柔らかい輪郭）では数値が悪化し、狙っていなかった
  場所（織り目のある実写背景）で品質ゲートの警告 6 本が消えた。結果は
  [8.10](phases/08-phase-27.md#810-結果2026-09-30) にある

## 7〜10. 段ごとの計画

狙い・順序の根拠・受け入れ条件は段ごとに `phases/` へ分けた。節番号は
分割前のまま変えていない。

| 節 | 内容 | 文書 |
|---|---|---|
| 7 | EC 特化のロードマップ（Phase 17〜26） | [07-roadmap-ec.md](phases/07-roadmap-ec.md) |
| 8 | Phase 27: 帯だけの closed-form matting | [08-phase-27.md](phases/08-phase-27.md) |
| 9 | Phase 28: 帯を遷移と厚みから引き直す | [09-phase-28.md](phases/09-phase-28.md) |
| 10 | Phase 29: 素材と文字を spec から組む | [10-phase-29.md](phases/10-phase-29.md) |
