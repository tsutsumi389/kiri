# 5. 進捗 — Phase 17〜21

節番号は分割前のまま変えていない。`計画 §8.6` のような参照が他の節・src / tests から
150 箇所以上張られていて、**番号が識別子として働いている**からである。どの節がどの
ファイルにあるかは [実装計画](../implementation-plan.md) の索引で引く。

記号（`[x]` / `[ ]` / `※`）の意味は [§5 進捗](../implementation-plan.md#5-進捗) にある。

- [x] Phase 17: 検査のワークフローを GitHub Actions に置く
  - [x] `.github/workflows/ci.yml`。fmt / test / MSRV の 3 ジョブで、回す中身は
        [4. CI](../implementation-plan.md#4-ci) のとおり。**新規のコードは無い**——いままで手元でしか
        回していなかったものを、そのまま機械へ移しただけである
  - [x] test は feature `segment` の有無で 2 系統。**別々のジョブにしてある。**
        同じ機械で 2 系統を同時に走らせると、統合テストが `target/` の中で
        相手の実行ファイルを掴んで偽の失敗になる
  - [x] **MSRV の検査に `--features segment` を付けていない。** `tract-onnx`
        0.23.7 の `rust-version` は 1.91 で、付ければ引数の綴りも見ずに落ちる。
        `cargo +1.85 check --locked --all-targets` が手元で通ることを確かめた
  - ※ **モデルは CI で取得しない。** ISNet の 176MB が無ければ推論の検査は黙って
        飛ぶ設計なので（`tests/segment.rs`）、CI が守るのはコンパイルと非推論部分
        である。`#[ignore]` にしていないのは、置いてある機械では走らせるため
  - ※ **リリース用のワークフローは置かない。** 製品化を決めるまでは検査だけにする
  - ※ **ツールチェーンの用意にサードパーティの action を使わない。** 標準ランナーに
        rustup が入っているので、公式の `actions/checkout` / `actions/cache` と
        rustup だけで足りる。Cargo.toml が依存について立てた基準を CI 側でも
        同じ重さで守る
  - ※ **`runs-on` は標準ランナーだけ。** public リポジトリなので分数は無制限で
        無料だが、larger runner は public でも課金される
  - ※ **x86_64 では nasm が要る。** `ravif` → `rav1e` の `asm` feature が x86 の
        アセンブリを持つ。手元の aarch64 macOS では要らないので、**CI に載せて
        初めて出た**——「手元でしか回していない」ことの実例がひとつ出た形である。
        asm を切らずにアセンブラを入れたのは、AVIF の出力バイト列が変わらない
        保証が無いため

- [x] Phase 18: 出力を 1 本の派生パイプラインへ畳み、sRGB の ICC を埋める
  - [x] `image_io/derive.rs` の `Derivation` / `render()` を `write_image()` が通る。
        convert / resize / rotate / cutout / batch の全経路が 1 本になった
  - [x] **ICC を除けば出力バイト列は 1 バイトも変わらない。** 旧バイナリ（main の
        d923e4a）との md5 比較 33 本（`tests/fixtures/backgrounds` の 3 枚の convert と、
        実写 2 枚（Display P3、`sips` で JPEG 化）・合成 2 枚の convert / cutout を、
        それぞれ PNG / JPEG / AVIF へ。cutout の JPEG は `--canvas 800x800 --flatten`）。
        AVIF はそのまま一致、PNG は iCCP を、JPEG は ICC の APP2 を抜くと一致する。
        増えるのは PNG で 318 バイト、JPEG で 534 バイト（APP2 の 18 + プロファイル 516）
  - [x] 書いた PNG / JPEG を `kiri info` に戻すと `color_profile` が
        `sRGB IEC61966-2.1` で、変換も警告も起きない（`color_space` だけでは ICC が
        無くても `sRGB` と答えるので、名前で確かめる）
  - [x] `outputs[].icc`（`embedded` / `nclx` / `none`）と `ICC_NOT_EMBEDDED`。
        警告は `--no-color-convert` で画素を変換しなかったときだけ出る
  - [x] `batch.rs` の `ItemSettings` と `SETTING_KEYS` の一致をテスト 1 本で守る
        （serde の derive が渡すフィールド名を横取りして比べる）
  - ※ **AVIF は `colr` を書けない。** ravif 0.13 に口が無く、sRGB は AV1 シーケンス
        ヘッダの CICP でしか名乗れない。`--no-color-convert` の画素でも名乗りを
        外せないので、`nclx` のまま警告だけを出す。kiri は AVIF を読めないので、
        名乗りの検査はテストの中のボックスと OBU の読み取りに頼っている
  - ※ **PNG に `sRGB` チャンクを併記しない。** png クレートが iCCP と排他にしており、
        PNG 3 も併存を勧めない。`sRGB` チャンクだけを見る古い読み手には名乗りが
        届かない
  - ※ **計画書が先に名指しする未実装の code を、`tests/cli.rs` の予定の code の表で
        許した。** 着手時点で `every_code_named_in_the_docs_exists` が落ちていた。
        許すのは計画書の中だけで、表に載ったまま実装された code は別の表明で落とす

- [x] Phase 19: `--max-bytes`（品質を梯子状に落として出力を上限へ収める）
  - [x] 探索は `image_io/derive.rs` の `encode_within_budget()` に閉じた。**固定の梯子**
        `85 / 75 / 65 / 55 / 45 / 35 / 25` のうち `--quality` より小さい段だけを上から試し、
        最初に収まった段で止める（既定の 75 なら 5 段、要求品質を入れて 6 回）
  - [x] `QUALITY_REDUCED` と `MAX_BYTES_UNREACHABLE`。`data` には「いくつからいくつへ」
        「何バイトになったか」「何回エンコードしたか」を入れ、未達のときは
        `smallest_bytes` / `smallest_quality` で**あとどれだけ足りないか**を言う
  - [x] `outputs[].quality_used`（PNG は null）と `outputs[].attempts`。**`SCHEMA_VERSION`
        は据え置き**——キーの追加は後方互換で、2 へ上げる根拠は Phase 20 の
        「`outputs[]` が複数になる」だけである（§7.4 の 1 番）
  - [x] spec の `max_bytes` は**数値でも文字列でも書ける**（`512000` と `"500k"`）。
        読めない値は `INVALID_MAX_BYTES` でその項目を落とす。`batch.rs` の 3 箇所
        （`ItemSettings` / `pick!` / `SETTING_KEYS`）は Phase 18 のテストが揃いを守った
  - [x] **`--max-bytes` を渡さない実行は 1 バイトも変わらない。** `attempts` は必ず 1 で、
        `quality_used` は `--quality` の指定値になる
  - ※ **未達でも要求品質のものを書く。** 下限まで降りて届かなければ、最初に取った
        バッファをそのまま書き出す（再エンコードもしない）。どうせ上限は破れている
        ので、画質まで捨てる理由が無い。成果物は残り、終了コードも変わらない
        ——合否で落とすのは Phase 21 の `--fail-on` の仕事である
  - ※ **PNG は段を降りない。** 無損失で `quality` を持たず、段を変えてもバイト列が
        1 バイトも動かない。段の数だけ同じエンコードに時間を払う代わりに、1 回で
        `MAX_BYTES_UNREACHABLE` を出す（`attempts` は 1、`quality_used` は null）
  - ※ **AVIF の `effort` は動かさない。** 時間が桁で変わるつまみを探索の軸にすると、
        梯子が数分になる。動かすのは `quality` だけにした
  - ※ **梯子は絶対値で、時刻にもタイムアウトにも依存させない。** 「制限時間まで
        二分探索する」形は機械ごとに違う答えを出す。要求品質からの相対（-10 ずつ）も
        採らなかった——`--quality 80` と `--quality 78` が別の着地点へ落ちる
  - ※ **`--optimize` と併せても時間は積にならない。** 計画時（§7.2）の見積もりは
        「総当たり × 梯子」だったが、探索はマスクの指標で候補を選んでおり、候補ごとに
        エンコードはしない。`write_image` は探索が終わって最終画像が決まった後に
        1 度だけ呼ばれるので、実際は**探索 + 最大 6 回のエンコードの和**である。
        エンコードが積になるのは、1 実行で複数の派生を書く Phase 20 からになる。
        ヘルプと README の「積になる」は誤った見積もりだったので書き直した
  - ※ **梯子の段ごとに画素の下ごしらえをやり直さない。** `encode` を `prepare`
        （値域の検査・アルファの走査・合成・警告）と `encode_prepared` に割り、探索は
        前者を 1 回だけ呼ぶ。割る前は cutout → JPEG の 24.5MP で 1 段ごとに 98MB の
        複製と全画素走査が積んでいた。「2 回目以降の警告を捨てる」後始末も、警告が
        `Prepared` に 1 組しか無くなった時点で構造的に消えた
  - ※ **`k` / `kb` は 1000 進にした。** 収めにいく先（ストアの出品規定、CDN の制限、
        メールの添付上限）は十進で書かれていることが多く、「500KB まで」に対して
        `500kb` が 512,000 バイトを許すと**解釈の誤りが上限を破る向きへずれる**。
        1024 系は `kib` / `mib` で明示的に言える
  - ※ **`quality_used` はエンコーダが受け取った値である。** `image` の JPEG
        エンコーダは `u8` しか受けないので、`--quality 33.3` は 33 として効く。
        丸めを `encode_jpeg` の中に閉じていたときは、報告だけが 33.3 を名乗って
        「実際に使った品質」が嘘になっていた。丸める場所は
        `OutputFormat::effective_quality` の 1 つだけにした
  - ※ **同じ数を 2 通りに綴らない。** `f32` は serde が `33.3` と書くのに、
        `serde_json::Value` へ入れると `f64` へ広がって `33.29999923706055` になる。
        結果の `quality_used` と警告の `data.quality_used` は突き合わせられる値なので、
        `report::quality_number` を両方が通る 1 つの関門にした
  - ※ **テストの上限を「基準の半分」で決め打ちにしたら、達成の検査が未達の道を
        通ったまま緑になった。** 200x200 の JPEG には ICC の APP2 が 534 バイト固定で
        乗るので、品質 25 まで落としても半分には届かない。**梯子の下限で実際に
        何バイトになるかを測ってから上限を決める**形に直した（`reachable_budget`）

- [x] Phase 20: 多派生出力 ＋ マニフェスト（`SCHEMA_VERSION` を 2 へ）
  - [x] `--derive 'width=1600,format=jpeg,quality=82,max_bytes=500k,role=hero'` を繰り返し
        指定できる。糖衣として `--sizes 400,800,1600` × `--formats avif,jpeg` の直積
        （**size が外・format が内**）。2 つは clap の `conflicts_with` で排他にした
        ——組み立て方が混ざると「どちらが勝つか」という覚える規則が増える
  - [x] 命名は `--naming '{stem}_{index}_{width}.{ext}'`。置換子は
        `{stem}` / `{index}` / `{width}` / `{height}` / `{ext}` / `{role}` の 6 つで、
        既定は `{stem}_{width}.{ext}`。**明示したら派生が 1 本でも効かせる**
        （予測可能性を優先した。数で効いたり効かなかったりするテンプレートは読めない）
  - [x] **書き始める前に全派生のパスを決め、3 つを検査する。** 派生どうしの衝突
        （`OUTPUT_NAME_COLLISION`）、付随出力との衝突（`SIDE_OUTPUT_CONFLICT`、
        `--manifest` も対象に入れた）、上書きの可否（`OUTPUT_EXISTS` /
        `DRY_RUN_OUTPUT_EXISTS`）。どれで落ちてもファイルは 1 つも書かれない
        （`a_name_collision_is_caught_before_anything_is_written` が
        出力ディレクトリの中身が 0 件のままであることまで見る）
  - [x] `--manifest path.json` は tmp + rename で書く。`{ schema_version,
        kiri_version, items: [{ input, outputs }] }` で、**時刻も所要時間も入れない**
        （成果物と並べて版管理できる）。cutout などは 1 要素、batch は実行全体で
        1 ファイル・成功した項目ごとに 1 要素
  - [x] `BatchReport` に `warnings` を足し、失敗した項目があるのに目録を書いたら
        `MANIFEST_PARTIAL`（`data.failed` に件数）。**1 入力の中の派生は部分失敗を
        許さない**——最初の失敗でその実行を止める（既存 `render` の挙動のまま）
  - [x] `batch.rs` の 3 箇所（`ItemSettings` / `pick!` / `SETTING_KEYS`）へ
        `derive` / `sizes` / `formats` / `naming` を同時に足した。値は数値でも
        文字列でも読め、CLI と同じ `DeriveSpec::set` を通る
  - ※ **`SCHEMA_VERSION` を 2 へ上げた唯一の根拠は `outputs[]` である。**
        型は `Vec<OutputReport>` のままなので、`outputs[0]` を読むコードは
        コンパイルも実行も通る——**通ったうえで 2 本目以降を黙って捨てる**。
        キーの追加（`outputs[].role`）と警告の `data.output` は単独では上げる理由に
        ならないが、上げる回を 1 回だけにするという決め（§7.2）に従ってここへ寄せた
  - ※ **`DRY_RUN_OUTPUT_EXISTS` の `data.path` は `output` へ改名した。** 派生に
        紐づく 6 つの警告（`ALPHA_FLATTENED` / `QUALITY_REDUCED` /
        `MAX_BYTES_UNREACHABLE` / `ICC_NOT_EMBEDDED` / `UPSCALED` /
        `DRY_RUN_OUTPUT_EXISTS`）が揃って `data.output` で「どの出力の話か」を
        名乗る規約にしたので、同じ意味のキーを 2 つ並べるより改名のほうが害が小さい。
        版を上げる回でなければやらない変更である
  - ※ **`Derivation` は `ResizePlan` ではなく `ResizeSpec` を持つ。** `plan` は
        寸法と指定だけの純関数なので、パスを決めた側と `render` が別々に呼んでも
        同じ答えになる。計画を持ち回ると「どの寸法で名前を付けたか」と「どの寸法で
        書いたか」が 2 つの値になり、食い違っても型は何も言わない
  - ※ **`--output` をディレクトリとしては読まない**（§7.2 の ※ のとおり）。
        `--output` は書き出し先そのもの（派生が 1 本のとき）か、`{stem}` と親
        ディレクトリを供給する名前の雛形（2 本以上のとき）になる
  - ※ **`OutputFormat::extension()` を足した。** `{ext}` は JPEG で `jpg` を返し、
        `outputs[].format` は従来どおり `"jpeg"` のままである。今日の
        `--output out.jpg` も同じ不揃いなので、**新しい不揃いを作ってはいない**
        （`the_extension_round_trips_through_from_path` が往復を固定する）
  - ※ **`--derive` の `fit` は `contain` / `cover` だけを受ける。** `exact` は
        縦横比を無視して枠へ変形するので、商品画像では事故でしかない。明示の入口は
        `kiri resize --fit exact` に残してある
  - ※ **`OUTPUT_NAME_COLLISION` は `--force` でも許さない。** 上書きの可否は
        「利用者の既存のファイルを壊してよいか」の話で、こちらは 1 回の実行が
        自分の成果物を自分で潰す指定である。通せば結果 JSON は 2 本とも書いたと
        報告し、実際には後の 1 本しか残らない
  - ※ **マニフェストは batch の spec のキーにしていない。** 実行全体で 1 つの
        ファイルなので、数百点が同じパスへ順に書けば最後の 1 件だけが残る目録に
        なる。入口は `kiri batch --manifest` だけにした
  - ※ **ピークの計測は JPEG で行う。** PNG の符号化はアロケータが 1 回あたり
        十数 MB を抱え込み、1400x1400 を 5 本書くと RSS が 48MB から 95MB へ伸びる。
        **これは派生の実装とは無関係**で、同じ寸法の 5 本でも `kiri batch` で同じ
        画像を 5 件並べても同じように伸びる（`image` の PNG エンコーダの
        確保・解放がそのまま常駐に残る）。同条件の JPEG では 5 本と 1 本の差が
        1MB を切る
  - ※ **レビューで出た 11 件を直した。** どれも実機で再現したもので、設計に
        触れたのは次の 5 つである（残りは一時ファイルの後始末・文書の綴り・
        重複キーの扱い）:
    - **寸法に依存しない命名の検査を `output::plan_naming` へ前倒しした。**
          テンプレートの解析も `{role}` の検査も最終画像の寸法を 1 つも見ないのに、
          `resolve` の中でやると `cutout --optimize` を回し切ってから綴り違いに
          気づくことになる。解いた `Naming` は `OutputPlan`（形式と命名の 2 つ）に
          まとめて `write_images` / `finish` へ渡す——2 度解いて片方だけが通る
          状態を作らないため。`{width}` に依存する衝突の検査は `resolve` に残る。
          併せて cutout の `--debug-mask` を `write_images` の後ろへ移した
          （先に書くと「どれで落ちてもファイルは 1 つも書かれない」が破れる）
    - **`naming::beside` を `Result` にし、綴った名前がファイル名 1 つで
          あることを検査する。** `Path::join` は引数が絶対パスなら基底を捨てるので、
          `{role}` や `--naming` の 1 語で `--output` の親の外へ書けていた。
          batch の `output` は `--base-dir` の下へ寄せる規約なのに、spec の
          `naming` / `derive[].role` はその関門を通らない——**spec は他人が
          生成しうるデータファイル**なので、ここは実装で閉じるしかない
    - **衝突の検査を `to_lowercase` で畳んだキーでも見る。** macOS の既定
          （APFS の case-insensitive）では `x_Hero.jpg` と `x_hero.jpg` が
          同じ 1 ファイルになり、通すと **JSON は 2 本書いたと報告し、ディスクには
          1 本しか無い**。機械可読なレポートが嘘をつくので MEDIUM ではなく HIGH
          として直した
    - **`--naming` の走査を 1 文字ずつのステートマシンにした。** 「`{` を探して
          その後ろの `}` を探す」形では最後の置換子より前の `}` を一度も見ず、
          `stem}_{width}.{ext}` が `stem}_200.jpg` として書き出されていた
          ——実装コメント自身が挙げていた反例である
    - **`UPSCALED` は契約の文面のほうを直した**（実装は据え置き）。resize 段の
          拡大は**最終画像そのもの**に起きたことで、派生ごとの事象ではない。
          無い帰属をでっち上げて `data.output` を付けるほうが嘘になるので、
          README と `kiri schema` に出どころが 2 つあることを書き分け、
          `data.output` の有無がそのまま区別になると明示した。
          `DRY_RUN_OUTPUT_EXISTS` が `--manifest` のパスを名乗る枝も同じ理由で
          文面の側を正した
  - ※ **batch の `--manifest` の上書き検査を `run()` の先頭へ移した。**
        `write_manifest` の中で問うていたので、検査が数百点を書き切った後になり、
        `OUTPUT_EXISTS` が `Err` として返って `BatchReport` が丸ごと捨てられていた
        ——利用者に残るのはエラー 1 行だけで、**何枚書かれたのかも、どれが成功し
        どれが失敗したかも返らない**。convert / resize / rotate / cutout の 4 つは
        最初から `run()` の先頭で問うており、batch だけが例外になっていた
  - ※ テスト名: `the_batch_manifest_overwrite_check_runs_before_any_item_is_written` /
        `a_cutout_that_fails_the_naming_check_writes_no_debug_mask` /
        `a_name_that_leaves_the_output_directory_is_refused` /
        `two_derivations_differing_only_in_case_are_a_collision` /
        `an_unmatched_closing_brace_is_refused_anywhere_in_the_template` /
        `a_derivation_key_written_twice_is_refused` /
        `a_failed_manifest_write_leaves_no_temporary_file` /
        `every_derive_key_refuses_a_second_value`。`UPSCALED` の書き分けは
        既存の `every_derivation_bound_warning_names_its_output` へ
        `kiri resize --allow-upscale` の枝を足して固定した

- [x] Phase 21: `--fail-on` / exit 5 の新設（`SCHEMA_VERSION` は 2 のまま据え置き）
  - [x] `ErrorKind` に 5 つ目 `Compliance`（exit 5）を足した。`meaning` は
        「規格未達（成果物はある。人が見る対象で、結果 JSON は通常どおり返る）」
        ——**exit 4 を流用しない。** 4 は「やり直せば直る失敗」で成果物が無く、
        5 は処理が通って成果物も書かれた結果が規格に達しなかったことを言う。
        この区別が無いと、エージェントは書けているファイルを捨てるか、落ちた
        切り抜きをそのまま納品するかのどちらかになる
  - [x] `--fail-on 'default,halo_ratio>0.05'`。カンマ区切りで `default` /
        `<指標><演算子><値>` / 真偽の指標（裸の `touches_edge`）の 3 種類を
        混ぜられる。演算子は `>` `<` `>=` `<=` で、**触れたら不合格**である。
        指標の名前は**結果 JSON の `mask.*` のキーそのまま**（7 つ）で、
        **短縮形は作らない**——JSON から読んだ語をそのまま書けることのほうが、
        打鍵の短さより重い
  - [x] `--fail-on default` は **`FATAL_CODES` ∪ `QUALITY_CODES` のいずれかが
        出たら不合格**。これは `optimize.rs` の `Trial::clean()` が「きれい」と
        呼ぶ**較正済みの集合に、測れなかった指標（`null`）を足したもの**である
        （`clean()` は測れなかった指標を「きれい」と数えるが、`default` は
        不合格にする。違うのはそこだけ）。別の集合を新しく定義すると、
        `--optimize` が「きれいな候補が見つかった」と言った結果を
        `--fail-on default` が落としうる。**同じ問いに 2 つの答えを持たせない**
  - [x] 結果 JSON の `compliance` ブロック（`fail_on` / `passed` / `code` /
        `checks[]`）。**`--fail-on` があるときだけ出す**（`optimize` / `segment` と
        同じ規約でキーごと現れない）ので、**`SCHEMA_VERSION` は 2 のまま据え置いた**
        ——加算だけの変更で、既存の読み方が誤読になる箇所が無い
  - [x] batch は `results[].status` に 3 つ目の `"rejected"` を足し、
        `BatchReport` に `rejected` を足した。実行全体の終了コードは
        「1 件でも不合格なら 5。ただし `failed > 0` の 4 が優先」。
        spec の `fail_on` は `batch.rs` の 3 箇所（`ItemSettings` / `pick!` /
        `SETTING_KEYS`）へ同時に足した
  - ※ **`--optimize` の順位関数には手を触れていない。** 探索の順位は 4 値の
        辞書式で較正済みで、合わせると較正をやり直すことになる（§7.2 の ※）。
        参照したのは `FATAL_CODES` / `QUALITY_CODES` という**集合の定義だけ**である
  - ※ **exit 5 は `Err` 経路を通さない。** 処理は成功していて成果物も存在する
        ので、結果 JSON を `ErrorReport` へ差し替えない。`outputs[]` も `mask` も
        捨てると、利用者は「何が不合格だったか」も「何が書かれたか」も追えなく
        なる——batch が数百枚書いた後に `BatchReport` を捨てていた Phase 20 の
        失敗と同じ形である。batch が `failed > 0` で 4 を返しつつ `BatchReport` を
        出している既存の形をそのまま踏襲した
  - ※ **測れなかった指標（`null`）は不合格にした。** `separability` の `null` は
        「前景が無い」、`halo_ratio` の `null` は「測る境界が無い」で、どちらも
        黙って合格を出してよい状態ではない。ただし「しきい値を超えた」とは別の
        事実なので `status` で `"unmeasurable"` と名乗って `"fail"` と区別する
        ——次の一手が違う（前者は素材か指示、後者はしきい値か設定を見る）
  - ※ **`checks[]` は指標ごとではなく code ごとに 1 行出す。** `default` の
        `foreground_ratio` は `FOREGROUND_TOO_SMALL` と `FOREGROUND_TOO_LARGE` の
        2 行、`touches_edge` は `SUBJECT_TOUCHES_EDGE` と `BBOX_RECOMMENDED` の
        2 行になる。**そうすると `checks[].code` の集合がそのまま
        `FATAL_CODES` ∪ `QUALITY_CODES` と突き合わせられる**ので、どちらかへ
        code を足したときに落ちる検査が 1 本で書ける。並びは `Metric::ALL` の順で
        決定的である
  - ※ **`code` は向きによって変わる。** `halo_ratio>0.05` は「縁が残っている」を
        厳しく見た指定なので `HALO_REMAINS` を名乗れるが、`halo_ratio<0.05` に
        対応する警告は無い。**無い帰属をでっち上げない**——当てはまらないところは
        `null` を出す（Phase 20 で `UPSCALED` の `data.output` について出した
        結論と同じ）
  - ※ **固定のしきい値を持たない 3 つは `threshold` を `null` にした。**
        `NOT_SEPARABLE` は画像ごとの `background.residual.p50` と比べ、外周接触の
        2 つは複合条件である。それらしい数字を載せた瞬間に、それが契約として
        読まれる
  - ※ **`QUALITY_GATE_FAILED` は `errors[]` ではなく `compliance.code` に出る。**
        カタログに置くのは exit 5 の語彙を `kiri schema` が配るためで、
        `ErrorBody` としては返さない。名乗る場所を結果の中に用意したのは、
        exit 5 だけが「エラー本体を持たない終了コード」になるのを避けるため
        ——エージェントは他の失敗とまったく同じ形（code を引いて分岐する）で
        扱える
  - ※ **同じ指標への二重指定は断る。** `halo_ratio>0.1,halo_ratio>0.2` は
        どちらが勝つかという覚える規則を増やすだけで、意図した条件は 1 つに
        書ける（Phase 20 の `a_derivation_key_written_twice_is_refused` と同じ扱い）
  - ※ **値域だけでなく向きまで検査する。** `halo_ratio>2` は「2 を超えたら落とす」と
        書いたつもりの指定だが、割合は 1 を超えないので**永久に発火しない門**に
        なる。書いた本人は合格が出続けるのを見て「通っている」と読む。
        **同じことが値域の端でも起こる**——`halo_ratio>1.0` は値域の中なのに
        永久に落ちず、比率を % と取り違えた `foreground_ratio>1.0` は全件を黙って
        通す。レビューで見つかったので、`>` は上限以上を、`<` は下限以下を断る
        形にした。端ちょうどで発火しうる `>=1.0` / `<=0.0` は通す。逆向きの
        「必ず発火する門」（`foreground_ratio>=0.0`）も通す——1 枚目の exit 5 で
        気づくので黙って害を成さず、「どの画像でも落ちること」を確かめる使い方が
        現にある
  - ※ **真偽の指標は裸のトークンにした。** 当初は `touches_edge=true` /
        `=false` と書けたが、実装は「書いた値と一致したら不合格」なので
        `=false` は「接していなければ落とす」——外周に接していない良い画像が
        exit 5 で落ち、しかも `default` の外周接触の検査を置き換えて消していた。
        **方向を選べる形にした結果、唯一意味のある方向がどちらか読めなくなった**
        ので、`=` を廃して裸の `touches_edge`（外周に接していたら不合格）だけを
        受ける。`>` が「触れたら不合格」と読めるのと同じ素直さを真偽にも与える。
        `operator` と `threshold` は `null` になり、`code` には
        `SUBJECT_TOUCHES_EDGE` を当てる（`BBOX_RECOMMENDED` は「bbox で解ける」と
        いう別の読み方なので、明示指定には素直なほうを当てる）。まだ配っていない
        契約なので、簡単に直せるのは今だけだった
  - ※ **書式の検査は入力を読む前に終わる。** CLI では clap の `value_parser` が、
        spec では `to_cutout_args` が `cutout::run` の前に通す。切り抜きを全部
        終えてから綴り違いに気づく形にしない（Phase 20 のレビューで
        `output::plan_naming` を前倒ししたのと同じ位置）
  - ※ **測れなかった実測値は `null` だけではない。** NaN や ∞ は JSON の数値に
        できないので `actual` が `null` になるのに、`status` は `pass` に落ちて
        いた（レビューで指摘）。`explicit_check` で `unmeasurable` に畳む——
        指定側の NaN は値域の検査が既に断っているので、これで両側が揃う
  - ※ **`QUALITY_GATE_FAILED` を `Error::new` で作れないようにした。** 「`ErrorBody`
        としては返らない」はカタログのコメントが宣言するだけの約束で、構造は
        何も禁じていなかった（レビューで指摘）。`Error::new` の
        `debug_assert!(code.kind() != ErrorKind::Compliance)` で関門にする——
        作れてしまうと結果 JSON が `ErrorReport` へ差し替わり、成果物があるのに
        `outputs[]` も `mask` も消える。**絶対条件が壊れる形がちょうどこれである**
  - ※ **人間向けの行は見た件数を言う。** 「8 件中 2 件不合格」という数をヘッダに
        出す。pass の行を飛ばすだけだと「見た上で通った」が人間向け出力から
        まるごと消えるためで、doc のほうを実装に合わせなかった。端末の行に
        markdown の `**` を漏らしていたのも直した（`src/main.rs` の他の
        `println!` に `**` を含むものは 1 つも無い）
  - ※ **`compliance.code` と `compliance.fail_on` を `kiri schema` の `fields[]` へ
        足した。** `code` の存在理由は「`errors[]` が配る語彙と結果を突き合わせ
        られる場所がここ以外に無い」ことなのに、**その場所自体が schema から
        引けなかった**（レビューで指摘）。`fail_on` は決まった選択肢を持たない
        文字列なので `unit` に `text` を足してある（`enum` と分けるのは、受け手が
        値を照合してよいかがそこで変わるため）
  - ※ **`FAIL_ON_METRICS` は `Metric::ALL` から const fn で組む。** 手で書き写した
        一覧を増やさないためで、`--fail-on` の長いヘルプも未知の指標を断るときの
        一覧もここから出る。schema の `mask.*` の `path` と過不足なく一致することは
        `the_fail_on_metrics_are_exactly_the_published_mask_fields` が固定する
  - ※ テスト名: `every_metric_maps_a_threshold_to_an_exit_code`（受け入れ基準 (a)。
        7 指標 × 触れる / 触れない を**実測値から組んだしきい値**で回すので、
        較正で指標の出方が動いても表の意味が保たれる）/
        `an_unmeasurable_metric_is_rejected_under_its_own_name`（(a) の 3 通り目）/
        `no_fail_on_means_no_compliance_block_and_no_new_exit_code`（(b)。
        `elapsed_ms` と `compliance` を落とした結果 JSON が 1 文字も変わらない）/
        `a_rejected_cutout_still_returns_the_whole_result_json` /
        `a_batch_rejection_exits_five_unless_something_actually_failed`（(c)）/
        `the_default_gate_is_exactly_the_fatal_and_quality_codes` /
        `the_readme_spells_the_real_default_gate`（README の囲みが実装の集合と
        一致する。既存の警告表の検査は**表**しか見ないので、`default` の内訳は
        素通りしていた）/
        `the_fail_on_metrics_are_exactly_the_published_mask_fields` /
        `an_explicit_threshold_beats_the_default_for_the_same_metric` /
        `the_compliance_checks_are_in_a_deterministic_order` /
        `a_malformed_fail_on_on_the_command_line_is_refused_by_the_parser` /
        `a_malformed_fail_on_in_a_spec_is_refused_with_a_code` /
        `a_spec_inherits_fail_on_from_the_defaults`。
        レビューで足した分:
        `a_gate_that_can_never_fire_is_refused_before_the_image_is_read`
        （値域の端。`a_gate_that_can_never_fire_is_refused_even_inside_the_range` と
        `a_boundary_gate_that_can_fire_is_accepted` が単体側で同じことを見る）/
        `an_explicit_edge_token_keeps_the_default_verdict_for_a_clean_image`
        （**良い画像が落ちない**ことと、置き換えるのが外周接触の 2 本だけである
        ことを同時に固定する。旧 `=false` はその両方を破っていた）/
        `a_bare_flag_token_fails_only_when_the_fact_is_true` /
        `a_bare_flag_token_names_the_plain_warning_code` /
        `the_old_equals_spelling_is_refused_with_the_bare_token_in_the_message` /
        `a_non_finite_measurement_is_unmeasurable_not_a_pass` /
        `a_compliance_code_cannot_be_built_as_an_error_body` /
        `the_human_readable_compliance_line_counts_what_it_looked_at` /
        `the_readme_exit_code_table_quotes_the_published_meanings`（`meaning()` の
        doc が名乗っていた契約をテストで本物にする。**その契約を足した当の
        コミットで 5 行目が破られていた**）。
        `the_published_prose_has_no_stray_spaces` は**隙間を 1 文字と決め打ち
        していた**ので、行継続の書き忘れ（空白 17 個）を 1 つも拾えなかった。
        空白の連なりを 1 つの隙間として見る形に直し、日本語の直後に 2 つ以上
        続くものは後ろが英数字でも咎める（列を揃える 2 連スペースは英数字の
        後ろにしか現れない）。
        既存の契約テストは `schema_returns_the_whole_contract`（出口に 5 を足した）と
        `every_published_field_exists_in_the_result`（`--fail-on` を渡す実行を
        1 つ足した）、`every_code_named_in_the_docs_exists`（計画書だけが先に
        名指ししてよい code の表から 2 つ消した）が追随している
