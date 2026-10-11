# 7. EC 特化のロードマップ（Phase 17〜26）

節番号は分割前のまま変えていない。`計画 §8.6` のような参照が他の節・src / tests から
150 箇所以上張られていて、**番号が識別子として働いている**からである。どの節がどの
ファイルにあるかは [実装計画](../implementation-plan.md) の索引で引く。

§6 が「いまの kiri を完成させる」残件なのに対し、ここは**EC の運用へ寄せるために
足すもの**を並べる。いまの kiri は「1 枚をきれいに抜く」方向に尖っていて、
「カタログ数百点を規格へ揃えて出す」側がほとんど無い。足りないのはそちらである。

## 7.1 順序を決めている 3 つの事実

順序は好みではなく、次の 3 つから決まっている。

1. **出力の合流点はすでに 1 箇所ある。** `commands/output.rs` の `write_image()` と
   `image_io/save.rs` の `encode()` を、convert / resize / cutout / batch の**全経路が
   通る**。多派生・`--max-bytes`・マニフェスト・ICC はすべてこの 2 関数の中か直上に
   着地するので、**手術は 1 回で済ませられるし、済ませるべき**である。バラバラに
   入れると同じ関数を 4 回開くことになる。

2. **バイト列を動かす変更は、バイト列に依存する機能より前に置く。** ICC の埋め込みは
   ファイルサイズを数百バイト〜数 KB 動かす。`--max-bytes` の探索結果も、多派生の
   全出力も、既存の決定性ゴールデンも、後から ICC を入れれば全部取り直しになる。

3. **pure Rust の縛りが 2 箇所で効く。** lossy WebP には libwebp（C）が要り、
   `image` 0.25 の WebP エンコーダは**可逆のみ**である（`codecs/webp/encoder.rs` が
   「lossy が要るなら libwebp を使え」と明記している）。AVIF のデコーダも実質
   dav1d（C）しかない。つまり **kiri は自分が吐いた AVIF の画素を読めない**。
   これが Phase 20 の形式選びと Phase 22 の `kiri lint` の設計を規定する。
   `Cargo.toml` が `tract-onnx` について立てた「`*-sys` が 1 つも現れない」という
   基準は、ここでも同じ重さで効く。

## 7.2 各フェーズ

### Phase 17: リリース用 CI（= §6 の P0）

**済（§5 の Phase 17）。ただし Phase 18 の後に入れた。** 安全網が手元にしか無い
状態でエンコーダに触るのは順序として逆である、と下に書いておきながらそうなった。
実害は出ていない——Phase 18 は旧バイナリとの md5 比較 33 本で自分の安全網を別に
立てていた——が、**次からは先に置く**。以降のフェーズはこの前提で書いてある。

**なぜここか。** 以降の全フェーズが出力バイト列とゴールデンを動かす。安全網が
手元にしか無い状態でエンコーダに触るのは順序として逆である。

中身は [4. CI](../implementation-plan.md#4-ci) のとおり。feature `segment` の有無で 2 系統要り、**MSRV の
検査に `--features segment` を付けてはならない**。規模 S〜M、新規コードなし。
実際の形は `.github/workflows/ci.yml` の 3 ジョブ（fmt / test × 2 系統 / MSRV 1.85）で、
2 系統を別ジョブにしたのは同じ `target/` を共有させないためである。

### Phase 18: 出力パイプラインの一本化 ＋ sRGB ICC の埋め込み

**済（§5 の Phase 18）。**

**なぜここか。** Phase 19 / 20 が同じ `encode()` を奪い合う。先に「1 つの最終画像 →
N 個の派生」という内部型を通し、**N = 1 のときに 1 バイトも変わらないことを固定**
してから機能を載せる。ICC をここへ畳むのは 7.1 の 2 による。

- `image_io/derive.rs`（新）に `Derivation { path, format, quality, effort,
  background, flatten, icc }` と
  `render(&RgbaImage, &[Derivation], dry_run: bool) -> Result<Vec<Rendered>>`
  （`Rendered { report: OutputReport, warnings }`。最初の失敗で止まる）を置いた。
  `write_image()` は `OutputOpts` から `Derivation` を 1 個作るだけで、外から見た
  挙動は変わらない。**`role` / 寸法 / `max_bytes` は置いていない。** 読まれない
  フィールドを先に置くと「指定したのに効かない」が型として作れてしまうので、
  それを読むフェーズ（19 / 20）で足す。寸法は `ResizeSpec` を 1 つ足す形にする
- `SaveOptions` に `icc: IccPolicy`（`Embed` / `None`。既定は `Embed`）を足した。
  PNG / JPEG は `image` 0.25.6 以降の `ImageEncoder::set_icc_profile` に渡すだけで、
  チャンクもセグメントも自前では挿さない。PNG は IHDR 直後の **iCCP 1 個だけ**
  （png クレートが iCCP と sRGB チャンクを排他にしており、PNG 3 も併存を勧めない）、
  JPEG は **APP0（JFIF）の直後**の APP2（SOI の直後に置くと JFIF に反する）。
  AVIF は `IccPolicy` を見ず、ravif が AV1 シーケンスヘッダの CICP（1 / 13 / 6 /
  full）で sRGB を名乗る。**`colr` ボックスは出ない**（既定値と同じなので
  avif-serialize が省く）。ICC ボックスも入れない
- **プロファイルのバイト列は外から持ってこず自前で生成する。** `color/icc.rs` が
  既に行列 + TRC 型を解釈しているので、逆向きに最小の v2 プロファイル（516B）を
  組んだ（`color/srgb_profile.rs`）。`qcms` や `lcms2` を引くより依存も権利も軽い
- **出すかどうかは画素の素性で決める。** `LoadedImage.srgb_pixels` が偽のとき
  （`--no-color-convert` で変換しなかった）だけ `IccPolicy::None` にして
  `ICC_NOT_EMBEDDED` を出す。preview のコンタクトシートは成果物ではないので常に `None`
- `batch.rs` の 3 箇所（`ItemSettings` のフィールド / `merged_over` の `pick!` /
  `SETTING_KEYS`）の一致は**テスト 1 本で守る**。serde の derive が渡すフィールド名を
  横取りして `SETTING_KEYS` と比べる。`pick!` は構造体リテラルなので読み忘れは
  コンパイラが止める。1 つの表へ畳むマクロは差分に見合わないので採らなかった

規模 L。衝突リスク（着手前の見立てと実態）:

- 中 → 無し — 「`image` 0.25 は ICC の埋め込み API を公開していない」という見立ては
  **外れていた**。0.25.6 で `set_icc_profile` が入っている。`Cargo.toml` の下限を
  `0.25.6` に上げただけで、チャンクやセグメントの自前の挿入は書いていない
- 中 → 小 — 「全ゴールデンが一度動く」も外れていた。保存されたバイト列のゴールデンは
  存在しなかった。取り直したのは入力とのバイト一致を見ていた
  `rotate_by_a_full_turn_returns_the_original_bytes` の 1 本だけ（iCCP を抜いて
  比べる）。既存の決定性テストは無修正で通る。**このフェーズ以外では出力バイト列を
  動かさない**ことは引き続き守る
- **小** — 「PNG/JPEG は ICC、AVIF は nclx」という非対称が契約に出る。
  `outputs[].icc`（`"embedded"` / `"nclx"` / `"none"`）で明示した

### Phase 19: `--max-bytes`

**済（§5 の Phase 19）。**

**なぜここか。** `render()` の中で「エンコード → 大きすぎたら品質を落として再試行」を
閉じる。Phase 20 の前に置くのは、**多派生になってから実装すると、派生ごとの探索
コストと報告形式を後付けすることになる**ため。1 派生で規約を確定させてから増やす。

- 品質の探索は**固定の梯子**にし、時刻やタイムアウトに依存させない。決定性は
  kiri の中核の約束である
- 動かすのは `quality` だけ。**AVIF の `effort` は動かさない**（時間が桁で変わる）
- `OutputReport` に `quality_used` / `attempts` を足す。`--dry-run` でも実測が出る
- ※ **JPEG は quality を下げてもサイズが単調に減らない区間がある。** テストは
  単調性ではなく「達成したら必ず `max_bytes` 以下」「達成できなければ必ず警告」で
  固定する

規模 M。衝突リスク **中** — 24.5MP の AVIF は 1 回のエンコードが数秒かかる。探索
回数の上限と、超えたら警告で降参する規約が要る。~~`--optimize` と併用すると総当たり
× 探索で時間が積になるので、見積もりをヘルプで言う。~~ **積ではなく和だった**
（§5 の Phase 19 の `※`）。

### Phase 20: 多派生出力 ＋ マニフェスト

**済（§5 の Phase 20）。**

**なぜここか。** この 2 つは同じデータ構造の表と裏（何を書いたかを列挙するのが
マニフェスト）で、別フェーズにすると `OutputReport` を 2 回設計し直すことになる。
**`SCHEMA_VERSION` を 2 へ上げるのはこの 1 回だけ**にする。

- `--derive 'width=1600,format=jpeg,quality=82,max_bytes=500k'` を繰り返し指定できる
  ようにし、糖衣として `--sizes 400,800,1600` × `--formats avif,jpeg` の直積を置く
- ※ **`--output` をディレクトリと解釈させる案は採らない。** `OUTPUT_EXISTS` の
  規約が壊れる
- 命名は `--naming '{stem}_{index}_{width}.{ext}'`。**書き始める前に全派生のパスを
  作って衝突を検査する**（書いてから気づくと半端な成果物が残る）
- 切り抜きは 1 回。派生は最終画像からのリサイズのみで、**逐次処理して都度解放する**
  （24.5MP × N を同時に持たない。並列は batch の項目単位に任せる）
- マニフェストは `--manifest path.json`。cutout では 1 入力ぶん、batch では実行全体で
  1 つ。tmp + rename で書く
- ※ **WebP は足さない。** 7.1 の 3 のとおり pure Rust では可逆しか書けず、写真素材の
  可逆 WebP は AVIF / JPEG にサイズで負ける。必要になったら独立した調査項目として切る
  - **後日、lossless に限って足した。** サイズの利点が小さいことを承知のうえで、
    WebP を指定してくる入稿先のために入力（静止画の lossy / lossless）と lossless の
    出力を持つ。依存は `image` の `webp` feature（pure Rust の `image-webp`、
    `-sys` 無し）だけで、**lossy は libwebp（C）が要るので引き続き入れない**。
    品質を持たない形式として PNG と同じ扱い（`--quality` は効かず、`--max-bytes` は
    段を降りない）。アニメーション WebP は 1 枚目を黙って使わず `UNSUPPORTED_FORMAT`
    で断る

規模 L。衝突リスク:

- **高（契約）** — `warnings` が「実行に 1 回ずつ」から「派生ごとに複数」へ変わる。
  `ALPHA_FLATTENED` や `DRY_RUN_OUTPUT_EXISTS` が 1 実行で複数回出る。全警告の
  `data` に「どの派生か」を示す `output` キーを足す規約を、ここで一律に入れる
- **小** — `--preview` / `--debug-mask` との衝突検査（`SIDE_OUTPUT_CONFLICT`）の
  対象が増える

### Phase 21: `--fail-on`

**済（§5 の Phase 21）。**

**なぜここか。** 「不合格を exit code で返す」という契約の変更を、**最も面積の
小さいところで 1 回だけ**行う。使う指標（`mask` の 7 項目）もしきい値
（`cutout/diagnostics.rs`）も**もう全部ある**ので、新しい計算は要らない。
Phase 22 が同じ exit code の語彙を使うので、その前に置く。

- `ErrorKind` は現在 `General` / `Argument` / `Input` / `Processing` の 4 種で、
  ここに 5 つ目 `Compliance`（exit 5）を足す。**exit 4「処理失敗」を流用しない**
  ——処理は成功していて、成果物が規格に達しなかっただけである。この区別が無いと、
  エージェントは「やり直せば直る失敗」と「人が見るべき結果」を分けられない
- `--fail-on halo>0.10,contour_roughness>0.16` 形式と、`kiri schema` の `warns`
  しきい値をそのまま使う `--fail-on default` の 2 つ
- batch は項目ごとの不合格を `status: "rejected"` として `succeeded` / `failed` とは
  別に数え、`BatchReport` に `rejected` を足す。実行全体の exit code は「1 件でも
  不合格なら 5」、ただし `failed > 0` の 4 が優先
- ※ **`--optimize` の順位関数は `--fail-on` のしきい値に合わせない。** 探索の順位は
  すでに 4 値の辞書式で較正されている。合わせると較正をやり直すことになる

規模 S〜M。衝突リスク **中（契約）** — 「0 以外は失敗」と読んでいる呼び出し側に
とって 5 は新しい意味である。`exit_codes[].meaning` に「成果物はある。人が見る
対象」と書く。

### Phase 22: `--profile` ＋ `kiri lint`

**済（§5 の Phase 22）。**

**なぜここか。** profile の実体は「1600px の JPEG を白背景で、占有率 85%、1MB 以内」
のような**複合指定の別名**であり、Phase 19 / 20 / 21 が揃って初めて片肺でなくなる。
先に置くと検査だけの機能になり、しかも**中間状態の契約が外へ出てしまう**
（一度配った契約は引っ込められない）。

- 表は `src/profile.rs`（新）に持つ。※ **外部 JSON で差し替える口は作らない**
  ——「契約を自分で配る」設計と決定性に反する。代わりに各プリセットへ
  `revision`（例 `"2026-09"`）を持たせ、`kiri schema` の新ブロック `profiles[]` と
  結果 JSON の `settings.profile` に出す
- 優先順位は **明示指定 > profile > 既定** の 1 本だけ。profile の値を明示指定が
  上書きしたら `PROFILE_OVERRIDDEN` で必ず報せる（「指定したのに効かない」を
  作らない、という `cli.rs` 全体の姿勢と同じ）
- `--fill-ratio` の既定 0.85 は doc コメントが「EC プラットフォームで広く求められる
  占有率」と規格を主張しており、**profile と二重定義になる**。profile 側を正とし、
  既定値のコメントから規格の主張を外す
- `kiri lint <file> --profile amazon` の**画素の検査は JPEG / PNG のみ**。AVIF は
  コンテナ（寸法・alpha、色の名乗りは `colr` があればそれを、無ければ AV1 シーケンス
  ヘッダの CICP）までとし、検査できなかった項目は
  `checks[].status: "skipped"` ＋ `PROFILE_UNCHECKABLE` で明示する。**黙って合格に
  しない**

規模 L。衝突リスク:

- **高** — 上記の AVIF デコード不可。曖昧にすると「lint が通ったのに落とされた」が
  起きる。`--help` と `kiri schema` の両方で明示する
- **中** — モール規格は変わる。`revision` を出す以上、古い kiri が古い規格で合格を
  出すことは避けられない。結果に `profile.revision` を必ず載せ、呼び出し側が鮮度を
  判断できるようにする

### Phase 23: `--rotate auto` ＋ セット内のスケール・余白の統一

**済（§5 の Phase 23）。**

**なぜここか。** 両方とも `subject`（`level_rotation` / `normalized_bbox`）を消費する。
前者は「測った角度を畳むだけ」の S で、後者の 2 パスは「pass 1 で subject を測って
持ち回る」構造そのものなので、**同じ機構を 1 度作って 2 つに使う**。

- `--rotate` を `f64 | "auto"` に広げる。`confidence` が `high` のときだけ適用し、
  そうでなければ 0 度のまま `ROTATE_AUTO_SKIPPED` で報せる。分解能 0.5 度の注意を
  ヘルプへ引く
- ※ **「`level_rotation` を適用する」という前提そのものが、同じ値の doc コメントに
  反していた。** `src/cutout/subject.rs` の `level_rotation` は「**自動では適用しない。**
  傾きを直すかどうかは構図の判断」と自分で書いており、**この計画はそれを読まずに
  「測った角度を畳むだけの S」と見積もっていた。** 実測でも doc のほうが正しく、
  水平に置いたフライパンへ `-21.7 / +31.9 / -43.3 度`が `confidence: high` で返る
  ——`confidence` は「主体をどれだけ確かに切り出せたか」しか言っておらず、
  **切り出せた形が向きを持つかどうかは 1 つも見ていない。** 実装では形の門
  （`level_fill_ratio` < `TILT_SHAPE_MIN_FILL` なら `not_rectangular` で見送る）を
  足して埋めた。**計画に無い条件を 1 つ増やしたのはここだけである**（§5 の Phase 23）
- batch に `set: { align: "height" | "bbox", fill_ratio: ... }` を足す。**pass 1 では
  切り抜きを回さない**——`--optimize` の探索段が使う縮小で subject だけを測り、
  全点の代表寸法（中央値）を決める。pass 2 は今までどおり `par_iter` で回す
- 揃えた結果 1 点だけが極端に外れるときは `SET_SCALE_CLAMPED` で報せる

規模 S（`--rotate auto`）/ M（セット統一）。衝突リスク **中** — 2 パス化と `--jobs`。
`commands/batch.rs` は `par_iter().map().collect()` で**もともとストリーミングでは
ない**（全件を集めてから返す）ので構造的な衝突は無い。増えるのは pass 1 の
decode コストで、1000 点級でも「2 回デコードする」が素直。キャッシュは要らない。

### Phase 24: ホワイトバランス／露出の正規化

**済（§5 の Phase 24）。**

**なぜここか。** **画素を動かす唯一のフェーズ**で、マスク品質の 7 指標すべてに
影響する。Phase 21 の合否と Phase 23 の一貫性が先に入っていれば「良くなったか」を
数値で言える。逆順にすると悪化を目視でしか検出できない。

- 既存の照明場 B(x, y)（`cutout/background.rs`）を使い、背景が中性（白/グレー）
  だという前提のもとで白点と露出を推定する。`--white-balance auto|off` /
  `--exposure auto|off`、**既定は off**
- **off のとき 1 バイトも変わらないこと**をテストで固定する（`--shadow` /
  `--segment` と同じ規約）
- 背景が中性でないと判定したら適用せず `WHITE_BALANCE_SKIPPED`
- 適用量を結果 JSON（`color.white_point_shift` 等）に出す

規模 L（計算は M だが較正とベンチの往復が長い）。衝突リスク **高** — 正規化は
切り抜きの**前**に掛かるので、`tolerance` の意味（背景色との ΔE）が実質変わる。
既定 off を崩さないことと、`--optimize` と併用したとき探索の前段に置くことを設計で
固定する。§6 の P2 と同じ注意が効く——**着手前に必ずベンチで見積もる**。

- ※ **「マスク品質の 7 指標すべてに影響する」は効果の所在を読み違えていた。**
  一様な色かぶりは**背景の中央値との ΔE をほとんど動かさない**ので、マスクへの
  効果は原理的に小さい。実測でも指標は良くも悪くもなる（`halo_ratio` と
  `contour_roughness` は下がり、`separability` は悪化する）。この段の価値は
  Phase 23 の隣——**セット内で色と明るさが揃うこと**にあり、7 指標は「悪化させて
  いないか」を見る側の物差しである（§5 の Phase 24）
- ※ **費用の警戒（「着手前に必ずベンチで見積もる」）は空振りだった。** 24.5MP の
  実写に ICC 変換の全画素パス（行列 + TRC）を掛けても `info` の所要時間は 0.44 秒で
  変わらない。実際の画素パスは 51ms、費用の大半は見立て（281ms）のほうである。
  **rayon は要らない**
- ※ **「`tolerance` の意味が実質変わる」という衝突リスクは、既定 off が丸ごと
  引き受けた。** 渡さない実行では新しいコードを 1 行も通らないので、実写 2 枚 ×
  3 通り（フラグなし / `--bbox` / `--optimize`）で出力バイト列と結果 JSON が
  main と一致する。`--optimize` の前段に置く要件もそこで同時に満たされる
  （正規化は `load` 直後で、探索はその後の画素を見る）

### Phase 25: `--reflect`

**済（§5 の Phase 25）。**

**なぜここか。** 依存が無く、`transform/shadow.rs` の構造（アルファを複製 → 変形 →
ぼかし → 色を塗る → 下に敷く）をほぼそのまま流用できる、最も安全で価値の小さい
項目。**どこかで詰まったときに前へ繰り上げてよい唯一のフェーズ**でもある。

`--reflect on` / `--reflect-height`（px@1000）/ `--reflect-opacity` /
`--reflect-gap`。合成順は「下地 → 影 → 反射 → 商品」。`reflect.bounds` /
`reflect.clipped` を `ShadowReport` と同型で出す。規模 M、衝突リスク小。

- ※ **「`transform/shadow.rs` の構造をほぼそのまま流用できる」は半分外れていた。**
  流用できたのは `ShadowSpec` / `ShadowBounds` の形と `clipped` の規約、そして
  px@1000 の掛け戻しの置き場所で、**算術は共有できない**——影はアルファだけを使って
  色を塗るが、反射は画素（RGB も）を写す段で、ぼかしも変形も要らない。結果として
  `transform/reflect.rs` の算術は 181 行で `shadow.rs` の 397 行の半分以下になり、
  費用も 1 桁安い（24.5MP で 9.4ms 対 148ms）
- ※ **「衝突リスク小」は 1 箇所だけ外れた。** `shadow::synth` が「アルファを作る」と
  「下へ敷く」を 1 関数で閉じていたので、そのまま反射を後から敷くと (1) 反射が影の
  下に入り、(2) `synth` の後の混ざったアルファから鏡像を取って**影を写した反射**に
  なる、の 2 つが同時に起きた。`alpha` + `compose` へ割って解いた（**振る舞いは
  変えていない**——既存の影のテストは 1 本も修正していない）。設計は design.md 4.16

### Phase 26: 出力の既定を根拠から引き直す

**済（§5 の Phase 26）。計画には無かった段である。**

**なぜ足したか。** Phase 17〜25 は機能を足す並びで、**既定値そのものを疑う段が
1 つも無かった。** そのあいだに `--quality` の既定 75 は design.md 3.4 の宿題
（「実画像での再計測を実装フェーズで行うこと」）を未回収のまま 25 フェーズを通り、
profile の canvas 1600 は「1000〜5000 のどの値でも同じように立つ 3 つの理由」から
選ばれたまま固定されていた。**どちらも、機能を足す限り誰も踏まない場所にある。**

**なぜここか。** 実写での計測ができるようになったのは Phase 10（`contour_roughness`
/ `rim_contamination` と `tests/real_backgrounds.rs`）以降で、profile の canvas を
商品の長辺から決めるには Phase 22 の `Rules` と Phase 23 の `content_bounds` が
要る。**Phase 25 までの積み上げが揃って初めて測れる**種類の判断である。

`--quality` の既定 90（40 dB に最も近づく q）と、profile の canvas の梯子
`1000 / 1500 / 2000 / 2500 / 3000`。**新しいフラグも code も 1 つも足していない。**
規模 S、衝突リスクは `--quality` の側だけ広い（既定値が全経路のバイト列を動かす）。

- ※ **`--quality` の側は「手書きを 1 箇所へ寄せてから動かす」で足りた**が、寄せる
  先が**実装側 3 箇所では終わらなかった**。4 つ目がテスト（参照エンコーダ）の中に
  あり、定数へ寄せた後の値の変更で初めて落ちた。**既定値を動かす段では、実装の
  grep だけでは足りない**
- ※ **canvas の側は「型を割る」でほとんど解けた。** 指示の見立てでは
  `batch.rs` の `attach_set` が壊れるはずだったが、`Option` の中身を「決め方」に
  替えたので `is_some()` の意味が変わらず、1 行も直さずに済んだ。**壊れる場所を
  先に名指ししてあったから、それを壊さない型を選べた**

## 7.3 依存関係

```
Phase 17 CI ──（安全網。以降すべての前提）
Phase 18 パイプライン一本化 + ICC
   └→ Phase 19 --max-bytes
         └→ Phase 20 多派生 + マニフェスト ── schema_version 2 はここだけ
               └→ Phase 22 --profile + kiri lint
Phase 21 --fail-on（exit 5 の新設）───────┘ 語彙を Phase 22 が再利用
Phase 23 --rotate auto + セット統一（subject を共有。Phase 22 の後が望ましい）
Phase 24 色の正規化（Phase 21 の物差しが要る）
Phase 25 --reflect（依存なし。いつでも繰り上げ可）
Phase 26 出力の既定の引き直し（Phase 10 の実写ベンチと Phase 22 の Rules が要る）
   ├→ --quality 90：Phase 19 の梯子の段は動かさない（出発点だけが上がる）
   └→ profile の canvas：Phase 23 の content_bounds を切り抜きの後で読む
```

**一度に直すべき点は Phase 18 の 1 箇所である。** Phase 19 / 20 と ICC はすべて
`write_image()` → `encode()` を通るので、そこへ `Derivation` と `render()` を
差し込み、`SaveOptions` に `icc` を足す手術を 1 回だけ行う。保存されたゴールデンは
無かったので、取り直したのは入力とのバイト一致を見ていた rotate の 1 本だけで、
**ICC 込みの決定性**は新しいテストで固定した（§7.5 の Phase 18）。以降のフェーズは
この上に載るだけになる。

## 7.4 契約への影響

新しい code は `warning.rs` / `error.rs` のカタログへ足す以外に作る方法が無い構造に
なっているので、フェーズごとに次を追加する。

| Phase | code | 出る条件 |
|---|---|---|
| 18 | `ICC_NOT_EMBEDDED` | `--no-color-convert` で画素を sRGB へ変換しなかった（PNG / JPEG は ICC を埋めず、AVIF は名乗ったまま） |
| 19 | `MAX_BYTES_UNREACHABLE` | 下限品質でも目標サイズに届かなかった |
| 19 | `QUALITY_REDUCED` | 要求品質から落として目標サイズを達成した |
| 20 | `MANIFEST_PARTIAL` | 一部の派生が失敗したままマニフェストを書いた |
| 22 | `PROFILE_OVERRIDDEN` | profile の値を明示指定が上書きした |
| 22 | `PROFILE_UNCHECKABLE` | lint が検査できない項目を飛ばした（AVIF の画素など） |
| 23 | `ROTATE_AUTO_SKIPPED` | `--rotate auto` を適用しなかった（0 度のまま）。`data.reason` が `no_subject` / `low_confidence` / `not_measurable` / `not_rectangular` の 4 値でどの条件かを言う |
| 23 | `SET_SCALE_CLAMPED` | `set` が求めた占有率が 1.0 を超えたので 1.0 で止めた（その点だけ目標の高さに届かない）。**横長商品では常態**で、不足分は `data.height_shortfall` |
| 23 | `SET_NOT_MEASURED` | `set` の代表寸法を 1 点も測れず、揃えなかった（各項目は自分で解決した `fill_ratio` のまま） |
| 24 | `WHITE_BALANCE_SKIPPED` | 白点を当てなかった。`data.reason` は `not_neutral` / `not_light` を除く 4 値（`not_neutral` / `no_material` / `gain_out_of_range` / `would_clip`） |
| 24 | `EXPOSURE_SKIPPED` | 露出を正さなかった（**計画の表に無い追加**）。`data.reason` は `not_light` / `no_material` / `gain_out_of_range` / `would_clip` |

**Phase 25 は warning code を 1 つも足さなかった**（済。表から行を 1 つ消した）。
反射がはみ出したことは `reflect.clipped` が真偽で言う——`--shadow` が同じ事実を
`shadow.clipped` だけで語っているのに、反射だけ警告を重ねると**同じ事実が 2 通りの
形で配られる**。既定 off の段で警告の出る条件が増えることの方が、受け手にとっては
新しい契約である。

**Phase 26 も warning code を 1 つも足していない**（済。表に行は増えない）。
既定値 2 つを引き直した段で、拡大は既存の `CANVAS_UPSCALED`、profile の上書きは
既存の `PROFILE_OVERRIDDEN` が言う。**新しい条件で出る警告が 1 つも無い**ので、
足すかどうかの判断そのものが起きなかった。

新しいエラー code:

- 引数（exit 2）: `INVALID_MAX_BYTES` / `INVALID_DERIVATION` /
  `INVALID_NAMING_TEMPLATE` / `OUTPUT_NAME_COLLISION` / `UNKNOWN_PROFILE` /
  `INVALID_FAIL_ON` / `INVALID_ROTATE` / `INVALID_SET`
- 一般（exit 1）: `MANIFEST_WRITE_FAILED`
- **新分類 `Compliance`（exit 5）**: `QUALITY_GATE_FAILED`（Phase 21）/
  `PROFILE_VIOLATION`（Phase 22）

**既存の意味が変わるもの**は 7 件ある。ここを黙って変えると古い読み手が誤読する。

1. `outputs[]` — 型は `Vec<OutputReport>` のままだが、**常に 1 要素だった前提が
   崩れる**。`SCHEMA_VERSION` を 2 へ上げる唯一の根拠（Phase 20）
2. `UPSCALED` — summary を「resize / 派生で元画像より大きくした」へ広げる
   （code は流用、意味は拡張）
3. `ALPHA_FLATTENED` / `DRY_RUN_OUTPUT_EXISTS` — 1 実行で複数回出るようになる。
   全警告の `data` に `output` キーを足す
4. `exit_codes[]` — 表そのものが伸びる。5 は「成果物はあるが人が見るべき」である
   ことを `meaning` に書く
5. spec の `rotate` — **文字列を受けるようになった**（Phase 23。緩める向きの変更で、
   `{"rotate": "90"}` は `SPEC_INVALID` で落ちていた）。`"auto"` を受けるために型を
   `Option<f64>` から `Option<serde_json::Value>` へ広げた以上、数値の綴りも CLI と
   同じパーサが読む——断れば「CLI では通る書き方が spec でだけ通らない」道具になる。
   **Phase 23 が挙動を変えた既存契約はこれ 1 つだけである**
6. `PROFILE_OVERRIDDEN`（`data.key` が `canvas`）— **`warnings[]` の中の位置が
   変わった**（Phase 26。code も `data` の形も 1 つも変わらない）。profile の canvas が
   入力依存になり、「profile が求めた値」が切り抜いた後にしか分からなくなったので、
   この 1 件だけが先頭ではなく結果の警告の後・キャンバス配置の警告の前に来る。
   **`data.profile` が固定値でなくなる**ことも同じ行の話で、同じ profile でも素材が
   違えば違う寸法を名乗る。並びで警告を拾っている読み手だけが影響を受ける
   （code で拾っていれば何も変わらない）
7. `--shadow-offset` / `--shadow-blur` / `--reflect-height` / `--reflect-gap` の
   **長辺 1000px 換算が、`--profile` を渡した実行では入力依存になった**（Phase 26。
   キーの名前も型も単位の綴りも 1 つも変わらない）。換算の基準は前から「最終画像の
   長辺」で、その規約は動かしていない——動いたのは基準そのもので、profile の canvas が
   1600 固定から 1000〜3000 の段になったぶん、**同じ指定が素材ごとに違う実寸を指す。**
   実測（`--profile amazon --shadow synth --reflect on`、既定のノブのまま）:
   1600 固定だった頃は入力によらず影 `offset [0, 19]` / `blur 15.8325`、反射
   `height 240` だったものが、**1000 の段に落ちる素材では `[0, 12]` / `9.8319` /
   `150`、3000 の段では `[0, 36]` / `29.8329` / `450` になる**（同じ 2 枚を変更前の
   バイナリで測ると、どちらも 19 / 15.8325 / 240 で揃う）。既存の規約どおりの帰結で
   あってバグではないが、**`--profile` と影・反射を併せた実行の実寸を固定値だと
   思っている読み手だけが影響を受ける**。寸法を必ず揃えたい実行は `--canvas` を
   明示する。**Phase 26 が挙動を変えた既存契約はこの 2 つである**——`--quality` の
   既定は値であって契約の形ではない

新しいブロック: `schema.profiles[]`（**実装の定数から組む**。`fields[]` と同じ）、
`outputs[].quality_used` / `.role` / `.icc`、lint の `LintReport { checks: [{ name,
status, expected, actual }] }`、`subject.level_fill_ratio`（Phase 23。`level_rotation`
をどれだけ信用してよいかを言う値で、**門を kiri の外でも引けるようにするために配る**）、
`BatchReport.set`（`{ align, fill_ratio, source, measured, clamped }`。`compliance` /
`optimize` と同じく、`set` を書いた実行にしか現れない）、`CutoutReport.color`
（Phase 24。`{ white_balance, exposure, status, source, white_point,
white_point_shift, gain, exposure_stops, clipped_ratio }`。**`--white-balance` /
`--exposure` に `auto` を渡した実行にしか現れず、`settings` には足さない**——
足せば既定の実行の JSON が変わり、「off なら 1 バイトも変わらない」が破れる）。
`settings.rotate` は `f64 | "auto"` の union になり、`unit` の語彙へ
`deg_or_auto` が増える——**受け手が `as_f64()` で読んでよいかがそこで変わる**ので
`deg` に相乗りさせない。Phase 24 は `gain`（線形へ掛ける倍率。1 を超えるのが正常）と
`stops`（2 の対数。0 が「動かさない」で負値も正常）の 2 つを足す——どちらも
`ratio` に相乗りさせない。

batch spec には `profile` / `max_bytes` / `derive` / `naming` / `fail_on` /
`white_balance` / `exposure` / `reflect*` と、最上位の `set` が増える。**そのたびに
`batch.rs` の 3 箇所を同時に触る**（Phase 18 のテストがこれを守る）。

## 7.5 テスト方針

- **Phase 18**（済。結果は §5 の Phase 18 に、テスト名はここに並べた）— (a)
  `Derivation` 1 個のとき、ICC 抜きの出力が Phase 17 と 1 バイトも一致する（構造変更の無害性。
  `icc_none_png_is_the_phase17_encoder_output` / `icc_none_jpeg_…` /
  `avif_bytes_do_not_depend_on_the_icc_policy` /
  `render_with_one_derivation_writes_what_encode_returns`）。(b) 書いた PNG/JPEG を
  `kiri info` に食わせると `color_profile` が sRGB を名乗る（**外部ツールに頼らない
  自己完結の検証**。`a_written_png_and_jpeg_name_srgb_when_read_back`）。
  (c) iCCP / APP2 が 1 個だけ、位置が規格どおりであることをバイト列で検査
  （`png_carries_exactly_one_iccp_right_after_ihdr` /
  `jpeg_carries_exactly_one_icc_app2_right_after_jfif` /
  `avif_names_srgb_only_through_the_av1_sequence_header` ほか）。(d) 決定性を
  ICC 込みで再固定（`png_and_jpeg_outputs_are_deterministic_with_icc` /
  `the_profile_bytes_are_pinned`）。加えて、`--no-color-convert` の画素を書いた
  ファイルに ICC が無く `ICC_NOT_EMBEDDED` が 1 回だけ出ることを、convert と batch の
  両方でファイルの中身から確かめる（`unconverted_pixels_are_written_without_the_srgb_icc`）
- **Phase 19**（済。結果は §5 の Phase 19 に、テスト名はここに並べた）。
  **受け入れ基準の letter はこの一覧が定義である**——テストの doc コメントが
  `受け入れ基準 (a)` のように引くので、参照先をここに置く。
  - **(a) 達成したら必ず `max_bytes` 以下**（`a_reachable_budget_always_lands_under_the_limit` /
    `the_ladder_stops_at_the_first_rung_that_fits`）。報告と実ファイルの両方を見る。
    JPEG と AVIF を、きつい上限と緩い上限の 2 点ずつ
  - **(b) 未達なら必ず `MAX_BYTES_UNREACHABLE`**
    （`an_unreachable_budget_writes_the_requested_quality_file` /
    `an_unreachable_budget_writes_the_requested_quality_untouched`）。**書いたファイルが
    `--max-bytes` 無しの出力と 1 バイト一致する**ことまで固定する
  - **(c) 同じ入力で `quality_used` と `attempts` が毎回同じ**
    （`the_same_input_lands_on_the_same_rung_every_time` /
    `the_landing_spot_is_the_same_every_time`）。**段で止まる経路を必ず通す**——
    未達の上限では 3 回とも同じ答えになり、探索を通らずに一致してしまう
  - **(d) 無害性の回帰**（`render_with_one_derivation_writes_what_encode_returns` が
    `attempts == 1` まで見る / `a_budget_that_already_fits_changes_nothing`）
  - **(e) PNG は段を降りない**（`png_ignores_the_budget_but_says_why` /
    `lossless_formats_never_walk_down_the_ladder` / `lossless_bytes_do_not_move_with_quality`。
    WebP（lossless）を足したときに 2 つとも PNG と WebP の両方を見る形へ改名した）
  - **(f) `--dry-run` でも探索は走る**（`a_dry_run_searches_for_the_budget_without_writing`）
  - **(g) 書式の解釈**（`parses_a_byte_budget_with_and_without_a_unit` /
    `the_decimal_units_never_exceed_the_binary_ones` / `rejects_a_malformed_byte_budget` /
    `rejects_a_byte_budget_that_overflows`）
  - **(h) spec 経由**（`a_spec_takes_the_budget_as_a_number_or_a_string` /
    `a_malformed_budget_in_a_spec_is_refused` /
    `a_malformed_budget_on_the_command_line_is_refused_by_the_parser`）
  - **(i) 警告の `data` が揃っている**（`the_quality_reduced_warning_carries_every_number` /
    `a_fractional_quality_is_spelled_the_same_everywhere`。報告と警告で同じ数が
    同じ字面になることまで見る）
  - **単調性は固定しない**（JPEG は品質を下げてもサイズが単調に減らない区間がある）。
    契約そのものの回帰は
    `the_readme_warning_table_lists_every_warning_in_the_contract` /
    `the_published_prose_spells_the_real_ladder` /
    `every_published_unit_is_in_the_known_vocabulary` が持つ
- **Phase 20**（済。結果は §5 の Phase 20 に、テスト名はここに並べた）。
  **受け入れ基準の letter はこの一覧が定義である**——テストの doc コメントが
  `受け入れ基準 (a)` のように引くので、参照先をここに置く。
  - **(a) 派生 1 個のときの完全一致**（最重要の回帰。
    `a_single_derivation_writes_the_same_bytes_to_the_same_path` /
    `adding_derivations_does_not_disturb_the_ones_already_there`）。**既存の
    md5 / 決定性テストを 1 本も書き換えずに通した**ことが第一の証拠で、244 本の
    うち Phase 20 で足したもの以外は 1 行も触っていない
  - **(b) マニフェスト JSON のゴールデン**（`the_manifest_is_a_byte_for_byte_golden`）。
    2 回走らせて同じバイト列になることと、キーの並び・結果 JSON の `outputs[]` との
    一致まで見る。**並びは書いたバイト列で見る**——`serde_json::Value` へ読み直すと
    `Map` が綴りで並べ替えてしまい、ファイルの中の順序は分からなくなる
  - **(c) 命名衝突を書き始める前に検出する**
    （`a_name_collision_is_caught_before_anything_is_written` /
    `force_does_not_excuse_two_derivations_sharing_a_path` /
    `a_malformed_naming_template_is_refused_with_a_code`）。**出力ディレクトリに
    1 ファイルも増えていない**ことまで assert する
  - **(d) ピークメモリが N に比例しない**
    （`five_derivations_do_not_cost_five_times_the_peak_memory`。
    [3.2b](../implementation-plan.md#32b-境界品質の回帰テストtestsedge_qualityrs) の `resident_kb()` と
    同じ `ps -o rss=` を、走っている子プロセスへ向けた）。**絶対値は書かない**——
    N=1 と N=5 のピークの増分が N=1 のピークの半分を切ることだけを見る
  - **(e) 派生に紐づく警告が `data.output` を持つ**
    （`every_derivation_bound_warning_names_its_output`）。6 つすべてを実際に
    鳴らし、値が `outputs[].path` のどれかと一致することまで見る
  - **(f) 排他と書式**（`derive_and_the_product_flags_cannot_be_mixed` /
    `a_malformed_derivation_on_the_command_line_is_refused_by_the_parser` /
    `the_product_of_sizes_and_formats_keeps_size_outside` /
    `the_extension_round_trips_through_from_path`）
  - **(g) spec 経由**（`a_spec_builds_derivations_through_the_same_gate` /
    `a_partial_batch_says_so_in_the_manifest_warning`）
  - **(h) 契約そのもの**（`the_schema_version_is_two_everywhere` /
    `the_manifest_obeys_the_overwrite_rules_and_dry_run` と、既存の
    `the_readme_warning_table_lists_every_warning_in_the_contract` /
    `every_code_named_in_the_docs_exists` / `every_published_field_exists_in_the_result`）
- **Phase 21**（済。結果は §5 の Phase 21 に、テスト名はそちらへ並べた）。
  **受け入れ基準の letter はこの一覧が定義である** — (a) 各指標 × しきい値 →
  exit code の対応表（触れる / 触れない / 測れない の 3 通り）。(b) `--fail-on` 無指定
  なら exit code も結果 JSON も 1 つも変わらない。(c) batch で「1 件不合格 + 0 件失敗 → 5」
  「1 件失敗 + 1 件不合格 → 4」の優先順位
- **Phase 22**（済。結果は §5 の Phase 22 に、テスト名はそちらへ並べた）。
  (a) `profiles[]` が実装の定数と一致する。(b) プリセットごとに
  「合格する合成画像」と「1 項目だけ外した合成画像」で合否と `checks[].name` を固定。
  (c) AVIF の lint で画素検査が `skipped` になり `PROFILE_UNCHECKABLE` が出る。
  (d) **profile が実際に求める項目**（`--format` / `--canvas` / `--fill-ratio` /
  `--output` の拡張子）で `PROFILE_OVERRIDDEN` が出て、**明示した値が成果物に
  現れる**。※ 当初は `--profile amazon --quality 60` と書いていたが、**これは
  成立しない**——品質を定めているモール規格が無く、出典の無い数値を `Rules` に
  入れない方針を採ったので、`write_defaults` は quality を一度も求めない
  （求めていない値は押しのけようがない）
- **Phase 23**（済。結果は §5 の Phase 23 に、テスト名はそちらへも並べた）。
  **受け入れ基準の letter はこの一覧が定義である**——テストの doc コメントが
  `受け入れ基準 (a)` のように引くので、参照先をここに置く。前半（`--rotate auto`）と
  後半（batch の `set`）で letter は別に振っている。
  - **前半 (a) 効くこと**（`cutout_rotate_auto_levels_the_subject`）。`auto` を適用した
    画像へもう一度 `info` を掛けて `level_rotation` が 0 に近いことを見る
    （許容 0.5 度。**分解能そのものが 0.5 度なので、それを下回る差は有意でない**）
  - **前半 (b) 適用しないときは 1 画素も回さない**
    （`cutout_rotate_auto_does_not_turn_what_it_cannot_trust` /
    `cutout_rotate_auto_leaves_a_handled_shape_alone` /
    `the_shape_gate_separates_what_the_rectangle_describes`）。4 つの `data.reason`
    （`no_subject` / `low_confidence` / `not_measurable` / `not_rectangular`）を
    実画像で踏む。**門が効きすぎていないことを同時に固定する**
    （`cutout_rotate_auto_still_levels_a_boxy_product`）——門だけを見る検査は
    「何も回さない」実装を緑にするので、角丸長方形（充填率 0.98 付近）が今までどおり
    水平になることを同じ往復で見る
  - **前半 (c) 無害性**（`a_run_without_a_rotation_never_mentions_the_auto_gate`）。
    `--rotate` を渡さない実行は門を 1 度も通らず、警告も `level_fill_ratio` 由来の
    分岐も出ない
  - **前半 (d) 決定性**（`cutout_rotate_auto_is_deterministic`）
  - **前半 (e) spec 経由**（`the_batch_spec_accepts_auto_and_a_number_for_the_rotation` /
    `a_malformed_rotation_in_a_spec_is_refused_with_a_code` /
    `every_unreadable_rotation_in_a_spec_lands_on_the_same_code`）。読めない角度は
    `INVALID_ROTATE` で**その項目だけ**が落ちる
  - **前半 (f) 契約**（`the_rotate_option_publishes_auto_as_a_choice` /
    `a_malformed_rotation_on_the_command_line_is_refused_by_the_parser` /
    `the_batch_spec_reads_an_angle_written_as_a_string`）。`--rotate` は自由な数値も
    取るので `PossibleValuesParser` では作れず、`accepts` が空のままだと**綴りを外した
    ときに code 無しの exit 2 で落ちる項目の候補を、呼ぶ前に知る手段が無くなる**。
    最後の 1 本は `the_batch_spec_rejects_an_angle_that_is_not_a_number` を置き換えた
    もので、**Phase 23 が緩めた唯一の既存契約がそこにある**
  - **後半 (a) 3 通りの距離で高さが ±1px**
    （`a_set_aligns_the_subject_height_across_shooting_distances`）。同じ商品を
    3 通りの距離で撮った合成セットで、**書いた画像を測り直した高さ**が揃う
  - **後半 (b) 無害性**（`a_spec_without_set_carries_no_set_block`）。`set` を書かない
    実行の結果 JSON に `set` のキーは 1 つも現れない
  - **後半 (c) `bbox` は全点同じ `f_i`**
    （`a_bbox_aligned_set_writes_the_same_bytes_as_one_fill_ratio`）。同じ
    `fill_ratio` を 1 つ書いた実行との**バイト一致**で言う
  - **後半 (d) 中央値**（`the_set_target_is_the_median_of_the_measured_occupancies`）
  - **後半 (e) 上限**（`an_extremely_wide_item_is_clamped_without_touching_the_others`）。
    `f_i > 1.0` の点は 1.0 で止めて `SET_SCALE_CLAMPED` を出し、**他の点は 1 バイトも
    影響を受けない**
  - **後半 (f) 測れないセット**（`a_set_that_measures_nothing_says_so_and_stands_aside`）。
    `set` は効かず `SET_NOT_MEASURED` が出て、各項目は自分で解決した `fill_ratio` で書く
  - **後半 (g) 決定性**（`the_same_set_spec_is_deterministic`）。`T` も全項目の出力
    バイト列も 3 回とも同じ
  - **後半 (h) 排他**（`a_set_that_cannot_take_effect_is_refused`）。`item` / `defaults` の
    `fill_ratio` との同時指定と、canvas の無い項目を `INVALID_SET` で断る
  - **後半 (i) `--jobs`**（`the_number_of_jobs_does_not_move_the_set_target`）。
    中央値は並べ替えてから採るので、並列度で 1 ビットも動かない
  - **後半 (j) 契約**（`a_set_overrides_the_profile_fill_ratio_and_says_so`。profile の
    `fill_ratio` を `set` が上書きし、`PROFILE_OVERRIDDEN` の `data.by` が `set` と
    名乗る）と、既存の `every_published_unit_is_in_the_known_vocabulary` /
    `every_published_field_exists_in_the_result` / `every_code_named_in_the_docs_exists` /
    `the_readme_warning_table_lists_every_warning_in_the_contract`
- **Phase 24**（済。結果は §5 の Phase 24 に、テスト名はここに並べた）。新規
  `tests/color_normalize.rs` ＋ `src/color/normalize.rs` の単体テスト。**受け入れ
  基準の letter はこの一覧が定義である。**
  - **(a) off で無変化**（`off_does_not_change_a_single_byte`）。フラグ無しと
    `off` 明示の出力がバイト一致。**着手前後で `tests/edge_quality.rs` と
    `tests/real_backgrounds.rs` の `--ignored` 一覧を取る**
    （[3.2b](../implementation-plan.md#32b-境界品質の回帰テストtestsedge_qualityrs) の作法）のはこの裏取りで、
    edge_quality 139 行 / real_backgrounds 279 行のうち**時間と RSS を含む行以外の
    差分は 0**。さらに main から建てたバイナリとの md5 突き合わせを添える
    （実写 2 枚 × フラグなし / `--bbox` / `--optimize`）
  - **(b) 既知の色かぶりを戻せる**（`a_known_colour_cast_is_undone`）
  - **(c) 既知の露出ずれを戻せる**（`a_known_exposure_offset_is_undone`）
  - **(d) 色のある背景は中性化しない**（`a_coloured_background_is_not_neutralised` /
    単体 `a_coloured_background_is_refused`）。出力は off とバイト一致
  - **(e) 白飛びする量は当てない**（`a_gain_that_would_clip_is_refused`。単体にも
    同名がある）。判定は**予算 0.03 を超えたこと**で言う——0.001 のような弱い
    しきい値で見ると「断った理由」を固定できない
  - **(f) 暗い背景に露出を当てない**（`a_dark_background_is_not_lifted_to_white` /
    単体 `a_dark_background_refuses_only_the_exposure`）
  - **(g) 決定性**（`the_same_normalisation_is_deterministic`）。3 回とも出力バイト列と
    `color` ブロックが同じ
  - **(h) 段ごとに落ちる**（`white_balance_applies_even_when_exposure_is_refused` /
    単体 `a_darkening_exposure_survives_a_clipping_white_point`）と、**合成の門が
    押し出している段を落とす**（`both_stages_are_never_worse_than_one`。両方 `auto` の
    出力が `--exposure auto` 単独とバイト一致すること——**§5 の ※ で直した欠陥が
    ここに固定されている**）と、**断り文句がどの段を落としたか名乗る**
    （`the_refusals_name_the_stage_that_pushed_the_gate`）
  - **(i) 探索の前段に掛かる**（`normalisation_runs_before_the_search`）。CLI を
    2 段に分けた実行とは比べられない（1 段目の出力は切り抜き済みで、2 段目の探索が
    見る材料が別物になる）ので、1 段目はライブラリの `normalise` 直呼びにしてある
  - **(j) 白点は場のセルから測る**
    （`the_white_point_comes_from_the_field_cells_when_a_field_is_built` /
    単体 `the_cell_median_is_taken_per_channel_and_ignores_outliers` /
    `an_even_number_of_cells_takes_the_upper_median`）。**`--background-model field`
    を明示して `color.source` を固定する**——合成シーンは `auto` では `flat` に
    落ちやすく、中央値の経路が「通るだけの検査すら無い」状態になりやすい
  - **(k) 範囲の門**（`an_unbounded_gain_is_refused_and_reports_no_gain` /
    単体 `an_unbounded_white_point_gain_is_refused_by_the_range_gate`）。
    `data` に有限でない数を載せないことも同じテストが見る
  - **(l) 8bit を 1 も動かさないゲインは当てない**
    （`a_gain_too_small_to_move_any_8bit_value_is_not_applied`）
  - **(m) spec**（`the_batch_spec_reads_white_balance` /
    `the_batch_defaults_carry_the_white_balance` /
    `an_unknown_white_balance_in_the_spec_is_refused`）
  - **(n) CLI**（`an_unknown_white_balance_value_is_refused_by_the_parser`。exit 2）
  - **(o) 契約** — `every_published_unit_is_in_the_known_vocabulary`（`gain` と
    `stops` を語彙へ足した）/ `every_published_field_exists_in_the_result`
    （**両方に `auto` を渡した `--dry-run` の実行を 1 つ足した**。片方だけでは、
    もう片方のキーが「出るが 0 のまま」なのか「出ない」のかを確かめられない）/
    `every_code_named_in_the_docs_exists`（「予定の code」の表から
    `WHITE_BALANCE_SKIPPED` を消した）/
    `the_readme_warning_table_lists_every_warning_in_the_contract`
    （README の表へ 2 行足した）
  - **較正とベンチの入口**は `print_the_calibration_table`（32 点の C\* / L\* /
    要るゲイン / clip）と `print_the_cost_on_a_large_image`（見立てと画素パスの
    所要時間）。どちらも `--ignored` で、実写は `KIRI_BENCH_DIR` から読む。
    **表は `cutout` と同じ読み込み経路（ICC → sRGB）を通す**——`image::open` で
    素通しすると白点が実行と食い違う（remote.jpg で C\* が 4.2 と 4.8 に分かれた）
- **Phase 25**（済。結果は §5 の Phase 25 に、テスト名はここに並べた）。`--shadow` と
  同型（`bounds` / `clipped` / 決定性 / `off` で無変化）。
  単体（`src/transform/reflect.rs`）:
  `the_reflection_mirrors_the_product_row_by_row`（どの行がどこへ落ちたか。
  **減衰で動かない量——インクの出る x の集合と RGB——で見る**ので、指示書の
  `an_unfaded_reflection_...` から名を替えた。減衰は常に掛かるので「unfaded」は
  嘘になる）/ `the_reflection_starts_one_gap_below_the_product`（隙間の行に 1 画素も
  漏らさないことまで）/ `the_reflection_fades_monotonically_to_its_foot` /
  `the_reflection_takes_the_product_colours` /
  `opaque_product_pixels_and_reflection_free_pixels_are_untouched` /
  `a_zero_opacity_leaves_the_image_untouched` /
  `a_zero_opacity_is_never_reported_as_clipped` /
  `a_zero_height_leaves_the_image_untouched` /
  `a_reflection_running_off_the_image_is_clipped_and_reported` /
  `a_reflection_pushed_entirely_off_the_image_still_reports_the_clipping` /
  `an_empty_product_lays_no_reflection` /
  `an_absurd_height_neither_panics_nor_overflows`（`u32::MAX` の高さ・隙間。走る行数を
  `baseline + 1` で抑えていることの検査でもある）/ `a_zero_sized_image_is_left_alone` /
  `a_product_touching_the_side_edges_is_reported_as_clipped`（**左右の縁の偽陽性は規約で
  あって不具合ではない**ことを固定する。1px 内側へ寄せれば偽になる側も見る）/
  `ink_above_a_transparent_band_still_counts_as_dropped`（外へ落ちた行で loop を
  抜けてはいけない理由）/ `the_same_input_produces_the_same_bytes`。
  影側に 1 本足した: `transform::shadow` の
  `splitting_synth_into_alpha_and_compose_changes_nothing`（`synth` と
  `alpha` + `compose` が**半透明の縁つきの素材で**バイト一致。**これが分割の担保で
  ある**——`--reflect off` との比較は同義反復なので裏にならない）。
  費用の入口は `print_the_reflection_cost_on_a_large_product`（`--ignored`）。
  CLI / 契約（`tests/cli.rs`）:
  `a_run_without_a_reflection_reports_no_reflect_block` /
  `an_explicit_reflect_off_writes_the_same_bytes_as_no_reflect_at_all`（成果物と結果 JSON の
  両方。**`outputs` は落とさず `path` だけ畳んで比べる**ので `bytes` / `width` /
  `height` / `quality_used` も比較対象に入る。ただし clap の既定値で `--reflect` 無しと
  `off` は同一の引数値になるので、**「Phase 25 の前と変わらない」の裏はここでは取れない**
  ——そちらは main から建てたバイナリとの md5 突き合わせで取る。Phase 24 と同じ作法で、
  テストに golden digest は埋めていない）/
  `the_reflection_is_reported_with_its_bounds`（1 行目が商品の下端の真下に来ることと、
  px@1000 の基準がキャンバスの長辺であること）/
  `the_reflection_reports_the_clipping_when_it_runs_off_the_canvas`（切れた / 全部
  はみ出した / 敷かなかった の 3 通り）/ `the_same_reflection_is_deterministic` /
  `the_reflection_sits_above_the_shadow`（**層の順を画素で固定する**。影を不透明な緑で
  真下へ出し、商品の真下が鏡像になっていることを見る）/
  `the_reflection_is_made_of_the_product_alone`（**指示書に無かった 1 本**。影を商品より
  上へ出し、鏡像が商品の上端を越える行を写しても影が降りてこないことを見る——
  `shadow::alpha` と `compose` を割った理由そのものの検査）/
  `an_explicit_reflect_off_does_not_disturb_a_shadow_only_run`（影だけの 3 通りの実行
  ——`--canvas` あり / なし、`--flatten` あり / なし——が `--reflect off` とバイト一致。
  **`place_on_canvas` から `compose_layers` へ差し替えた側**を通す経路の検査でもある）/
  `a_flattened_canvas_burns_the_reflection_between_the_background_and_the_product`
  （**今回いちばん壊れやすい 2 行**——`flatten_here` の条件と下地の載せ直しの条件を
  `layers.any()` へ広げた箇所——を画素で固定する。全画素が不透明であること＋商品の
  真下が白でなく鏡像であること＋足元が下地へ溶けること）/ パーサ
  `an_unknown_reflect_value_is_refused_by_the_parser` /
  `an_out_of_range_reflect_opacity_is_refused` / `too_large_a_reflect_height_is_refused`
  （上限ちょうどが通ることまで）/ `too_large_a_reflect_gap_is_refused` / spec
  `the_batch_spec_reads_reflect` / `the_batch_defaults_carry_the_reflect` /
  `an_unknown_reflect_in_the_spec_is_refused` /
  `an_out_of_range_reflect_height_is_refused_in_a_spec_too` /
  `a_misspelled_reflect_height_key_suggests_the_right_one` /
  `the_spec_defaults_for_the_reflection_match_the_cli`（**指示書に無かったもう 1 本**。
  同じ既定値が clap と `commands/batch.rs` の 2 箇所に書かれていて、`reflect` は
  `CutoutOptions` を通らないので既存の既定値の突き合わせでは押さえられない）。
  schema: `schema_publishes_the_four_knobs_of_the_reflection`（影の 5 ノブの 1 本を写した。
  `accepts` が `["off","on"]`、3 つの既定値、summary が「鏡像」と px@1000 を名乗ること、
  long help が合成順を言うこと）。
  既存の契約テストは `every_published_field_exists_in_the_result`
  （`--reflect on` の `--dry-run` の実行を 1 本足した）/
  `every_published_unit_is_in_the_known_vocabulary`（**語彙は 1 つも足していない**
  ——`px` / `ratio` / `bool` / `enum` で足りる）/
  `the_readme_warning_table_lists_every_warning_in_the_contract`（**新しい code は
  無い**ので README の警告表は触っていない）が追随している

## 7.6 却下した代替案

- ※ **`--profile` を最初に置く。** profile は複合指定の別名なので、多派生と
  `--max-bytes` が無い状態では検査しかできない片肺の機能になる。しかも中間状態の
  `--profile` が契約として外へ出てしまい、**一度配った契約は引っ込められない**
- ※ **ICC の埋め込みを最後に回す。** 7.1 の 2 のとおり、`--max-bytes` の探索結果も
  全派生のバイト列も既存ゴールデンも取り直しになる
- ※ **セット統一を新サブコマンド `kiri set` として作る。** spec / `ItemSettings` /
  警告の継承 / `--jobs` / `--dry-run` の規約をもう 1 系統持つことになり、
  `kiri schema` が配る契約が二重化する。batch の `defaults` の隣に `set` を足すほうが
  既存の継承規約をそのまま使えて、エージェントが覚える規則が増えない
- ※ **合否を警告のまま exit 0 で返し、仕分けは呼び出し側の jq に任せる。** いまの
  問題はまさに「28 の警告があるのに合否が無い」ことで、そこを外部化すると
  「契約を自分で配る」設計から合否だけが漏れる。加えて exit code で分岐できないと、
  batch の数百点を仕分ける最短経路が JSON の全走査になる
- ※ **多派生の形式に WebP を足す。** 7.1 の 3 のとおり pure Rust では可逆しか
  書けず、写真素材では AVIF / JPEG にサイズで負ける。lossy には libwebp（C）が要り、
  `Cargo.toml` が `tract-onnx` について立てた基準を崩す
  - **後日、lossless の WebP だけを足した**（上の Phase 20 の注を参照）。lossy を
    入れない判断は変わっていない
- ※ **`kiri lint` を AVIF も含めた完全な検査として設計する。** pure Rust の AVIF
  デコーダが実質存在しない以上、C 依存か巨大な自前実装のどちらかになる。
  **検査できる範囲を正直に返す**ほうが、黙って合格を出すより安全である
