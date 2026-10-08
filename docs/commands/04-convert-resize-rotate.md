# convert / resize / rotate

> [README](../../README.md) › [コマンド別ドキュメント](README.md) › convert / resize / rotate

## kiri convert

形式変換のみを行う。

```
$ kiri convert product.jpg -o product.avif --json
```

| オプション | 既定値 | 説明 |
|---|---|---|
| `--format` | 拡張子から推論 | `avif` / `png` / `jpeg` |
| `--quality` | 90 | 0-100。既定は実写で商品領域の PSNR が 40 dB（視覚的無損失の目安）に最も近づく値。バイトを詰めるなら `--max-bytes` |
| `--effort` | 6 | AVIFのエンコード速度 1-10。小さいほど高品質・低速 |
| `--max-bytes` | — | 出力の上限バイト数（例 `500k`）。収まるまで品質を梯子状に落とす |
| `--background` | `#FFFFFF` | 透過を保持できない形式へ出力する際の合成色 |
| `--derive SPEC` | — | 派生を 1 本ずつ指定する（[複数のサイズと形式](03-output.md#複数のサイズと形式をまとめて書く--derive)） |
| `--sizes N,N,N` | — | 幅の並び。`--formats` との直積になる |
| `--formats f,f` | — | 形式の並び |
| `--naming TEMPLATE` | `{stem}_{width}.{ext}` | 派生のファイル名の付け方 |
| `--manifest PATH` | — | 書いたものを列挙する JSON |
| `--no-color-convert` | | 埋め込み ICC を解釈せず、画素の値をそのまま使う |
| `--force` | | 出力先が既に存在する場合に上書きする |
| `--dry-run` | | 書き出さずに結果だけ返す |

`--derive` / `--sizes` / `--formats` / `--naming` / `--manifest` は `resize` /
`rotate` / `cutout` でも同じように使える。

## kiri resize

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

| オプション | 既定値 | 説明 |
|---|---|---|
| `--width` / `--height` | — | 出力の枠。一方だけならアスペクト比を保つ |
| `--fit` | `contain` | 両方指定したときの枠への当てはめ方 |
| `--allow-upscale` | | 元画像より大きくすることを許す |
| `--format` | 拡張子から推論 | `avif` / `png` / `jpeg` |
| `--quality` | 90 | 0-100。既定は実写で商品領域の PSNR が 40 dB（視覚的無損失の目安）に最も近づく値。バイトを詰めるなら `--max-bytes` |
| `--effort` | 6 | AVIFのエンコード速度 1-10。小さいほど高品質・低速 |
| `--max-bytes` | — | 出力の上限バイト数（例 `500k`）。収まるまで品質を梯子状に落とす |
| `--background` | `#FFFFFF` | 透過を保持できない形式へ出力する際の合成色 |
| `--flatten` | | 透過を残さず `--background` の色で塗り潰す |
| `--no-color-convert` | | 埋め込み ICC を解釈せず、画素の値をそのまま使う |
| `--force` | | 出力先が既に存在する場合に上書きする |
| `--dry-run` | | 書き出さずに結果だけ返す |

## kiri rotate

画像を回す。**角度は時計回りが正**で、負値は反時計回りになる。「右に傾いて
撮れたので左へ戻す」を `--angle -3` と書ける向きに合わせてある。

```
$ kiri rotate product.jpg -o product.png --angle -3
product.png  1703x2081  png  6.5 MB  (34 ms)
  回転      357°  (再サンプリング)
```

```
$ kiri rotate product.jpg -o rotated.png --angle 90 --json
{
  ...
  "rotate": {
    "angle": 90.0,
    "resampled": false
  }
}
```

`rotate` は**回転したときにしか現れないキー**である。`convert` と `resize` の
JSON には出ない。「回さなかった」と「そもそも回せない」を混同させないため、
`null` も返さない。

**`angle` は指定値ではなく実際に適用した角度**で、`[0, 360)` へ正規化した
時計回りの度数を返す。`--angle -90` は `270`、`--angle 450` は `90` になる。
指定をそのまま返さないのは、同じ操作が無数の書き方を持つためで、結果を
比較する側は正規化された 1 通りだけを見ればよい。

### 90度単位は画素を作り直さない

`90` / `180` / `270` は画素の入れ替えだけで回すため**無劣化**で、`resampled`
は `false` になる。JPEG を読んで PNG へ出す場合でも、回転そのものは色を
1 ビットも変えない。それ以外の角度は Catmull-Rom（4×4 タップ）で補間し直し、
`resampled` は `true` になる。

補間は**事前乗算アルファ**で行う。`resize` が `use_alpha` に頼っているのと
同じ理由で、素の RGB を混ぜると透明な画素の色が境界に滲む。切り抜き済みの
PNG を回すのは主用途そのものなので、ここを外すと商品の輪郭が色づく。

### 余白は透過で埋め、四隅を欠かさない

90 度単位以外では、回した絵を囲む外接矩形まで出力が広がる。**寸法は
切り上げる**——外接矩形はちょうど `w·|cosθ| + h·|sinθ|` なので、丸めると四隅が
小数画素ぶん欠ける。回転で情報が減ってはならない。

増えた余白はアルファ 0 になる。透過を持てない形式（JPEG）へ出すと
`--background` の色で塗られ、`ALPHA_FLATTENED` の警告が付く。

```
$ kiri rotate product.jpg -o rotated.jpg --angle 30
rotated.jpg  2386x2533  jpeg  688.4 KB  (78 ms)
  回転      30°  (再サンプリング)
警告: jpeg は透過を保持できないため #FFFFFF で合成しました
      透過を残すには、出力先の拡張子を .png か .avif にしてください
```

### EXIF の向きとの関係

**EXIF の回転は読み込み時に適用済み**なので、`--angle` は「見えている絵を
何度回すか」を意味する。EXIF の値に足し込むわけではない。iPhone の写真
（`exif_orientation` 6）を `--angle 90` で回せば、正立させたうえでさらに
90 度回った絵が出る。

### 切り抜きと併せるときは、切り抜いてから回す

**逆順にすると `cutout` の背景推定が壊れる。** 回転が四隅に作った余白が画像の
外周に乗り、その余白まで背景色の標本として数えられる。外周が「素材」と
「余白」の 2 種類に割れた時点で `uniformity` は落ち、単色背景で撮った写真でも
対象外と判定されうる。

| オプション | 既定値 | 説明 |
|---|---|---|
| `--angle` | — | 時計回りに回す角度(度)。負値は反時計回り。90の倍数のみ無劣化 |
| `--format` | 拡張子から推論 | `avif` / `png` / `jpeg` |
| `--quality` | 90 | 0-100。既定は実写で商品領域の PSNR が 40 dB（視覚的無損失の目安）に最も近づく値。バイトを詰めるなら `--max-bytes` |
| `--effort` | 6 | AVIFのエンコード速度 1-10。小さいほど高品質・低速 |
| `--max-bytes` | — | 出力の上限バイト数（例 `500k`）。収まるまで品質を梯子状に落とす |
| `--background` | `#FFFFFF` | 透過を保持できない形式へ出力する際の合成色 |
| `--flatten` | | 透過を残さず `--background` の色で塗り潰す |
| `--no-color-convert` | | 埋め込み ICC を解釈せず、画素の値をそのまま使う |
| `--force` | | 出力先が既に存在する場合に上書きする |
| `--dry-run` | | 書き出さずに結果だけ返す |

**一括で回すなら `cutout --rotate` を使う。** `kiri rotate` は回転だけを行う
コマンドで、`batch` の spec は `cutout` の設定を並べるものである。切り抜きと
回転を 1 本の実行に畳めば順序を間違えようがなくなるので、spec の `rotate` キーも
`cutout --rotate` へ繋がっている（[切り抜いた後に回す](05-5-finishing.md#切り抜いた後に回す--rotate)）。
