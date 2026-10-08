---
name: kiri
description: EC 商品画像を kiri CLI で加工する。背景透過の切り抜き、白背景化、モール規格（Amazon / Shopify など）に合わせたキャンバス配置、落ち影・反射の合成、AVIF / JPEG / PNG への変換、リサイズ、回転、既存画像の規格検査、複数枚の一括処理。「商品写真の背景を抜いて」「白背景にして」「Amazon 用の画像を作って」「この画像は規格を満たす？」のような依頼で使う。
---

# kiri で商品画像を作る

kiri は単色背景の商品写真を切り抜いて、Web 配信用に仕上げる CLI である。
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
- exit code の意味は次のとおり

| code | 意味 | どうするか |
|---|---|---|
| 0 | 成功 | `warnings[]` を見る |
| 2 | 引数の誤り | `error.code` があれば hint に従う。無ければ綴りの誤りなので schema を引き直す |
| 3 | 入力の異常 | HEIC なら下記。ほかは利用者に伝える |
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

1. **`warnings[].hint` に従う**（例: `--tolerance を上げると背景の残りが減ります`）
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

## 切り抜けないもの

次のものは、色だけでは正しく切り抜けない。面の指示で救えないときは、無理に続けず利用者に伝える。

- 半透明の商品（ガラス瓶、透明パッケージ）
- 背景より明るい映り込み
- 輪郭と背景の色がほとんど同じもの
- 生活シーンのような複雑な背景（`--segment` を入れた build なら、モデルで補える）

**「粗い」と言われたら、原因を 4 つに切り分ける。** 輪郭（`mask` の指標と preview）、
解像度（`--canvas`）、圧縮（`--quality`）、被写体そのもの（ホコリや傷は kiri では消せない）。
