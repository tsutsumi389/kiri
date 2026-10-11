# kiri

AIエージェントから使われることを前提とした、EC商品画像のための画像編集CLI。

単色背景の商品写真を対象に、背景透過の切り抜き・リサイズ・回転・キャンバス配置・Web配信形式への変換を1コマンドで行う。

> **開発中です。** 主要なコマンドは一通り動作します（`info` / `convert` / `resize` / `rotate` / `cutout` / `batch` / `schema` / `model`）。
> 帯だけの closed-form matting は `--matting closed-form` として入りましたが、
> **狙っていた「柔らかい輪郭」には当たりませんでした**（当たった先は織り目のある
> 実写背景です。[3 つ目の値](docs/commands/05-3-boundary.md#closed-form-は織り目に効き柔らかい輪郭には効かない)）。
> 進捗は [docs/implementation-plan.md](docs/implementation-plan.md) を参照してください。

## 特徴

- **依存ゼロの単一バイナリ** — Python環境も外部ツールも不要。pure Rust で完結する（C のライブラリを 1 つも引かない）
- **背景は 1 色でなくてよい** — 照明ムラは[照明場](docs/commands/05-2-background.md#背景は-1-色ではなく場で持つ)として推定し、不織布や段ボールの織り目には堤防を自動で合わせる。境界は形ではなく**色から matting として**解く
- **どこが商品かを面で教えられる** — `--trimap` / `--alpha-trimap` / `--fg-mask` / `--bg-mask` / `--fg-polygon` / `--bg-polygon`。**数値ノブにはもう余地が無い**——粗いトライマップを 1 枚渡すだけで輪郭の誤差が 5 分の 1 になる
- **色で解けないものはモデルに聞ける** — 任意の `--segment` でセグメンテーションモデルを**粗マスクの供給源**として使う。既定では走らず、モデルは別ファイル。[意味の事前知識](docs/commands/05-4-guidance.md#意味の事前知識モデルに何を聞くか)を参照
- **設定の探索を kiri に任せられる** — `--optimize` は `info` → `cutout` → 調整という 3 手のループを 1 コマンドへ畳む。[探索を kiri に任せる](docs/commands/05-4-guidance.md#探索を-kiri-に任せる--optimize)を参照
- **仕上げまで 1 本で** — キャンバス配置、[落ち影の合成](docs/commands/05-5-finishing.md#落ち影を合成する)（`--shadow synth`）、[反射の合成](docs/commands/05-5-finishing.md#反射を合成する)（`--reflect on`）、回転、Web配信形式への変換
- **結果を自分で採点する** — `mask` が返す 7 項目（前景比率・ハロー・境界の色差・遷移幅・輪郭の粗さ・縁の汚染・外周接触）と、機械可読な 34 の警告
- **AIエージェント向けの構造化I/O** — `--json` で結果を返し、stdout はJSONのみ、ログは stderr に分離
- **契約を自分で配る** — `kiri schema` がオプションと警告・エラー code の一覧を返す。README を読ませなくてよい
- **書かずに試せる** — `--dry-run` は成果物を 1 バイトも変えずに、書いたときと同じ結果を返す
- **決定的な動作** — 同じ入力からは常に同じ出力が得られる
- **AVIF出力** — Web配信に適した形式。1000×1000 の単色背景の商品写真で約 55ms（所要時間は素材の細かさに比例する——不織布の織り目で約 80ms、乱数で約 200ms）

## インストール

```
cargo install --path .
```

## 最短手順

```
# 1. 素材を測る（背景が単色か、商品がどこにあるか）
$ kiri info product.jpg --json

# 2. 背景を抜いて、モール規格のキャンバスへ収める
$ kiri cutout product.jpg -o out.jpg --profile amazon

# 3. 規格を外したら exit 5 で知る
$ kiri cutout product.jpg -o out.jpg --profile amazon --fail-on default
```

書き出さずに結果だけ見たいときは `--dry-run` を足す。成果物は 1 バイトも変わらない。

**エージェントから使うなら、まず `kiri schema --summary --json` を読ませる。**
使うコマンドが決まったら `kiri schema cutout --brief --json` のようにその分だけ引く。
オプションと警告・エラー code の一覧はそれが返すので、この README を文脈へ載せる
必要はない（全体は 220KB あり、[絞り方](docs/commands/01-schema.md#必要な分だけ引く)を参照）。

### Claude Code から使う

このリポジトリは Claude Code のプラグインとして Skill を配っている。入れておくと、
「商品写真の背景を抜いて」「Amazon 用の画像を作って」と頼むだけで、Claude が
`info` → `cutout --dry-run --preview` → preview を目で確かめる → 面の指示で直す、
という手順で kiri を使う。

```
/plugin marketplace add tsutsumi389/kiri
/plugin install kiri@kiri
```

中身は [plugins/kiri/skills/kiri/SKILL.md](plugins/kiri/skills/kiri/SKILL.md) の 1 枚で、
kiri 本体は別に入れておく（`cargo install --path .`）。

## コマンド

| コマンド | 何をするか | 詳細 |
|---|---|---|
| `kiri schema` | 契約（オプション・警告・エラー code）を JSON で配る | [01-schema.md](docs/commands/01-schema.md) |
| `kiri info` | 素材を測る。背景の均一度・商品の位置・傾き | [02-info.md](docs/commands/02-info.md) |
| `kiri convert` | 形式を変換する | [04-convert-resize-rotate.md](docs/commands/04-convert-resize-rotate.md) |
| `kiri resize` | 大きさを変える | [04-convert-resize-rotate.md](docs/commands/04-convert-resize-rotate.md) |
| `kiri rotate` | 回す。90 度単位は無劣化 | [04-convert-resize-rotate.md](docs/commands/04-convert-resize-rotate.md) |
| `kiri cutout` | **背景を透過して切り抜く。**キャンバス配置・影・反射・規格判定まで | [05-cutout.md](docs/commands/05-cutout.md) |
| `kiri batch` | spec を渡してまとめて処理する。セット内で大きさと余白を揃える | [06-batch.md](docs/commands/06-batch.md) |
| `kiri lint` | 既存の画像をモール規格で検査する | [07-lint.md](docs/commands/07-lint.md) |
| `kiri compose` | 素材と文字を 1 枚へ組む | [08-compose.md](docs/commands/08-compose.md) |
| `kiri model` | セグメンテーションモデルの素性と置き場所を返す | [09-model.md](docs/commands/09-model.md) |

入力を読んで出力を書くコマンドに共通する事柄——**色空間の扱い**、出力が sRGB を
名乗ること、`--max-bytes`、`--derive` による複数出力——は
[出力の共通事項](docs/commands/03-output.md) にまとめてある。

`kiri cutout` は分量が大きいので、全オプションの表を
[05-cutout.md](docs/commands/05-cutout.md) に置き、その先の理屈・実測・限界を
6 つの主題へ分けてある（[読む順](docs/commands/05-cutout.md#読む順)）。

## 対応形式

- **入力**: JPEG, PNG, WebP（静止画。lossy / lossless とも読む）
- **出力**: AVIF, PNG, JPEG, WebP（**lossless のみ**）
- 出力は sRGB を名乗る（PNG / JPEG / WebP は ICC、AVIF は AV1 の色情報）

**WebP の出力は lossless だけである。** lossy の WebP を書けるエンコーダは
libwebp（C）にしか無く、依存ゼロの単一バイナリを崩してまでは入れない。
lossless の WebP は写真素材では AVIF にも JPEG にもサイズで負けるので、
**WebP を指定してくる入稿先のための形式**と考えてほしい。品質を持たないので
`--quality` は効かず、`--max-bytes` も PNG と同じく段を降りない。
1 辺 16384px が形式の上限で、超えると `WEBP_ENCODE_FAILED` で断る。

**アニメーション WebP は入力にできない**（`UNSUPPORTED_FORMAT`）。黙って
1 枚目を使うと、それが商品を代表していなくても気づけないためで、使うフレームを
`webpmux -get frame 1 in.webp -o frame.webp` のように取り出してから渡す。
**フレームが 1 枚しかないアニメーションも断る。** 判定はアニメーションを名乗る旗
（VP8X）で行い、枚数は数えない——1 枚なら代表の問題は無いが、そのフレームは
キャンバスの一部に置かれた矩形でありうる（ANMF のオフセット）ので、静止画として
読むと寸法と位置がずれうる。取り出せば静止画になるので、手順は同じである。

AVIF 入力はデコードに dav1d（C）が必要なため非対応とした。

### HEIC / HEIF は読めない

**iPhone で撮ったままの HEIC は入力にできない。** HEVC のデコーダは pure Rust に
存在せず、libheif や libde265（いずれも C）を持ち込まなければ実装できない。
入稿素材が iPhone 撮影であることは多いが、依存ゼロを崩す判断はしていない。

代わりに**原因と手順を返す**。拡張子や形式の判別に失敗しただけのメッセージでは、
エージェントは別の拡張子を試すような無駄な再試行に入るためである。

```
$ kiri info IMG_0251.HEIC --json
{
  "error": {
    "code": "UNSUPPORTED_FORMAT",
    "message": "HEIC/HEIF は入力として未対応です（pure Rust の HEVC/AV1 デコーダが無いため）",
    "hint": "macOS: sips -s format jpeg -s formatOptions 95 in.heic --out in.jpg / その他: magick in.heic -quality 95 in.jpg（または libheif の heif-convert）"
  }
}
```

```
# macOS なら追加インストールなしで変換できる
$ sips -s format jpeg -s formatOptions 95 IMG_0251.HEIC --out IMG_0251.jpg
$ kiri cutout IMG_0251.jpg -o out.avif --json
```

変換後の JPEG には Display P3 の ICC が埋め込まれたままだが、kiri が読み込み時に
sRGB へ変換するので、そこで色が転ぶことはない。

## 対象範囲

**単色背景**（白・グレー等のスタジオ撮影背景）のEC商品画像を対象とする。
**「単色」は塗りつぶしという意味ではない**——照明ムラは
[照明場](docs/commands/05-2-background.md#背景は-1-色ではなく場で持つ)として推定し、不織布や段ボールの織り目には
堤防を自動で合わせる。

生活シーン写真などの複雑背景は、既定では対象外。色だけでは品質が出ない。
そうした画像は `kiri info` の `uniformity` で事前に検出でき、警告が返る。
**`--segment` を有効にした build なら、そこにモデルの粗マスクを差し込める**
（[意味の事前知識](docs/commands/05-4-guidance.md#意味の事前知識モデルに何を聞くか)）。

単色背景であっても、次の場合は正しく切り抜けない。

- **輪郭のコントラストが ΔE 9 を下回る** — 落ち影の裾は最も急なところで 1px あたり
  ΔE 1.9 変化する。それより緩い輪郭は影の傾斜と区別できない。角の丸みや JPEG の
  滲みで段差がさらに 3-5px に広がると、この下限は上がる
- **商品の明度が背景色を横切る** — 上を明るく照らされた淡色の商品は、明度が変化する
  途中で背景と完全に同じ色になる行を持つ。そこには色の手がかりが 1bit も無い
- **背景より明るい映り込み** — 影とは逆向きなので `--shadow-tolerance` の対象外
- **半透明な商品**（ガラス瓶、透明パッケージ）

いずれも `--bbox` や `--fg-seed`、`--fg-polygon` / `--bg-polygon` / `--trimap` で
**どこが商品かを空間的に教えれば**救済できる。輪郭のコントラストが足りない素材でも、
確定前景と確定背景を面で渡せば、色の手がかりが要るのは残った帯だけになる。

## exit code

| コード | 意味 |
|---|---|
| 0 | 成功 |
| 1 | 一般エラー |
| 2 | 引数不正（書式や値域の誤りは code を伴わず stderr にのみ出る） |
| 3 | 入力ファイル異常 |
| 4 | 処理失敗 |
| 5 | 規格未達（成果物はある。人が見る対象で、結果 JSON は通常どおり返る） |

この表の文言は `kiri schema --json` の `exit_codes[]` が同じものを返す。

**5 は「0 以外は失敗」と読んでいる呼び出し側にとって新しい意味である。**
5 が出るのは `--fail-on` を渡した実行と `kiri lint` の 2 つだけで、
4 が「やり直せば直る失敗」（成果物が無い）なのに対し、5 は処理が通って成果物も
書かれた結果が規格に達しなかったことを言う。`ErrorReport` には差し替わらないので、
`outputs[]` も `mask` も、`kiri lint` なら `checks[]` も通常どおり読める。
**名乗る code は出どころで違う**——`--fail-on` は結果 JSON の
`compliance.code` に `QUALITY_GATE_FAILED`（[合否を exit code で返す](docs/commands/05-6-verify.md#合否を-exit-code-で返す--fail-on)）、
`kiri lint` は `LintReport.code` に `PROFILE_VIOLATION`（[kiri lint](docs/commands/07-lint.md#kiri-lint)）を置く。

`--json` 指定時はエラーも JSON で stdout に返る。**code と exit code の対応は
`kiri schema --json` の `errors[]` が返す。**

**ただし引数の書式や値域で落ちた場合は JSON が返らない。** 検証は clap が行い、
kiri のエラー型を通らないため、`--json` を付けても **stdout は空のまま exit 2 で
終わる**（説明は stderr に出る）。`errors[]` のどの code にも対応しない唯一の失敗
なので、`stdout` が空で終了コードが 2 なら、綴りか値域の誤りとして stderr を読む。

```
$ kiri rotate product.jpg -o out.png --angle sideways --json
error: invalid value 'sideways' for '--angle <ANGLE>': 'sideways' は有限な数値である必要があります
```

```json
{ "schema_version": 2, "error": { "code": "OUTPUT_EXISTS", "message": "...", "hint": "--force を付けると上書きします" } }
```

## ドキュメント

| | 何が書いてあるか |
|---|---|
| [コマンド別ドキュメント](docs/commands/) | **使い方。**コマンドごとのオプション・実測・限界 |
| [設計ドキュメント](docs/design.md) | **なぜその方式か。**技術選定と決定の根拠。切り抜きアルゴリズム（§4）は主題ごとに [docs/design/](docs/design/) へ分かれていて、design.md の §4 に番号から引く索引がある |
| [実装計画](docs/implementation-plan.md) | **いつ何を作ったか。**フェーズ分割・テスト方針・残件の順序。フェーズごとの記録（§5 / §7〜§11）は [docs/phases/](docs/phases/) にある |

**番号と見出しは分割しても変えていない。** README やソースのコメントが
`design.md 4.13` や `計画 §8.6` のように番号で、あるいは見出しの名前で参照して
いるので、そこを識別子として固定し、どのファイルにあるかは各文書の索引で引く。

## ライセンス

MIT
