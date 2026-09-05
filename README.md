# kiri

AIエージェントから使われることを前提とした、EC商品画像のための画像編集CLI。

単色背景の商品写真を対象に、背景透過の切り抜き・リサイズ・キャンバス配置・Web配信形式への変換を1コマンドで行う。

> **開発中です。** 現在 `info` / `convert` / `resize` が動作します。主機能である `cutout` は実装中です。
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
