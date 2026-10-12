# 12. Phase 31: note の見出し画像を規格として組む

節番号は分割前のまま変えていない。どの節がどのファイルにあるかは
[実装計画](../implementation-plan.md) の索引で引く。

`kiri compose` は文字と素材を組み、はみ出し・コントラスト・重なり・安全領域を
測る。**ただし測っているのは原寸の絵である。** 見出し画像が原寸で見られることは
ほとんど無い——note の一覧では横 144〜192px に縮んで並ぶ。原寸で 4.5 を超えた
コントラストも、64px の見出しも、そこでは読めないことがある。

この段は 2 つを足す。

1. **`--profile note`** — キャンバス・安全領域・容量の上限を note の規定から当てる
2. **縮小表示での読みやすさの測り** — profile が持つ表示幅へ縮めた絵で、文字の
   大きさとコントラストを測り直す

## 12.1 なぜここか

1. **AI が他のツールで代わりに測れない。** 縮小した絵を見て「読めるか」を判断する
   のは、目で見ても数でも、エージェントには難しい。合成そのものは HTML と
   ブラウザでもできるが、**縮小後の読みやすさを数で返す道具は無い**。kiri が
   持つ意味があるのはここである
2. **Phase 29 が先送りした宿題である。** §10.9 は `--profile note` を「spec が
   固まった後の独立した段にする」として却下した。spec はその後動いていない
3. **既存の出力を 1 バイトも動かさない。** `--profile` を渡さない compose の
   結果 JSON とバイト列は変わらない

**YouTube など他のサービスは対象にしない**（利用者の判断。2026-10-12）。表の
形は複数の規格を持てるようにしておくが、行は note の 1 つだけである。

## 12.2 note の規定（一次情報、2026-10-12 に確認）

出典: [登録画像の推奨サイズ一覧](https://www.help-note.com/hc/ja/articles/360000231642)

| 項目 | 規定 | kiri での扱い |
|---|---|---|
| 記事の見出し画像の推奨サイズ | 1280 × 670 px | キャンバスの既定 |
| よりきれいに表示するなら | 1920 × 1006 px | 比が同じ（1.910 / 1.909）なので許す |
| 推奨と異なるサイズ | トリミングされて表示される | 比が外れたら警告 |
| 容量 | 各画像 最大 10MB | 書いたバイト数を検査 |
| マガジン一覧などの表示 | 1280 × 454 px の範囲が表示される | 安全領域の既定（上下中央） |
| スマホのブラウザ・アプリ | 画像の中央部分が切り取られて表示される | 同上 |

**安全領域は上下中央の 1280 × 454 である。** (670 − 454) / 2 = 108 なので
`[0, 108, 1280, 454]`。キャンバスが 1920 × 1006 なら同じ比で拡大する。
この値は 08-compose.md の例にある `safe_area` と一致する。

**容量の 10MB は「最大」と書かれているので「以下」で読む。** 10MB は 1000 進の
10,000,000 バイトとする（`--max-bytes` の k を 1000 進にしたのと同じ決め方）。
1280 × 670 の RGBA は非圧縮でも 3.4MB なので、普通には触れない。

## 12.3 一覧での表示幅（実測、2026-10-12）

**note は一覧でのサムネの大きさを公表していない。** note.com のトップを
ブラウザで開き、記事の見出し画像（`uploads/images/.../rectangle_large`）の
描画サイズを CSS px で測った。

| 画面 | 描画サイズ（CSS px） | object-fit |
|---|---|---|
| PC（横 1440） | 192 × 101、128 × 67、96 × 50 | cover |
| スマホ（横 375、DPR 2） | 144 × 75、96 × 50 | cover |

**どれも比は 1.91 前後で、cover でも 1280 × 670 なら切られない。**

profile には次の 2 つを「表示」として持つ。

| 名前 | 幅（CSS px） | 根拠 |
|---|---|---|
| `pc_list` | 192 | PC の一覧で最も多かった大きさ |
| `mobile_list` | 144 | スマホの一覧で最も多かった大きさ |

**96px は持たない。** ランキング等の小さな枠で、そこで 11px の文字を要求すると
1280 幅で 147px の見出しが必要になり、見出し画像の作り方として現実的でない。
**実測値なので note の改修で変わりうる。** `revision` に測った年月を持つ。

## 12.4 作るもの

### 12.4.1 規格の表は compose 用に別に持つ

`src/profile.rs` の `Rules` は EC のモール規格（占有率・背景色・透過）のための
表で、cutout / batch / lint が読む。**note をそこへ混ぜない。** 混ぜると
`kiri cutout --profile note` が書式として通り、意味の無い規格が当たる
（`compose::gate` が cutout の指標と表を分けたのと同じ理由）。

`src/compose/profile.rs`（新）に次を持つ。

```rust
pub struct ComposeProfile {
    pub name: &'static str,        // "note"
    pub revision: &'static str,    // "2026-10"
    pub summary: &'static str,
    pub source: &'static str,      // 12.2 の URL
    pub canvas: (u32, u32),        // (1280, 670)
    pub safe_area: [f64; 4],       // [0, 108, 1280, 454]（canvas 基準）
    pub max_bytes: Option<u64>,    // Some(10_000_000)
    pub displays: &'static [Display],
}
pub struct Display { pub name: &'static str, pub width: u32 }
```

名前の検査は `kiri compose --profile` の clap の値検査で行い、未知の名前は
既存の profile と同じく exit 2 で断る。

### 12.4.2 profile が spec へ当てるもの

**明示 > profile の順で勝つ。** cutout の `--profile` と同じ約束である。

| 項目 | spec に書いてある | spec に無い |
|---|---|---|
| `canvas.width` / `height` | spec の値を使う。比が profile の比から 0.5% 以上外れたら `PROFILE_ASPECT_MISMATCH` | profile の値を使う |
| `safe_area` | spec の値を使う | profile の値を、キャンバスの大きさへ比で合わせて使う |

**`canvas.width` / `canvas.height` を spec で省けるようにする**のはこの段の
spec の変更である。省けるのは `--profile` を渡したときだけで、渡さずに省いたら
今と同じく spec の読み込みで断る（code は既存の spec 不正のもの）。
`kiri schema` の `compose_spec[]` はこの条件を説明に書く。

比の判定: `|(w/h) / (pw/ph) − 1| >= 0.005`。1920 × 1006 は 0.1% なので通る。

### 12.4.3 縮小表示での測り

profile を渡したときだけ、文字の層ごと・表示ごとに測る。

**測る絵は、全層を重ね終えた最終のキャンバスである。** 後から重なった層や
（次の段で入る）縁取りまで含めた「実際に見える絵」を縮める。

1. 最終のキャンバスを表示幅 `W` へ縮める。高さは `round(H × W / canvas_w)`。
   縮小は面積平均（box）で、sRGB の値のまま平均する（ブラウザの縮小に近い）。
   透過の画素は、出力形式が透過を保てないなら潰す色へ重ねてから、保てるなら
   **白**へ重ねてから縮める（note の一覧の地は白。12.6 の決定 2）
2. 同じ縮小をその文字の被覆（グリフのアルファ）にもかける
3. 表示上の文字の大きさ `size_px = size × W / canvas_w`
4. 表示上のコントラスト:
   - 前景 = 縮めた被覆が 0.5 以上の画素の、縮めた絵の相対輝度の中央値
   - 背景 = その文字の `placed` 矩形を縮めた範囲で、縮めた被覆が 0.05 未満の
     画素の相対輝度の中央値
   - 比 = (明 + 0.05) / (暗 + 0.05)。前景か背景の画素が 1 つも無ければ `None`
5. 判定（**1 つでも当たれば読めない**）:
   - `size_px < 11` → 理由 `too_small`
   - 被覆 0.5 以上の画素が 0 → 理由 `strokes_lost`（線が 1px 未満に潰れた）
   - 比 `< 4.5` → 理由 `low_contrast`

**11px の根拠**: Apple の Human Interface Guidelines が本文の最小として挙げる
11pt。kiri が決めた数ではない（4.5 が WCAG の数であるのと同じ扱い）。
**表示上の大きさは文字の size（em）で測る**——CSS の font-size と同じ量で、
spec に書いた値から計算で出るので、エージェントが直すときに逆算できる。

**原寸のコントラスト（`text_contrast`）とは別に持つ。** 原寸は「文字色と背後」、
縮小は「見えている前景と背景」で、量の定義が違う。片方を置き換えると、
原寸でしか起きない問題（細い線の背後だけ暗い、など）が消える。

### 12.4.4 結果 JSON

- 最上位に `profile: { name, revision, source } | null`（`--profile` 無しなら
  キーごと出さないか null。既存の compose の出力が変わらない方を選ぶ）
- 文字の層の `layers[].display[]`:
  `{ name, width, size_px, contrast, legible, reasons[] }`
  （`contrast` は測れなければ null）
- `output.bytes` と profile の `max_bytes` の比較結果は警告で言う

### 12.4.5 警告

| code | いつ | data | remedy |
|---|---|---|---|
| `TEXT_ILLEGIBLE_AT_DISPLAY` | 12.4.3 の判定に 1 つでも当たった | `layer`, `display`, `width`, `size_px`, `contrast`, `reasons` | too_small なら size を上げる（必要な size は `11 × canvas_w / width`）。low_contrast なら文字色か背後を変える。strokes_lost なら weight を上げる |
| `PROFILE_ASPECT_MISMATCH` | 12.4.2 の比の判定 | `expected`, `actual` | canvas を profile の大きさにする |
| `PROFILE_MAX_BYTES_EXCEEDED` | 書いたバイト数 > `max_bytes` | `bytes`, `max_bytes` | JPEG / AVIF で書く、または quality を下げる |

`--dry-run` ではバイト数が決まるなら同じく判定する（既存の dry-run が
エンコードまでしているかに従う。していなければ判定せず、そのことを
文書に書く）。

### 12.4.6 `--fail-on`

compose の `Metric` に足す。

| 指標 | 量 | 既定の門（`default`） |
|---|---|---|
| `display_text_size` | 全文字層・全表示での `size_px` の最小 | `< 11` |
| `display_contrast` | 同じく `contrast` の最小（null は数えない） | `< 4.5` |
| `display_strokes_lost` | `strokes_lost` に当たった層があるか（旗） | 立ったら |

**profile 無しでこれらを `--fail-on` に書いたら、測っていないので判定できない。**
既存の compose の門が「測れなかった指標」をどう扱うかに合わせる（無ければ
その旨を `checks[]` に出し、合格扱いにしない方向で揃える）。

### 12.4.7 契約

- `kiri schema`: `compose` の options に `--profile`、`compose_profiles[]`
  （12.4.1 の全項目）、警告 3 本、`fail_on` の指標 3 本
- `kiri schema --summary` / `--brief` の要約にも載せる
- 既存の schema のスナップショットや契約のテストがあれば更新する

## 12.5 受け入れ条件

1. `--profile` 無しの compose は、既存のテスト spec で結果 JSON（`elapsed_ms`
   を除く）と出力の md5 が main と一致する
2. `canvas` を省いた spec が `--profile note` で 1280 × 670 になり、
   `safe_area` が `[0,108,1280,454]` になる
3. 1920 × 1006 は警告なし、1280 × 720 は `PROFILE_ASPECT_MISMATCH`
4. 白地に黒 64px の見出しは原寸で警告なし、`--profile note` で
   `mobile_list`（144）の `size_px` が 7.2 になり `too_small` で警告
5. 白地に黒 110px の見出しは両表示で `legible: true`
6. 写真や中間色の地に、原寸のコントラストが 4.5 をわずかに超える細い文字を
   置くと、縮小で `low_contrast` または `strokes_lost` になる例を 1 つ以上
   テストで固定する（**縮小の測りが原寸の測りと違うことを言える証拠**）
7. 同じ入力から同じ結果（決定性）
8. `cargo test --release`（feature 有無を直列で）、clippy（両系統、`-D warnings`）、
   fmt、MSRV 1.85 がすべて通る
9. 08-compose.md に「note の見出し画像を組む」の節、README のコマンド表の
   compose 行に `--profile note` の一言、implementation-plan.md の索引に本段

## 12.6 決めたこと

1. **表示幅は 192 と 144 の 2 つ**（12.3）
2. **透過の地は白として縮める。** note の一覧の地は白で、透過の PNG を
   上げたときに見えるのはその白だからである。原寸の `text_contrast` が
   「見る人の背景を知らないので測らない」としたのと違うのは、ここでは
   見る場所（note の一覧）が決まっているからである
3. **縮小は box。** Lanczos は縮小で輪郭を立たせ、細い線を実際より読めるように
   見せる向きに外れる
4. **原寸の測りは置き換えない**（12.4.3 の末尾）
5. **既存の挙動に従うとした 4 点は、`--profile` を渡さない出力が動かない側を
   採った**（2026-10-12、実装時に決めた）
   - **`profile` のキーは null ではなく省く。** 結果 JSON の `compliance` が
     `--fail-on` 無しでキーごと消えるのと同じ扱いである。null で出すと、
     渡さない実行の JSON に 1 キー増える。`layers[].display` も同じく省き、
     出るのは `--profile` を渡し、`role` が `decoration` でない文字の層だけ
     である（`decoration` は原寸の `text_contrast` も測らない）
   - **`--dry-run` でも `PROFILE_MAX_BYTES_EXCEEDED` を判定する。** 既存の
     dry-run は `save::encode` まで通して `output.bytes` を返しており
     （`commands/compose.rs` の `write`）、書かないだけでバイト数は決まって
     いる。判定しない理由が無い
   - **`--fail-on default` は `--profile` が無ければ `display_*` を足さない。**
     既存の門は「相手が居ない」を `skipped`（合格）、「相手は居るが値が無い」を
     `unmeasurable`（不合格）に分けている。profile 無しの `default` に 3 行を
     足すと、どちらに振っても既存の `default` の `checks[]` が動く（`skipped`
     なら行が増え、`unmeasurable` なら exit 5 に変わる）ので、そもそも
     足さない。**明示して書いた `display_*` は `unmeasurable`** で残す——
     測っていないものを合格にはしない（12.4.6 の方向どおり）。profile は
     あるが読めることを要求する文字が 1 つも無ければ `skipped`
   - **canvas の寸法は 1 辺ずつ埋める。** 幅だけを書けば高さは profile から
     来て、比が外れれば `PROFILE_ASPECT_MISMATCH` が言う。profile を渡さずに
     省いた spec の断り方（serde の「width が無い」で `SPEC_INVALID`）は
     1 文字も変えていない——埋めるのは生の JSON の段で、型は必須のままである
6. **`--fail-on` の長いヘルプの真偽の指標の行に `(しきい値を取らない)` を
   添えた。** 真偽の指標（`outside_safe_area`）が最後の行でなくなり、日本語で
   終わる行の次に字下げした行が来て、`kiri schema` が配る文面に日本語の直後の
   空白が並んだ（`the_published_prose_has_no_stray_spaces`）。結果 JSON では
   なく `--help` と schema の文面だけが変わる

## 12.7 この段でやらないこと

- 文字の縁取り・帯・図形レイヤ（次の段。`TEXT_ILLEGIBLE_AT_DISPLAY` の
  remedy に縁取りを書けるのはその後）
- `--preview`（次の段）
- YouTube など他のサービス（12.1）
- 自動折り返し・自動縮小（§10.9 のまま）

## 12.8 実装（2026-10-12）

| 作ったもの | 場所 |
|---|---|
| 規格の表（`ComposeProfile` / `Display`、行は note だけ） | `src/compose/profile.rs` |
| 縮小（box）と測り、`TEXT_ILLEGIBLE_AT_DISPLAY` の組み立て | `src/compose/display.rs` |
| canvas / safe_area を profile から埋める | `compose::parse_with` |
| 比の検査・容量の検査・表示ごとの測り | `src/commands/compose.rs` |
| `display_text_size` / `display_contrast` / `display_strokes_lost` | `src/compose/gate.rs` |
| `compose_profiles[]`・`layers[].display[]` の `fields[]` | `src/commands/schema.rs` |

### 受け入れ条件の実測

字体はリポジトリに置いていないので（§10.10）、テストはその機械のシステムの
字体を 1 つ借りる。下の数はこの機械（macOS）で借りた **Arial Unicode MS** の
もので、テストは字体に依らない関係だけを表明している。見出しは `ABAB` の 1 行。

| 条件 | 地 / 文字 | 原寸 `text_contrast` | `pc_list`（192） | `mobile_list`（144） |
|---|---|---|---|---|
| 4 | 白 / 黒 64px | 21.0（警告なし） | 9.6px・11.20・too_small | **7.2px**・6.69・too_small |
| 5 | 白 / 黒 110px | 21.0 | 16.5px・14.74・legible | 12.375px・14.16・legible |
| 6 | #767676 / 白 100px | **4.54**（警告なし） | 15.0px・**3.91**・low_contrast | 11.25px・**3.00**・low_contrast |

条件 6 は字体を変えても同じ向きに出る（手元で試した値、110px）。

| 字体 / weight | 原寸 | `pc_list` | `mobile_list` |
|---|---|---|---|
| Hiragino Sans 100 | 4.54 | 2.47・low_contrast | strokes_lost |
| Hiragino Sans 400 | 4.54 | 3.84・low_contrast | 3.41・low_contrast |
| Helvetica 400 | 4.54 | 3.99・low_contrast | 3.71・low_contrast |
| Arial 400 | 4.54 | 3.44・low_contrast | 3.67・low_contrast |

**条件 1 は main（6c380ec）の build と並べて確かめた。** 画像だけ・文字と画像・
透過の下地・はみ出し・decoration・後から覆う層を含む spec 3 本を、出力 4 形式
（png / jpg / webp / avif）× 追加の引数 5 通り（無し・`--dry-run`・
`--fail-on default`・`--flatten --background`・`--fail-on outside_safe_area,…`）の
60 通りで走らせ、`elapsed_ms` と出力パスを除いた結果 JSON・終了コード・人間向けの
出力・書いたファイルの md5 がすべて一致した。

