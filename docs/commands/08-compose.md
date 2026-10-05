# kiri compose

> [README](../../README.md) › [コマンド別ドキュメント](README.md) › kiri compose

**切り抜いた素材と文字を 1 枚へ組む。** EC の商品ページには商品そのもののほかに、
寸法や使い方を文字で説明する画像が並ぶ。note の見出し画像も同じ形をしている。

**組版器ではなく合成器である。** どこに何を置くかは spec が決め、kiri は置いて、
**測って、言う**。

```
$ kiri compose layout.json -o thumb.jpg
$ kiri compose layout.json -o thumb.jpg --dry-run --json   # 画素を書かずに幾何だけ
```

```json
{
  "canvas": { "width": 1280, "height": 670, "background": "#f5f2ec" },
  "font": { "family": "Noto Sans JP", "path": "fonts/NotoSansJP.ttf" },
  "safe_area": [0, 108, 1280, 454],
  "layers": [
    { "id": "subject", "type": "image", "role": "subject",
      "source": "out/product.png", "rect": [700, 85, 500, 500], "fit": "contain" },
    { "id": "heading", "type": "text", "role": "heading",
      "lines": ["素材を組み合わせる", "それだけをやる"],
      "rect": [80, 150, 560, 200],
      "size": 64, "weight": 700, "color": "#1f2328", "line_height": 1.3 }
  ]
}
```

素材とフォントのパスは **spec からの相対**で解く。spec の形は
`kiri schema --json` の `compose_spec[]` が配るので、**README を読ませる代わりに
それを引けばよい。**

## 書いた枠と、実際に置かれた矩形は違う

`rect` は枠である。画像はそこへ内接して縮むので、**枠を書いた側は仕上がりの寸法を
知らない**——素材の縦横比を知らないからである。`layers[].placed` が実際に置かれた
矩形を返す。

```
$ kiri compose layout.json -o thumb.png --dry-run --json | jq '.layers[] | {id, rect, placed}'
{ "id": "subject", "rect": [700, 85, 500, 500], "placed": [825, 85, 250, 500] }
```

## 縮めない、折り返さない

収まらない文字は**縮めも折り返しもせず**、`TEXT_OVERFLOW` で言う。どちらも
spec に書いた指定が黙って消える向きの親切で、`--fill-ratio` が商品を枠へ合わせる
のとは**約束の向きが逆**である。

日本語の自動折り返しは禁則処理（行頭に句読点を置かない、など）と字詰めを伴い、
それを持つと kiri は合成器ではなくなる。どこで割るかは文意の問題でもあるので、
**`lines` は行の配列で受け取る。割る材料（行ごとの実測幅）は返すが、割らない。**

## 無い字体は黙って代替しない

指定の family が見つからなければ `FONT_NOT_FOUND` で断り、**1 枚も書かない。**
代替へ落ちると、**同じ spec が機械ごとに違う絵になる**——`SEGMENT_UNAVAILABLE`
が feature の無い build で黙って off へ落ちないのと同じ向きである。

`font.path` を書けばそのファイルだけを読む。実際に使った字体のパスと SHA-256 は
結果 JSON の `font` に出るので、**後から「何で組まれたか」を辿れる。**

## 測って返す（`--fail-on`）

| 指標 | 警告 | 何を見るか |
|---|---|---|
| `text_overflow` | `TEXT_OVERFLOW` | 文字が `rect` からはみ出した量(px) |
| `text_contrast` | `TEXT_CONTRAST_LOW` | 文字と**その文字が実際に載っている背後**とのコントラスト比 |
| `layer_overlap` | `LAYERS_OVERLAP` | 文字が覆う画素のうち `subject` と重なった比 |
| `outside_safe_area` | `OUTSIDE_SAFE_AREA` | `safe_area` の外へ出たか |

```
$ kiri compose layout.json -o thumb.png --fail-on default
$ kiri compose layout.json -o thumb.png --fail-on text_overflow>0,text_contrast<4.5
```

**コントラストは背後の実測である。** 背景色の指定から計算すると、下に画像が
敷いてある場合に必ず外れる——ちょうどそこが読めなくなる場所である。しきい値の
4.5 は WCAG 2.1 の AA で、kiri が決めた数ではない。

**重なりは字が実際に覆う画素で数える。** 外接矩形で数えると、行間と字間の空白まで
商品に重なったことになる。

**`--fail-on` の指標は `kiri cutout` のものとは別である。** あちらは `mask.*` を
見る。同じ表に混ぜると `cutout --fail-on text_overflow` が書式として通り、cutout に
文字は 1 つも無いので**永久に発火しない門**になる。

## 持っていないもの

`--derive` / `--manifest` / `--max-bytes` は無い。1 枚を組んで 1 枚を書く
コマンドなので、派生が要るなら組んだ結果を `kiri convert` / `kiri resize` へ
渡せばよい。使えない項目をヘルプに並べると、**`kiri schema` が「指定できる」と
言った先に出口が無い**ことになる。
