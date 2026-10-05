# kiri lint

> [README](../../README.md) › [コマンド別ドキュメント](README.md) › kiri lint

既にあるファイルが `--profile` の規格を満たすかを**検査する。書かない。**
`cutout --profile` が「規格へ収めて書く」側で、こちらは「収まっているかを見る」側
である。**表は 1 つ**（`kiri schema --json` の `profiles[]`）で、両側がそれを読む。

```
$ kiri lint product_amazon.jpg --profile amazon
product_amazon.jpg
  規格      amazon (2026-09)  合格
  対象      jpeg 1600 x 1600  76182 バイト
    o format        "jpeg"  (要求 ["jpeg","png"])
    o longest_side  1600  (要求 {"max":10000,"min":500})
    o alpha         false  (要求 false)
    o color_space   {"color_converted":false,"color_named":true,"color_space":"sRGB","icc_profile":true}  (要求 "sRGB")
    o background    {"border_px":48,"delta_e":0.0,"rgb":[255,255,255],"uniformity":1.0}  (要求 {"delta_e_max":2.0,"rgb":[255,255,255]})
    o fill_ratio    {"bbox":[92,233,1507,1366],"source":"colour","value":0.885}  (要求 0.85)
```

`--profile` は**必須**である。既定の規格を置くと、`kiri lint out.jpg` が何の規格で
答えたのかを結果から読み直すことになる。

```json
$ kiri lint product_amazon.jpg --profile amazon --json
{
  "schema_version": 2,
  "input": "product_amazon.jpg",
  "profile": {
    "name": "amazon",
    "revision": "2026-09"
  },
  "file_size": 76182,
  "width": 1600,
  "height": 1600,
  "format": "jpeg",
  "passed": true,
  "code": null,
  "checks": [
    {
      "name": "format",
      "status": "pass",
      "expected": [
        "jpeg",
        "png"
      ],
      "actual": "jpeg"
    },
    {
      "name": "longest_side",
      "status": "pass",
      "expected": {
        "max": 10000,
        "min": 500
      },
      "actual": 1600
    },
    {
      "name": "alpha",
      "status": "pass",
      "expected": false,
      "actual": false
    },
    {
      "name": "color_space",
      "status": "pass",
      "expected": "sRGB",
      "actual": {
        "color_converted": false,
        "color_named": true,
        "color_space": "sRGB",
        "icc_profile": true
      }
    },
    {
      "name": "background",
      "status": "pass",
      "expected": {
        "delta_e_max": 2.0,
        "rgb": [
          255,
          255,
          255
        ]
      },
      "actual": {
        "border_px": 48,
        "delta_e": 0.0,
        "rgb": [
          255,
          255,
          255
        ],
        "uniformity": 1.0
      }
    },
    {
      "name": "fill_ratio",
      "status": "pass",
      "expected": 0.85,
      "actual": {
        "bbox": [
          92,
          233,
          1507,
          1366
        ],
        "source": "colour",
        "value": 0.885
      }
    }
  ],
  "warnings": []
}
```

- `checks[]` には**評価したものを全部載せる**（`pass` も）。落ちたものだけを
  載せると、「見た上で通った」と「そもそも見ていない」が区別できない。
  **並びは決定的**で、同じ入力を 2 回検査した結果はバイト列として一致する
- **`name` は一意である**（`compliance.checks[]` とはそこが違う）
- **規定の無い条件は 1 行も出さない。** `shopify` に `background` や `fill_ratio` の
  行が無いのは取りこぼしではなく、その規格が構図を規定していないからである。
  「どんな値でも合格」として `pass` で並べると、そのことが結果から読めなくなる
- `expected` は規格が求めた値。**飛ばした項目でも返す**——検査できなかったことと、
  何を求められていたかは別の事実である
- **背景に完全一致は求めない。** JPEG は 8x8 のブロックごとに量子化するので、
  純白で塗った面でも書き出した画素は 255 のまま揃わない。許容する色差は
  `expected.delta_e_max` が一緒に配るので、読む側が自分で `!=` を書かずに済む
- `fill_ratio` は**どう測ったか**を `actual.source` が名乗る。切り抜き済みの
  成果物ではアルファの外接矩形（`alpha`）が、不透明な画像では色から見立てた主体
  （`colour`）が正解になる
- `background` は**どれだけの幅を見た数なのか**を `actual.border_px` が名乗る。
  外周の中央値がそのまま合否になるので、帯は `--border` そのものではない。
  `max(短辺/33, --border)`（1600px なら 48px）から始めて、**その帯が単色と
  言えるまで半分ずつ狭め**、言えた最も広い帯で測る（狭められるのは
  1/4 まで。1600px なら 48 → 24 → 12）。既定の `--border 2` は切り抜きの
  ための値で、そこだけを見ると**白い縁 1 本で灰色一面の画像が「背景は純白」に
  なる**。`--border` は下限として効き、広げる向きにだけ働く。
  `kiri info --border <border_px>` を走らせれば、lint が見たのと同じ背景色が出る
- **帯を狭めるのは、主体が帯に入ったときに背景でない画素で測らないためである。**
  占有率が `1 - 2/33 ≒ 0.939` を超えると主体そのものが外周 48px に入る。どの
  規格にも占有率の上限は無い（寄りのトリミングは合法）ので、`cutout --profile
  amazon --fill-ratio 0.97` で書いた純白背景の画像はここを通る。占有率が
  0.985 を超えて残る縁が短辺の 1/132 を切ると、狭める側の下限に当たって
  `unmeasurable` になる——そこまで細い縁の中央値は背景を代表しない
- **測れないものは `pass` と言わない。** 外周に不透明な画素が 1 つも無ければ
  （透過 PNG）`background` は `unmeasurable` で `actual` は `null` になる
  ——アルファ 0 の画素が持つ RGB は表示に使われない値なので、それを「測った
  背景色」として配ると作り話になる。外周が単色として扱えないとき
  （`uniformity` が 0.90 を切る）も `unmeasurable` だが、**そのときも `rgb` と
  `delta_e` は返す**——「背景がベージュで、白から ΔE 33 外れている」は合否に
  使わない数でも、素材を撮り直すかレタッチするかを決めるのはその数である
  （`status` が合格でないことを言い切っているので、添えても嘘にはならない）
- `color_space` は**ファイルが名乗っているか**を `actual.color_named` が言う。
  ICC も EXIF ColorSpace も無いファイルは何も名乗っていないので `unmeasurable`
  である（AVIF の CICP が `unspecified` のときとまったく同じ扱い）。`kiri info` が
  同じファイルに `"sRGB"` と答えるのは「kiri がその画素を sRGB として扱った」と
  いう別の事実で、**規格が問うているのはファイルの名乗りのほう**である

`status` は 4 つ。**合格は `pass` だけである。**

| status | 意味 | 次の一手 |
|---|---|---|
| `pass` | 見た上で規格を満たしている | — |
| `fail` | 規格に触れている | その項目を直す |
| `skipped` | **この形式では構造的に測れない**（AVIF の画素） | JPEG か PNG で渡し直す |
| `unmeasurable` | 測ろうとしたが、この画像からは出なかった（主体が見つからない、外周に不透明な画素が 1 つも無い、背景が単色でない、色空間を何も名乗っていない） | 素材を見る。形式を変えても同じ結果になる |

不合格なら **exit 5**、`code` は `PROFILE_VIOLATION`。処理そのものは成功していて
検査対象のファイルもそのままあるので、**結果 JSON は `ErrorReport` に差し替わらない**
（[exit code](../../README.md#exit-code)）。引数の誤り（`--profile` の綴り違いや指定漏れ）は
exit 2、読めない・辿れないファイルは exit 3 である。

## AVIF は画素まで見られない

kiri は AVIF をデコードできない（dav1d は C のライブラリで、依存ゼロを崩す）。
**コンテナから読める事実だけで判定し、画素を読まないと測れない項目は飛ばす。**

```
$ kiri lint product.avif --profile square-white
product.avif
  規格      square-white (2026-09)  不合格
  対象      avif 1600 x 1600  1812 バイト
    x format        "avif"  (要求 ["jpeg","png"])
    o longest_side  1600  (要求 {"max":null,"min":1000})
    o square        true  (要求 true)
    o alpha         false  (要求 false)
    o color_space   {"full_range":true,"matrix":6,"primaries":1,"source":"sequence_header","transfer":13}  (要求 "sRGB")
    - background    skipped  (要求 {"delta_e_max":2.0,"rgb":[255,255,255]})
    - fill_ratio    skipped  (要求 0.85)
  code      PROFILE_VIOLATION
警告: background / fill_ratio は画素を読まないと測れないため検査していません（kiri は avif をデコードできないので、コンテナから読める事実だけで判定しました）
      飛ばした項目も合格ではないので passed は false です。構図まで見るなら JPEG か PNG を渡してください
```

**黙って合格にしない。** 飛ばした項目は `pass` ではないので `passed` は落ち、
`PROFILE_UNCHECKABLE` がどれを飛ばしたかを `data.checks` に配列で入れる
（文面から項目名を抜き直さずに済む）。寸法・透過・色の名乗りはコンテナから読める
ので、そちらは AVIF でも普通に検査する——色は `colr` があればそれを、無ければ
AV1 シーケンスヘッダの CICP を読む。

**画素を要求しない規格なら、AVIF でも飛ばす項目は 1 つも無い。** `shopify` は寸法と
バイト数と形式しか規定していないので、`PROFILE_UNCHECKABLE` も出ない。
