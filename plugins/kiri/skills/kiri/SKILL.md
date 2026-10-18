---
name: kiri
description: EC 商品画像を kiri CLI で加工する。背景透過の切り抜き、白背景化、モール規格（Amazon / Shopify など）に合わせたキャンバス配置、落ち影・反射の合成、AVIF / JPEG / PNG / WebP（lossless）への変換、リサイズ、回転、既存画像の規格検査、複数枚の一括処理、切り抜いた素材と文字を 1 枚へ組む合成（note の見出し画像・サムネイル・EC の説明画像）。「商品写真の背景を抜いて」「白背景にして」「Amazon 用の画像を作って」「この画像は規格を満たす？」「note の見出し画像を作って」「サムネイルに文字を入れて」のような依頼で使う。
---

# kiri で商品画像を作る

kiri は単色背景の商品写真を切り抜いて、Web 配信用に仕上げる CLI である。
切り抜いた素材と文字を 1 枚へ組む（`kiri compose`）こともできる。
**エージェントから呼ばれることを前提に作ってあり**、`--json` を付ければ stdout は JSON だけ、
exit code にも意味がある。README を読む必要はない。

## 最初に

```bash
kiri --version
```

見つからなければ、利用者に入れてもらう（Rust のツールチェーンが要る）。

```bash
cargo install --git https://github.com/tsutsumi389/kiri
```

**オプションの綴り・既定値・候補は推測しない。** 契約は kiri 自身が返す。

```bash
kiri schema --summary --json          # 入口。コマンド一覧と exit code、規格（約 4KB）
kiri schema cutout --brief --json     # 使うコマンドの綴り・既定値・候補・しきい値
```

引数なしの `kiri schema --json` は 220KB あるので読み込まない。

## 呼び方の約束

- **常に `--json` を付ける。** ログは stderr に出るので、stdout をそのまま JSON として読む
- 失敗は `{"error": {"code", "message", "hint"}}`、注意は `warnings[]` の `{code, message, hint}`。
  **`hint` は次の一手そのもの**なので、まず従う
- `hint` の無い警告は、`kiri schema <command> --brief --json` の `warnings[].remedy` が
  何を試すかを言う。`remedy` も無い警告は、知らせるだけで直すものは無い
- exit code の意味は次のとおり

| code | 意味 | どうするか |
|---|---|---|
| 0 | 成功 | `warnings[]` を見る |
| 2 | 引数の誤り | `error.code` があれば hint に従う。無ければ綴りの誤りなので schema を引き直す |
| 3 | 入力の異常 | HEIC なら下記。アニメーション WebP（1 フレームでも）は hint の `webpmux -get frame 1 in.webp -o frame.webp` で静止画を取り出して渡し直す。ほかは利用者に伝える |
| 4 | 処理の失敗 | `error.hint` を読む |
| 5 | 規格未達 | **成果物はある。** どの条件で落ちたかを利用者に伝える |

- **入力ファイルを上書きしない。** 出力先は入力と別の名前にする
- `OUTPUT_EXISTS` は、上書きしてよいファイルであることを確かめてから `--force` を付ける

## 切り抜きの手順

### 1. HEIC なら JPEG にする

kiri は HEIC を読めない（`UNSUPPORTED_FORMAT`, exit 3）。macOS なら追加のインストールは要らない。

```bash
sips -s format jpeg -s formatOptions 95 IMG_0001.HEIC --out IMG_0001.jpg
```

### 2. 素材を測る

```bash
kiri info in.jpg --json
```

- `background.uniformity` は背景がどれだけ 1 色か。低くても、kiri は照明ムラを場として扱える
- `subject.normalized_bbox` / `subject.confidence` は商品の位置と、その推定の確からしさ
- `warnings[].hint` が `--bbox ... --normalized` を勧めていれば、それを次の手で使う

### 3. 書き出さずに試し、preview を見る

```bash
kiri cutout in.jpg -o out.png --dry-run --preview /tmp/kiri-preview.png --force --json
```

- `--dry-run` は成果物を書かない。**ただし `--preview` は書き出す**ので、2 回目以降は
  `--force` が要る。preview の置き場所は一時ディレクトリにする
- **preview を Read で開いて目で確かめる。** 3 枚のパネルが並び、左から
  元画像（0.1 刻みの座標グリッド付き）、マスク、結果である。原寸の出力は大きすぎて見られないので、
  見るのはこれにする
- 結果 JSON の `mask` は品質の指標である。`halo_ratio`（背景の残り）、`contour_roughness`（輪郭の粗さ）、
  `rim_contamination`（縁の汚れ）など。しきい値は `kiri schema cutout --brief --json` の `fields[]` が返す

### 4. 直す

上から順に試す。

1. **`warnings[].hint` に従う**（例: `--tolerance を上げると背景の残りが減ります`）。
   hint が無ければ schema の `warnings[].remedy` を見る
2. **どこが商品かを面で教える。** 数値の調整では直らない輪郭も、これで直る。
   preview の元画像パネルのグリッドから座標を読み、正規化座標で渡す
   ```bash
   kiri cutout in.jpg -o out.png --dry-run --preview /tmp/kiri-preview.png --force --json \
     --normalized \
     --fg-polygon X1,Y1,X2,Y2,X3,Y3,... \
     --bg-polygon X1,Y1,X2,Y2,X3,Y3,...
   ```
   `--fg-polygon` の内側は必ず商品、`--bg-polygon` の内側は必ず背景（影など、消したい所）になる。
   どちらも複数回指定できる。座標はこの画像の preview から読む（例の値を使い回さない）。
   たとえば商品の下に影が残っていれば、その帯を囲む横長の四角形を `--bg-polygon` に渡す。
   指定した多角形は、次の preview の元画像に重ねて描かれる（緑が前景、赤が背景）。
   指定がずれていないかはそこで確かめる。前景の多角形は**輪郭の少し内側**に置く
3. **探索を kiri に任せる。** `--optimize` は tolerance / bbox / 背景の持ち方を総当たりする。
   24MP で十数秒かかる
4. `--tolerance` などの数値は最後に回す。効く範囲が狭く、外すと急に崩れる

`--trimap` / `--fg-mask` / `--bg-mask` を使えば、画像で範囲を渡すこともできる。
寸法は EXIF の向きを適用した後の入力と一致させる。

### 5. 本番を書き出す

納得したら `--dry-run` を外し、最後に使った指定のまま書き出す。
利用者にも preview を見せて、仕上がりを確かめてもらう。

## モール規格に合わせる

```bash
kiri cutout in.jpg -o out.jpg --profile amazon --fail-on default --json
kiri lint existing.jpg --profile amazon --json      # 既にある画像を検査する
```

- 規格の一覧と、それぞれの条件・出典は `kiri schema --summary --json` の `profiles[]` にある
- `--profile` は、キャンバス、商品の占有率、形式、背景色、容量の既定値をまとめて決める。
  **決まった寸法は規格の要求そのものではない。** たとえば amazon は 500〜10000px の範囲を
  求めているだけである。解像度が欲しければ `--canvas 3000` のように上書きし、
  画質が欲しければ `--quality` を上げる。そのあと `kiri lint` で合格を確かめる
- `--rotate auto` は傾きを水平に戻すだけである。向きを 90 度変えたいなら、その角度を自分で足す
- 落ち影（`--shadow synth`）と反射（`--reflect on`）は、メイン画像の規格（純白背景）に
  反することがある。サブ画像として作るのがよい

## 複数枚を処理する

1 枚ずつ回すより `kiri batch` がよい。セットの中で、商品の大きさと余白を揃えられる。
spec の書き方は `kiri schema batch --brief --json` が返す。
batch は `--preview` を受けないので、うまくいかない素材は 1 枚だけ `cutout` で直してから spec に戻す。

## 素材と文字を 1 枚へ組む（見出し画像・サムネイル）

`kiri compose` は spec（JSON）に書いた通りに素材と文字を置き、**測って返す**。
組版器ではない。収まらない文字を縮めも折り返しもしない。

```bash
kiri schema compose --brief --json    # compose_spec[] が spec の形、warnings[].remedy が直し方（約 45KB）
kiri compose layout.json -o thumb.png --dry-run --json      # 画素を書かずに幾何と測りだけ
kiri compose layout.json -o thumb.png --fail-on default --json
```

- **spec の形は推測しない。** `compose_spec[]` を引いて書く。素材とフォントのパスは
  **spec からの相対**で解かれるので、spec は素材の近くに置く
- 商品の素材は先に `kiri cutout` で透過 PNG にしておき、`role: "subject"` の画像層で置く
- **字体はファイルで指す**（`font.path`）。見つからなければ `FONT_NOT_FOUND` で 1 枚も書かない。
  代わりの字体へ黙って落ちることはない。`FONT_GLYPHS_MISSING` が出たら、
  `data.missing` の文字を持つ字体に替える
- **太さは字体のファイルが持つ face でしか描けない。** 1 ウェイトのファイル（W4 など）では
  `weight` を変えても絵が変わらず、`FONT_WEIGHT_UNAVAILABLE` が出る。見出しを W8、補足を W4 に
  するような強弱は、**文字の層ごとに `font` を書いて別のファイルを指す**（書かない層は最上位の
  `font` を継ぐ）。実際に組んだ字体と描かれた太さは `layers[].font` に返る
- **`lines` は行の配列である。** 長い見出しはどこで割るかを自分で決めて行に分ける。
  行ごとの実測幅は `layers[]` に返る
- `rect` は枠で、画像は内接して縮む。実際に置かれた矩形は `layers[].placed` を見る
- **文字の `rect` を書く前に `--free-regions` で空きを引く。** 商品と文字だけを置いた spec を
  `kiri compose layout.json -o thumb.png --dry-run --free-regions --json` に通すと、
  `free_regions.regions[].rect` に**商品の不透明な画素と既存の文字から `margin`（1280 幅で 24px）
  を超えて離れた空き矩形**が面積の大きい順に返る。その 1 つを文字の `rect` にそのまま書けば、
  `LAYERS_OVERLAP` や `OUTSIDE_SAFE_AREA` で書き直す往復が要らない。外接矩形（`opaque_rect`）の
  内側の空き（瓶の肩の横など）も出る。候補が無ければ `--free-margin` を小さくするか、商品の `rect` を
  寄せて空きを作る。図形と `decoration` の画像は避けないので、帯や背景の写真の上にも候補が出る
- **位置を決めたい文字の層は spec から抜いてから引く。** 文字はどの role でも障害物になるので、
  動かしたい見出しを残すと自分の移動先を塞ぐ。`jq 'del(.layers[] | select(.id == "heading"))' layout.json > probe.json`
  のように外した spec（素材の相対パスが崩れないよう元の spec と同じ場所に置く）で引く。残した文字は避けられる
- **`TEXT_OVERFLOW` や `too_small` が出たら、`size` を当て推量で書き換えずに `--size-range` を引く。**
  `kiri compose layout.json -o thumb.png --dry-run --profile note --size-range --json` の
  `layers[].size_range` に、文字の層ごとの `{min, max, limited_by}` が返る。`min` 以上 `max` 以下の
  整数を `size` に書けば、はみ出しも `too_small` も出ない（`max` は組み直して確かめた値。`min` は
  `--profile` が無ければ null）。**`min > max` なら `size` では解けない**——`limited_by` が `width` なら
  `lines` を割り直すか `rect` を広げ、`height` なら行を減らすか `line_height` を詰めるか `rect` を高くする。
  **`max` も `limited_by` も null なら、どの整数も収まらない**（縁が太すぎて `rect` に 1 字も入らないなど）。
  `rect` を広げるか、`stroke` の `width` を細くするか外す。`limited_by` が `none` なら上限は無い。
  `stroke.width` 未満の `size` は書けない（`SPEC_INVALID`）ので、`min` がそれを下回るときは `stroke.width` 以上を書く
- **画像は透過の余白を切り詰めてから枠へ当てはめる**（既定の `"trim": "alpha"`）。`cutout` の
  `--canvas` や `--rotate` で余白が付いた素材でも、商品そのものが `rect` いっぱいに置かれる。
  余白ごと置きたいときだけ `"trim": "none"` を書く
- 商品が実際に占める矩形は `layers[].opaque_rect` を見る。`outside_safe_area` もこれで判定するので、
  透過の余白が `safe_area` の外へ出ても警告されない
- **全面に敷く背景の写真は `role: "decoration"` で置く。** decoration の画像は `safe_area` を測らない
  （`outside_safe_area` は null）ので、外へ出ても `OUTSIDE_SAFE_AREA` は出ない。切られては困る画像は `subject`
- 後の層ほど上に描かれる。文字は `layers` の末尾側に置く
- **文字の縁取りは `"stroke": {"color": "#000000", "width": 8}`。** 外側にだけ付き、字は細らない。
  `width` は 1 以上 `size` 以下。字は縁の幅だけ `rect` の内側へ寄せて組まれるので、`rect` は
  縁を含めた大きさで書く
- **帯や札は `"type": "shape"` の層**（`rect` / `color` / `opacity` / `radius`。`role` は書けない）。
  写真の上の見出しは、文字の**前**に半透明の帯を敷くと読める。文字の**後**に置くと文字を覆い、
  `TEXT_OBSCURED` が出る
- 警告はどれも `remedy` が直し方を言う。よく出るのは次のとおり

| 警告 | 直し方 |
|---|---|
| `TEXT_OVERFLOW` | `--size-range` の `max` 以下に `size` を下げる。範囲が空なら `rect` を広げるか `lines` を短くする |
| `TEXT_CONTRAST_LOW` | 文字の `color` か、その文字が載っている下地を変える（背後の実測で、しきい値は WCAG の 4.5）。写真の上で変えられなければ `stroke` で縁取るか、文字の下に `shape` の帯を敷く |
| `LAYERS_OVERLAP` | 文字の `rect` を商品の外へ寄せる。`--free-regions` の候補の 1 つを `rect` に書く |
| `TEXT_OBSCURED` | 文字の層を、覆っている層より後ろ（配列の末尾側）へ移す |
| `OUTSIDE_SAFE_AREA` | 層を `safe_area` の内側へ入れる |
| `FONT_WEIGHT_UNAVAILABLE` | その太さの face を持つファイル（または可変フォント）を `layers[].font` で指す。無ければ `weight` を `data.actual` に合わせる |

**書き出した絵を Read で開いて目で確かめる。** 見出し画像は小さいので、出力そのものを見ればよい。
利用者にも見せる。一覧に縮んだ姿は `--profile` と `--preview` で書き出せる（下の節）。

### note の見出し画像

```bash
kiri compose header.json -o header.png --profile note --fail-on default --json
```

- `--profile note` を付けると、`canvas` の寸法と `safe_area` を省ける（1280 x 670、
  上下中央の `[0, 108, 1280, 454]`）。書けば書いた値が勝つ。比が 1280:670 から外れると
  `PROFILE_ASPECT_MISMATCH`（表示側で切り取られる）
- **一覧での縮小表示（PC 192px / スマホ 144px）でも読めるかを測る。** 結果は
  `layers[].display[]` に出て、読めなければ `TEXT_ILLEGIBLE_AT_DISPLAY` が理由を言う

| 理由 | 直し方 |
|---|---|
| `too_small` | `size` を `11 × canvas の幅 / 表示幅` 以上にする（1280 幅ならスマホで 98）。`--size-range` の `min` がその値で、`max` を超えるなら `lines` か `rect` を直す |
| `strokes_lost` | `weight` を上げる（`FONT_WEIGHT_UNAVAILABLE` が出ていれば効かないので、太いファイルを `layers[].font` で指す） |
| `low_contrast` | 文字色か下地を変える。変えられなければ `stroke` で縁取る（`width` は `canvas の幅 / 表示幅` 以上。1280 幅ならスマホで 9）か、文字の下に `shape` の帯を敷く。原寸の `text_contrast` が通っていても、縮めると落ちることがある |

- 縁取りのある層の `display[]` には `against` が出る。`stroke` なら縁が縮小で残って縁と比べた、
  **`ground` なら縁が地に溶けて効いていない**（`width` を太くする）
- **`--preview` を付けると、測りが見た縮小の絵を `<出力の stem>.<表示名>.png`
  （`header.pc_list.png` / `header.mobile_list.png`）に書く。** Read で開けば「一覧でどう潰れたか」が
  見える。書いた先は結果の `previews[]` に出る。`--profile` が要り、`--dry-run` とは併用できない

- 原寸で読めても一覧では読めないことが多い。**見出しは大きく、短く、太く**が出発点になる
- `role: "decoration"` の文字は縮小表示の測りから外れる。読めなくてよい飾りにだけ使う
- compose の `--profile` は cutout / lint の `--profile` とは別の表である。
  `kiri compose --profile amazon` は断られる。compose で使える規格は `kiri schema --summary --json` の `compose_profiles[]` にある

## 切り抜けないもの

次のものは、色だけでは正しく切り抜けない。面の指示で救えないときは、無理に続けず利用者に伝える。

- 半透明の商品（ガラス瓶、透明パッケージ）
- 背景より明るい映り込み
- 輪郭と背景の色がほとんど同じもの
- 生活シーンのような複雑な背景（`--segment` を入れた build なら、モデルで補える）

**「粗い」と言われたら、原因を 4 つに切り分ける。** 輪郭（`mask` の指標と preview）、
解像度（`--canvas`）、圧縮（`--quality`）、被写体そのもの（ホコリや傷は kiri では消せない）。
