# 13. Phase 32: 縁取り・図形の層・縮小表示のプレビュー

節番号は分割前のまま変えていない。どの節がどのファイルにあるかは
[実装計画](../implementation-plan.md) の索引で引く。

Phase 31 は「一覧に縮んだら読めない」を数で言えるようにした。**言えるだけで、
直す手が spec に無い。** `TEXT_ILLEGIBLE_AT_DISPLAY` の `low_contrast` に返せる
remedy は「文字色か下地を変える」だけで、写真の上に置いた見出しではどちらも
選べないことが多い。サムネで普通に使われる 2 つの手——**文字の縁取り**と、
**文字の下に敷く帯**——を spec に足す（§12.7 で次の段へ送ったもの）。

もう 1 つは **`--preview`** である。縮小表示の測りは数を返すが、数だけでは
「どこが潰れたか」が分からない。測った絵そのものを書き出す。

## 13.1 なぜここか

1. **§12.7 が送った宿題である。** remedy に縁取りを書けるのはこの段の後、と
   書いてある
2. **測りの側の準備は済んでいる。** 縮小の測りは最終のキャンバスを縮める
   （§12.4.3）ので、帯は何もしなくても測りに入る。縁取りは「背景」の定義に
   1 つ手を入れるだけで済む
3. **既存の出力を 1 バイトも動かさない。** `stroke` も `shape` も `--preview` も
   書かなければ、結果 JSON とバイト列は main と一致する

## 13.2 作るもの

### 13.2.1 文字の縁取り（`layers[].stroke`）

```json
{ "id": "heading", "type": "text", "role": "heading", "lines": ["見出し"],
  "rect": [80, 150, 560, 120], "size": 100, "color": "#ffffff",
  "stroke": { "color": "#000000", "width": 6 } }
```

| キー | 型 | 意味 |
|---|---|---|
| `color` | `#rrggbb` | 縁の色。不透明のみ |
| `width` | px | **字の輪郭から外へ見える幅。** 1 以上 `size` 以下（§13.7 の 8 で下限を 1 に改めた） |

- **外側だけに付く。** SVG の stroke は輪郭の内外に半分ずつ乗るので、
  `stroke-width = 2 × width` で描き、`paint-order="stroke"` で塗りを上に重ねる。
  内側の半分は塗りに隠れ、字が細らない。角は `stroke-linejoin="round"`
- **`placed` は縁を含む外接矩形**になる（usvg の `abs_stroke_bounding_box`）。
  見える画素はそこまで広がるので、`text_overflow` と `outside_safe_area` も
  縁を含めて判定する
- **覆いの画素（`layer_overlap` / `text_obscured` / `painter`）も縁を含む。**
  縁は見えるインクである
- **原寸の `text_contrast` は塗りの色と縁の色の比**になる。縁は字のすぐ周り
  を必ず囲むので、字が接する「背後」は縁である。背後の画素を数え直すと、
  太い字の内側（縁が届かない）が地の色として混ざって、縁を付けたのに数が
  動かない
- `stroke` を書かない層は今までと 1 画素も変わらない（縁の無い SVG を出す）

### 13.2.2 縮小表示での背景の取り方（縁取りがある層だけ）

§12.4.3 の背景は「外接矩形の中で、塗りの被覆が 0.05 未満の画素」だった。
縁取りがあると、そこには縁と地の両方が入り、中央値がどちらに付くか決まらない。

縁取りのある層は、塗りの被覆に加えて **塗り + 縁の被覆**も同じ縮小で縮め、
次の順で背景を選ぶ。

1. **縁（halo）**: 塗りの被覆が 0.05 未満で、塗り + 縁の被覆が 0.5 以上の画素。
   1 つでもあれば、それを背景とする（`against: "stroke"`）
2. **地（ground）**: 1 つも無ければ、塗り + 縁の被覆が 0.05 未満の画素
   （`against: "ground"`）

**縁が縮小で潰れたら地と比べる**のが要点である。1280 幅を 144 へ縮めると
1 画素は原寸の約 8.9px 四方で、1〜2px の縁はそこで地に溶ける。そのとき
「縁があるから読める」と言えば嘘になる。縁が生き残る太さなら、縁と比べる。

`display[]` に `against` を足す。**縁取りのある層にだけ出る**——無い層の
`display[]` は Phase 31 と同じ形のまま。

### 13.2.3 図形の層（`"type": "shape"`）

```json
{ "id": "band", "type": "shape", "rect": [0, 400, 1280, 160],
  "color": "#000000", "opacity": 0.6, "radius": 0 }
```

| キー | 型 | 既定 | 意味 |
|---|---|---|---|
| `id` / `type` / `rect` | | | 他の層と同じ |
| `color` | `#rrggbb` | 必須 | 塗りの色 |
| `opacity` | 0 < x ≤ 1 | 1 | 塗りの不透明度 |
| `radius` | px, ≥ 0 | 0 | 角の丸み。幅か高さの半分を超えれば SVG の規則どおり半分で止まる |

- **矩形だけを持つ。** 帯・座布団・丸い札はこれで足りる。円や多角形は必要が
  出てから `kind` を足す（既定を矩形にすれば足しても互換は崩れない）
- **`role` は書けない。結果では `decoration`。** 図形は情報を持たないので、
  コントラストも重なりも測る理由が無い
- **`outside_safe_area` は null。** 端まで敷いた帯が表示で切られても欠ける
  情報は無い。測ると、普通の構図が毎回 `OUTSIDE_SAFE_AREA` を出す
- **後から重ねた図形は文字を覆う**——`painter` を塗るので、文字の上に不透明度
  0.5 以上の図形を置けば `TEXT_OBSCURED` が言う
- **文字の下に敷いた帯は、何もしなくてもコントラストの測りに入る。** 原寸は
  「重ねる直前のキャンバス」、縮小は「最終のキャンバス」を測るからである
- 描画は文字と同じく resvg に SVG の `<rect>` で渡す（縁のアンチエイリアスを
  文字とそろえる）

結果の `layers[]` には `kind: "shape"`、`role: "decoration"`、`rect` と
`placed`（= `rect`）が出る。文字・画像の測りのキーは null。

### 13.2.4 `--preview`

```
$ kiri compose header.json -o header.png --profile note --preview
```

**縮小表示の測りが見た絵を、そのまま PNG で書く。** 出力の隣に
`<出力の stem>.<表示名>.png`（`header.pc_list.png` / `header.mobile_list.png`）。

- 書くのは測りが使った絵そのもの（透過は白か潰す色へ重ね、box で縮め、8bit へ
  丸めた RGB）。**別の縮小で作り直さない**——作り直すと、見た絵と測った絵が
  食い違う
- 大きさは表示の CSS px（192 × 101 / 144 × 75）。拡大はしない
- **`--profile` が要る**（clap の `requires`。無ければ exit 2）。表示の幅は
  profile が持つ
- **`--dry-run` とは併用できない**（clap の `conflicts_with`。exit 2）。dry-run は
  1 バイトも書かない約束で、黙ってプレビューだけ書かない形にもしない
- 測らない表示（表示幅 ≥ キャンバスの幅）は書かない
- 既存のファイルは `--force` が無ければ断り、ディレクトリは `--force` があっても
  断る。画素を描く前に全部のパスを確かめ、全部をエンコードしてから書く。書く
  途中で失敗したら、その実行が新しく作ったファイルを消す（`--force` で上書きした
  ファイルは戻せない。§13.7 の 10）
- 結果 JSON の最上位に `previews[]`: `{ display, path, width, height }`。
  `--preview` を渡さなければキーごと出ない

### 13.2.5 remedy

| 警告 | 理由 | 足す手 |
|---|---|---|
| `TEXT_ILLEGIBLE_AT_DISPLAY` | `low_contrast` | `stroke` で縁取る（縮小で残る太さ = 表示上 1px 以上、つまり `canvas の幅 / 表示幅` px 以上）。または文字の下に `shape` の帯を敷く |
| `TEXT_CONTRAST_LOW` | | 同じく `stroke` か `shape` の帯 |

`too_small` と `strokes_lost` の手は変えない。縁は塗りの線を太らせない
（`strokes_lost` は塗りの被覆で判定する）。

## 13.3 契約への影響

| 場所 | 変更 |
|---|---|
| `compose::keys` | `TEXT_LAYER` に `stroke`、`STROKE`、`SHAPE_LAYER` |
| spec の検査 | `stroke.width` は有限で 1 ≤ w ≤ `size`、`opacity` は 0 < x ≤ 1、`radius` は有限で ≥ 0。外れたら `SPEC_INVALID`。未知のキーは `SPEC_UNKNOWN_FIELD`（`shape` に `role` を書いても） |
| `kiri schema` | `compose_spec[]` に上の形、`layers[].display[].against`、`previews[]`、`--preview` |
| `schema_version` | **2 のまま。** キーと値が増えるだけ |
| error / warning | 新しい code は無い |

## 13.4 受け入れ条件

1. `stroke` / `shape` / `--preview` を使わない spec の結果 JSON（`elapsed_ms`
   を除く）と出力のバイト列が main と一致する（`tests/compose.rs` の釘を含む）。
   **ただし §13.2.5 で手を足した 2 つの警告の hint の文は変わる**——
   `TEXT_CONTRAST_LOW` か `TEXT_ILLEGIBLE_AT_DISPLAY` の `low_contrast` を出す spec
   では、hint の文だけが main と違う（§13.7 の 6）
2. #767676 の地に白 100px の見出し（Phase 31 の条件 6）は `--profile note` で
   `low_contrast`。黒の `stroke` を幅 8 で付けると、`pc_list` で `against:
   "stroke"`・`contrast >= 4.5`・`legible: true`。原寸の `text_contrast` は 21
3. 同じ見出しに幅 1 の縁では、`mobile_list` で `against: "ground"` になり
   `low_contrast` が残る（**縁が潰れたら効いたと言わない**）
4. 白地に白に近い文字（`TEXT_CONTRAST_LOW` が出る）の下に黒の `shape` を
   不透明度 0.8 で敷くと、`TEXT_CONTRAST_LOW` が消える
5. 文字の上に不透明な `shape` を重ねると `TEXT_OBSCURED`
6. `shape` の層は `role: "decoration"`・`outside_safe_area: null`・`display` 無し
7. `--preview` で 2 枚が書かれ、寸法が 192 × 101 と 144 × 75。書いた PNG の
   画素から計算した相対輝度が、測りの使った輝度と一致する
8. `--preview` は `--profile` 無しで exit 2、`--dry-run` と併せて exit 2。
   プレビューの先に既存のファイルがあれば `--force` 無しで断り、**出力も書かない**
9. 不正な `stroke` / `shape` の値とキーを断る（13.3 の表のとおり）
10. 同じ入力から同じ結果（決定性）
11. `cargo test`（`ci-test` プロファイル、feature 有無を直列で）、clippy（両系統、
    `-D warnings`）、fmt、MSRV 1.85 がすべて通る
12. 08-compose.md・`kiri schema`・Skill・implementation-plan.md の索引を更新する

## 13.5 この段でやらないこと

- 円・多角形・グラデーション・影（`kind` を足せば後から入る）
- 縁取りの半透明・二重の縁
- プレビューの拡大（表示の CSS px のまま書く。拡大して見るのは見る側の仕事）
- 自動折り返し・自動縮小（§10.9 のまま）

## 13.6 実装（2026-10-15）

| 作ったもの | 場所 |
|---|---|
| `stroke` / `shape` のキー（`keys::TEXT_LAYER` の `stroke`、`STROKE`、`SHAPE_LAYER`）と値の検査 | `src/compose/mod.rs` |
| 縁取りの SVG（`stroke-width = 2 × width`・`paint-order="stroke"`・`stroke-linejoin="round"`）と縁を含む外接矩形 | `src/compose/text.rs` |
| 図形の層を resvg の `<rect>` で描く | `src/compose/shape.rs`（新規） |
| 縮めた RGB を持ち、輝度をそこから出す（`Shrunk::rgb`）・縁と地から背景を選ぶ（`Against`）・`low_contrast` の手 | `src/compose/display.rs` |
| 縁のある層の原寸コントラスト（`stroked_contrast`）・`TEXT_CONTRAST_LOW` の手 | `src/compose/measure.rs` |
| 図形の層の重ね方（`painter` を塗り、`subject` は塗らない）・塗りだけの組み直し・`--preview` の書き先の確認と書き出し | `src/commands/compose.rs` |
| `--preview`（`requires = "profile"`、`conflicts_with = "dry_run"`） | `src/cli.rs` |
| `previews[]`（`ComposePreviewReport`） | `src/report.rs` |
| `compose_spec[]` の 2 行、`layers[].display[].against` と `previews[]` の `fields[]` | `src/commands/schema.rs` |
| `TEXT_CONTRAST_LOW` / `TEXT_ILLEGIBLE_AT_DISPLAY` の `remedy` | `src/warning.rs` |
| 人間向けの行（`against` と `preview`） | `src/main.rs` |

**usvg 0.48.1 は両方を持つ。** `paint-order` は `<text>` の span ごとに読まれ
（`parser/text.rs`）、`Node::abs_stroke_bounding_box` は文字の節では輪郭を平らに
した後の縁込みの矩形を返す（`text/mod.rs` の `convert`）。代わりを探す必要は
無かった。

### 受け入れ条件の実測

字体は §12.8 と同じく借りる。テストがこの機械（macOS）で借りたのは
**Arial Unicode MS** で、表明は字体に依らない関係だけである。見出しは `ABAB` の
1 行、地は #767676、文字は白 100px、縁は黒。

| 条件 | 縁 | 原寸 `text_contrast` | `pc_list`（192） | `mobile_list`（144） |
|---|---|---|---|---|
| （Phase 31 の 6） | 無し | 4.54 | 3.91・low_contrast | 3.00・low_contrast |
| 2 | 幅 8 | **21.0** | **12.60・`stroke`**・legible | 8.41・`stroke`・legible |
| 3 | 幅 1 | 21.0 | 3.22・`ground`・low_contrast | **2.74・`ground`**・low_contrast |

（縁の幅だけ内側へ寄せて組む §13.7 の 7 の後の値。寄せる前は幅 8 で 15.03 /
7.01、幅 1 で 3.51 / 2.63 だった——字が 8px 動くと縮小の格子に対する位置が
変わり、値が数割動く。条件 2 の `--fail-on default` は寄せた後で通る）

幅 8 は `mobile_list` の 1 画素（8.9px）より細いが、縁の輪は字の外周を全部
回るので、塗りが 0.05 未満で縁が 0.5 以上の画素が残る。幅 1 で縮めた値が縁の
無いときより下がるのは、前景（塗りが 0.5 以上）の画素に黒い縁が混ざって暗く
なるからである——**細い縁はむしろ読みにくくする**、を数が言っている。

字体を変えても同じ向きに出る（手元で試した値）。

| 字体 | 縁 | `pc_list` | `mobile_list` |
|---|---|---|---|
| Hiragino Sans | 無し | 4.17・low_contrast | 3.16・low_contrast |
| Hiragino Sans | 幅 1 | 3.44・`ground` | 2.83・`ground` |
| Hiragino Sans | 幅 8 | 14.08・`stroke` | 9.56・`stroke` |
| Helvetica | 無し | 3.57・low_contrast | 3.38・low_contrast |
| Helvetica | 幅 1 | 3.09・`ground` | 2.77・`ground` |
| Helvetica | 幅 8 | 13.14・`stroke` | 10.07・`stroke` |

`start` / `center` / `end` の 3 通り × 3 字体 × 幅 1 / 8 の 18 通りで、
`text_overflow` は全部 0、幅 8 は `--fail-on default` が全部通った。

条件 4 は 400 x 200 の白地に #eeeeee の 60px（`AB`）。原寸の `text_contrast` は
**1.16**（`TEXT_CONTRAST_LOW`）で、文字の前に黒の帯（不透明度 0.8）を敷くと
**10.89** になり、警告が消える。

**条件 1 は main（06479fc）の build と並べて確かめた。** 画像だけ・文字と画像と
decoration・透過の下地・後から覆う層とキャンバスの外の文字、の spec 4 本を、
出力 4 形式（png / jpg / webp / avif）× 追加の引数 6 通り（無し・`--dry-run`・
`--fail-on default`・`--flatten --background`・`--profile note`・
`--profile note --fail-on default,display_contrast<3`）の 96 通りで走らせ、
`elapsed_ms` と出力パスを除いた結果 JSON・終了コード・人間向けの出力・書いた
ファイルの md5 を突き合わせた。**違ったのは §13.2.5 で足した手の文だけである**
（`TEXT_CONTRAST_LOW` と `TEXT_ILLEGIBLE_AT_DISPLAY` の `low_contrast` の hint。
36 通りに現れる）。その文を取り除くと 96 通りすべてが一致した。

| 条件 | 検査 |
|---|---|
| 1 | `without_a_profile_the_output_matches_the_build_before_phase_31`（釘、変更なし）・`without_the_new_options_no_new_keys_appear`・`text::tests::a_layer_without_a_stroke_emits_the_same_svg_as_before`・上の 96 通り |
| 2 | `a_thick_stroke_survives_the_shrink_and_makes_the_heading_legible`・`a_stroked_heading_with_room_does_not_overflow`・`a_stroke_the_colour_of_the_fill_is_not_called_low_contrast`・`display::tests::a_stroke_the_colour_of_the_fill_is_judged_against_the_ground`・`display::tests::a_weak_stroke_is_kept_when_the_ground_is_no_better`・`display::tests::a_thick_halo_is_the_background`・`measure::tests::a_stroked_contrast_is_the_fill_against_the_stroke` |
| 3 | `a_hairline_stroke_lost_in_the_shrink_is_compared_with_the_ground`・`display::tests::a_halo_lost_in_the_shrink_falls_back_to_the_ground` |
| 4 | `a_band_beneath_pale_text_clears_the_low_contrast` |
| 5 | `an_opaque_shape_over_text_obscures_it` |
| 6 | `a_shape_layer_reports_as_an_unmeasured_decoration` |
| 7 | `the_preview_is_the_picture_the_display_measure_saw`・`the_preview_reproduces_the_measured_contrast`・`display::tests::the_luminance_is_derived_from_the_kept_rgb` |
| 8 | `the_preview_needs_a_profile_and_refuses_a_dry_run`・`an_existing_preview_blocks_every_write_without_force`・`a_directory_at_a_preview_path_blocks_every_write_even_with_force`・`commands::compose::tests::a_failed_write_removes_the_files_it_created` |
| 9 | `bad_stroke_and_shape_values_are_refused`・`compose::tests` の 5 本（`stroke.width` 0.01 / 0.99 を含む） |
| 10 | `strokes_shapes_and_previews_are_deterministic` |
| 12 | `the_schema_publishes_strokes_shapes_and_previews`・`tests/cli.rs` の `every_published_field_exists_in_the_result`（`against` と `previews[].*` を実際の結果から引く）。文書は 08-compose.md・Skill・implementation-plan.md の索引 |

縁が `placed` と `text_overflow` に入ることは `the_placed_rect_includes_the_stroke`
が見る（本当に狭い枠では縁の幅以上はみ出す）。

## 13.7 実装で決めたこと

1. **縁のある行の矩形は、字の送りの矩形と縁の矩形の和である**（7 で、字の送りの
   矩形を縁の幅だけ広げた範囲で止めるよう改めた）。 §13.2.1 は
   「`placed` は `abs_stroke_bounding_box`」と書いたが、usvg の文字の
   `abs_bounding_box` は字の送りと ascent / descent から組んだ矩形で、
   `abs_stroke_bounding_box` は輪郭から組んだ矩形である。後者だけを採ると、
   `ABAB` のように字の天地が ascent / descent より狭い行で、**縁を付けたのに
   `placed` の高さが縮む**（Arial Unicode MS、縁 8 で 134px → 約 88px）。和を採れば
   縮まず、縁が字の送りの外へ出る向き（左右の字頭・字尻）ではそのぶん広がる。
   `line_widths` も同じ矩形から取る——「1 行なら行の幅と `placed` の幅が一致する」
   を縁の有無で崩さない。縁の無い層は今までどおり `abs_bounding_box` だけを見る
2. **塗りだけの被覆は、縁の属性を落として組み直して取る**（`text::layout_fill`）。
   縁込みの絵から塗りを取り出す道は無い（`paint-order` で重なった画素は塗りの色
   しか残さない）。**位置は縁付きのまま**にする——7 の寄せまで外すと塗りが縁の
   幅だけずれ、前景も縁の輪も別の画素を指す（寄せを入れた直後に実際に起き、
   幅 8 の見出しが 2.47・`ground` を返した）。組み直すのは `--profile` を渡し、
   読めることを要求する縁付きの層だけで、1 層あたり resvg の描画が 1 回増える
3. **縁のある層の `text_contrast` も、1 画素も描かれなければ null。** 色の比は
   描かなくても計算できるが、描かれていない文字に比を返すと
   `TEXT_NOT_RENDERED` と並んで「読める」と言うことになる
4. **プレビューの PNG には出力と同じく sRGB の ICC を埋める**（`IccPolicy::Embed`）。
   画素は変わらない
5. **未知の `type` の断りの文が `image / text / shape` になった。** `type` を
   書かなかったときの hint（`"type": "image"` か `"type": "text"`）は変えていない
6. **remedy の文が変わるので、`TEXT_CONTRAST_LOW` と `low_contrast` を出す spec の
   結果 JSON は hint の文だけ main と違う。** 条件 1 の「一致」はこの文を除いた
   ものである（§13.2.5 がそう求めている）
7. **縁のある層は、縁の幅だけ `rect` の内側へ寄せて組む**（レビューの指摘）。
   縁は字の輪郭から外へ出るので、寄せないと `start` 揃えの字頭（`A` の左の余白は
   ほぼ 0）と最初の行の天で縁が必ず枠の外へ出て、**どう `rect` を書いても
   `text_overflow` が縁の幅だけ残った**——条件 2 の spec が `--fail-on default` で
   落ちていた。`start` は右へ、`end` は左へ、最初のベースラインは下へ `width` だけ
   寄せ、`center` は左右に等しく出るので動かさない。縁の無い層の SVG は 1 文字も
   変わらない（足し算もしない。`+ 0.0` でも `-0` の綴りが動く）。

   **寄せただけでは 0 にならない字体がある。** Arial Unicode MS の `A` は輪郭が
   字の送りの矩形から左へ 0.098px 出ており、縁の矩形の和を採ると 0.10px の
   はみ出しが残った。縁の無い層はこの出っ張りを数えていない（`placed` は送りの
   矩形）ので、**縁のある行の矩形を「送りの矩形を 4 辺とも `width` だけ広げた
   範囲」で止める**（`text::intersect`）。和は送りの矩形を必ず含むので、縁を
   付けて矩形が縮むことは無い。
8. **`stroke.width` の下限を 1 にした**（レビューの指摘）。0.01px の縁は 1 画素も
   塗らないのに、原寸の `text_contrast` は塗りと縁の色の比を返し、白地に白の
   字が 21 を名乗った（`MIN_STROKE_WIDTH`）
9. **縁が塗りと同じ色でも「読めない」と言わない**（レビューの指摘）。黒地に白の
   字と白の縁は、塗りと縁の比が 1.0 で `TEXT_CONTRAST_LOW` を出していた。縁が
   字を何からも分けていないとき、分けているのは地である。
   - 原寸: `text_contrast` = max(塗りと縁の色の比, 塗りの色と、塗り + 縁が覆う画素の
     背後の中央値との比)。背後が透けて色が決まらなければ前者だけ
   - 縮小: 縁と比べた比が 4.5 に届かなければ地（塗り + 縁の被覆が 0.05 未満）とも
     比べ、良いほうを採る。`against` は採ったほうを名乗る。届いていれば縁のまま
10. **書く約束を、守れる形で言い直した**（レビューの指摘）。§13.2.4 の「1 つでも
    書けなければ 1 つも書かない」は、書く途中の失敗（ディスクが一杯など）までは
    守れない。いまの約束は次のとおりで、`--help` と 08-compose.md と
    `previews[].path` の notes に同じことを書いた。
    - 画素を描く前に全部の先を確かめる。既存のファイルは `--force` が無ければ、
      **ディレクトリは `--force` があっても**断る（`--preview` の実行だけ。
      渡さない実行の断り方は変えない）
    - 出力とプレビューを**全部エンコードし終えてから**書き始める
    - 書く途中で失敗したら、**この実行が新しく作ったファイルを消す**。`--force` で
      上書きしたファイルは前の中身を持っていないので戻せず、エラーの hint が
      どれを上書きしたかを言う（`commands::compose::write_all`）。
      `write_encoded` が作った親ディレクトリは消さない
11. **プレビューの名前は `OsString` のまま組む**（`preview_path`）。UTF-8 でない
    stem が `to_string_lossy` で `�` に化けると、出力と違う名前の隣に書く
12. **プレビューは縮めた絵の一覧だけから組む。** 書く先の確かめと同じく
    `display::measurable` で選んだ表示の一覧なので、確かめた先と書く先が
    食い違わない（前は名前で探し、見つからなければ黙って飛ばしていた）
13. `kiri schema` は `previews[]` を 1 行で配っていたのを、他の配列と同じく
    `previews[].display` / `.path` / `.width` / `.height` の 4 行にした。
    `layers[].opaque_rect` の `null_means` に図形の層を足した

## 13.8 実写での検証（2026-10-15）

白い不織布の上のリモコン（`IMG_0251`）を背景に `cover` で敷き、切り抜いた同じ
リモコンを右に立てた note の見出しを 3 通り組んだ。字体はヒラギノ角ゴシック W8
（見出し 104px）と W4（キャプション 44px）、文字は白。見出しの下は明るい
不織布である。

| 版 | 見出し 原寸 | 見出し `pc_list` | 見出し `mobile_list` | キャプション `mobile_list` |
|---|---|---|---|---|
| そのまま | 2.59 `TEXT_CONTRAST_LOW` | 2.46 low_contrast | 2.33 low_contrast | 2.13 too_small・low_contrast |
| 縁 8px（キャプション 4px）、#111418 | 18.47 | 15.68 `stroke` | 10.31 `stroke` | 2.70 `stroke`・too_small・low_contrast |
| 帯（#111418、不透明度 0.6、角 24） | 8.62 | 8.16 | 7.42 | 4.19 too_small・low_contrast |

**`--preview` の絵を 4 倍に拡大して目で見た印象と、数の順位が一致した。**
そのままの見出しは不織布に溶けて読みにくく、縁と帯の見出しははっきり読める。
キャプションは 3 通りとも潰れており、縁 4px の版は縮めた塗りが縁に埋もれて
黒い塊に見える——`against: "stroke"` で 2.70 という数はその見た目どおりである。

**見つけた欠陥を 1 つ直した。** 縁 4px のキャプションの `low_contrast` に
「stroke で縁取る（width 9 以上）」と返していた——既に縁を持つ層である。
`against` で手を分けた: `ground` なら縁の width を太らせる手、`stroke` なら
文字色と縁の color を離すか、size か weight を上げて塗りが縁に埋もれないように
する手を返す（`display::tests::a_stroked_layer_is_not_told_to_add_a_stroke`）。

`OUTSIDE_SAFE_AREA` は背景の写真（`bg`、`role: decoration` の画像）と商品の
両方に出た。背景の写真が安全領域の外へ出るのは全面に敷く以上当然で、図形の層と
同じ理由で測らなくてよいが、画像の層の振る舞いを変えるのはこの段の範囲の外
なので、残件として記す。

**追記（2026-10-15）**: 残件は後続の PR で片付けた。`role: decoration` の画像は
`outside_safe_area` を null にし、`OUTSIDE_SAFE_AREA` を出さない
（`a_full_bleed_decoration_image_is_not_judged_against_the_safe_area`）。
