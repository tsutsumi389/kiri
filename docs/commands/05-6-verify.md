# cutout: 結果の検証と規格

> [README](../../README.md) › [コマンド別ドキュメント](README.md) › [kiri cutout](05-cutout.md) › 結果の検証と規格

`kiri cutout` の説明は 7 つのファイルに分かれている。全体の索引は [kiri cutout](05-cutout.md#読む順) にある。

## 結果の検証

`--json` が返す `mask` を見れば、画像を開かずに失敗を検出できる。

```json
{
  "color_space": "Display P3",
  "color_profile": "Display P3",
  "color_converted": true,
  "background": {
    "rgb": [249, 249, 247], "uniformity": 1.0,
    "perimeter_delta_e": { "p50": 0.4, "p90": 1.2, "max": 2.8 },
    "texture": { "p50": 0.0, "p90": 1.0 },
    "model": "flat", "field_range": [0.0, 0.0],
    "residual": { "p50": 0.4, "p90": 1.2, "max": 2.8 }
  },
  "subject": {
    "bbox": [528, 200, 1073, 1601],
    "normalized_bbox": [0.33, 0.1, 0.671, 0.801],
    "area_ratio": 0.2287, "capture_ratio": 0.9932, "leftover_ratio": 0.0,
    "delta_e": 68.3, "touches_edge": false, "confidence": "high"
  },
  "settings": {
    "tolerance": 12.0, "edge_threshold": 8.0,
    "step_tolerance": 2.2, "shadow_tolerance": 35.0, "seal": 1,
    "cleanup": 2, "feather": 1, "despill": true, "refine": true,
    "matting": "guided", "smooth_contour": 2.0, "reclassify": true,
    "background_model": "flat",
    "band_min_radius": 2, "smooth_radius_px": 2,
    "shadow": "off", "reflect": "off"
  },
  "color": {
    "white_balance": "auto", "exposure": "off",
    "status": "applied", "source": "field",
    "white_point": [178, 173, 164], "white_point_shift": 4.8,
    "gain": [0.9433, 1.0071, 1.1203],
    "exposure_stops": 0.0, "clipped_ratio": 0.015
  },
  "mask": {
    "foreground_ratio": 0.2164,
    "bbox": [528, 200, 1073, 1601],
    "touches_edge": false,
    "separability": 68.3,
    "halo_ratio": 0.0,
    "edge_width": 1.0,
    "contour_roughness": 0.006,
    "rim_contamination": 0.001
  },
  "canvas": {
    "width": 1000, "height": 1000, "fill_ratio": 0.85,
    "content": [333, 850], "offset": [333, 75], "scale": 0.605
  },
  "warnings": []
}
```

- `foreground_ratio` が極端（0.01未満 / 0.99超）なら警告が出る
- `touches_edge` が `true` なら商品が見切れている（ただし後述の誤診に注意）
- `subject` は商品と思われる塊の位置。意味と信頼度の判定根拠は
  [`kiri info` の節](02-info.md#subject--商品はどこにあるか)を参照
- `uniformity` が低ければ単色背景ではない。ただし**理由は 2 つある**——単色で
  ないのか、単色に照明が乗っているのか。`background.residual.p50` が
  `perimeter_delta_e.p50` よりはっきり小さければ後者で、`background.model` が
  `"field"` になって照明場が吸う（[背景は 1 色ではなく場で持つ](05-2-background.md#背景は-1-色ではなく場で持つ)）
- `separability` が `residual.p50` を下回れば、その画像は救えない。**分子と分母は
  同じモデルで測る**——`background.model` が `"field"` なら `separability` も場に
  対する色差になるので、比べる相手も場に対する分布でなければ辻褄が合わない
  （`"flat"` では `residual` は `perimeter_delta_e` と同じ値になる）
- `halo_ratio` が 0.10 を超えれば境界に背景色が残っている（警告が出る）
- `edge_width` は鮮鋭な輪郭なら 1〜3。素材の遷移が広ければ 6 前後まで伸びるのが正常で、鮮鋭なはずの輪郭で 6 を超えたらぼやけている
- `contour_roughness` が 0.16 を超えれば輪郭がギザギザに蛇行している（警告が出る）
- `rim_contamination` が 0.02 を超えれば縁に背景のテクスチャが張り付いている（警告が出る）
- `separability` / `halo_ratio` / `edge_width` / `contour_roughness` / `rim_contamination`
  は測れなければ `null`。0 ではない。`rim_contamination` は**帯の半分以上で
  判定できなかったとき**も `null` になる（分母は判定できた画素なので、
  判定不能が大半を占めたまま割合を返すと、残りについて「汚染されていない」と
  言ったことになってしまう）
- `color` は `--white-balance` / `--exposure` のどちらかに `auto` を渡した実行にだけ
  現れる。**`white_balance` / `exposure` は要求値**で、実際に当たったかどうかは
  `status` と `gain` が言う——段ごとに落ちるので、`auto` を渡したことから当たったことは
  導けない（[背景をグレーカードとして色を正す](05-1-preprocess.md#背景をグレーカードとして色を正す--white-balance----exposure)）
- `color_converted` が `true` なら、以降の数値はすべて sRGB へ変換した後の値
- `settings` は**実際に効いた**設定。結果が期待と違ったとき、指定が効いたのか
  既定のまま走ったのかを画像を開かずに切り分けられる。`batch` は `defaults` と
  項目の継承が絡むので、項目ごとの結果にも同じものが入る
- `settings.band_min_radius` は**実際に効いた帯幅の下限**(px)。指定値からは
  読めない——長辺 1000px 換算で掛け戻されたうえ、輪郭が粗ければ粗さぶんだけ
  持ち上がる（`edge_threshold` の自動調整と同じ規約）。`--no-refine` では帯
  そのものが無いので**キーごと現れない**
- `settings.background_model` は**実際に効いた背景のモデル**（`"flat"` /
  `"field"`）。既定の `auto` は `uniformity` を見てどちらかを選ぶので、指定値
  からは読めない
- `settings.smooth_radius_px` は**実際に効いた平滑化の半径**(px)。
  `--smooth-contour` は長辺 1000px 換算なので、20MP では指定値の 5 倍前後に
  なり、48px で頭打ちになる。**要求値と実効値が食い違いうるものは、両方を
  結果に出す**

`separability` は切り抜き境界の内側で測った商品と背景の色差。**`foreground_ratio` は
「どれだけ残ったか」しか言わず、その輪郭が妥当かを何も語らない。**`separability` は
「輪郭が実際の色の違いによって引かれたのか」を示す。

`--bbox` を指定した場合、矩形の辺は輪郭として数えない。bbox の外は色によらず背景と
確定させた領域なので、その境目は「どこに矩形を置いたか」でしかないためである。
**実素材では bbox 指定時の境界画素の約6割が矩形の辺そのものになり、除外しないと
値が置き場所に支配される。**

これが背景自身のばらつき（`perimeter_delta_e.p50`）を下回る場合、背景を飲み込める
tolerance は商品も飲み込む。**両立する値が存在しないため、パラメータ調整を続けても
無駄である。**この状況では警告が出るので、AI は再試行を諦めて素材の撮り直しを
提案できる。

```
警告: 商品と背景の色差 (ΔE 12.1) が背景自身のばらつき (ΔE 21.5) を下回っています。
      背景を消せる tolerance では商品も消えるため、パラメータ調整では改善しません
```

`halo_ratio` は境界近傍で不透明なのに、元画素の色が局所背景と見分けがつかない
（ΔE≤3）画素の割合。**`separability` は境界の内側を測るため、前景の外側に張り付いた
背景色の縁を検出できない。**この縁は白背景では見えず、納品先が黒や色付きの下地だった
ときに初めて輪郭の光として現れる。書き出しの時点で知らせる必要がある。

```
警告: 境界の 12% が背景色のまま不透明で残っています (halo_ratio=0.12)。
      白以外の下地に載せると輪郭が光ります
```

`edge_width` は境界法線方向にアルファが 0.9 から 0.1 へ落ちるまでの幅(px)の中央値。
refine 済みの鮮鋭な輪郭はちょうど 1.00 になり、**1〜3 なら鮮鋭**である。ただし
**大きい値がそのまま欠陥を意味するわけではない。**8px かけて溶ける素材では 6.5 が
正解であって、素材の遷移幅に追従した結果である。**鮮鋭なはずの輪郭で 6 を超えたら**
ぼやけていると読む。
`null` は「値が 0」ではなく「遷移を1本も追えなかった」で、前景が無いか、
商品が見切れていて輪郭が画像の中に存在しない場合に出る。`halo_ratio` も同様に、
測る境界が無ければ `null` を返す。**0 と `null` は区別する。**0 と報告すると
「縁が残っていない」という良い結果に見えてしまうためである。

### `halo_ratio` と `edge_width` では見えない 2 つの欠陥

実写（白い不織布の上の黒いリモコン、20MP）を最良設定で切り抜くと
`halo_ratio` 0.0011 / `separability` 54.7 と**どちらも合格を返す**のに、拡大すると
上辺・下辺がギザギザで、不織布の灰色の粒が輪郭に張り付いている。

- `halo_ratio` は「局所背景と ΔE≤3」という絶対的な基準で縁を数える。繊維の
  ばらつきが ΔE 5〜10 ある布では、張り付いた繊維がその基準を外れて数から漏れる
- `edge_width` はアルファ**遷移の幅**しか見ないので、輪郭が輪郭に沿って
  蛇行していても値が動かない

そこで 2 つ足した。

`contour_roughness`（単位 `px_at_1000`）は、二値の輪郭が「それを滑らかにした参照
輪郭」からどれだけ離れているかの**平均**。実素材に正解は無いが、**自分自身を
ぼかしたもの**なら必ず作れる。滑らかな輪郭はぼかしても 0.5 の等高線が動かないので
0 に近く、蛇行していればその分がそのまま距離になる。

**長辺 1000px へ縮めたときの px で報告する。** EC の納品先はその寸法で、20MP の
1px のギザギザは縮めれば 0.18px となって見えない。きれいに解けた合成シーンは
0.01 前後、実写の布でフィルが届かなかった結果は 0.32 以上、20MP の実写を最良設定で
切り抜いた結果は 0.39 になる。**0.06 未満なら滑らかと読んでよい。**

平均であって中央値ではない。チャンファー距離は 1px 刻みでしか測れないので、
中央値は「0 か 1px か」の 2 値にしかならず、しきい値がその段の上に乗ってしまう
（長辺 6600px を超える素材では中央値 2px 以上でないと発火しない、という解像度
依存がそこから出ていた）。平均なら「輪郭画素の何割が参照から離れているか」が
連続量として出る。**ただし画素ごとの距離は平滑化が届く距離（箱ぼかし 3 回ぶんの
3r、換算 6px 相当）で頭打ちにする。** 参照輪郭が消えた場所では距離が伸び続けて
チャンファーの飽和値（85px）に達し、そのまま平均に入ってしまう——その値は
輪郭の粗さではなく u8 の上限である。

**幅 3px 級の細部は粗さとして数える。** 平滑化参照は σ = 2px（換算）でぼかすので、
ストラップやひもは参照から消え、その分がまるごと距離になる。合成の 3px ストラップ
（S5）で 0.08 で、しきい値 0.16 は下回るが、いちばん近いクリーン点になる。

`rim_contamination`（単位 `ratio`）は、境界の内側 3px（長辺 1000px 換算）の帯に
ある前景画素のうち、**元の色が局所前景より局所背景にはっきり近い**ものの割合。
2 択の最近傍分類なので、絶対値の基準を持たない——背景のばらつきが大きくても
「商品の色ではないもの」を数えられる。

**近さは、局所前景・局所背景それぞれの散らばり（σ）で正規化してから、線形 RGB で
比べる。** 平均色までの距離をそのまま比べると、不織布の暗い孔や繊維の影が「黒い
商品との混色（アルファ 0.3〜0.6）」と区別できない。区別できるのは「その色は背景
テクスチャの**散らばりの範囲内**か」だけである。σ に 0.015 の下駄（JPEG のノイズ床）を
履かせておくのは、単色で撮れた背景で σ が 0 になると正しい混色画素まで「散らばりの
外」へ出てしまうためである。線形で測るのは、合成が光の量の足し算だからで、
sRGB のまま比べると**ガンマぶんだけ暗い側へ偏る**（黒い商品と白い背景を真アルファ
0.5 で混ぜた画素は、sRGB では背景に 2.8 倍近く見える）。

**判定できない画素は数えない。** 局所前景と局所背景が散らばりの中で重なっていれば
（淡色商品 × 白背景）、どちらに近いかは答えようがない。分母は判定できた画素なので、
**帯の半分以上で判定できなければ値ではなく `null` を返す**。

```
警告: 境界が滑らかではありません（長辺 1000px 換算で 0.39 px のギザギザ）。
      背景のテクスチャが輪郭に乗っている可能性があります
警告: 境界の内側 9.6% が、商品の色より背景の色に近いままです (rim_contamination=0.096)。
      輪郭に背景のテクスチャが張り付いている可能性があります
```

`CONTOUR_ROUGH` には**実行時のヒントを付けない**。粗さを直す数値のノブは無く、
**実行できない助言は助言が無いより悪い**（信頼度 low で bbox を勧めないのと
同じ判断である）。直す手は面の指示——輪郭の帯だけを不明にした `--trimap`——で、
渡す画像は素材ごとに作るものなので、その手順は `kiri schema` の
`warnings[].remedy` が配る
（[警告と次の一手](#警告と次の一手remedy)）。`RIM_CONTAMINATED` のヒント（`--tolerance` を上げる、文面は
`HALO_REMAINS` と同じ）は、**`HALO_REMAINS` が一緒に出ているときだけ**付く。
縁が「背景色のまま」残っているなら上げれば減るが、汚染だけが出ている状態は別物で、
中間グレーの商品に落ち影がかかったケースでは tolerance をどちらへ動かしても値が
動かない——縁に乗っているのが背景色そのものではなく、影や繊維との混色だからである。

実写のリモコン（20MP）では `--tolerance 60`（最良）で `contour_roughness` 0.391 /
`rim_contamination` 0.096、40 まで落とすと 1.214 / 0.161 になる。**最良設定でも
両方が発火する**——旧来の指標は同じ画像を「合格」と呼んでいた。
較正の詳しい表は [設計ドキュメント](../design.md) の 4.8 にある。

`rim_contamination` には**見えない欠陥がある**。マスクが背景を大きく飲み込むと、
窓の中の「確定前景」そのものが背景色になり、帯の画素は素直に前景寄りと出る
（飲み込みが極端なら、判定できる画素が半分を割って `null` になる。同じリモコンを
`--tolerance 12` で回すとそうなる）。`BBOX_RECOMMENDED` や `HALO_REMAINS` が
同時に出ているときは、そちらを先に読むこと。

## warnings は機械可読である

警告はエラーと同じ形をしている。**日本語の散文を文字列マッチさせる必要は無い。**

```json
"warnings": [
  {
    "code": "BBOX_RECOMMENDED",
    "message": "背景が均一でないため背景側が前景として残っています",
    "hint": "--bbox 0,0.354,0.9834,0.662 --normalized を指定してください",
    "data": {
      "normalized_bbox": [0.0, 0.354, 0.9834, 0.662],
      "foreground_ratio": 0.5305
    }
  }
]
```

- `code` — 分岐に使う識別子。**文言は推敲で変わるが、これは契約として動かさない**
- `message` — 人間向けの説明。テキスト出力では `警告: <message>` として出る
- `hint` — 次に打つ手。無ければキーごと消える（`null` は出さない）
- `data` — 判断に使った数値そのもの。`message` から正規表現で抜き直さずに済む。
  無ければキーごと消える

| code | 意味 |
|---|---|
| `LOW_UNIFORMITY` | 背景の均一度が低い（単色背景ではない） |
| `BBOX_RECOMMENDED` | 背景が不均一で背景側が前景として残っている。bbox で解ける |
| `SUBJECT_TOUCHES_EDGE` | 前景が画像の外周に接している（商品の見切れ） |
| `NOT_SEPARABLE` | 主体と背景の色差が背景自身のばらつきを下回る。調整では改善しない |
| `FOREGROUND_TOO_SMALL` | 前景比率が小さすぎる。商品が消えている可能性がある |
| `FOREGROUND_TOO_LARGE` | 前景比率が大きすぎる。背景が残っている可能性がある |
| `HALO_REMAINS` | 境界に背景色のままの縁が残っている。`--tolerance` を上げると減る |
| `CONTOUR_ROUGH` | 輪郭がギザギザに蛇行している。背景のテクスチャが輪郭に乗っている |
| `RIM_CONTAMINATED` | 縁の色が商品より背景に近い（`HALO_REMAINS` も出ていれば `--tolerance` で減る） |
| `EDGE_THRESHOLD_RAISED` | 背景のテクスチャに合わせて堤防を引き上げた |
| `MATTING_NOT_CONVERGED` | `--matting closed-form` が反復の上限で止まった。帯のアルファは解き切れていない |
| `BACKGROUND_FIELD_USED` | 背景が均一でないため、1 色ではなく照明場として推定した（直すものは無い） |
| `BACKGROUND_FIELD_SKIPPED` | 外周の帯の大半が背景でないため、照明場を諦めて 1 色で測った |
| `MASK_ORIENTATION_IGNORED` | 指示の画像が EXIF Orientation を持つが、マスクは生の画素として読むので適用していない |
| `CONSTRAINT_EMPTY` | 渡した空間的な指示が 1 画素も塗らなかった（空のマスク、画像の外だけを指す多角形） |
| `OPTIMIZE_NO_CLEAN_CANDIDATE` | `--optimize` が候補をすべて試しても致命的な警告が残った（調整では解けない） |
| `SEGMENT_UNCERTAIN` | モデルが対象を掴めておらず、不明の帯が広すぎる（結果は `--segment off` に近づく） |
| `MODEL_PATH_IGNORED` | `--model-path` を渡したが `--segment off` なのでモデルを読んでいない |
| `MODEL_SIZE_UNEXPECTED` | `--model-path` のファイルが既知のモデルと大きさが違う（指定を尊重してそのまま読んだ） |
| `MODEL_DIGEST_UNEXPECTED` | `--model-path` のファイルのダイジェストが既知のモデルと違う（指定を尊重してそのまま読んだ） |
| `CANVAS_UPSCALED` | キャンバス配置で商品を拡大した |
| `DRY_RUN_OUTPUT_EXISTS` | `--dry-run` の出力先が既にある。本番実行には `--force` が要る |
| `UPSCALED` | `resize` / 派生で元画像より大きくした |
| `ALPHA_FLATTENED` | 出力形式が透過を保持できないので合成した |
| `QUALITY_REDUCED` | `--max-bytes` に収めるため要求品質から品質を落とした |
| `MAX_BYTES_UNREACHABLE` | 下限品質でも `--max-bytes` に届かなかった（要求品質のまま書いた） |
| `ICC_NOT_EMBEDDED` | 画素が sRGB でないので ICC を埋め込まなかった（AVIF は名乗ったまま） |
| `PREVIEW_FAILED` | プレビューを書き出せなかった（成果物自体は書けている） |
| `COLOR_PROFILE_UNSUPPORTED` | ICC が LUT 型などで sRGB へ変換できなかった |
| `COLOR_CONVERSION_SKIPPED` | `--no-color-convert` により変換していない |
| `COLOR_SPACE_UNCALIBRATED` | EXIF が uncalibrated で ICC も無い |
| `MANIFEST_PARTIAL` | 一部の項目が失敗したまま `--manifest` を書いた（成功分だけが載っている） |
| `PROFILE_OVERRIDDEN` | `--profile` が求めた値を明示指定が押しのけた（`--output` の拡張子と、`--derive` / `--sizes` / `--formats` が書いた形式・寸法を含む）。項目ごと・派生ごとに 1 件出る |
| `PROFILE_UNCHECKABLE` | 画素を読まないと測れない項目を `kiri lint` が検査していない（AVIF）。飛ばした項目は `data.checks` にある |
| `ROTATE_AUTO_SKIPPED` | `--rotate auto` を適用しなかった（0 度のまま）。`data.reason` が `no_subject` / `low_confidence` / `not_measurable` / `not_rectangular` のどれかを言う |
| `SET_SCALE_CLAMPED` | `batch` の `set` が求めた占有率が 1.0 を超えたので 1.0 で止めた（その点だけ目標の高さに届いていない）。不足分は `data.height_shortfall` |
| `SET_NOT_MEASURED` | `batch` の `set` が 1 点も代表寸法を測れなかったので揃えていない（各項目は自分で解決した `fill_ratio` のまま） |
| `WHITE_BALANCE_SKIPPED` | 背景が中性でないため白点を当てなかった。`data.reason` が `not_neutral` / `no_material` / `gain_out_of_range` / `would_clip` のどれかを言う |
| `EXPOSURE_SKIPPED` | 背景の水準から露出を正せなかった。`data.reason` が `not_light` / `no_material` / `gain_out_of_range` / `would_clip` のどれかを言う |
| `TEXT_OVERFLOW` | `compose` の文字が `rect` に収まらなかった。**縮めても折り返してもいない**（`data.text_overflow` が px で不足を言う） |
| `TEXT_CONTRAST_LOW` | `compose` の文字と、その文字が実際に載っている背後とのコントラスト比が低い（`data.text_contrast` が実測値） |
| `LAYERS_OVERLAP` | `compose` の文字が `subject` の不透明部分と重なっている（`data.layer_overlap` が文字の面積に対する比） |
| `OUTSIDE_SAFE_AREA` | `compose` の要素が `safe_area` の外へ出た（表示側で切られる範囲にある） |
| `TEXT_OBSCURED` | `compose` の文字が**後の層に覆われている**。`text_contrast` は背後を測った値なので、覆われた文字でも高い値を返す（`data.text_obscured` が覆われた比） |
| `TEXT_NOT_RENDERED` | `compose` の文字が 1 画素も描かれなかった。`rect` がキャンバスの外にあるか、字体にその文字のグリフが無い |
| `FONT_GLYPHS_MISSING` | `compose` が要求した字体に、spec の文字のグリフが無い。描けば豆腐が並ぶ（`data.missing` が欠けている文字を言う） |

この表は `kiri schema --json` の `warnings[]` が同じものを返す。**README を読ませる
代わりにそれを引けばよい。**

### 警告と次の一手（remedy）

`hint` が付かない警告でも、次の一手は `kiri schema` の `warnings[].remedy` に
ある。**実行時の `hint` と役目が違う。** `hint` はその実行の数値を埋めた 1 手
（`--bbox 0,0.354,0.9834,0.662 --normalized`）で、`remedy` は数値を持たない
一般の手順である（`hint の --bbox <値> --normalized をそのまま渡す`）。

```
$ kiri schema cutout --brief --json | jq '.warnings[] | select(.code == "CONTOUR_ROUGH")'
{
  "code": "CONTOUR_ROUGH",
  "summary": "輪郭がギザギザに蛇行している。背景のテクスチャが輪郭に乗っている",
  "remedy": "数値の調整では直らない。--debug-mask のマスクを外部の道具で縮めて --trimap を作る。…"
}
```

- **欄は全警告で必須である。** 書き忘れるとコンパイルが通らない。直すものが無い警告は
  `None` と明示し、そのときだけ `remedy` をキーごと持たない（`BACKGROUND_FIELD_USED` /
  `BACKGROUND_FIELD_SKIPPED` / `EDGE_THRESHOLD_RAISED` / `COLOR_CONVERSION_SKIPPED` /
  `QUALITY_REDUCED`）
- **勧めるオプションの綴りは実在する。** remedy に書いた `--オプション` がどの
  コマンドにも無ければテストが落ちる。改名した日に古い綴りを勧め続けることは無い

## 合否を exit code で返す（`--fail-on`）

警告は出るが、そのままでは**合否にならない**。`--fail-on` は既にある判断を
終了コードへ繋ぐ。**新しい指標もしきい値も増えていない。**

```
$ kiri cutout product.jpg -o out.png --fail-on 'default,halo_ratio>0.05'
...
  規格      **不合格**  (--fail-on default,halo_ratio>0.05)
    x halo_ratio 0.1234 > 0.05  HALO_REMAINS
$ echo $?
5
```

書式はカンマ区切りで、3 種類のトークンを混ぜて書ける。

| 形 | 意味 |
|---|---|
| `default` | 既定の合格条件（下記） |
| `<指標><演算子><値>` | 数値の指標にしきい値を置く（`halo_ratio>0.10`） |
| `touches_edge` | 真偽の指標。外周に接していたら不合格 |

- 演算子は `>` `<` `>=` `<=` の 4 つ。**これに触れたら不合格**である
  （`halo_ratio>0.10` は「0.10 を超えたら落とす」）
- **発火しえない条件は断る。** `halo_ratio>1.0` は値域の中だが、割合は 1 を
  超えないので永久に落ちない——`halo_ratio>2` を断るのと同じ理由で、書式の
  誤りとして exit 2 になる。比率を % と取り違えた `foreground_ratio>1.0` が
  黙って全件を通し続けるのを防ぐためである。`1` ちょうどを落とすなら
  `halo_ratio>=1.0` と書く（逆向きの「必ず落ちる条件」は 1 枚目の exit 5 で
  気づくので断らない）
- **真偽の指標に `=` は付けない。** `touches_edge` とだけ書けば「外周に接して
  いたら不合格」である。向きを選べる形（`=true` / `=false`）にすると、
  **唯一意味のある向きがどちらか読めなくなる**——「接していないことを咎める」
  指定を欲しがる利用者はいない。`>` が「触れたら不合格」と読めるのと同じ
  素直さを真偽の指標にも与える
- 指標の名前は**結果 JSON の `mask.*` のキーそのまま**である
  （`foreground_ratio` / `separability` / `halo_ratio` / `edge_width` /
  `contour_roughness` / `rim_contamination` / `touches_edge`）。短縮形は無い
  ——JSON から読んだ語をそのまま書けることのほうが、打鍵の短さより重い
- `default` は他と混ぜられる。同じ指標が `default` と明示の両方に現れたら
  **明示が勝つ**（`--tolerance` を明示すると探索の軸から外れるのと同じ規約）。
  **勝つのは指標ごとである**——`--fail-on 'default,foreground_ratio>0.9'` は
  `FOREGROUND_TOO_LARGE` だけでなく `FOREGROUND_TOO_SMALL` の検査も置き換える。
  同じ指標の二重指定は断るので、書き足して取り戻すことはできない
- **測れなかった指標（`null`）は不合格**である。`separability` の `null` は
  「前景が無い」、`halo_ratio` の `null` は「測る境界が無い」で、どちらも黙って
  合格を出してよい状態ではない。ただし「しきい値を超えた」とは別の事実なので、
  `status` は `unmeasurable` と名乗って `fail` と区別する
- 書式や値の誤りは `INVALID_FAIL_ON`。**CLI では clap が先に断る**ので
  code を伴わない exit 2 になり（`--max-bytes` / `--derive` と同じ）、
  `INVALID_FAIL_ON` が出るのは `batch` の spec 経由だけである

`default` は **`FATAL_CODES` ∪ `QUALITY_CODES` のいずれかが出たら不合格**とする。

```
NOT_SEPARABLE / FOREGROUND_TOO_SMALL / FOREGROUND_TOO_LARGE /
SUBJECT_TOUCHES_EDGE / BBOX_RECOMMENDED / HALO_REMAINS /
CONTOUR_ROUGH / RIM_CONTAMINATED
```

これは `--optimize` が「きれい」と呼ぶ**較正済みの集合に、測れなかった指標
（`null`）を足したもの**である。`Trial::clean()` は測れなかった指標を「きれい」と
数えるが、`--fail-on default` はそれを不合格にする——そこだけが違う。
別の集合を定義すると、`--optimize` が「きれいな候補が見つかった」と言った結果を
`--fail-on default` が落とす、という食い違いが起こりうる。同じ問い（この切り抜きは
納品してよいか）に 2 つの答えを持たせない。

判定の内訳は結果 JSON の `compliance` に出る。**`--fail-on` を渡したときだけ現れ、
渡さない実行の結果は 1 バイトも変わらない**（`optimize` / `segment` と同じ規約）。

```json
"compliance": {
  "fail_on": "default,halo_ratio>0.05",
  "passed": false,
  "code": "QUALITY_GATE_FAILED",
  "checks": [
    { "name": "halo_ratio", "status": "fail", "operator": "gt",
      "threshold": 0.05, "actual": 0.1234, "code": "HALO_REMAINS" },
    { "name": "separability", "status": "unmeasurable", "operator": null,
      "threshold": null, "actual": null, "code": "NOT_SEPARABLE" },
    { "name": "contour_roughness", "status": "pass", "operator": "gt",
      "threshold": 0.16, "actual": 0.04, "code": "CONTOUR_ROUGH" }
  ]
}
```

- `fail_on` は利用者が書いた文字列そのまま。**何を頼んだかが結果だけで分かる**
- `passed` は「`fail` も `unmeasurable` も 1 つも無い」こと
- `checks[]` には**評価したものを全部載せる**（`pass` も）。落ちたものだけを
  載せると、「見た上で通った」と「そもそも見ていない」が区別できない。
  **並びは決定的**で、指定の順には依らない
- **`name` は一意ではない。** `default` は指標ではなく code ごとに 1 行出すので、
  `foreground_ratio` が 2 行、`touches_edge` が 2 行並ぶ。`name` をキーにして
  畳むと片方が黙って消える——**一意なのは `code` のほう**である
- **`default` の `status` と `actual` は出どころが違う。** 合否は実際に出た警告
  （生の値で判定）から取り、`actual` は小数第 4 位で丸めた `mask.*` の値である。
  4 桁目で両者がずれうるので、生の `halo_ratio` が 0.10003 なら
  `{"status":"fail","threshold":0.1,"actual":0.1}` という、自分で
  `actual > threshold` を確かめると食い違って見える行が出る（窓は 5e-5 幅）。
  明示のしきい値にはこのずれが無い。また外周接触の 2 つは片方の警告しか出ないので、
  `actual` が `true` なのに `status` が `pass` の行（もう片方の code）も並びうる
- キーは常に出し、当てはまらないところは `null`。固定のしきい値を持たない条件
  （`NOT_SEPARABLE` は画像ごとの `background.residual.p50` と比べ、外周接触の 2 つは
  複合条件）と、真偽の指標では `operator` も `threshold` も `null` になる
- `code` は不合格のときだけ `QUALITY_GATE_FAILED` を名乗る。**exit 5 は結果 JSON を
  エラーに差し替えない**ので、`kiri schema --json` の `errors[]` が配る語彙と
  結果を突き合わせられる場所がここになる

**exit 5 でも結果 JSON は通常どおり全部返る。** 処理は成功していて成果物も
書かれている——`outputs[]` も `mask` も `background` もそのままである。
5 は「やり直せば直る失敗」（exit 4）ではなく、**人が見るべき結果**を指す。

## 規格をプリセットで指定する（`--profile`）

モール規格は「1600px の JPEG を白背景で、占有率 85%」のような**複合指定**である。
`--profile` はそれに名前を付けたもので、`--canvas` / `--fill-ratio` / `--format` /
`--background` / `--flatten` / `--max-bytes` の 6 つをまとめて決める。

```
$ kiri cutout product.png -o out.jpg --profile amazon
out.jpg  2000x2000  jpeg  125.0 KB  (544 ms)
  背景色    #F9F9F7  (均一度 1.00, tolerance 12)
  外周ΔE    p50 0.0  p90 0.0  max 0.0
  外周勾配  p50 0.0  p90 0.0
  前景比率  63.8%
  境界色差  ΔE 72.2  (tolerance 12)
  輪郭粗さ  0.00 px  (1000px 換算, 警告 0.16 超)
  縁の汚染  0.0%  (警告 2.0% 超)
  前景範囲  240,190 - 2159,1709
  キャンバス 2000x2000  占有率 86%  配置 1720x1362 @ 140,319  (倍率 0.90)
  主体候補  0.09,0.086,0.91,0.914  (面積 64.5%, 信頼度 high, colour 由来)
  傾き      --rotate 0 で水平になる
```

| name | revision | 何を要求するか | 出典 |
|---|---|---|---|
| `amazon` | 2026-09 | 長辺 500〜10000px／純白背景（ΔE76 2.0 以内）／占有率 85% 以上／JPEG・PNG／透過なし／sRGB | [Amazon Seller Central](https://sellercentral.amazon.com/help/hub/reference/external/G1881) |
| `shopify` | 2026-10 | 長辺 5000px 以下／25MP 以下／20MB 未満／PNG・JPEG・WebP（構図の規定なし） | [Shopify Help Center](https://help.shopify.com/en/manual/products/product-media/product-media-types) |
| `square-white` | 2026-09 | 正方形／長辺 1000px 以上／純白背景／占有率 85% 以上／JPEG・PNG／透過なし／sRGB | kiri 自身の定義（モール規格ではない） |

条件そのもの（数値・出典・版）は `kiri schema --json` の `profiles[]` が機械可読で
配る。**この表を読ませる代わりにそれを引けばよい。**

- **`revision` は kiri がその規格を写し取った時点である。** モール規格は変わるので、
  古い kiri が古い規格で合格を出すことは避けられない。効いた profile は結果の
  `settings.profile` が名前と版で言うので、**鮮度は呼び出し側が判断する**
- **`shopify` は構図を規定しない。** 背景色も占有率も検査せず、書く側でも決めない
  ——ストアの見せ方は出店者が決めるもので、kiri の好み（白背景・占有率 85%）を
  規格の顔をして押し付けない。透過もそのまま残る
- **占有率は「見える範囲」で数える。** 背景色を要求する規格は `--flatten` を
  立てるので、上の `配置`（`canvas.content`）にはフェザーの見えない縁が含まれる。
  `配置 ÷ キャンバス` が `占有率` より少し大きく出るのはそのためで、理由は
  「EC向けの整形」の節に書いた
- **書く側だけである。** 出来上がったファイルが規格を満たしているかは
  [`kiri lint`](07-lint.md#kiri-lint) が同じ表を見て答える
- **`cutout` にしかない。** profile は占有率（`--fill-ratio` と `--canvas`）を含む
  複合指定で、`convert` / `resize` / `rotate` には占有率を実現する手段が無い。
  半分だけ効く指定は「指定したのに効かない」を作る

**キャンバスの寸法は入力から決まる。** `--canvas` を指定しない実行で profile が
選ぶのは、**占有率を通したときに拡大にならない最大の段**で、梯子は
`1000 / 1500 / 2000 / 2500 / 3000` である。上の実行が 2000x2000 になったのは、
切り抜いた商品の長辺が 1920px で、占有率 86% を通すと 2000 までは縮小のまま
（`2000 × 0.86 = 1720 ≤ 1920`）、2500 では拡大になるからである。

- **見るのは切り抜き後の商品の外接矩形であって、画像の長辺ではない。** 商品が
  小さく写っている素材で拡大を見逃さないため
- **段に丸めるのは、同じ撮影セットなら同じ段に落ちて寸法が揃うようにするため。**
  入力ごとに連続の値を返すと、`--fill-ratio` が揃えようとしているものが崩れる。
  **揃うのは同じ段に落ちる限りで、段の境界を跨ぐ素材が混ざると揃わない**
  ——占有率 86% で寸法が動く境界は商品の長辺 1290 / 1720 / 2150 / 2580 px の 4 本で、
  1289px なら 1000、1290px なら 1500 になる。長辺は切り抜きの結果なので
  `--tolerance` や `--feather` が 1 画素動かせば飛ぶ。**寸法を必ず揃えたい実行は
  `--canvas` を明示すること**
- **天井は 3000。** 占有率 86% を通すと商品の長辺が 2580px になり、4K
  ディスプレイの短辺 2160px で全画面表示しても等倍を割らない。これ以上大きく
  しても見る側の画素数を超えるだけである
- **最小段 1000 でも拡大になる素材は、拡大したうえで `CANVAS_UPSCALED` で
  報せる。** 倍率は固定 1600 だった頃より必ず小さくなる（`--profile amazon
  --optimize --rotate auto` の実測で、700x525 の入力が 2.6411 倍 → 1.6507 倍）
- 選んだ段は最後に規格の上下限と総画素数へ押し込むので、**`kiri lint` に
  矛盾する寸法は出ない**
- `--canvas` を明示すればそちらが勝ち、`PROFILE_OVERRIDDEN` が profile の
  求めた値と並べて言う。**この 1 件だけは warnings の末尾寄り**に出る
  ——profile が求めた寸法は切り抜いた後にしか分からないためである

**優先順位は「明示指定 > profile > 既定」の 1 本だけ**である。明示した項目は
profile より強く、押しのけが起きた項目ごとに `PROFILE_OVERRIDDEN` が
「profile が求めた値」と「実際に効いた値」を並べて出す。

```
$ kiri cutout product.png -o out2.png --profile amazon
out2.png  2000x2000  png  83.6 KB  (524 ms)
  背景色    #F9F9F7  (均一度 1.00, tolerance 12)
  ...
  キャンバス 2000x2000  占有率 86%  配置 1720x1362 @ 140,319  (倍率 0.90)
警告: --profile amazon は --format jpeg を求めましたが、出力先 out2.png の拡張子が示す png で書きます
      profile の形式で書くなら出力先の拡張子を .jpeg にしてください（拡張子と中身が食い違うファイルを作らないため、拡張子は形式の明示指定として扱います）
```

**形式だけは `--output` の拡張子も明示指定として数える**ので、そこだけ
「`--format` > `--output` の拡張子 > profile > 既定」の 4 段になる。優先順位に
素直に従って profile が勝つと、`-o out.png --profile amazon` が **`.png` という
名前のファイルに JPEG を書く**。拡張子と中身が食い違えば `outputs[].path` が
嘘をつき、配信側も他のツールも拡張子で形式を判断するので、その嘘は kiri の外まで
運ばれる。profile の形式で書くなら拡張子をそちらへ揃えること。

`PROFILE_OVERRIDDEN` の `data` は spec の綴りで項目を名乗る。

```json
{ "code": "PROFILE_OVERRIDDEN",
  "data": { "key": "format", "profile": "jpeg", "used": "png" } }
```

**同じ値に落ち着いた項目では黙っている。** この警告が答えているのは「profile を
指定したのに効かなかった項目はどれか」であり、`-o out.png --profile shopify`
（shopify は PNG を第一候補にする）のように同じ値なら、並べても読む側の次の一手は
1 つも変わらない。

**拡張子が未知の綴りなら、profile を付けても断る。** `-o out.xyz` は `--profile` の
有無に関わらず `UNKNOWN_OUTPUT_FORMAT`（exit 2）である。ここで profile に形式を
決めさせると、`--profile` を付けただけで `.xyz` という名前の JPEG が黙って書かれる
——拡張子と中身を食い違わせないという上の判断を、同じ指定の別の綴りで破ることに
なる。拡張子を**綴っていない**パス（`--naming` の雛形）では今までどおり profile が
形式を決める。

**`--derive` / `--formats` が書いた形式は、上の 4 段を 1 つも通らない。** 派生は
自分の `format` を持てるので、`--profile amazon --derive 'format=avif'` は profile の
形式指定を丸ごと迂回する。**そのまま黙って通すと、amazon で書いたものが同じ
amazon の `kiri lint` で落ちる**ので、許容の外へ出た派生ごとに 1 件ずつ報せる。

```json
{ "code": "PROFILE_OVERRIDDEN",
  "data": { "key": "format", "profile": ["jpeg", "png"], "used": "avif",
            "derive": 0, "role": "hero" } }
```

パスはまだ綴れない（多派生の名前は最終画像の寸法が決まってから決まる）ので、
`derive`（`outputs[]` と同じ並びの添字）と、書いてあれば `role` でどの出力かを指す。
**`format` を書かなかった派生には profile の形式が継承される**——派生は
`--output` の解決結果（`--format` > 拡張子 > profile）を継ぐので、迂回しうるのは
形式を自分で書いた派生だけである。

**寸法も同じように迂回する。** `--derive 'width=400'` / `--sizes 400` は
`--canvas` も `--longest-side` も通らないので、`--profile amazon --sizes 400` は
400x400 を書く——amazon は長辺 500 以上を求めるので、**その出力は同じ amazon の
`kiri lint` で `longest_side` が fail になる**。形式の迂回と違って出力が実際に
規格違反になるので、こちらは同じ形で派生ごとに 1 件ずつ報せる。

```json
{ "code": "PROFILE_OVERRIDDEN",
  "data": { "key": "longest_side", "profile": { "min": 500, "max": 10000 },
            "used": 400, "derive": 0 } }
```

照らすのは `longest_side_min` / `longest_side_max` / `max_pixels` の 3 つ
（`key` はそれぞれ `longest_side` / `max_pixels`）で、**実際に書く寸法で照らす。**
`width` だけを書いた派生が何 px になるかは最終画像の縦横比・`fit` ・
`allow_upscale` で決まるので、指定した数をそのまま規格に当てると、縦長の素材で
長辺 1200 になる実行にまで「規格の外です」と言うことになる。

**`width` も `height` も書かなかった派生は対象外である**——その派生は最終画像を
そのまま書き、最終画像の寸法は profile が `--canvas` / `--fill-ratio` /
`--longest-side` を決めた結果なので、規格の寸法はそこから継承されている
（`format` を書かなかった派生が `--output` の解決結果を継ぐのと同じ関係）。

**`--profile` を渡さない実行は 1 バイトも変わらない。** 結果 JSON に
`settings.profile` は現れず、成果物も profile を足す前と同じである
（`optimize` / `compliance` と同じ規約）。

### 楽天と Yahoo! ショッピングを載せていない理由

両社のガイドライン本文は**ログインの内側にあり、一次情報として読めない。**
出典の無い数値を `kiri schema` が配ると、不合格の根拠を利用者が辿れない
——「kiri がそう言うから」以上のことが言えない合否に、納品を止める重みは無い。
`revision` を持つ設計なので、一次情報が手に入った時点で足せる。**推測で埋めて
後から直す**のは、一度配った契約を引っ込めることになるので採らない。

同じ理由で、`amazon` にはファイルサイズの上限が入っていない。二次情報では
10MB と書かれていることが多いが、`source` の URL から辿れない。**規定なしとして
検査もしない**——黙って通すのではなく、そもそも条件が無い。

## 「見切れ」と「bbox が要る」を取り違えないために

`touches_edge` が `true` でも、それが**商品の見切れとは限らない**。

背景が単色でないまま `--bbox` を指定せずに走らせると、外周からのフィルが背景を
消しきれず、**背景側が前景として残ったまま画像の端に達する**。実写（不織布の上の
リモコン）ではこれが起きて `foreground_ratio` 0.53 / `touches_edge` true になったが、
商品はどこも見切れていなかった。bbox を与えると `touches_edge` は false になる。

見切れは撮り直すしかないが、こちらは bbox 一つで解ける。**同じ文言で報せると、
AI は解ける問題を諦めてしまう。**そこで kiri は両者を別の code で分ける。

- **外周に接している** かつ `bbox` 未指定 かつ 背景が不均一 かつ 主体の信頼度が high
  → `BBOX_RECOMMENDED`（`SUBJECT_TOUCHES_EDGE` は出さない。誤診だから）
- それ以外で外周に接している → `SUBJECT_TOUCHES_EDGE`

**外周接触は外せない条件である。** ここは「外周接触という同じ事実を、見切れと
読むか前景の失敗と読むか」の分岐であって、不均一な背景そのものへ反応する警告では
ない。`uniformity` が言えるのは「単色背景ではない」までで、「背景側が前景として
残った」の証拠は `touches_edge` だけが持つ。なだらかな勾配の背景でも切り抜きが
完璧に決まることはあり（`foreground_ratio` 0.16 / `touches_edge` false /
`halo_ratio` 0.0 / `separability` 76.4）、そこで「残っています」と断言すると
AI は直すものが無いまま 2 周目を回す。

`info` の `LOW_UNIFORMITY` も同じ材料で `hint` を変える。`uniformity` だけでは
「bbox を足せば救える画像」と「本当に救えない画像」を区別できないためである
（実写ではリモコン 0.201 / キーボード 0.155 で、どちらも「単色背景ではない」）。

| 主体 | hint |
|---|---|
| high かつ `delta_e` > 外周 `p50` | `--bbox <値> --normalized` を勧める |
| high かつ `delta_e` ≦ 外周 `p50` | 勧めない。加えて `NOT_SEPARABLE` を出す |
| low（面積不足） | 勧めない。面積と捕捉率を示して撮り直しを提案する |
| low（捕捉率不足） | 勧めない。「背景と違う画素が散っている」と述べる |
| low（`leftover_ratio` 超過） | 勧めない。「検出した矩形の外にも背景でないものが大きく写っており、この矩形は主体を取りこぼしている」と述べる |

**low の理由ごとに文面を分ける。** 一本の文面しか持たないと、取りこぼし由来の
low で「主体を特定できませんでした（面積 14.1%, 捕捉率 98.1%）」と、自分が並べた
数値と矛盾することを言う。AI は次の一手を決められず、数値のほうを疑い始める。

**実行可能な助言は信頼度 high のときだけ出す。** low で bbox を勧めると、
キーボードのような素材で「キーボードですらない右端の 0.4% の領域」へ誘導して
しまう。誤った助言は助言が無いより悪い。数値（`subject`）は low でも返す。

## AIに結果を見せる

JSON だけで判断できない場合に `--preview` を使う。kiri の出力は原寸（数千 px・
数十 MB）で視覚モデルにそのまま渡せないため、**外部ツールを挟まずに検証用の1枚を
書き出せることが依存ゼロの前提を保つうえで要る。**

```
$ kiri cutout product.jpg -o product.png --preview check.png
```

`check.png` は「元画像 | マスク | 結果」を横に並べた1枚になる。

- **元画像** — 0.1 刻みの座標グリッドを重ねる。0.5 の線だけ濃い。
  AI はこれを見て `--bbox --normalized 0.04,0.18,0.99,0.76` のように返せる
- **マスク** — 白が前景。商品が消えたのか背景が残ったのかを切り分けられる
- **結果** — 市松模様の上に合成する。透過と白い商品を取り違えないため

3面に分けるのは、**結果だけを見ても「なぜ失敗したか」が分からない**ためである。

`--preview` と `--debug-mask` は `--output` と同じ上書き規約に従う。既存ファイルへ
書くには `--force` が要り、`--output` と同じパスは指定できない。**付随出力は本出力の
後に書かれるため、パスが衝突すると成果物を壊したうえで結果 JSON が壊れる前の情報を
報告してしまい、エージェントには検知できない。**

プレビューの書き出しに失敗しても処理自体は成功として返し、`warnings` で伝える。
検証用の付随物を理由にエラーを返すと、「成果物は書けているのにエラー」となって
エージェントが再実行し、今度は `OUTPUT_EXISTS` で二重に詰まるためである。

## 書き出さずに試す

`--dry-run` は成果物を書かずに、書いたときと同じ結果 JSON を返す。

```
$ kiri cutout product.jpg -o product.png --tolerance 18 --dry-run --json
{
  "dry_run": true,
  "outputs": [{ "path": "product.png", "format": "png", "width": 1600, "height": 2000, "bytes": 861432,
                "icc": "embedded", "quality_used": null, "attempts": 1 }],
  "mask": { "foreground_ratio": 0.2164, "separability": 68.3, "halo_ratio": 0.0, "edge_width": 1.0 },
  "warnings": []
}
```

**救済フェーズは同じ画像へ何度もパラメータを振る。** そのたびに本番のパスへ書かせると、
失敗した試行が納品物を上書きする。成果物を守るために `--force` を常用させるのは順序が
逆で、探索そのものが書かなければよい。

- **エンコードまでは実際に行う。** `bytes` は見積もりではなく実測値で、品質と形式の
  判断を本番実行なしに下せる。`ALPHA_FLATTENED` のような書き出し由来の警告も同じに出る
- **`--preview` と `--debug-mask` は書き出す。** 本出力は成果物だが、この 2 つは検証用の
  付随物である。**「本番を壊さずに目で確かめる」ことこそ dry-run の用途**なので、
  ここで書かないと `--dry-run` と `--preview` が併用できず、目視のたびに納品物を潰す
- **本出力の上書き検査をしない。** 1 バイトも書かない実行を止める理由が無いため。
  ただし本番実行なら `OUTPUT_EXISTS` で落ちていた場合は `DRY_RUN_OUTPUT_EXISTS` で先に
  知らせる。黙って通すと、dry-run の成功を見て本番へ進んだ AI がそこで初めて詰まる
- **`--preview` / `--debug-mask` の検査は残る。** こちらは実際に書くので、既存のファイルを
  壊しうる。同じ検証パスへ繰り返し書きたければ `--force` を添える。**dry-run と併せた
  `--force` は本出力を書かないので安全である**（本出力は `--dry-run` が先に止める）
- **`dry_run` キーは常に出す。** 省いて「無ければ書いた」にすると、古いバージョンで
  走った結果と書いた結果が同じ形になり、成果物が無いのにあるものとして次へ進む事故を
  防げない

`batch` にも同じフラグがある。数百点の spec を本番へ流す前に、警告の出る項目だけを
洗い出せる。

```
$ kiri batch spec.json --dry-run --json
```
