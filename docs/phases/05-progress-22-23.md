# 5. 進捗 — Phase 22〜23

節番号は分割前のまま変えていない。`計画 §8.6` のような参照が他の節・src / tests から
150 箇所以上張られていて、**番号が識別子として働いている**からである。どの節がどの
ファイルにあるかは [実装計画](../implementation-plan.md) の索引で引く。

記号（`[x]` / `[ ]` / `※`）の意味は [§5 進捗](../implementation-plan.md#5-進捗) にある。

- [x] Phase 22: `--profile` ＋ `kiri lint`（`SCHEMA_VERSION` は 2 のまま据え置き）
  - [x] 規格の表を `src/profile.rs` に置いた。**唯一の定義は `Rules`** で、書く側の値
        （`--canvas` / `--fill-ratio` / `--format` / `--background` / `--flatten` /
        `--max-bytes` の 6 つ）は `write_defaults` がそこから計算する。同じ規格を
        書く側と見る側の 2 通りに書き下すと、片方を直したときにもう片方が古い規格の
        まま残る。プリセットは `amazon` / `shopify` / `square-white` の 3 つで、
        どれも `revision`（`"2026-09"`）と `source` を持ち、`kiri schema` の新ブロック
        `profiles[]` と結果 JSON の `settings.profile` に出る
  - [x] `kiri lint <file> --profile <name>`。**書かない。** 背景も主体も `info` と
        まったく同じ経路（`load_with` → `see_background` → `analyse_background_seen`）で
        測る——lint だけが別の測り方を持つと、`kiri info` が返した背景色と lint が
        照らした背景色が食い違い、**どちらが本当かを利用者が確かめる手段が無くなる**
  - [x] レビュー指摘（実際に動かして見つかったもの）を直した。
        - **合否を分ける帯を `field_band`（`max(短辺/33, --border)`）にした。**
          既定の `--border 2` は切り抜きのための値で、cutout ではそこから取った色が
          閾値の種にしかならないが、lint では外周の中央値が**そのまま合否になる**。
          1600px の画像の縁 0.125% を見て「背景は純白」と答えていたため、外周 2px
          だけ白い灰色一面の画像が全項目 pass / exit 0 になっていた。新しい定数は
          置かず、「場の推定でここは背景だと信じてよい帯」として既にあるものを
          使う。`--border` は下限として効き、使った幅は `checks[].actual.border_px`
          が名乗る（`kiri info --border <その値>` で同じ数を再現できる）
        - **測れていないものを `pass` と言わない。** 外周に不透明な画素が 1 つも
          無ければ `background` は `unmeasurable`（`actual` は `null`）——透過 PNG に
          `rgb: [0,0,0]` / `delta_e: 100.0` を配っていた。外周が単色として扱えない
          （`is_uniform` が偽）ときも `unmeasurable` で、しきい値は `cutout` が
          `LOW_UNIFORMITY` を出すのに使っているものをそのまま通す
        - **色空間を 1 つも名乗っていないファイルを `pass` にしない。**
          `LoadedImage::color_named`（ICC か EXIF ColorSpace があったか）を足し、
          偽なら `unmeasurable`。**AVIF の CICP が unspecified のときと同じ扱い**で、
          形式で合否が変わる枝を作らない。`kiri info` の `color_space` は 1 文字も
          動かない（あちらが答えているのは「画素をどう扱ったか」である）
        - **画素を要する条件を規定しない規格では見立てを走らせない**
          （`regulates_composition`）。判定は `Check::needs_pixels` から組むので、
          `checks[]` に出ない項目のためだけに 25MP の背景推定を払うことが無い。
          出力は 1 バイトも変わらない（5.29MP の shopify で 0.03s → 0.01s）
        - `kiri schema` に `lint_checks[]`（`{name, needs_pixels}`）を足し、
          散文に書き写してあった `checks[].name` と `status` の一覧を定数から
          組むようにした（`PROFILE_NAMES` / `FAIL_ON_METRICS` と同じ作法）
  - [x] 再レビュー（実測）が見つけた回帰を直した。
        - **帯を広げたことで、規格を満たす画像が落ちるようになっていた。**
          `field_band` は固定幅（1600px で 48px）なので、占有率が
          `1 - 2/33 ≒ 0.939` を超えると**主体そのものが帯に入る**。そこで落ちた
          `uniformity` を「背景が単色でない」と読んで `unmeasurable` を返して
          いたため、`cutout --profile amazon --fill-ratio 0.95` で書いた
          純白背景・正方形・sRGB ICC 付きの画像が、同じ amazon の lint で
          exit 5 になっていた（0.94 は通る）。どの profile にも占有率の上限は
          無く、寄りのトリミングは全規格で合法なので、これは**規格が許す構図で
          書いたものを規格で落とす**誤りである。`settled_band` を置き、
          `field_band` から**単色と言えるまで半分ずつ（1/4 まで）狭めて**
          最も広い帯を採るようにした。実測（1600px）: 0.95 → 帯 24、
          0.96 → 24、0.97 → 24、0.98 → 12 で、どれも `uniformity` 1.0 / pass。
          0.94 は帯 48 のまま 1 バイトも変わらない
        - **主体の外接矩形を標本から除く形は採らなかった。** lint は測った帯幅を
          `checks[].actual.border_px` で名乗り、「`kiri info --border <その値>`
          で同じ背景色が出る」ことを契約にしている。矩形を抜いた標本は 1 つの数で
          言い表せないので、契約のほうを失う。加えて主体は背景の推定値に対して
          検出されるので、帯が主体で汚れていると**主体が 1 つも検出されなくなる**
          （占有率 0.98 が実際にそれで、`fill_ratio` まで `unmeasurable` だった）
          ——除く対象が消えるため、除く形では直らない
        - **`--border` を上書き（狭める向き）として受ける案も採らなかった。**
          逃げ道にはなるが、`--border 2` と書けば「外周 2px は純白です」と
          言わせられるということであり、上の H3（外周 2px だけ白い灰色一面）を
          オプション 1 つで開け直すことになる。実害のほうは `settled_band` が
          直したので、逃げ道そのものが要らない
        - **`unmeasurable` でも `rgb` と `delta_e` を返すようにした。** 均一度の
          検査を色差の計算より前に置いていたため、実写のベージュの布を検査した
          人の手元に「測れませんでした」しか残らず、**「白から ΔE 33 外れている」
          という次の一手を決める唯一の数**が結果から消えていた。`color_space` の
          `unmeasurable` が CICP を残しているのと同じ作法で、合格でないことは
          `status` が言い切っている
        - **`--derive` / `--sizes` の寸法も profile を迂回していた。**
          直前の修正は `spec.format` しか見ておらず、`--profile amazon --sizes 400`
          は警告を 1 件も出さずに 400x400 を書き、その出力は同じ amazon の lint で
          `longest_side` が fail になっていた。形式の迂回と違って**出力が実際に
          規格違反になる**ので実害は直接的である。`longest_side_min` /
          `longest_side_max` / `max_pixels` に照らし、派生ごとに
          `PROFILE_OVERRIDDEN` を出す。**照らすのは実際に書く寸法**で、
          `resize::plan`（`output::resolve` と `render` が使うのと同じ純関数）に
          最終画像を渡してから当てる——指定した数をそのまま規格に当てると、
          縦長の素材で長辺 1200 になる実行にまで「規格の外です」と言うことになる。
          寸法を 1 つも書かなかった派生は最終画像をそのまま書くので対象外
          （形式を書かなかった派生が `--output` の解決結果を継ぐのと同じ関係）
        - **`iref` の `auxl` に上限が無かった。** `BTreeSet` 化で item ごとの
          線形探索は消えたが、集合そのものの大きさが入力に比例して伸び続けて
          いた。`MAX_PROPERTY_REFS` と同じ要領のカウンタ（`MAX_AUXL_LINKS`、
          値は `MAX_ASSOCIATIONS` と同じ）を置いた。実測（`auxl` 4000 箱 ×
          65535 対応、524MB）で 0.86s / RSS 2506MB → 0.05s / RSS 506MB
        - **`MAX_ASSOCIATIONS` の根拠が事実と合っていなかった。** 「256×256 =
          65536 枚の倍」で 131072 を置いていたが、**アルファも grid なら
          65536×2 + 2 = 131074** で、理論上の最大構成をちょうど弾く位置に上限が
          あった。`1 << 18`（262144）へ上げ、doc の計算を事実に合わせた
  - [x] **占有率を「見える範囲」で数えるようにした**（Phase 26 の受け入れ検証中に
        見つけた、Phase 22 からあった不具合。Phase 26 が作ったものではない）。
        `--profile amazon` で書いた画像が同じ amazon の lint で `fill_ratio` に
        落ちることがあり、**商品を拡大して配置したときだけ**起きていた。
        - 原因は物差しの食い違いである。キャンバスへ載せる矩形（`content_bounds`、
          アルファが 0 でない画素）には、フェザーと境界帯のアルファが 1 から
          立ち上がる**見えない縁**が片側 5〜6px 含まれる。下地へ落とせば下地の色
          そのものなので `kiri lint` には見えず、測る矩形は狙った占有率より痩せる。
          痩せる幅は**縁の厚み × 倍率 ÷ キャンバスの 1 辺**で、縁の厚みは px で
          決まる（素材の大きさに依らない）ので**倍率がそのまま不足の係数になる**。
          実測（合成シーン、1600px / 0.86）: 倍率 0.99 → 0.860、1.49 → 0.859、
          1.97 → 0.849、3.90 → 0.842、6.37 → **0.820**（lint は 0.837 を測って
          exit 5）。`FILL_RATIO_MARGIN` の 0.01 と `detect_subject` の
          `BBOX_MARGIN` の +0.02 が倍率 4 までは不足を飲んでいた
        - 直したのは**書く側**である。切り詰めはそのまま（フェザーを落とさない）で、
          `canvas::plan` へ渡す占有率だけを `切り詰めた範囲 ÷ 見える範囲` 倍する
          （`visible_bounds` / `visible_fill_correction`）。`canvas.rs` には 1 行も
          触れない——`set` を 1 つの数へ畳んだのと同じ作法である。補正後は同じ
          5 枚で 0.864〜0.886 に収まり、倍率との相関が消える
        - **前景の芯（アルファ `FOREGROUND_THRESHOLD` 以上）は見えなくても数える。**
          床が無いと胴体が背景と同じ色の商品で矩形が点まで縮む——400x400 の白地に
          ΔE 1.7 の矩形と 20px の黒い点を置いた画像を `--tolerance 1` で通すと、
          占有率が 1.0 で止まって `content` が 1600x1600（余白ゼロ）になった。
          床を張って 1394x1394 に戻した。補正が受け持つのは**芯の外にある
          フェザーだけ**で、芯そのものを「見えない」と言い出す権限は無い
        - 測定は**外側から内へ 4 回舐める**（見える範囲は切り詰めた矩形にほぼ
          等しいので、どの走査も数行・数列で止まる）。実写 24.5MP（crop 4.99MP）の
          実測で、全画素を舐める形は +0.12s、外側から舐める形は +0.02s で
          同じ矩形を返す。合成は `save::flatten_onto` と同じ整数式を使う
          ——予測しているのは書き出したファイルの画素そのものである
        - **「見える」の判定に新しいしきい値を置かない。** 下地へ落とした色と
          下地の ΔE76 が `UNIFORM_DELTA_E`(5.0) を超えるか、で決める——lint が
          主体を見立てるのに使っているしきい値の下限そのもので、下地を塗った
          キャンバスでは外周が 1 色なので両者は同じ数になる。アルファのしきい値で
          書かないのは、見えるかどうかが商品の色に依るためである（白に近い商品は
          アルファ 60 でも見えない）
        - **透過を残す実行では補正しない。** lint はそこではアルファの外接矩形で
          測るので、`content_bounds` が測ったものとぴたり同じである。判定は
          `writes_opaque`（`--flatten`、または透過を扱えない形式）で、JPEG は
          `--flatten` を書かなくても `save` が潰すので対象に入る
        - **`FILL_RATIO_MARGIN` を広げる案は採らなかった。** 不足は倍率に比例して
          増えるので（倍率 10 なら 0.06 を超える）**どんな定数でも足りない倍率が
          必ずある**うえ、倍率 6 を飲む定数は拡大しない実行の商品まで一律に
          大きくする。**lint の閾値を下げる案も採らなかった**——
          `UNIFORM_DELTA_E` は `cutout` が「背景と同じ色」と言うのに使っている数
          そのもので、下げれば切り抜き自体が変わる。lint だけに 2 つ目の数を
          置くのは `lint.rs` の冒頭が禁じている。そもそも**アルファ 3 の画素を
          白へ落としたものは白であり、lint の測りは正しい**
        - 既存の往復テスト（`..._end_to_end`）は 2000px の素材で走らせており、
          コメントがそう宣言しているとおり**拡大を 1 度も踏まない**。
          `what_a_profile_writes_passes_the_same_profiles_lint_even_when_it_upscales`
          を足し、300 / 400 / 600 / 900px の素材（倍率 6 台〜2 台）で往復させる。
          **拡大したことを `canvas.scale` と `CANVAS_UPSCALED` で確かめてから
          合否を見る**——素材の構図が変われば倍率は動くので、そこを見ないと
          テストが緑のまま何も踏まなくなる。補正を外すと amazon / 300px
          （倍率 6.31）で `fill_ratio=fail` / exit 5 になることを確認した
  - [x] `--profile` が形式を決めてよい場面を絞った。**拡張子はあるが kiri の
        知らない綴り**（`-o out.xyz`）では profile の有無に関わらず
        `UNKNOWN_OUTPUT_FORMAT` で断る——`OutputFormat::from_path` の `None` が
        「拡張子が無い」と「未知の拡張子」を兼ねていたため、`--profile` を
        付けるだけで `.xyz` という名前の JPEG が書けていた。また
        `--derive` / `--formats` が自分で書いた形式は優先順位の 4 段を 1 つも
        通らないので、profile の `formats` の外へ出た派生ごとに
        `PROFILE_OVERRIDDEN` を 1 件出す（`data` の `derive` / `role` でどの出力かを
        名乗る）——黙って通すと**amazon で書いたものが同じ amazon の lint で落ちる**
  - [x] batch の spec に `profile` キーを足した（`ItemSettings` / `pick!` /
        `SETTING_KEYS` の 3 箇所）。未知の名前は CLI と同じ関門（`profile::named`）で
        `UNKNOWN_PROFILE` として断る。spec でだけ黙って既定へ落ちると、**数百点を
        書き切った後に「規格へ収めたはずのものが収まっていない」**という最も遅い
        気づき方になる
  - [x] `--fill-ratio` の既定 0.85 の doc から「EC プラットフォームで広く求められる
        占有率」という**規格の主張を外した**。規格が求める占有率は profile の表が
        持つ。同じ主張を 2 箇所に置くと、片方を直したときにもう片方が残る
  - ※ **外部 JSON で表を差し替える口は作らなかった。** kiri は契約を自分で配る設計
        （`kiri schema`）で、同じ版の kiri が同じ入力から同じ結果を出すことを約束して
        いる。表を外から差し替えられると、`kiri schema` が配った `profiles[]` と実際に
        効く規格が食い違いうるし、`revision` が指すものが実行環境ごとに変わる。
        規格が変わったら kiri の版を上げる、というのがここでの答えである
  - ※ **楽天と Yahoo! ショッピングは載せなかった。** 両社のガイドライン本文は
        ログインの内側にあり、**一次情報として読めない。** 出典の無い数値を
        `kiri schema` が配ると、不合格の根拠を利用者が辿れない——「kiri がそう言うから」
        以上のことが言えない合否に、納品を止める重みは無い。`revision` を持つ設計
        なので、一次情報が手に入った時点で足せる。**推測で埋めて後から直す**のは、
        一度配った契約を引っ込めることになるので採らない。同じ理由で `amazon` の
        ファイルサイズ上限も入れていない（二次情報では 10MB と書かれることが多いが、
        `source` の URL から辿れない）
  - ※ **`Rules` には「触れたら不合格」になる条件だけを置いた。** 推奨（1000px 以上なら
        ズームが効く、など）を混ぜると `kiri lint` が推奨違反で落とすことになり、
        規格と kiri の好みが同じ表の中で見分けられなくなる。kiri の判断は
        `write_defaults` 側（当時は固定の目標長辺 1600）に置き、そこでは必ず根拠を
        述べる
        （※ この固定値は Phase 26 で梯子へ置き換わった。**定数名だけをここから
        外した**のは、`every_code_named_in_the_docs_exists` が「文書が名指しする
        識別子は実在する」ことを求めており、消えた名前を計画書が呼び続けると、
        次にそれを読んだエージェントが実在しない定数を探しに行くためである。
        当時の値 1600 と判断そのものは記録のまま残してある）
  - ※ **`Option` の `None` は「規定なし」で、「どんな値でも合格」ではない。**
        規定していない条件は `checks[]` に 1 行も出さない——`pass` として並べると、
        `shopify` が構図（背景色・占有率）を見ていないことが結果から読めなくなる。
        `pass` は「見た上で通った」を意味する語である。真偽で持つ 3 つ
        （`square` / `alpha_allowed` / `srgb_required`）も同じ扱いで、**偽の側が
        「規定なし」**である
  - ※ **形式の優先順位を「`--format` > `--output` の拡張子 > profile > 既定」の
        4 段にした。** 優先順位の 1 本（明示指定 > profile > 既定）に素直に従って
        拡張子を「未指定なら」の推論（＝既定の振る舞い）と読むと、profile が勝って
        **`-o out.png --profile amazon` が `.png` という名前のファイルに JPEG を書く**。
        拡張子と中身が食い違うファイルは `outputs[].path` が嘘をつくことになり、
        配信側も他のツールも拡張子で形式を判断するので、その嘘は kiri の外まで
        運ばれる。**綴った名前のほうが profile より具体的な指示である**と読むのが、
        「指定したのに効かない」を作らないという `cli.rs` 全体の姿勢に合う。
        押しのけたことは `--format` で押しのけたときとまったく同じ
        `PROFILE_OVERRIDDEN` / 同じ `data` の形で言う（押しのけた主が指定だったか
        綴った名前だったかで、読む側の分岐を増やす理由は無い）
  - ※ **`PROFILE_OVERRIDDEN` は項目ごとに 1 件出し、同じ値なら黙る。** まとめて
        1 件にすると、どの指定が profile を押しのけたのかを `message` の散文から
        抜き直すことになる。逆に同じ値に落ち着いた項目（`-o out.png --profile shopify`）
        まで出すと、「profile を指定したのに効かなかった項目はどれか」という問いに
        答えを足さない行が並ぶ
  - ※ **`passed` は「全項目 `pass`」である。** `fail` も `unmeasurable` も `skipped` も
        合格ではない（`compliance::FailOn::evaluate` と同じ定義）。**exit 5 の意味は
        「成果物はある。人が見る対象」**で、「検査できなかったので人が見てほしい」は
        まさにそれである。3 つを分けるのは**次の一手が違う**ためで、そこだけが
        名前を分ける理由である——`fail` はその項目を直す、`skipped` は形式を変えれば
        必ず測れるようになる（JPEG か PNG で渡し直す）、`unmeasurable` は形式を
        変えても同じ結果になるので素材を見る。`skipped` を `unmeasurable` に畳むと、
        「JPEG で出し直せば答えが出る」という最も安い次の一手が消える
  - ※ **AVIF の画素は 1 つも読めない。** pure Rust の AVIF デコーダが実質存在せず、
        dav1d（C）は `Cargo.toml` が立てた基準を崩す。コンテナから読める事実
        （寸法・`has_alpha`・色の名乗り）だけで判定し、`background` と `fill_ratio` を
        `skipped` ＋ `PROFILE_UNCHECKABLE` で明示する。**どれを飛ばしたかは
        `data.checks` に配列で必ず入れる**——文面から項目名を抜き直させない。
        警告は `checks[]` から数える（別に数え上げると、条件を 1 つ足したときに
        `checks[]` と警告が食い違いうる）
  - ※ **実測: kiri が書く AVIF の `av1C` は `configOBUs` を持たない。** ペイロードは
        ちょうど 4 バイト（`81 3f 40 00`）で、シーケンスヘッダを 1 バイトも含まない
        （`an_av1c_box_from_kiri_carries_no_config_obus`）。したがって CICP は
        `iloc` を辿って **mdat 側の主画像のシーケンスヘッダ**から読んでいる。
        これは Phase 18 の節の記述（「ravif が AV1 シーケンスヘッダの CICP
        （1 / 13 / 6 / full）で sRGB を名乗る」「`colr` ボックスは出ない」）と
        矛盾しないが、**どこにシーケンスヘッダがあるかはそこに書かれていなかった**
        ——`av1C` の中だけを見る読み方では `ColorNaming::Unknown` になり、
        `srgb_required` を持つ規格の `color_space` が全部の AVIF で
        `unmeasurable` に落ちる
  - ※ **CICP の「未指定」（2）は `fail` ではなく `unmeasurable` にした。**
        `AvifMeta` は埋め込み ICC の有無を持たないので、`fail` と断じると
        **実際には sRGB を名乗っているファイルを規格違反として落とす**。落とすほうへ
        外すのは、合否に納品を止める重みがある以上いちばん高くつく誤りである。
        `unmeasurable` は合格ではない（`passed` は落ちる）ので黙って通すことにも
        ならない
  - ※ **書いたものが同じ規格の lint で落ちないよう、余裕は書く側へ寄せた。**
        `FILL_RATIO_MARGIN` = 0.01（`canvas::plan` の `round()` で最大 0.5px、
        JPEG の縁のにじみと `--feather` のぶんを飲む）と
        `BACKGROUND_DELTA_E_TOLERANCE` = 2.0（JPEG は 8x8 の量子化で純白を 255 の
        まま揃えない。**CIE76 の ΔE 2.0 は kiri が独自に決めた数ではない**ことに
        意味がある——規格の合否を分ける線に、根拠を辿れない数を置かない）。
        占有率は**書く側とまったく同じ測り方**で出す（`max(bbox_w/W, bbox_h/H)`。
        面積比にすると、正方形のキャンバスに正方形の商品を 0.86 で置いた画像が
        0.74 と出て、余裕をいくら積んでも防げない形で落ちる）
  - ※ **`kiri lint` は AVIF を先に判別する。** `load::load_with` は AVIF に対して
        必ず `UNSUPPORTED_FORMAT` を返すので、そちらを先に呼ぶと「失敗したので
        次を試す」という形になる。エラーを握り潰して次へ進む経路を作ると、
        本当に壊れた JPEG まで AVIF の判定へ流れ、利用者が受け取るのは
        「AVIF として辿れません」という的外れな文面になる
  - ※ **統合テストは実装の定数を 1 つも書き写さない。** 素材の寸法も占有率も
        上限バイト数も `kiri schema` が配る `profiles[]` から組む（下限 + 200px、
        下限 + 0.03、√(上限 ÷ 4) + 100）。書き写すと、規格が改訂されたときに
        **テストだけが古い世界を緑のまま語る**。素材も `tests/common` の
        `product_image` ではなく矩形 1 つの `flat_product` を新設した——角丸・陰影・
        ノイズを持つ素材では外接矩形が常に高さの 0.68 になり、`fill_ratio_min` 0.85 と
        純白背景を同時に満たせないので、**「1 項目だけ外した画像」が作れない**
  - ※ テスト名: `the_published_profiles_are_the_names_the_parser_accepts`
        （受け入れ基準 (a)。`profiles[]` を `profile::ALL` と直に突き合わせると
        同じ定数を左右に置くだけで何も守らないので、**表の形**——配った名前で
        本当に呼べるか、名前が一意か、`revision` / `source` を名乗るか、`rules` の
        各項目が約束した型で出ているか——を固定する。`accepts` は clap の候補
        そのものなので、「候補として案内した名前が引けない」が起こりえない）/
        `a_conforming_image_passes_every_check_of_its_profile`（(b) の前半。
        3 プリセットすべてを回す）/ `breaking_one_rule_fails_exactly_that_check`
        （(b) の後半。`longest_side` / `background` / `fill_ratio` / `alpha` /
        `square` の 5 通りを、**落ちた `checks[].name` がちょうど 1 つ**であることで
        固定する。透過は「背景の RGB は白のまま、アルファだけ 0」にする——黒で
        埋めると背景の検査まで一緒に落ち、「1 項目だけ」でなくなる）/
        `a_file_over_the_byte_budget_fails_only_the_size_check`（(b) の
        `file_size`。単色の PNG は数 KB まで縮むので `noisy_image` が要る）/
        `a_profile_that_regulates_no_composition_checks_none_of_it`
        （**規定の無い項目は検査もしない。** 同じ画像が amazon では落ちることまで
        見て、shopify が通したのが見落としでないことを言う）/
        `an_avif_skips_the_pixel_checks_and_says_which`（(c)）/
        `a_profile_without_pixel_rules_skips_nothing_in_an_avif`（(c) の裏。
        飛ばした項目が無いときに警告が出ないことは、警告が `checks[]` から
        数えられている証拠でもある）/
        `an_explicit_option_beats_the_profile_and_actually_takes_effect`（(d)。
        **警告が出ることだけを見ない**——押しのけた値が成果物に現れていることを
        結果 JSON と `kiri lint` の実測から確かめる。警告だけを見ると「警告を
        出しながら profile の値で書く」という最悪の形を通す）/
        `the_output_extension_overrides_the_profile_format`（(d) の 4 通り目。
        書いたファイルの先頭 8 バイトが PNG の署名であることまで見る——
        `outputs[].format` が同じ嘘をついても気づけるようにするため）/
        `a_profile_decides_nothing_outside_the_items_it_declares`
        （**無害性の回帰。** `--profile` を渡した実行との単純な比較にはできない
        ——profile は canvas を必ず決めるので、明示で打ち消そうにも「canvas を
        指定しない」とは書けず、両者が同じバイト列になる指定は存在しない。
        代わりに profile が触ると宣言した 6 項目を**全部明示した**実行を
        profile あり / なしで走らせる。このとき profile は 1 つの値も決められない
        ので、**成果物が 1 バイトでも違えば宣言の外へ手を伸ばしている**）/
        `what_a_profile_writes_passes_the_same_profiles_lint_end_to_end`
        （**書く側と検査する側が同じ規格を指していることの、唯一の実行による
        保証。** `profile.rs` と `lint.rs` の単体テストは組み立てた `Facts` の上で
        見るので、JPEG の量子化もキャンバス配置の丸めも 1 つも乗らない）/
        `two_lint_runs_agree_byte_for_byte`（並びを含む決定性。**バイト列で
        比べる**——`Value` へ読み直すと `Map` が綴りで並べ替える）/
        `lint_writes_nothing_at_all`（`--dry-run` のような打ち消しが無いので、
        書かないことそのものが契約）/
        `a_spec_names_a_profile_and_inherits_it_from_the_defaults` /
        `an_unknown_profile_in_a_spec_is_refused_with_a_code`（`rakuten` /
        `Amazon` / 末尾空白 / 空。**近いから通すをしない**——`rakuten` は載せて
        いないだけで実在する規格の名前である）/
        `lint_separates_a_bad_request_from_a_bad_input`（`--profile` 無しと未知の
        名前は exit 2 で stdout が空、読めない / 辿れないファイルは exit 3 で
        `ErrorReport`。**入力が存在しなくても exit 3 にならない**ことが、引数の
        検査が先に走っている証拠である）。
        既存の契約テストは `the_readme_warning_table_lists_every_warning_in_the_contract`
        （`PROFILE_OVERRIDDEN` / `PROFILE_UNCHECKABLE` の 2 行を README へ足した）と
        `every_published_field_exists_in_the_result`（`--profile` を渡す実行と
        `kiri lint` の実行が既に足されていた）が追随している

- [x] Phase 23: `--rotate auto` ＋ batch の `set`（`SCHEMA_VERSION` は 2 のまま据え置き）
  - [x] `--rotate` を `f64 | "auto"` へ広げた。`subject.level_rotation` は**もともと
        「`--rotate` にそのまま渡せる値」として返る契約**で、渡すところだけが利用者の
        手作業だった。auto は**測りと回転を繋ぐだけ**で、新しい幾何は 1 行も書いていない
  - [x] `cli::parse_rotate` と `RotateArg { Degrees(f64), Auto }`。spec の `rotate` も
        `Option<f64>` → `Option<serde_json::Value>` へ広げ、**CLI とまったく同じパーサを
        通す**（`--max-bytes` と同じ事情——エージェントが書く JSON には `90` と `"90"` の
        両方が現れるうえ、`auto` は数値では表せない）。読めない値は `INVALID_ROTATE` で
        その項目だけを落とす。黙って 0 度へ落とすと、**水平出しを頼んだつもりの項目が
        回らないまま数百点に混ざる**
  - [x] **既存の契約を 1 つ緩めた。** 以前は `{"rotate": "90"}` が `SPEC_INVALID` で
        落ちていたが、`"auto"` を受ける以上、文字列の `"90"` を断る理由が無くなる
        （断れば「CLI では通る書き方が spec でだけ通らない」道具になる）。テストは
        `the_batch_spec_rejects_an_angle_that_is_not_a_number` を
        `the_batch_spec_reads_an_angle_written_as_a_string` へ置き換えた。
        **Phase 23 で挙動を変えた既存契約はこれ 1 つだけである**
  - [x] `settings.rotate` が `f64 | "auto"` の union になった。auto を 0 に潰すと
        「回さないと指定した」と「測って決めろと指定したが測れなかった」が**同じ JSON に
        なる**。`kiri schema` の `unit` 語彙へ `deg_or_auto` を足したのは、受け手が
        `as_f64()` で読んでよいかがそこで変わるためで、`deg` に相乗りさせない
  - [x] 適用しない 4 つの場合はどれも 0 度のままにして `ROTATE_AUTO_SKIPPED` で報せる。
        `data.reason` は機械可読な 4 値（`no_subject` / `low_confidence` /
        `not_measurable` / `not_rectangular`）。**どの条件で落ちたかで次の一手が変わる**
        ——`low_confidence` なら `--bbox` で主体を教えれば済むが、`not_measurable`
        （丸いもの）は何を渡しても測れないので角度を自分で決めるしかない
  - [x] `kiri rotate --angle` には auto を足していない。**切り抜きを通らないので subject を
        1 度も測っていない。** 測る経路をそこへ足すのは「渡された角度を回す」という
        このサブコマンドの仕事ではなく、`kiri cutout` と二重の測り方を持つことになる
  - ※ **形の門（`not_rectangular`）は計画に無い。** `src/cutout/subject.rs` の
        `level_rotation` の doc 自身が「**自動では適用しない。** 傾きを直すかどうかは
        構図の判断」と書いており、**計画 Phase 23 はその注意書きに正面から反していた**
        （§7.2 へ追記した）。同じ値を消費する計画が、書いた当人の但し書きを読んでいない
  - ※ **実測が doc のほうを裏付けた。** 水平に置いたフライパン（円＋取っ手、真値 0 度）に
        `-21.7 / +31.9 / -43.3 度`を `confidence: high` で返す。円も安定しない
        （直径 240 → 45.0 度、320 → 0.0 度、420 → `None`）。**最小面積外接矩形が
        「物の向き」を意味するのは、その物が実際に矩形に近いときだけ**で、円に取っ手が
        1 本生えた形では最小の位置が取っ手と円の接し方——つまり輪郭の量子化——で決まる。
        このまま配ると、**仕上がりを目で見るまで誰も気づけない形で絵が傾く**
  - ※ **広がり（`TILT_AMBIGUOUS`）でも最小外接矩形の縦横比でも分離できない。**
        広がりは「全向きで平ら」しか見ていないので、フライパン（17.1%）は傾いた長方形
        （51.9%）と同じく門の 2% をはるかに上回って通る。縦横比に至っては順序が逆で、
        **正しく置かれたフライパン（1.71）が、本当に傾いた長方形（1.63）より上に来る**
        ——「1 に近いほど条件が悪い」という読み方ではフライパンのほうが条件の揃った形に
        見えることになり、線を引く向きが決まらない
  - ※ **分離できたのは凸包の面積 / 最小面積外接矩形の面積である。** 最小外接矩形が向きを
        語るのはその形が矩形に近いときだけで、**この比はまさにそれを測っている**。
        長方形は 1.0、角丸で 0.98、取っ手つきで 0.88、円は π/4≈0.785 が上限になる。
        `level_rotation` と**同じ凸包から一度に**返し（`Tilt { rotation, fill }`）、
        包を 2 度組んで値が食い違う余地を型の上で作らない。分母を最小の面積にしたのは、
        最大で割ると**傾きの曖昧さと形の矩形らしさが 1 つの数に混ざる**ためで、ここで
        言いたいのは「採ろうとしているその矩形が、形をどれだけ説明しているか」である
  - ※ **`TILT_SHAPE_MIN_FILL` = 0.85 は谷の中央であって、当てはめた値ではない。**
        合成 35 枚＋実写 4 枚で較正した結果、**誤る側の最大が 0.797（円 d=240）、
        正しく回せる側の最小が 0.881（取っ手つきマグ t=10）**で、そのあいだ 0.084 に
        他の標本は 1 つも無い。実写（不織布の上のリモコン）は 0.903〜0.968 で、いずれも
        谷の上側にある。**角丸を較正に並べてあるのが要点で**、四隅を落とすと充填率は
        その面積ぶん下がる（1.000 → 0.98 付近）——門をそこまで上げると携帯・本・箱が
        回らなくなる
  - ※ **成績: 合成 35 枚で誤って回す件数が 0 になった。** 正しく回す 21、止めて正解 8、
        惜しくも止めた 4（楕円 t=0 / L 字 t=0 / L 字 t=12 / 三角 t=0）。残る 2 枚は門の
        手前で決着する（円 d=160 は `confidence` が low、円 d=420 は `level_rotation` が
        `None`）。門が無ければ誤って回すのは 8 件だった。**見送りは「回さずに
        `ROTATE_AUTO_SKIPPED` で報せる」＝安全側の失敗**で、回してしまうほうは目で見る
        まで気づけない
  - ※ **`subject.level_fill_ratio` として結果 JSON にも出す。** `level_rotation` を自分で
        読んで回す経路でも同じ値で同じ判断ができないと、**門は kiri の中だけのものになる**。
        警告の `data` に測った値（`level_fill_ratio`）としきい値（`min_fill_ratio`）の
        両方を載せるのも同じ理由で、片方だけでは受け手が「あと少しだったのか、全く違うのか」
        を分けられない
  - ※ **`level_rotation` の戻り値は 1 つも動かしていない。** `kiri info` の出力は不変で、
        門が効くのは `--rotate auto` にだけである。角度だけを返して黙る案（門に掛かったら
        `level_rotation` を `None` にする）は採らなかった——「measurable だが向きを
        語らない」を `None` に畳むと、**既存の「測れない形では `None`」という意味が
        2 通りになる**
  - [x] `main.rs` の `print_subject` も同じ門で断るようにした。ここを直さないと
        **人が読む行だけがフライパンに `--rotate -21.7` を勧め続ける**。結果 JSON が
        止めているものを端末の行が勧めるのは、同じ実行が 2 つの答えを返すのと同じである
  - [x] batch の spec の**最上位**へ `set: { align, fill_ratio }` を足した。1 枚ずつ
        `fill_ratio` を決めても並べたときには揃わない——占有率は「外接矩形がキャンバスの
        何割か」なので、縦長と横長が混ざると高さがばらつく。`defaults` の隣に置かなかった
        のは、**揃える相手が「このセットの全点」で、1 点だけを見ても決まらない**ため
        である。`defaults` に置けば項目側で上書きでき、「自分だけ別の基準で揃える」という
        意味を持たない指定が書けてしまう
  - [x] `align` は 2 値。`height` は**出力上の商品の高さを共通の `T*CH` にする**
        （撮影距離の正規化）、`bbox` は外接矩形を共通の枠へ長辺基準で収める
  - [x] **`canvas.rs` には 1 行も触れていない。** `plan()` が
        `scale = min(CW*f/cw, CH*f/ch)` を返すので、狙った倍率 s_i を出したければ渡す
        `f` のほうを組み替えればよい——`f_i = s_i * max(cw_i/CW, ch_i/CH)` を代入すると
        `min()` の中が両方 s_i になる。`bbox` は `f_i = T`（全点同じ）、`height` は
        `f_i = T * max(1, (CH*cw)/(CW*ch))` に畳める。**配置の算術は 1 つのままで、
        セット統一は「何を渡すか」だけの話になる。** 式は 1350 通りの数値で検算して
        単体テストにも残した——出力を測る検査は丸めの後しか見られないので、**式の誤りを
        1px の差として受け取る**ことになる
  - [x] `T` は `set.fill_ratio` があればそれ、無ければ pass 1 で測った占有率の**中央値**。
        `BatchReport.set.source` が `specified` / `median` のどちらかを名乗るのは、
        **同じ数でも次の一手が変わる**ためで、書いた値なら書き直せばよいが、中央値は
        セットの構成を変えない限り動かない
  - [x] **pass 1 では切り抜きを回さない。** 要るのは `subject.normalized_bbox` だけで、
        それは `see_background` が背景の見立てと一緒に返す。増えるのは decode 1 回ぶんで、
        1000 点級でも「2 回デコードする」が素直（キャッシュは要らない）。
        **`set.fill_ratio` を書いた実行では pass 1 ごと省く**——中央値を採る相手がいない
        ので、測っても 1 点も使わない decode を全点ぶん積むだけになる
  - [x] 並べ替えてから中央を採るので順序に依らず、**`--jobs` を変えても 1 ビットも
        動かない。** `--dry-run` でも測りは走る（Phase 19 の「`--dry-run` でも探索は
        走る」と同じ規約）。読めない項目と主体が `None` の項目は材料から外す——1 点の
        素材事故がセット全体を止めないためで、本当のエラーは pass 2 が項目ごとに報告する。
        **1 点も測れなければ `set` は効かず `SET_NOT_MEASURED`** でそう言う
  - [x] プールは 1 本だけ建てて 2 つの pass で使い回す。2 本建てると `--jobs` の意味が
        「同時に走る件数」から「pass ごとの上限」へ**静かに変わる**
  - [x] 優先順位は **明示指定 > set > profile > 既定**。`item` / `defaults` に
        `fill_ratio` がある spec に `set` を足したら、**spec を読んだ時点で**
        `INVALID_SET` で断る（成果物は 1 つも書かれない）——どちらを消すかは書いた人に
        しか決められないので、黙ってどちらかを勝たせない。`set` があるのに canvas が
        取れない項目も `INVALID_SET` で、こちらは他の失敗と同じくその項目だけが落ちる。
        判定を `to_cutout_args` ではなく解けた `CutoutArgs` の側でするのは、
        **profile が決めた canvas も数えたい**ため
  - [x] profile の `fill_ratio` は `set` が上書きし、`PROFILE_OVERRIDDEN` の `data.by` が
        `set` と名乗る。**Phase 22 の「同じ値なら黙る」はここでは通さない**——`set` が
        効かせるのは点ごとの `f_i` で、`T` が profile の値と偶然一致しても実際に効く値は
        点ごとに違う
  - [x] `CanvasReport.fill_ratio` は**効いた `f_i`** を返す（canvas ブロックは効いた値を
        語る規約）。`set` を使わない実行では要求値と同じなので出力は 1 バイトも変わらない。
        `BatchReport.set` は `set` を書いた実行にしか現れず、**1 点も測れずに効かなかった
        実行でも現れない**（揃えていない実行に書く値が無い。そのことは
        `SET_NOT_MEASURED` が理由つきで言う）
  - ※ **`kiri set` という新しいサブコマンドにはしなかった**（§7.6 の却下案のとおりに
        決着した）。spec / `ItemSettings` / 警告の継承 / `--jobs` / `--dry-run` の規約を
        もう 1 系統持つことになり、`kiri schema` が配る契約が二重化する
  - ※ **`SET_SCALE_CLAMPED` は計画の見立てと違う。** §7.2 の箇条書きは「揃えた結果
        1 点だけが極端に外れるとき」と書いていたが、**横長商品では常態である。** 正方のキャンバスで height 揃えなら
        `f_i = T * max(1, cw/ch)` なので、`T=0.85` では**切り抜いた内容の**縦横比
        1.18:1 を超える横長で必ず当たる。式の誤りではなく物理で、高さ `0.85*CH` を
        与えれば横幅は `0.85*CH*(cw/ch)` を要求し、それがキャンバスの幅を越える。
        1000x1000 / `T=0.85` では、内容の縦横比 1.23:1 で 37px、1.65:1 で 244px、
        1.98:1 で 345px 足りない（**binding なのは素材の縦横比ではなく content bounds の
        ほうである**）。黙って「高さが揃った」ことにせず、1.0 で止めて**要求した `f_i`・
        使った 1.0・目標との差**を `data` に載せる——数 px の不足なのか半分しか無いのかで
        次の一手が変わる
  - ※ テスト名（`--rotate auto`）: `cutout_rotate_auto_levels_the_subject`
        （受け入れ基準 (a)。書いた画像へもう一度 `info` を掛けて測り直す往復で見る）/
        `cutout_rotate_auto_does_not_turn_what_it_cannot_trust`（(b)。信用できない
        見立てでは 1 画素も動かさない）/ `cutout_rotate_auto_leaves_a_handled_shape_alone`
        （(b) の拡張。取っ手が 1 本生えた円は回さない）/
        `cutout_rotate_auto_still_levels_a_boxy_product`（(b) の拡張。**門が効きすぎて
        いないこと**——門だけを見る検査は「何も回さない」実装を緑にするので、角丸が
        今までどおり通ることを同じ往復で固定する）/
        `the_shape_gate_separates_what_the_rectangle_describes`（(b) の拡張。門の値
        そのものを `kiri info` で見る）/
        `a_run_without_a_rotation_never_mentions_the_auto_gate`（(c)。`--rotate` を
        渡さない実行は門を 1 度も通らない）/ `cutout_rotate_auto_is_deterministic`（(d)）/
        `the_batch_spec_accepts_auto_and_a_number_for_the_rotation` /
        `a_malformed_rotation_in_a_spec_is_refused_with_a_code` /
        `every_unreadable_rotation_in_a_spec_lands_on_the_same_code`（(e)）/
        `the_rotate_option_publishes_auto_as_a_choice`（(f)。`--rotate` は自由な数値も
        取るので `PossibleValuesParser` では作れず、`accepts` が空のままだと**綴りを
        外したときに code 無しの exit 2 で落ちる項目の候補を、呼ぶ前に知る手段が無くなる**）/
        `a_malformed_rotation_on_the_command_line_is_refused_by_the_parser` /
        `the_batch_spec_reads_an_angle_written_as_a_string`（(f)。**緩めた契約があったのは
        この 1 本の場所である**）。
        テスト名（`set`）: `a_set_aligns_the_subject_height_across_shooting_distances`
        （(a)。同じ商品を 3 通りの距離で撮ったセットで、出力上の高さが ±1px に揃う）/
        `a_spec_without_set_carries_no_set_block`（(b)）/
        `a_bbox_aligned_set_writes_the_same_bytes_as_one_fill_ratio`（(c)。**全点が同じ
        `f_i` になる**ことを、同じ `fill_ratio` を 1 つ書いた実行とのバイト一致で言う）/
        `the_set_target_is_the_median_of_the_measured_occupancies`（(d)）/
        `an_extremely_wide_item_is_clamped_without_touching_the_others`（(e)。**他の点が
        1 バイトも影響を受けない**ことまで見る）/
        `a_set_that_measures_nothing_says_so_and_stands_aside`（(f)）/
        `the_same_set_spec_is_deterministic`（(g)。`T` も全項目の出力バイト列も 3 回とも
        同じ）/ `a_set_that_cannot_take_effect_is_refused`（(h)）/
        `the_number_of_jobs_does_not_move_the_set_target`（(i)）/
        `a_set_overrides_the_profile_fill_ratio_and_says_so`（(j)）。
        既存の契約テストは `every_published_unit_is_in_the_known_vocabulary`
        （`deg_or_auto` を語彙へ足した）/ `every_published_field_exists_in_the_result`
        （`set` を書いた `--dry-run` の batch 実行を 1 つ足した。**`set` ブロックは
        `compliance` / `optimize` と同じく専用の実行でしか現れない**）/
        `every_code_named_in_the_docs_exists`（**計画書だけが先に名指ししてよい
        「予定の code」の表から** `ROTATE_AUTO_SKIPPED` と `SET_SCALE_CLAMPED` の
        2 つを消した。実装済みのまま表に残すと、その表明のほうが落ちる）/
        `the_readme_warning_table_lists_every_warning_in_the_contract`
        （README の表へ 3 行足した）が追随している
