# kiri schema

> [README](../../README.md) › [コマンド別ドキュメント](README.md) › kiri schema

オプションと code の一覧を返す。**エージェントが最初に読むのはこれ。**

```
$ kiri schema --json
{
  "schema_version": 2,
  "kiri_version": "0.1.0",
  "exit_codes": [{ "code": 0, "meaning": "成功" }, ...],
  "errors":   [{ "code": "OUTPUT_EXISTS",  "exit_code": 2, "summary": "出力先が既に存在する。--force が要る" }, ...],
  "warnings": [{ "code": "LOW_UNIFORMITY", "summary": "背景の均一度が低い（単色背景ではない）" }, ...],
  "profiles":  [{ "name": "amazon", "revision": "2026-09", "rules": { ... } }, ...],
  "lint_checks": [{ "name": "format", "needs_pixels": false }, ...],
  "global_options": [{ "name": "--json", "global": true, "takes_value": false, ... }],
  "commands": [
    {
      "name": "cutout",
      "about": "背景を透過して商品を切り抜く",
      "arguments": [{ "name": "input", "required": true, ... }],
      "options": [
        { "name": "--tolerance", "default": "12", "takes_value": true, "summary": "背景色との色差(ΔE)の許容量..." },
        { "name": "--edge-threshold", "default": null, ... }
      ]
    }, ...
  ]
}
```

**この README は 1000 行ある。** 文脈に丸ごと載せられる長さではないし、載せたところで
オプションの綴りと既定値は散文の中に埋まっている。必要なのは「どう呼ぶか」と
「返ってきた code が何を意味するか」だけなので、それを 1 コマンドで配る。

- `commands[].options[]` は **clap のパーサそのものから組み立てる。** 手で書いた一覧は
  必ず実装から離れ、離れた一覧は「指定したのに効かない」という最も追いにくい失敗を招く
- **`default` が `null` の項目は既定値を名乗らない。** `--edge-threshold` は「未指定」と
  「8 を明示」を区別するので、ここに 8 が出てはならない。出れば「指定しなくても 8 が
  効く」という誤った前提がそのまま行動に変わる
- `errors[].exit_code` は code から一意に決まる。**同じ code が場所によって違う番号を
  返すことはない。**「この失敗なら何番」で分岐が書ける
- `warnings[]` / `errors[]` は実装と同じ表から生成される。警告もエラーもそこへ足す以外に
  作る方法が無いので、**載っていない code が飛んでくることは構造的に起こらない**
- `fields[]` は結果の値の読み方（しきい値と `null` の意味）。後述
- `profiles[]` は `--profile` / `kiri lint` が見る規格の表（条件・出典・版）、
  `lint_checks[]` は `kiri lint` が見る条件の一覧（`{name, needs_pixels}`）。
  **`checks[].name` の綴りを散文から抜き直さずに済ませる**ためで、
  `needs_pixels` が真の項目は AVIF では必ず `skipped` になる——渡す前に
  どれが飛ぶかを予測できる
- `accepts` は受け付ける値の一覧（`--format` なら avif / png / jpeg）。**綴りを外すと
  clap が code 無しの exit 2 で落ちる**ので、呼ぶ前に知れる必要がある。自由な値を取る
  項目ではキーごと消える。数値の範囲は clap から読めないため、必要なものは `summary`
  の文面に書いてある（`--preview-size` は 32-4096）
- `detail` は `--help` の長い説明。**指定の前に知っていないと選びようがないこと**が
  書いてある（90 度単位だけが無劣化、など）。無ければキーごと消える

全体で 44KB ある。必要な節だけ引くとよい。

```
$ kiri schema --json | jq '.warnings'
$ kiri schema --json | jq '.fields[] | select(.path | startswith("mask."))'
$ kiri schema --json | jq '.commands[] | select(.name == "cutout") | .options'
```

`--json` を付けなければ人間向けの要約を返す（他のコマンドと同じ規約）。そちらには
オプションの一覧を出さない。7 コマンド分を並べると読めなくなるし、人間には `--help`
という専用の入口がある。テキストで価値があるのは「どんな code が返りうるか」の
見通しで、これは `--help` のどこにも無い。

## fields — 値の読み方

`fields[]` は結果 JSON の各項目について、**しきい値と `null` の意味**を返す。

```json
{
  "path": "mask.halo_ratio",
  "appears_in": ["cutout"],
  "unit": "ratio",
  "nullable": true,
  "null_means": "測る境界が無かった。0（縁が残っていない）ではない",
  "warns": [{ "code": "HALO_REMAINS", "operator": "gt", "threshold": 0.1 }],
  "summary": "境界近傍で不透明なのに、元の色が局所背景と見分けがつかない画素の割合",
  "notes": "白い下地では見えず、黒や色付きの下地に載せて初めて輪郭の光として現れる"
}
```

**警告はしきい値を越えたときにしか出ない。** `halo_ratio` が 0.07 だったとき、
それが良い値なのかは、しきい値がどこにあるかを知らなければ判断できない。そこを
知るために README を読ませるのでは、`kiri schema` が契約を配る意味が半分しか
果たせない。

- `warns[].threshold` は**実装の定数そのもの**。較正で定数を動かせば schema も動く
- **単一のしきい値で決まる警告だけを載せる。** `NOT_SEPARABLE` は固定値ではなく
  `background.residual.p50`（画像ごとの値）と比べるので、`threshold` を
  載せられない。こういう条件は `notes` で述べる。**載せられないものを載せて
  `threshold` を嘘にするより、載せないほうがよい**
- `gates` は `subject.confidence` が `high` になる条件（`area_ratio` ≥ 0.05、
  `capture_ratio` ≥ 0.70、`leftover_ratio` < 0.15 の AND）
- `appears_in` は値が現れるコマンド。`mask.*` は `cutout` にしか出ないので、
  `info` で待っても来ない
- `nullable` が `true` の項目は `null_means` を必ず持つ。**0 と `null` を
  混同させないため**で、`halo_ratio` を 0 と読むと「縁が残っていない」という
  良い結果に見えてしまう
- `notes` はしきい値では表せない読み方。`edge_width` の「1〜3 なら鮮鋭、8px
  かけて溶ける素材では 6.5 が正解」のような、文脈に依存する判断がここに入る

数値は定数から、散文は手で書いている。**最も動きやすいものを最も強く守る**分け方で、
較正のたびに動く数値には書き写す余地を残していない。

## schema_version

結果 JSON はすべて `schema_version` を名乗る。**失敗の JSON も同じ。**

```json
{ "schema_version": 2, "error": { "code": "INPUT_UNREADABLE", "message": "..." } }
```

**キーが増えただけでは上げない。** 既存のキーの意味や型が変わったとき、つまり
今までの読み方が誤読になるときだけ上げる。版を名乗らなければ、契約が動いたときに
古い読み手が黙って誤読する。**黙って間違えるのが最も高くつく。**

### 2 になった理由

**`outputs[]` が常に 1 要素だという前提が崩れた。** 1 回の実行で複数の派生を書ける
ようになった（[複数のサイズと形式をまとめて書く](03-output.md#複数のサイズと形式をまとめて書く--derive)）。
型は今までと同じ配列なので、`outputs[0]` を読むコードはコンパイルも実行も通る
——**通ったうえで 2 本目以降を黙って捨てる。** 無視した結果が「成果物が 1 つしか
無い」という誤った事実になるので、版で断る。

同じ版で入った変更が 2 つある。どちらも単独では上げる理由にならないが、**版を
上げる回は 1 回だけにする**という方針でここへ寄せた。

- `outputs[].role` が増えた（キーの追加）
- 派生に紐づく警告が `data.output` で「どの出力の話か」を名乗るようになった。
  `DRY_RUN_OUTPUT_EXISTS` の `data.path` はこの `output` へ**改名した**

`--derive` などを渡さない実行では、`outputs[]` は今までどおり 1 要素で、
出力バイト列も出力パスも 1 バイトも変わらない。
