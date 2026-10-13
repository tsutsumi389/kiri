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

## 透過の余白は切り詰めてから当てはめる（`trim`）

**`kiri cutout` の出力は、商品の外が透過の余白である。** `--canvas` は余白を
意図して足し、`--rotate` は回した四隅のぶんだけ余白を広げる。-91 度で回した
5788 x 4386 の出力では、商品は横幅の 24% しか占めない。これを余白ごと `contain`
で `[860, 40, 380, 590]` へ入れると、**商品は横 85px ほどの豆粒になる。**

画像の層は既定で、**不透明度が 0 でない画素の外接矩形で素材を切り詰めてから**
`fit` する。捨てるのは完全に透明な画素だけなので、`contain` では見た目は 1 画素も
消えない。不透明な写真（背景の層）では何も起きない。**`cover` と `exact` では
効き方が変わる**——`cover` で枠からはみ出して切り落とされるのは余白ではなく商品に
なり、`exact` が引き伸ばすのは素材のキャンバスではなく商品になる。

しきい値は不透明度 0 で、半透明の影や縁は残る。`cutout` は背景の不透明度を 0 へ
落とし切るので、遠くに薄い画素が残って矩形が素材いっぱいに広がることはない
（-91 度で回したリモコンと `--canvas 1400` の出力の 2 つで、矩形が商品に乗ることを
確かめた）。他のツールが作った素材で切り詰めが効かないときは、`opaque_rect` を見る。

| `trim` | 当てはめるもの |
|---|---|
| `"alpha"`（既定） | 不透明な画素の外接矩形 |
| `"none"` | 素材のキャンバスごと（余白も枠に内接させる） |

**互換への影響。** `trim` を足す前の compose は `"none"` の振る舞いだった。
透過の余白を持つ素材を置いていた spec は、**同じ `rect` でも商品が大きく置かれ、
`placed` が変わる。** 余白込みで位置を揃えた素材など、余白そのものを構図として
使っていた spec と、`cover` で余白のほうを切り落としたかった spec には
`"trim": "none"` を書く。不透明な素材を置く spec は 1 バイトも変わらない。

### 実際に商品が占める矩形（`opaque_rect`）

`layers[].opaque_rect` は、画像の**不透明な画素が実際に占める矩形**である。
`placed` は素材の矩形なので、`"trim": "none"` の素材では透過の余白まで含む。
値は画素の格子へ外向きに丸めた整数である。文字の層と、1 画素も不透明でない画像では null。

**`outside_safe_area` はこちらで判定する。** `--canvas 1400 --fill-ratio 0.9` で
作った素材を `[800, 0, 480, 670]` へ `"trim": "none"` で余白ごと置くと `placed` は `[800, 95, 480, 480]`
で safe_area `[0, 108, 1280, 454]` の上へ出るが、不透明な画素は内側にある。
見えない余白が外へ出ても表示で切られるものは無いので、`OUTSIDE_SAFE_AREA` は
出さない。描く画素が 1 つも無い画像は `false` である。

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
| `outside_safe_area` | `OUTSIDE_SAFE_AREA` | `safe_area` の外へ出たか（画像は `opaque_rect` で、文字は `placed` で判定） |

```
$ kiri compose layout.json -o thumb.png --fail-on default
$ kiri compose layout.json -o thumb.png --fail-on text_overflow>0,text_contrast<4.5
```

**コントラストは背後の実測である。** 背景色の指定から計算すると、下に画像が
敷いてある場合に必ず外れる——ちょうどそこが読めなくなる場所である。しきい値の
4.5 は WCAG 2.1 の AA で、kiri が決めた数ではない。

**1/64px 以下のはみ出しは 0 と数える。** 文字の外接矩形は f32 から上がって
くるので、y=190 に置いた字が 189.99998 になる、といった端数が出る。これを数えると
`text_overflow>0` が見た目に何も起きていない構図を落とす。`outside_safe_area` も
同じ幅を見逃す。1/64px を超えたはみ出しは、差し引かずに測ったとおりの量を返す。

**重なりは字が実際に覆う画素で数える。** 外接矩形で数えると、行間と字間の空白まで
商品に重なったことになる。

**`--fail-on` の指標は `kiri cutout` のものとは別である。** あちらは `mask.*` を
見る。同じ表に混ぜると `cutout --fail-on text_overflow` が書式として通り、cutout に
文字は 1 つも無いので**永久に発火しない門**になる。

## note の見出し画像を組む（`--profile note`）

```
$ kiri compose header.json -o header.png --profile note
$ kiri compose header.json -o header.png --profile note --fail-on default
```

**note の規定をキャンバスへ当て、一覧に縮んだときに読めるかを測る。** 原寸の
測りだけでは足りない——見出し画像が原寸で見られることはほとんど無く、note の
一覧では横 144〜192px に縮んで並ぶ。原寸で 4.5 を超えたコントラストも、64px の
見出しも、そこでは読めないことがある。

| 項目 | spec に書いてある | spec に無い |
|---|---|---|
| `canvas.width` / `height` | spec の値。比が 1280:670 から 0.5% 以上外れたら `PROFILE_ASPECT_MISMATCH` | 1280 x 670 |
| `safe_area` | spec の値 | 上下中央の `[0, 108, 1280, 454]`。キャンバスが違えば合わせる（下を参照） |

**`"safe_area": null` は書かなかったのと同じ**で、profile の範囲が入る。
キャンバスの比が規格と同じ（`PROFILE_ASPECT_MISMATCH` が出ない）なら範囲を比で
拡大する。外れていれば、**note が中央を規格の比へ切り取った後に残る範囲**を
使う——1280 x 720 なら上下 25px ずつ切られるので `[0, 133, 1280, 454]` になる。

**明示 > profile。** cutout の `--profile` と同じ約束である。1920 x 1006 は比が
同じ（0.1% 差）なので黙って通る。`canvas` の寸法を省けるのは `--profile` を
渡したときだけで、渡さずに省けば今までどおり `SPEC_INVALID` で断る。
書いたバイト数が 10MB（10,000,000 バイト）を超えたら
`PROFILE_MAX_BYTES_EXCEEDED` が出る。**`--dry-run` でも出る**——dry-run も
エンコードまでは通してバイト数を返している。

**cutout / lint の `--profile` とは別の表である。** `kiri compose --profile amazon`
も `kiri cutout --profile note` も書式の段階で断る。表そのもの（出典・安全領域・
上限・表示幅）は `kiri schema --json` の `compose_profiles[]` が配る。

### 縮小表示での読みやすさ

全層を重ね終えた**実際に見える絵**を、2 つの表示幅へ面積平均で縮めて、
文字の層ごとに測り直す。

| 表示 | 幅（CSS px） | 根拠 |
|---|---|---|
| `pc_list` | 192 | PC の一覧で最も多かった描画幅 |
| `mobile_list` | 144 | スマホの一覧で最も多かった描画幅 |

**表示幅は note が公表していない。** note.com を開いて測った値なので、改修で
変わりうる。結果の `profile.revision` が測った年月を言う。

```
$ kiri compose header.json -o header.png --profile note --json | jq '.layers[1].display'
[
  { "name": "pc_list",     "width": 192, "size_px": 9.6, "contrast": 11.2,
    "legible": false, "reasons": ["too_small"] },
  { "name": "mobile_list", "width": 144, "size_px": 7.2, "contrast": 6.69,
    "legible": false, "reasons": ["too_small"] }
]
```

| 理由 | いつ | 直し方 |
|---|---|---|
| `too_small` | `size_px`（= `size × 表示幅 / canvas の幅`）が 11 未満 | `size` を `11 × canvas の幅 / 表示幅` 以上にする（1280 幅なら mobile_list で 98） |
| `strokes_lost` | 縮めた被覆が 0.5 に届く画素が 1 つも無い（線が 1px 未満に潰れた） | `weight` を上げる |
| `low_contrast` | 縮めた絵での前景と背景のコントラスト比が 4.5 未満 | 文字色か、その文字が載っている下地を変える |

1 つでも当たれば `legible: false` で、`TEXT_ILLEGIBLE_AT_DISPLAY` が出る。
11 は Apple の Human Interface Guidelines が本文の最小とする 11pt で、kiri が
決めた数ではない（4.5 が WCAG の数であるのと同じ扱い）。**大きさは em で測る**
——CSS の font-size と同じ量で、spec の `size` から逆算できる。

**原寸の `text_contrast` とは別の量である。** あちらは文字色と背後、こちらは
縮めた絵で見えている前景（その文字の被覆が 0.5 以上の画素）と背景（外接矩形の
中で 0.05 未満の画素）の相対輝度の中央値を比べる。中間色の地（#767676）に白の
100px は原寸で 4.54 だが、縮めると線が地の色と混ざって 3〜3.9 まで落ちる——
**原寸の値だけを読んだ呼び出し側は「読める」と判断する。**

透過の画素は、出力が透過を保てるなら**白**（note の一覧の地）へ、保てないなら
潰す色へ重ねてから縮める。縮小は box（面積平均）で、Lanczos のように輪郭を
立たせて細い線を実際より読めるように見せることはしない。

測らないものは次の 3 つで、**測ったように見せない。**

- `role` が `decoration` の文字と画像の層——`display` のキーごと出ない
- **原寸で 1 画素も描かれなかった文字**——同じく `display` が無い（原因は
  `TEXT_NOT_RENDERED` が言う。縮めて線が潰れたのではない）
- **表示幅がキャンバスの幅以上の表示**——縮小にならないので測らず、`display[]` に
  行ごと出ない（1 つも測らなければ空の配列）

背景の画素が 1 つも無ければ `contrast` は null で、低コントラストとは言わない。
門の `display_*` は、測れた値が 1 つも無ければ `unmeasurable` になる。

`--fail-on` には縮小表示の指標が 3 つ増える。

| 指標 | 量 | `default` の門 |
|---|---|---|
| `display_text_size` | 全文字層・全表示での `size_px` の最小 | `< 11` |
| `display_contrast` | 同じく `contrast` の最小（null は数えない） | `< 4.5` |
| `display_strokes_lost` | `strokes_lost` に当たった表示があるか（旗） | 立ったら |

**`--profile` を渡さない `default` はこの 3 つを足さない。** 足すと、これまで
通っていた `--fail-on default` が「測っていない」で一斉に落ちる。明示して
書いた場合は、測っていないので `unmeasurable`（exit 5）になる。

## 持っていないもの

`--derive` / `--manifest` / `--max-bytes` は無い。1 枚を組んで 1 枚を書く
コマンドなので、派生が要るなら組んだ結果を `kiri convert` / `kiri resize` へ
渡せばよい。使えない項目をヘルプに並べると、**`kiri schema` が「指定できる」と
言った先に出口が無い**ことになる。
