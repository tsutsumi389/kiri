# kiri

AIエージェントから使われることを前提とした、EC商品画像のための画像編集CLI。

単色背景の商品写真を対象に、背景透過の切り抜き・リサイズ・キャンバス配置・Web配信形式への変換を1コマンドで行う。

> **開発中です。** 現在 `info` / `convert` / `resize` / `cutout` が動作します。
> EC向けの整形（キャンバス配置・背景色合成）とバッチ処理は実装中です。
> 進捗は [docs/implementation-plan.md](docs/implementation-plan.md) を参照してください。

## 特徴

- **依存ゼロの単一バイナリ** — Python環境も外部ツールもMLモデルも不要。pure Rust で完結する
- **AIエージェント向けの構造化I/O** — `--json` で結果を返し、stdout はJSONのみ、ログは stderr に分離
- **決定的な動作** — 同じ入力からは常に同じ出力が得られる
- **AVIF出力** — Web配信に適した形式。1000×1000 を約57msでエンコードする

## インストール

```
cargo install --path .
```

## 使い方

### kiri info

処理の前に画像の情報を得る。AIエージェントが座標を出す前に寸法を知り、`uniformity` で
その画像が処理可能か（単色背景か）を判断するために使う。

```
$ kiri info product.jpg
product.jpg
  寸法      1600 x 2000
  形式      jpeg
  EXIF回転  1
  色空間    sRGB
  透過      なし
  背景色    #F9F9F7  (均一度 1.00)
```

```
$ kiri info product.jpg --json
{
  "width": 1600,
  "height": 2000,
  "format": "jpeg",
  "exif_orientation": 1,
  "orientation_applied": false,
  "color_space": "sRGB",
  "has_alpha": false,
  "background": { "rgb": [249, 249, 247], "uniformity": 1.0 },
  "warnings": []
}
```

`uniformity` は外周サンプルのうち推定背景色から ΔE≤5 に収まる割合。0.9 を下回る画像は
単色背景ではないため、kiri の対象外として警告が出る。

### kiri convert

形式変換のみを行う。

```
$ kiri convert product.jpg -o product.avif --json
```

| オプション | 既定値 | 説明 |
|---|---|---|
| `--format` | 拡張子から推論 | `avif` / `png` / `jpeg` |
| `--quality` | 75 | 0-100。AVIF は75を超えるとサイズが急増する |
| `--effort` | 6 | AVIFのエンコード速度 1-10。小さいほど高品質・低速 |
| `--background` | `#FFFFFF` | 透過を保持できない形式へ出力する際の合成色 |
| `--force` | | 出力先が既に存在する場合に上書きする |

### kiri resize

`--width` と `--height` の一方だけを指定すればアスペクト比を保って拡縮する。
両方指定した場合は `--fit` が枠への当てはめ方を決める。

```
$ kiri resize product.jpg -o product.avif --width 1000
product.avif  1000x1250  avif  4.0 KB  (108 ms)
```

| `--fit` | 挙動 | 1600×2000 を 1000×1000 の枠へ |
|---|---|---|
| `contain`（既定） | 枠に収まるよう縮小。アスペクト比を保つ | 800×1000 |
| `cover` | 枠を覆うよう縮小し、はみ出しを中央で切る | 1000×1000 |
| `exact` | アスペクト比を無視して枠ちょうどに変形 | 1000×1000 |

**拡大は既定で拒否する。** 要求サイズが元画像より大きい場合はエラーになる。
黙って縮めると出力が要求と食い違い、バッチ処理で気づけないため。

```
$ kiri resize small.jpg -o out.avif --width 3000 --json
{
  "error": {
    "code": "UPSCALE_NOT_ALLOWED",
    "message": "1600x2000 から 3000x3750 への拡大が必要です",
    "hint": "--allow-upscale を付けると拡大しますが、画質は劣化します"
  }
}
```

補間は Lanczos3。透過画像は事前乗算つきで補間するため、境界に背景色がにじまない。

### kiri cutout

背景を透過して商品を切り抜く。**bbox は任意**で、未指定なら全自動で判定する。

```
$ kiri cutout product.jpg -o product.png
product.png  1600x2000  png  841.4 KB  (338 ms)
  背景色    #F9F9F7  (均一度 1.00, tolerance 12)
  前景比率  21.6%
  前景範囲  528,200 - 1073,1601
```

| オプション | 既定値 | 説明 |
|---|---|---|
| `--bbox x1,y1,x2,y2` | — | この外側を無条件に背景とする。未指定なら全自動 |
| `--normalized` | | 座標を 0.0-1.0 の正規化座標として解釈する |
| `--fg-seed x,y` | — | 「ここは必ず前景」を指定する。複数回指定可 |
| `--tolerance` | 12 | 背景色との色差(ΔE)の許容量 |
| `--edge-threshold` | 8 | 輪郭でフィルを止める勾配のしきい値。0 で無効 |
| `--cleanup` | 2 | 孤立ノイズ除去と穴埋めの半径(px) |
| `--feather` | 1 | 境界フェザリングの半径(px) |
| `--debug-mask PATH` | — | 生成したマスクを PNG で書き出す |

#### 仕組み

単に「背景色に近い画素」を消すのではなく、**画像の外周から到達できる背景色領域だけ**を
消す。これにより、商品内部に背景と同じ色があっても外周から届かないため生き残る。
白背景に白い商品を置いた EC で最頻出のケースがこれで解ける。

さらに **1px あたりの輝度変化が急峻な輪郭ではフィルを止める**。落ち影は数十 px かけて
なだらかに変化するのに対し、商品の輪郭は 1px で急変するため、この差で両者を分けられる。
これがないと、影を消せる許容量の下では淡い色の商品が背景ごと消えてしまう。

#### 結果の検証

`--json` が返す `mask` を見れば、画像を開かずに失敗を検出できる。

```json
{
  "background": { "rgb": [249, 249, 247], "uniformity": 1.0 },
  "tolerance": 12.0,
  "mask": {
    "foreground_ratio": 0.2164,
    "bbox": [528, 200, 1073, 1601],
    "touches_edge": false
  },
  "warnings": []
}
```

- `foreground_ratio` が極端（0.01未満 / 0.99超）なら警告が出る
- `touches_edge` が `true` なら商品が見切れている
- `uniformity` が低ければ単色背景ではない

## 対応形式

- **入力**: JPEG, PNG
- **出力**: AVIF, PNG, JPEG

WebP は実用的なロッシー圧縮に libwebp（C）が必要なため、AVIF入力はデコードに dav1d（C）が
必要なため、いずれも非対応とした。依存ゼロの単一バイナリを優先した結果である。

## 対象範囲

**単色背景**（白・グレー等のスタジオ撮影背景）のEC商品画像を対象とする。

生活シーン写真などの複雑背景は対象外。MLモデルを使わない方針のため品質が出ない。
そうした画像は `kiri info` の `uniformity` で事前に検出でき、警告が返る。

## exit code

| コード | 意味 |
|---|---|
| 0 | 成功 |
| 1 | 一般エラー |
| 2 | 引数不正 |
| 3 | 入力ファイル異常 |
| 4 | 処理失敗 |

`--json` 指定時はエラーも JSON で stdout に返る。

```json
{ "error": { "code": "OUTPUT_EXISTS", "message": "...", "hint": "--force を付けると上書きします" } }
```

## ドキュメント

- [設計ドキュメント](docs/design.md) — 技術選定と決定の根拠
- [実装計画](docs/implementation-plan.md) — フェーズ分割と進捗

## ライセンス

MIT
