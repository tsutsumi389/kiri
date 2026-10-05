# kiri batch

> [README](../../README.md) › [コマンド別ドキュメント](README.md) › kiri batch

仕様ファイルに従って複数の画像を一括処理する。**AI エージェントから使う際の本命はこれ。**

```
$ kiri batch spec.json
  out/p1.avif  1000x1000  4.0 KB
! out/p2.avif  1000x1000  4.9 KB
  out/p3.avif  1000x1000  4.2 KB
x broken.jpg  失敗

4 件中 3 件成功、1 件失敗、1 件に警告  (387 ms)
```

```json
{
  "defaults": {
    "canvas": "1000x1000",
    "fill_ratio": 0.85,
    "format": "avif",
    "tolerance": 12
  },
  "items": [
    { "input": "p1.jpg", "output": "out/p1.avif" },
    { "input": "p2.jpg", "output": "out/p2.avif", "tolerance": 6 },
    { "input": "p3.jpg", "output": "out/p3.avif", "bbox": [200, 150, 1100, 1600] }
  ]
}
```

`defaults` は全項目に適用され、項目側の指定が優先される。指定できるキーは `cutout` の
オプションと対応する（`bbox` / `normalized` / `fg_seeds` / `trimap` / `alpha_trimap` / `fg_mask` /
`bg_mask` / `fg_polygons` / `bg_polygons` / `tolerance` / `border` /
`cleanup` / `feather` / `despill` / `refine` / `matting` / `smooth_contour` /
`reclassify` / `background_model` / `segment` / `model_path` / `optimize` /
`color_convert` / `edge_threshold` /
`step_tolerance` / `shadow_tolerance` / `shadow` / `shadow_offset` / `shadow_blur` /
`shadow_color` / `shadow_opacity` / `reflect` / `reflect_height` / `reflect_opacity` /
`reflect_gap` / `seal` / `rotate` / `canvas` / `fill_ratio` /
`format` / `quality` / `effort` / `max_bytes` / `background` / `flatten` /
`derive` / `sizes` / `formats` / `naming`）。

最上位に書けるのは `defaults` / `items` と、セット全体を 1 つの基準で揃える
[`set`](#セット内でスケールと余白を揃えるset) の 3 つだけである。

`trimap` / `alpha_trimap` / `fg_mask` / `bg_mask` は画像のパスで、`input` と同じく
**仕様ファイルの場所**を基準に解決する。`fg_polygons` / `bg_polygons` は `[[x1,y1,x2,y2,...], ...]` で、
1 つの配列が 1 つの多角形になる。

`edge_threshold` は CLI と同じく「書かない」と「`8` と書く」を区別する。書かなければ
背景のテクスチャに応じて自動調整され、書けばその値に従う。

`optimize` を `true` にすると、その項目で探索が走る（[探索を kiri に任せる](05-4-guidance.md#探索を-kiri-に任せる--optimize)）。
**同じ項目に書いた `tolerance` / `bbox` / `background_model` は探索の軸から外れる**
——CLI で明示したときと同じ規約で、spec では「書いたかどうか」がそのまま明示に
あたる。1 件が数秒から十数秒になるので、数百点に一律で付ける値ではない。

`max_bytes` は**数値でも文字列でも書ける**（`"max_bytes": 500000` と `"max_bytes": "500k"` は
同じ意味になる。数値は常にバイト数で、単位を付けたいときだけ文字列にする）。読めない値
——0、小数、単位の綴り違い——はその項目を `INVALID_MAX_BYTES` で落とす。黙って無視すると、
上限が効かないまま数百点が仕上がる。**1 件の失敗で全体は止まらない**ので、その実行の
終了コードは他の失敗と同じ 4 になる（[サイズの上限に収める](03-output.md#サイズの上限に収める--max-bytes)）。

`rotate` は**数値でも文字列でも書ける**（`90` / `"90"` / `"auto"`）。`auto` は
`cutout --rotate auto` と同じで、CLI と同じパーサを通る
（[傾きを測って直す](05-5-finishing.md#傾きを測って直す--rotate-auto)）。読めない値はその項目を
`INVALID_ROTATE` で落とす——黙って 0 度へ落とすと、水平出しを頼んだつもりの項目が
回らないまま数百点に混ざる。**1 件の失敗で全体は止まらない**ので、その実行の
終了コードは他の失敗と同じ 4 になる。

`segment` は `cutout --segment` と同じ 3 値（`off` / `auto` / `isnet`）で、
`model_path` は `trimap` などと同じく仕様ファイルの場所を基準に解決する。
**モデルは全項目で 1 度だけ読み、推論は 1 本ずつ通す**ので、`--jobs` を
上げてもモデルのぶんのメモリは増えない。ただし**時間は並べられない**
（1 件あたり 1.3 秒がそのまま積む）ので、数百点に一律で付ける値ではない。
混ざった素材なら `auto` を `defaults` に書けば、色で解ける画像は素通りする
（[意味の事前知識](05-4-guidance.md#意味の事前知識モデルに何を聞くか)）。

`matting` と `background_model` と `segment` は綴りを検査する。未知の値を既定へ
落とすと、その項目だけ黙って別の設定で処理され、数百点を回した後に仕上がりを
見るまで気づけない。

`derive` / `sizes` / `formats` / `naming` は項目ごとに派生を組む
（[複数のサイズと形式](03-output.md#複数のサイズと形式をまとめて書く--derive)）。`derive` は
`--derive` と同じキーを持つオブジェクトの配列で、値は数値でも文字列でも書ける。

```json
{
  "defaults": { "sizes": [400, 800, 1600], "formats": ["avif", "jpeg"] },
  "items": [
    { "input": "p1.jpg", "output": "out/p1.png" },
    { "input": "p2.jpg", "output": "out/p2.png",
      "derive": [
        { "width": 1600, "format": "jpeg", "quality": 82, "max_bytes": "500k", "role": "hero" },
        { "width": 400, "format": "avif", "role": "thumb" }
      ] }
  ]
}
```

`derive` と `sizes` / `formats` の同時指定は `INVALID_DERIVATION` でその項目を落とす
（CLI と同じ排他）。未知のキーと読めない値も同じ code になる。

`fail_on` は `--fail-on` とまったく同じ書式の文字列で書く
（[合否を exit code で返す](05-6-verify.md#合否を-exit-code-で返す--fail-on)）。読めない値は
`INVALID_FAIL_ON` でその項目を落とす。

```json
{
  "defaults": { "fail_on": "default" },
  "items": [
    { "input": "p1.jpg", "output": "out/p1.png" },
    { "input": "p2.jpg", "output": "out/p2.png", "fail_on": "default,halo_ratio>0.05" }
  ]
}
```

条件に触れた項目は `results[].status` が `"rejected"` になり、`BatchReport` の
`rejected` に数えられる。**`result` は通常どおり入る**（`CutoutReport` が
`compliance` を持つ）。

- **`rejected` はこの版で加わったキーで、`fail_on` を書かない実行でも常に出る**
  （そのときは必ず 0）。`total` / `succeeded` / `failed` と同じく数え上げの一部なので、
  条件を書いたときだけ現れる形にはしていない
- **`succeeded` の定義は変えていない。** 不合格でも処理は成功しており、成果物は
  書かれている。`succeeded` を「書けた件数」として読んでいるなら、それは今も正しい
- 実行全体の終了コードは **1 件でも不合格なら 5**。ただし **`failed > 0` の 4 が
  優先する**——両方あるときに 5 を返すと、成果物が 1 つも無い項目があることが
  番号から消え、「見れば分かる結果」として扱われてしまう
- 人間向けの出力では不合格の行に `R` が付く（警告の `!` とは別の印である）

## セット内でスケールと余白を揃える（`set`）

同じ商品を距離を変えて撮った数枚、あるいは 1 回の撮影で撮った数十点は、
**1 枚ずつ `fill_ratio` を決めても並べたときに揃わない。** 占有率は
「外接矩形がキャンバスの何割か」なので、縦長と横長が混ざると高さがばらつく。
`set` は揃える基準を**セット全体で 1 つ**決める。

```json
{
  "set": { "align": "height", "fill_ratio": 0.85 },
  "defaults": { "canvas": "1000x1000", "format": "jpeg" },
  "items": [
    { "input": "near.jpg", "output": "out/near.jpg" },
    { "input": "far.jpg",  "output": "out/far.jpg" }
  ]
}
```

`set` は**最上位に置く**（`defaults` の隣ではない）。揃える相手は「このセットの
全点」で、1 点だけを見ても決まらないためである。`defaults` に置けば項目側で
上書きできる形になり、「自分だけ別の基準で揃える」という意味を持たない指定が
書けてしまう。

- **`align: "height"`** — 全点の商品の**出力上の高さ**を共通の `T × キャンバス高`
  にする。撮影距離の違いを正規化する側で、縦長と横長が混ざっていても高さが揃う
- **`align: "bbox"`** — 外接矩形を共通の枠 `(T × 幅, T × 高さ)` に長辺基準で
  収める。はみ出さないことを優先する側で、**全点で `fill_ratio` に `T` を書いたのと
  同じ結果**になる

`align` は必須で、綴りを外したら `INVALID_SET` で断る。未知の値を既定へ落とすと、
頼んだのと別の基準でセット全体が揃い、仕上がりを並べて見るまで気づけない。

**`fill_ratio` は省けるのが普通の使い方である。** 省くと `kiri` が全点の
占有率を測り、その**中央値**を目標 `T` にする。「このセットにとって自然な大きさ」を
自分で決めなくてよいのが狙いで、どちらで決まったかは結果の `set.source`
（`"specified"` / `"median"`）が言う。

測るのは切り抜きの前に 1 度だけで、**切り抜きは回さない**（背景の見立てと
一緒に返る主体の外接矩形だけを見る）。増えるのは decode 1 回ぶんである。
`fill_ratio` を書いた実行では**測ること自体を省く**——中央値を採る相手がいない。
読めない画像と主体を検出できない画像は材料から外れ、**1 点も測れなければ
`set` は効かず `SET_NOT_MEASURED`** が出る（そのとき各項目は自分で解決した
`fill_ratio` のまま処理される。個々の失敗は `results[]` が言う）。

```json
"set": { "align": "height", "fill_ratio": 0.3401, "source": "median", "measured": 24, "clamped": 2 }
```

**点ごとに実際に効いた占有率は `set.fill_ratio` ではない。** そちらは
`results[].result.canvas.fill_ratio` が返す——`height` 揃えでは横長の点ほど
大きな占有率を要求するためである。

### 横長の商品は上限で止まる（`SET_SCALE_CLAMPED`）

占有率は 1.0 を超えられない。正方のキャンバスで高さを `T × 高さ` にすると、
横幅は `T × 高さ × (商品の横 / 商品の縦)` を要求するので、**縦横比が `1/T` を
超える横長では必ずキャンバスの幅を越える**（`T = 0.85` なら 1.18:1 から）。

これは異常ではなく物理である。`kiri` は黙って「高さが揃った」ことにせず、
占有率を 1.0 で止めて `SET_SCALE_CLAMPED` を出す。`data` には要求した占有率・
使った 1.0・**実際に得た高さと目標の差**（`height_shortfall`）が入るので、
数 px の不足なのか半分しか無いのかを結果だけで分けられる。止まった件数は
`set.clamped` が数える。

1000x1000 のキャンバスに `T = 0.85` で揃えたときの実測では、縦横比 1.5:1 の商品で
高さが目標より 40px 足りず、2.0:1 で 243px、2.4:1 で 346px 足りない。
`T = 0.7` なら止まり始めるのは 1.43:1 からで、2.4:1 での不足は 196px に縮む。

横長を多く含むセットでは、`canvas` を横長にするか、`fill_ratio` を下げるか、
`align` を `bbox` にする。

### 優先順位

**明示指定 > `set` > `profile` > 既定** の 1 本である。

- `defaults` か項目に `fill_ratio` を書いた spec に `set` を足すと、spec を
  読んだ時点で `INVALID_SET` で断る。どちらを消すかは書いた人にしか決められない
  ので、黙ってどちらかを勝たせない（**成果物は 1 つも書かれない**）
- `profile` の占有率は `set` が上書きする。上書きしたことは
  `PROFILE_OVERRIDDEN` が言い、`data.by` が `"set"` と名乗る
- `set` があるのに `canvas` が（spec からも `profile` からも）取れない項目は
  `INVALID_SET` でその項目が落ちる。`set` はキャンバス上の占有率の話なので、
  canvas 無しでは意味を持たない。**1 件の失敗で全体は止まらない**ので、その
  実行の終了コードは他の失敗と同じ 4 になる

`--jobs` を変えても `T` は 1 ビットも動かない（測った値を並べ替えてから中央を
採るので、順序に依らない）。`--dry-run` でも測りは走る。

**マニフェストは実行全体で 1 つ**なので、spec のキーではなく `kiri batch --manifest`
で渡す。数百点が同じパスへ順に書けば、最後の 1 件だけが残る目録になってしまう。

| オプション | 既定値 | 説明 |
|---|---|---|
| `--base-dir DIR` | 仕様ファイルの場所 | 相対パスの基準ディレクトリ |
| `--jobs N` | CPU数 | 同時に処理する画像の枚数。スレッド数ではない |
| `--manifest PATH` | — | 書いたものを列挙する JSON。成功した項目だけが載る |
| `--force` | | 全項目で上書きを許可する |
| `--dry-run` | | 1 件も書き出さずに全項目の結果だけ返す |

**1件の失敗で全体を止めない。** 数百点を回すバッチでは、失敗を報告しつつ残りを処理し
切るほうが有用なため。失敗があった場合は終了コード 4 で知らせ、詳細は `results[]` に入る。
結果の並びは仕様ファイルの順序を保つ（並列実行でも）。

**仕様の誤りは黙って無視しない。** 綴り違いのキーがあればエラーにして候補を示す。
無視すると「指定したはずの設定が効いていない」という最も気づきにくい失敗を生むため。

```
$ kiri batch spec.json --json
{
  "error": {
    "code": "SPEC_UNKNOWN_FIELD",
    "message": "items[0] に未知のキー 'tolerence' があります",
    "hint": "'tolerance' の綴り違いではありませんか"
  }
}
```

## AIエージェントからの使い方

想定している流れはこう。

0. `kiri schema --json` で呼び方と code の意味を引く（初回のみ）
1. AI が対象画像を `kiri info` で確認し、仕様 JSON を書き出す。
   `uniformity` が低い画像では `subject.confidence` を見て、`high` なら
   `subject.normalized_bbox` をその項目の `bbox` に入れる
2. `kiri batch spec.json --json` で一括処理する
3. 結果の `failed` / `rejected` / `with_warnings` と各項目の
   `mask.foreground_ratio` を検証する。`fail_on` を書いておけば、**終了コードだけで
   「見るべき件があるか」が分かる**（4 なら失敗、5 なら不合格）
4. 失敗した項目だけ `tolerance` や `bbox` を調整して再実行する。
   **探索は `--dry-run` で回す**（成功している他の項目の成果物を壊さないため）
5. それでも直らない項目は `kiri cutout --preview` で画像を見て判断する。
   数値ノブで動かないなら**空間で教える**——`fg_polygons` / `bg_polygons` /
   `trimap` は spec からも渡せる

`batch` は既定でプレビューを書き出さない。数百点を回す通常の経路では JSON だけで
完結すべきで、全件で画像を吐けば無駄な I/O になるためである。プレビューは
**救済フェーズの道具**として `cutout` 側にある。

全件の座標を AI が出す必要はない。単色背景では自動判定が成立するため、AI の仕事は
「結果を見て、うまくいかなかった数枚を救済する」ことに絞られる。
