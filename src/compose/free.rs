//! `kiri compose --free-regions` — **文字を置ける空き矩形の候補を返す。**
//!
//! compose は置いて、測って、言う。言えるのは「置いた結果がまずい」ことだけで、
//! **どこへ置けばよかったか**は言わない。spec を書く側は商品の形を知らず、
//! `opaque_rect` は外接矩形にすぎない——瓶の肩の横や、斜めに置いたリモコンの
//! 脇のように、矩形の内側にも文字を置ける場所が大きく残る（14-phase-33.md §14.1）。
//!
//! **置くかどうかは spec が決める。** ここは候補を返すだけで、層を動かさない
//! （合成器であって組版器ではない、という §10 の線のまま）。
//!
//! # 何を避けるか
//!
//! `decoration` でない画像の**不透明度が 0 でない画素**と、全ての文字の
//! **`placed` の矩形**である（§14.2.2）。画像を外接矩形で数えると、瓶の肩の横の
//! ような空きが消える。文字を画素で数えると、行間へ別の文字を差し込む候補が
//! 出る。図形と `decoration` の画像は**文字を載せる下地**なので数えない。
//!
//! # 求め方
//!
//! 1. 障害物を原寸のビットマスクに持つ（`Obstacles`）
//! 2. `margin` だけ膨らませ、`CELL` px の格子へ**保守的に**畳む。セルを
//!    `margin` だけ広げた範囲に障害物の画素が 1 つでもあれば、そのセルは埋まって
//!    いる——セルごとに「膨らませたマスクに 1 画素でも掛かるか」を問うのと
//!    同じ答えで、それを横と縦に分けた 2 回の累積で出す（`Obstacles::free_cells`）
//! 3. 格子の上で**極大な空き矩形**を全部並べる（`maximal`。行ごとの
//!    ヒストグラムとスタックで O(セル数)）
//! 4. `min_size` より小さいものを捨て、面積の大きい順に並べ、**面積の
//!    `MAX_CONTAINED` 以上がすでに残した候補のどれかの中に入るもの**を捨て、
//!    `MAX_REGIONS` 個で止める
//!
//! 格子に畳むのは、極大矩形の数が輪郭のギザギザの数に比例して増えるからである。
//! 原寸のまま並べると、切り抜きの縁の 1 画素ごとに矩形が生まれる。
//!
//! # 格子は `within` の左上から引く
//!
//! 線は `within` の左上から `CELL` ごとに引き、**右端と下端だけは `within` の端
//! そのものを線にする**（最後のセルが `CELL` より狭くなる）。端数のセルを丸ごと
//! 埋まり扱いにすると、障害物が 1 つも無い spec で `within` そのものを返せない
//! （§14.4 の 2）。端数のセルも `within` の外の画素は 1 つも含まないので、
//! 「候補は `within` の内側にしか出ない」という保守性は崩れない。

use image::RgbaImage;
use serde::Serialize;

use crate::compose::display::MIN_DISPLAY_SIZE;
use crate::compose::measure::SUBPIXEL_TOLERANCE;
use crate::compose::profile::ComposeProfile;

/// 探索の格子の大きさ(px)。**変えられない。**
///
/// `margin` の既定（1280 幅で 24px）より十分小さく、1280x670 を 160x84 =
/// 13,440 セルに畳む。結果の `cell` に出すのは、候補の座標がこの倍数へ内向きに
/// 丸まっていることを呼ぶ側が知るためである。
pub const CELL: u32 = 8;

/// 返す候補の上限。
///
/// 1 枚の見出し画像に置く文字の塊は見出し・補足・価格程度で、それを超える候補は
/// 選ぶ手間を増やすだけである。
pub const MAX_REGIONS: usize = 8;

/// 面積のこの割合以上が、すでに残した候補の**どれか 1 つ**の中に入る候補は捨てる。
///
/// **「重なったら捨てる」にはしない。** L 字型の空きでは横長と縦長の 2 つの
/// 極大矩形が角で重なり、どちらにも別の使い道がある（横長は 1 行の見出し、
/// 縦長は箇条書き）。7 割を超えて既に返した 1 つの中に入るなら、それはその候補の
/// 言い換えにすぎない。
///
/// **残した候補の和では数えない。** 中央に正方形の商品を置くと、上下の帯は
/// 左右の帯と両端の角で重なる。和で数えると、帯の太さによっては下の帯が
/// 「7 割が左右の帯の中」として消え、4 つの帯が 3 つになる（§14.4 の 3）。
pub const MAX_CONTAINED: f64 = 0.7;

/// `margin` の既定を決める、1280 幅のときの距離(px)。
///
/// 文字が商品に触れて見えない距離として、note の 1280 幅で 24px を置く。
/// 幅に比例させるのは、同じ構図を 1920 幅で組んでも同じ見え方の余白になるため
/// である。
pub const MARGIN_AT_1280: f64 = 24.0;

/// `--profile` が無いときの `min_size` の高さ。キャンバスの短辺に対する百分率。
pub const MIN_HEIGHT_PERCENT_OF_SHORT_SIDE: u64 = 8;

/// 結果 JSON の `free_regions`。
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct FreeRegionsReport {
    /// 探した範囲 `[x, y, 幅, 高さ]`。`safe_area` があればそれをキャンバスへ
    /// 切り詰め、画素の格子へ内向きに丸めたもの。無ければキャンバス全体
    pub within: [u32; 4],
    /// 障害物から空けた距離(px)。候補と障害物の間には、少なくともこの数の
    /// 画素が挟まる
    pub margin: u32,
    /// これより小さい候補は捨てた `[幅, 高さ]`
    pub min_size: [u32; 2],
    /// 探索の格子(px)。`CELL`
    pub cell: u32,
    /// 面積の大きい順。同じなら `(y, x)` の小さい順、それも同じなら幅の小さい順
    /// （同じ左上・同じ面積の縦長と横長は、縦長が先）
    pub regions: Vec<FreeRegion>,
}

/// 空いている矩形の 1 つ。
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct FreeRegion {
    /// `[x, y, 幅, 高さ]`。中のどの画素も、障害物の画素から `margin` を超えて
    /// 離れている（チェビシェフ距離。正方形の近傍で見る）
    pub rect: [u32; 4],
    /// `within` に対する面積の比
    pub area_ratio: f64,
}

/// `margin` の既定。`round(canvas の幅 × 24 / 1280)`。
pub fn default_margin(canvas_width: u32) -> u32 {
    (f64::from(canvas_width) * MARGIN_AT_1280 / 1280.0).round() as u32
}

/// `--free-margin` の値を、実際に空ける画素数へ畳む。
///
/// **端数は切り上げる。** 23.5px を 23 にすると、書いた距離より近い候補を返す。
///
/// **キャンバスの辺より広い距離も切り詰めない。** そのまま空けた結果（障害物が
/// 1 つでもあれば候補は無く、無ければ `within` そのもの）を返し、結果の `margin` も
/// 渡した値のままである。値の上限は `cli::parse_free_margin` が
/// `compose::MAX_CANVAS_SIDE` で断るので、ここで `u32` へ飽和することは無い。
pub fn margin_from(px: f64) -> u32 {
    px.ceil() as u32
}

/// 候補の大きさの下限 `[幅, 高さ]`（§14.2.4）。
///
/// 高さは、`--profile` があれば**最も狭い表示で `MIN_DISPLAY_SIZE` px になる
/// 文字 1 行の高さ**（`11 × canvas の幅 / 表示幅` の切り上げ。note なら
/// 11 × 1280 / 144 = 97.8 → 98）、無ければキャンバスの短辺の 8% の切り上げで
/// ある。幅は高さの 2 倍——2 文字を下回る幅に見出しは入らない。
///
/// **整数で割る。** `0.08 × 短辺` を `f64` で切り上げると、ちょうど割り切れる
/// 短辺で 1 ulp の誤差が 1px 大きい下限に化ける。
pub fn min_size(canvas: (u32, u32), profile: Option<&ComposeProfile>) -> [u32; 2] {
    let (w, h) = (u64::from(canvas.0), u64::from(canvas.1));
    let narrowest = profile.and_then(|p| p.displays.iter().map(|d| d.width).min());
    let height = match narrowest {
        // `MIN_DISPLAY_SIZE` は整数の 11 である（`compose::display` の doc）
        Some(display) => (MIN_DISPLAY_SIZE as u64 * w).div_ceil(u64::from(display.max(1))),
        None => (w.min(h) * MIN_HEIGHT_PERCENT_OF_SHORT_SIDE).div_ceil(100),
    };
    let height = u32::try_from(height).unwrap_or(u32::MAX);
    [height.saturating_mul(2), height]
}

/// 探す範囲。`safe_area` があればそれ、無ければキャンバス全体。
///
/// **キャンバスへ切り詰め、画素の格子へ内向きに丸める。** キャンバスの外の候補は
/// 描けず、端数の画素を含めると `within` の外へ半画素はみ出した候補を返す。
/// 丸めは `SUBPIXEL_TOLERANCE` だけ見逃す——比で合わせた `safe_area` の
/// `108.000000001` を 109 へ切り上げると、端の 1 行を理由なく失う。
pub fn within(canvas: (u32, u32), safe_area: Option<[f64; 4]>) -> [u32; 4] {
    let Some([x, y, w, h]) = safe_area else {
        return [0, 0, canvas.0, canvas.1];
    };
    let inward = |start: f64, len: f64, limit: u32| -> (u32, u32) {
        let limit = f64::from(limit);
        let a = (start - SUBPIXEL_TOLERANCE).ceil().clamp(0.0, limit);
        let b = (start + len + SUBPIXEL_TOLERANCE).floor().clamp(0.0, limit);
        (a as u32, (b - a).max(0.0) as u32)
    };
    let (x0, width) = inward(x, w, canvas.0);
    let (y0, height) = inward(y, h, canvas.1);
    [x0, y0, width, height]
}

/// 障害物の原寸マスク。**`--free-regions` を渡した実行でしか作らない。**
///
/// **1 画素 1 ビットで持つ。** 上限の 16384 x 16384 でも 32MiB に収まる
/// （`bool` で持つと 256MiB、累積和を原寸で持つとさらに 1GiB を要る）。
#[derive(Debug, Clone)]
pub struct Obstacles {
    width: u32,
    height: u32,
    /// 1 行あたりの語の数
    words: usize,
    bits: Vec<u64>,
}

impl Obstacles {
    pub fn new(width: u32, height: u32) -> Self {
        let words = (width as usize).div_ceil(64);
        Obstacles {
            width,
            height,
            words,
            bits: vec![0; words * height as usize],
        }
    }

    fn set(&mut self, x: usize, y: usize) {
        self.bits[y * self.words + x / 64] |= 1 << (x % 64);
    }

    #[cfg(test)]
    fn get(&self, x: usize, y: usize) -> bool {
        self.bits[y * self.words + x / 64] >> (x % 64) & 1 == 1
    }

    /// 行 `y` の `[x0, x1)` を埋める。語の単位で塗る。
    fn set_span(&mut self, y: usize, x0: usize, x1: usize) {
        if x0 >= x1 {
            return;
        }
        let row = &mut self.bits[y * self.words..(y + 1) * self.words];
        let (first, last) = (x0 / 64, (x1 - 1) / 64);
        for (k, word) in row.iter_mut().enumerate().take(last + 1).skip(first) {
            let lo = if k == first { x0 % 64 } else { 0 };
            let hi = if k == last { (x1 - 1) % 64 + 1 } else { 64 };
            let mask = if hi - lo == 64 {
                u64::MAX
            } else {
                ((1u64 << (hi - lo)) - 1) << lo
            };
            *word |= mask;
        }
    }

    /// 当てはめた画像の、不透明度が 0 でない画素を障害物にする。
    ///
    /// `at` はキャンバス上の左上で、負でもよい。**キャンバスの外の画素は数えない**
    /// ——描かれない画素は見えず、避ける相手にならない。
    ///
    /// **しきい値は 0 である。** Lanczos で当てはめた絵は、縁の外に不透明度 1〜3 の
    /// 裾を数画素ぶん作る（`commands::compose::opaque_in_fitted` の doc）。目には
    /// 見えないが、ここではそれも避ける——候補が商品に近づく向きには倒れない。
    pub fn stamp(&mut self, src: &RgbaImage, at: (i64, i64)) {
        let (cw, ch) = (i64::from(self.width), i64::from(self.height));
        let x0 = at.0.max(0);
        let x1 = (at.0 + i64::from(src.width())).min(cw);
        let y0 = at.1.max(0);
        let y1 = (at.1 + i64::from(src.height())).min(ch);
        for cy in y0..y1 {
            let sy = (cy - at.1) as u32;
            for cx in x0..x1 {
                if src.get_pixel((cx - at.0) as u32, sy)[3] > 0 {
                    self.set(cx as usize, cy as usize);
                }
            }
        }
    }

    /// 矩形 `[x, y, 幅, 高さ]` を障害物にする。文字の `placed` に使う。
    ///
    /// **外向きに画素の格子へ丸める**（掛かった画素は全部埋まる）。ただし
    /// `SUBPIXEL_TOLERANCE` までの端数は見逃す——usvg の f32 から上がってくる
    /// `189.99998` で、1 行上の画素まで埋めない（`measure::overflow` と同じ扱い）。
    pub fn fill_rect(&mut self, rect: [f64; 4]) {
        let [x, y, w, h] = rect;
        if !rect.iter().all(|v| v.is_finite()) || w <= 0.0 || h <= 0.0 {
            return;
        }
        let outward = |start: f64, len: f64, limit: u32| -> (usize, usize) {
            let limit = f64::from(limit);
            let a = (start + SUBPIXEL_TOLERANCE).floor().clamp(0.0, limit);
            let b = (start + len - SUBPIXEL_TOLERANCE).ceil().clamp(0.0, limit);
            (a as usize, b as usize)
        };
        let (x0, x1) = outward(x, w, self.width);
        let (y0, y1) = outward(y, h, self.height);
        for row in y0..y1 {
            self.set_span(row, x0, x1);
        }
    }

    /// 格子のセルごとに、`margin` だけ膨らませた障害物が 1 画素も掛からないか。
    /// 並びは行優先（`j * nx + i`）。
    ///
    /// **膨らませたマスクを作らない。** 欲しいのは「セルを `margin` だけ広げた範囲
    /// `[xs[i] - m, xs[i + 1] + m) × [ys[j] - m, ys[j + 1] + m)` に障害物の画素が
    /// 1 つでもあるか」だけで、正方形の近傍は横と縦に分けられる。
    ///
    /// 1. 行ごとに、その行の累積（`w + 1` 個を使い回す）で、列のセルを横に広げた
    ///    範囲 `[xs[i] - m, xs[i + 1] + m)` に当たりがあるかを `hits` に書く
    /// 2. 列のセルごとに、`hits` の縦の累積（`h + 1` 個を使い回す）で、縦に広げた
    ///    範囲 `[ys[j] - m, ys[j + 1] + m)` に当たりがあるかを問う
    ///
    /// 抱える表は `hits`（列のセルの数 × 見る行の数の `bool`）だけで、上限の
    /// 16384 x 16384 でも 2048 x 16384 = 32MiB である。キャンバスの外は障害物の
    /// 無い画素として数える。
    fn free_cells(&self, xs: &[u32], ys: &[u32], margin: u32) -> Vec<bool> {
        let (w, h) = (self.width as usize, self.height as usize);
        let (nx, ny) = (xs.len() - 1, ys.len() - 1);
        let m = margin as usize;
        let lo = |v: u32| (v as usize).saturating_sub(m);
        let hi = |v: u32, limit: usize| (v as usize).saturating_add(m).min(limit);
        let spans: Vec<(usize, usize)> = (0..nx).map(|i| (lo(xs[i]), hi(xs[i + 1], w))).collect();
        // 見る行は、within を縦に margin だけ広げた範囲で足りる
        let (top, bottom) = (lo(ys[0]), hi(ys[ny], h));
        let rows = bottom - top;

        // 列のセルごとに縦へ連続させる（2 の手順で続けて読むため）
        let mut hits = vec![false; nx * rows];
        let mut prefix = vec![0u32; w + 1];
        for y in top..bottom {
            let row = &self.bits[y * self.words..(y + 1) * self.words];
            if row.iter().all(|&word| word == 0) {
                continue;
            }
            for x in 0..w {
                prefix[x + 1] = prefix[x] + (row[x / 64] >> (x % 64) & 1) as u32;
            }
            for (i, &(a, b)) in spans.iter().enumerate() {
                if prefix[b] > prefix[a] {
                    hits[i * rows + (y - top)] = true;
                }
            }
        }

        let mut free = vec![false; nx * ny];
        let mut column = vec![0u32; rows + 1];
        for i in 0..nx {
            for (k, &hit) in hits[i * rows..(i + 1) * rows].iter().enumerate() {
                column[k + 1] = column[k] + u32::from(hit);
            }
            for j in 0..ny {
                let (a, b) = (lo(ys[j]) - top, hi(ys[j + 1], h) - top);
                free[j * nx + i] = column[b] == column[a];
            }
        }
        free
    }
}

/// 格子の上の矩形。セルの番号で `[l, r) × [t, b)`。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct CellRect {
    l: usize,
    t: usize,
    r: usize,
    b: usize,
}

/// 空き矩形の候補を求める（モジュールの doc の「求め方」）。
///
/// 純粋な関数で、同じ入力からは並びまで同じ結果を返す。
pub fn find(
    obstacles: &Obstacles,
    within: [u32; 4],
    margin: u32,
    min_size: [u32; 2],
) -> Vec<FreeRegion> {
    let [wx, wy, ww, wh] = within;
    if ww == 0 || wh == 0 {
        return Vec::new();
    }
    let xs = lines(wx, ww);
    let ys = lines(wy, wh);
    let (nx, ny) = (xs.len() - 1, ys.len() - 1);

    let free = obstacles.free_cells(&xs, &ys, margin);

    let pixel =
        |c: &CellRect| -> [u32; 4] { [xs[c.l], ys[c.t], xs[c.r] - xs[c.l], ys[c.b] - ys[c.t]] };
    let mut candidates: Vec<[u32; 4]> = maximal(&free, nx, ny)
        .iter()
        .map(pixel)
        .filter(|[_, _, w, h]| *w >= min_size[0] && *h >= min_size[1])
        .collect();
    // 面積の大きい順、同じなら (y, x) の小さい順。**幅まで鍵に入れる**——同じ
    // 左上・同じ面積の矩形（横長と縦長）が並んだとき、列挙の順に依らせない
    candidates
        .sort_by_key(|[x, y, w, h]| (std::cmp::Reverse(u64::from(*w) * u64::from(*h)), *y, *x, *w));

    let within_area = u64::from(ww) * u64::from(wh);
    let mut kept: Vec<[u32; 4]> = Vec::new();
    for rect in candidates {
        if kept.len() == MAX_REGIONS {
            break;
        }
        let area = u64::from(rect[2]) * u64::from(rect[3]);
        let contained = kept
            .iter()
            .any(|k| intersection(k, &rect) as f64 >= MAX_CONTAINED * area as f64);
        if !contained {
            kept.push(rect);
        }
    }
    kept.into_iter()
        .map(|rect| FreeRegion {
            rect,
            area_ratio: (u64::from(rect[2]) * u64::from(rect[3])) as f64 / within_area as f64,
        })
        .collect()
}

/// `free_regions` の全体を組む。`find` に、探した条件をそのまま添える。
pub fn report(
    obstacles: &Obstacles,
    within: [u32; 4],
    margin: u32,
    min_size: [u32; 2],
) -> FreeRegionsReport {
    FreeRegionsReport {
        within,
        margin,
        min_size,
        cell: CELL,
        regions: find(obstacles, within, margin, min_size),
    }
}

/// 格子の線。`start` から `CELL` ごとに引き、最後は `start + len` で閉じる。
fn lines(start: u32, len: u32) -> Vec<u32> {
    let end = start + len;
    let mut out: Vec<u32> = (start..end).step_by(CELL as usize).collect();
    out.push(end);
    out
}

/// 極大な空き矩形を全部並べる。**どの向きにも 1 セルも広げられない矩形**である。
///
/// 行 `j` ごとに、各列の「`j` で終わる空きの高さ」を持ち、高さが単調に増える
/// スタックで歩く。列 `i` で高さ `top` の棒を下ろすとき、`[s, i)` × 高さ `top` は
///
/// - 左（列 `s - 1`）と右（列 `i`）は `top` より低いので広げられない
/// - 上は、`[s, i)` のどこかの列の高さがちょうど `top` なので広げられない
///
/// 下へ広げられるか（行 `j + 1` の `[s, i)` が全部空いているか）だけを別に見る。
/// 広げられるなら次の行でより高い矩形として出るので、ここでは出さない。棒を
/// 下ろすのは列ごとに高々 1 回なので、全体で O(セル数) である。
fn maximal(free: &[bool], nx: usize, ny: usize) -> Vec<CellRect> {
    let mut out = Vec::new();
    let mut heights = vec![0usize; nx];
    let mut stack: Vec<(usize, usize)> = Vec::with_capacity(nx);
    // 次の行の、埋まっているセルの数の累積
    let mut blocked = vec![0usize; nx + 1];
    for j in 0..ny {
        let row = &free[j * nx..(j + 1) * nx];
        for (h, &f) in heights.iter_mut().zip(row) {
            *h = if f { *h + 1 } else { 0 };
        }
        let below = (j + 1 < ny).then(|| &free[(j + 1) * nx..(j + 2) * nx]);
        if let Some(below) = below {
            for (i, &f) in below.iter().enumerate() {
                blocked[i + 1] = blocked[i] + usize::from(!f);
            }
        }
        stack.clear();
        for i in 0..=nx {
            let h = heights.get(i).copied().unwrap_or(0);
            let mut start = i;
            while let Some(&(s, top)) = stack.last() {
                if top < h {
                    break;
                }
                stack.pop();
                start = s;
                if top == h {
                    // 同じ高さは 1 本の棒にまとめる（同じ矩形を 2 度出さない）
                    continue;
                }
                let stuck = below.is_none() || blocked[i] > blocked[s];
                if stuck {
                    out.push(CellRect {
                        l: s,
                        t: j + 1 - top,
                        r: i,
                        b: j + 1,
                    });
                }
            }
            if h > 0 {
                stack.push((start, h));
            }
        }
    }
    out
}

/// 2 つの矩形が重なる画素の数。
fn intersection(a: &[u32; 4], b: &[u32; 4]) -> u64 {
    let span =
        |a0: u32, a1: u32, b0: u32, b1: u32| u64::from(a1.min(b1).saturating_sub(a0.max(b0)));
    span(a[0], a[0] + a[2], b[0], b[0] + b[2]) * span(a[1], a[1] + a[3], b[1], b[1] + b[3])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rects(regions: &[FreeRegion]) -> Vec<[u32; 4]> {
        regions.iter().map(|r| r.rect).collect()
    }

    /// 決定的な擬似乱数（xorshift）。性質テストの再現性のため固定シードで回す。
    struct Rng(u64);
    impl Rng {
        fn next(&mut self) -> u32 {
            let mut x = self.0;
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            self.0 = x;
            (x >> 32) as u32
        }
        fn below(&mut self, n: u32) -> u32 {
            self.next() % n.max(1)
        }
    }

    /// 障害物が無ければ、`within` そのものが 1 つだけ返る（§14.4 の 2）。
    /// **`within` の端が `CELL` の倍数でなくても**削らない。
    #[test]
    fn with_no_obstacle_the_whole_range_is_the_only_region() {
        let obstacles = Obstacles::new(1280, 670);
        let within = [0, 108, 1280, 454];
        let regions = find(&obstacles, within, 24, [196, 98]);
        assert_eq!(rects(&regions), [within]);
        assert_eq!(regions[0].area_ratio, 1.0);

        // 左上も端数のとき
        let within = [3, 5, 101, 77];
        let regions = find(&Obstacles::new(120, 90), within, 4, [10, 5]);
        assert_eq!(rects(&regions), [within]);
    }

    /// 中央の正方形の周りに、左右の帯と上下の帯の 4 つが返る（§14.4 の 3）。
    #[test]
    fn a_square_in_the_middle_leaves_four_bands() {
        let mut obstacles = Obstacles::new(400, 400);
        obstacles.fill_rect([160.0, 160.0, 80.0, 80.0]);
        let margin = 16;
        let regions = find(&obstacles, [0, 0, 400, 400], margin, [32, 16]);
        assert_eq!(regions.len(), 4, "{regions:?}");
        for r in &regions {
            let [x, y, w, h] = r.rect;
            let apart = x + w + margin <= 160
                || x >= 240 + margin
                || y + h + margin <= 160
                || y >= 240 + margin;
            assert!(apart, "{:?} が正方形から {margin}px 離れていない", r.rect);
        }
        // 上の帯は全幅で、正方形の上端から margin を空けて 8 の倍数へ内向きに丸めた位置で止まる
        assert!(rects(&regions).contains(&[0, 0, 400, 144]), "{regions:?}");
    }

    /// 外接矩形の右上が透明な L 字では、外接矩形の内側にかかる候補が出る
    /// （§14.4 の 4）。外接矩形だけを障害物にすると、その候補は出ない。
    #[test]
    fn an_l_shape_leaves_room_inside_its_bounding_box() {
        let (w, h) = (400u32, 400u32);
        // 縦棒 [100, 100, 60, 200] と横棒 [100, 240, 200, 60]。外接矩形は [100, 100, 200, 200]
        let mut image = RgbaImage::new(200, 200);
        for (x, y, p) in image.enumerate_pixels_mut() {
            if x < 60 || y >= 140 {
                *p = image::Rgba([0, 0, 0, 255]);
            }
        }
        let mut shaped = Obstacles::new(w, h);
        shaped.stamp(&image, (100, 100));
        let mut boxed = Obstacles::new(w, h);
        boxed.fill_rect([100.0, 100.0, 200.0, 200.0]);

        let min = [64, 32];
        let by_shape = find(&shaped, [0, 0, w, h], 16, min);
        let by_box = find(&boxed, [0, 0, w, h], 16, min);
        let intrudes = |r: &[u32; 4]| {
            let [x, y, rw, rh] = *r;
            x < 300 && x + rw > 100 && y < 300 && y + rh > 100
        };
        let inside: Vec<_> = rects(&by_shape).into_iter().filter(intrudes).collect();
        assert!(
            !inside.is_empty(),
            "外接矩形の内側に候補が無い: {by_shape:?}"
        );
        assert!(
            rects(&by_box).iter().all(|r| !intrudes(r)),
            "外接矩形で数えると内側には出ない: {by_box:?}"
        );
        for r in &inside {
            assert!(!by_box.iter().any(|b| b.rect == *r));
        }
    }

    /// どの候補の、どの画素も、障害物の画素からチェビシェフ距離で `margin` を
    /// 超えて離れている（§14.4 の 6）。原寸のマスクを総当たりで見る。
    ///
    /// 併せて、候補が `within` の内側にあること、`min_size` 以上であること、
    /// 左上が `within` の左上から `CELL` の倍数にあること、並びが面積の降順で
    /// あることも見る。
    #[test]
    fn every_pixel_of_every_region_keeps_the_margin() {
        let mut rng = Rng(0x2545_F491_4F6C_DD1D);
        let mut checked = 0;
        for case in 0..60 {
            let (w, h) = (40 + rng.below(140), 40 + rng.below(120));
            let mut obstacles = Obstacles::new(w, h);
            for _ in 0..rng.below(6) {
                let rect = [
                    f64::from(rng.below(w)) - 5.5,
                    f64::from(rng.below(h)) - 3.25,
                    f64::from(1 + rng.below(30)) + 0.4,
                    f64::from(1 + rng.below(30)),
                ];
                obstacles.fill_rect(rect);
            }
            for _ in 0..rng.below(40) {
                let (x, y) = (rng.below(w) as usize, rng.below(h) as usize);
                obstacles.set(x, y);
            }
            let wx = rng.below(w / 4);
            let wy = rng.below(h / 4);
            let within = [wx, wy, w - wx - rng.below(w / 4), h - wy - rng.below(h / 4)];
            let margin = rng.below(12);
            let min = [rng.below(20), rng.below(10)];
            let regions = find(&obstacles, within, margin, min);
            assert!(regions.len() <= MAX_REGIONS);

            let m = i64::from(margin);
            for r in &regions {
                let [x, y, rw, rh] = r.rect;
                assert!(rw >= min[0] && rh >= min[1], "case {case}: {r:?}");
                assert!(x >= within[0] && y >= within[1], "case {case}: {r:?}");
                assert!(x + rw <= within[0] + within[2], "case {case}: {r:?}");
                assert!(y + rh <= within[1] + within[3], "case {case}: {r:?}");
                assert_eq!((x - within[0]) % CELL, 0, "case {case}: {r:?}");
                assert_eq!((y - within[1]) % CELL, 0, "case {case}: {r:?}");
                for py in i64::from(y)..i64::from(y + rh) {
                    for px in i64::from(x)..i64::from(x + rw) {
                        for oy in (py - m).max(0)..(py + m + 1).min(i64::from(h)) {
                            for ox in (px - m).max(0)..(px + m + 1).min(i64::from(w)) {
                                let hit = obstacles.get(ox as usize, oy as usize);
                                assert!(
                                    !hit,
                                    "case {case}: {r:?} の ({px}, {py}) が障害物 ({ox}, {oy}) \
                                     から {margin}px 以内"
                                );
                            }
                        }
                    }
                }
            }
            let areas: Vec<u64> = regions
                .iter()
                .map(|r| u64::from(r.rect[2]) * u64::from(r.rect[3]))
                .collect();
            assert!(areas.windows(2).all(|p| p[0] >= p[1]), "case {case}");
            // 決定性
            assert_eq!(regions, find(&obstacles, within, margin, min));
            checked += regions.len();
        }
        // 候補が 1 つも出ない入力ばかりでは、何も確かめていない
        assert!(checked > 100, "確かめた候補が {checked} 個しか無い");
    }

    /// セルの空き判定は、セルを margin だけ広げた範囲を原寸のマスクで総当たり
    /// した答えと一致する（保守的すぎもしない）。
    #[test]
    fn free_cells_match_a_brute_force_dilation() {
        let mut rng = Rng(0xD1B5_4A32_D192_ED03);
        for case in 0..80 {
            let (w, h) = (1 + rng.below(150), 1 + rng.below(110));
            let mut obstacles = Obstacles::new(w, h);
            for _ in 0..rng.below(30) {
                obstacles.set(rng.below(w) as usize, rng.below(h) as usize);
            }
            if rng.below(2) == 0 {
                obstacles.fill_rect([
                    f64::from(rng.below(w)) - 3.0,
                    f64::from(rng.below(h)),
                    f64::from(rng.below(90)) + 0.5,
                    f64::from(1 + rng.below(9)),
                ]);
            }
            let (wx, wy) = (rng.below(w), rng.below(h));
            let (ww, wh) = (1 + rng.below(w - wx), 1 + rng.below(h - wy));
            let (xs, ys) = (lines(wx, ww), lines(wy, wh));
            let margin = rng.below(20);
            let free = obstacles.free_cells(&xs, &ys, margin);
            let m = i64::from(margin);
            for j in 0..ys.len() - 1 {
                for i in 0..xs.len() - 1 {
                    let y0 = (i64::from(ys[j]) - m).max(0);
                    let y1 = (i64::from(ys[j + 1]) + m).min(i64::from(h));
                    let x0 = (i64::from(xs[i]) - m).max(0);
                    let x1 = (i64::from(xs[i + 1]) + m).min(i64::from(w));
                    let hit =
                        (y0..y1).any(|y| (x0..x1).any(|x| obstacles.get(x as usize, y as usize)));
                    assert_eq!(
                        free[j * (xs.len() - 1) + i],
                        !hit,
                        "case {case}: cell ({i}, {j})"
                    );
                }
            }
        }
    }

    /// 語の単位で塗った範囲は、1 画素ずつ塗った範囲と一致する。
    #[test]
    fn a_span_sets_exactly_its_bits() {
        let mut rng = Rng(0x0123_4567_89AB_CDEF);
        for _ in 0..300 {
            let w = 1 + rng.below(300);
            let (a, b) = (rng.below(w + 1) as usize, rng.below(w + 1) as usize);
            let (x0, x1) = (a.min(b), a.max(b));
            let mut by_span = Obstacles::new(w, 2);
            by_span.set_span(1, x0, x1);
            let mut by_bit = Obstacles::new(w, 2);
            (x0..x1).for_each(|x| by_bit.set(x, 1));
            assert_eq!(by_span.bits, by_bit.bits, "w {w}: [{x0}, {x1})");
        }
    }

    /// 極大矩形の列挙は、総当たりで見つけた極大矩形と一致する。
    #[test]
    fn maximal_rectangles_match_a_brute_force_search() {
        let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
        for _ in 0..200 {
            let (nx, ny) = (1 + rng.below(7) as usize, 1 + rng.below(6) as usize);
            let free: Vec<bool> = (0..nx * ny).map(|_| rng.below(4) != 0).collect();
            let all_free = |c: &CellRect| (c.t..c.b).all(|j| (c.l..c.r).all(|i| free[j * nx + i]));
            let mut expected = Vec::new();
            for t in 0..ny {
                for b in t + 1..=ny {
                    for l in 0..nx {
                        for r in l + 1..=nx {
                            let c = CellRect { l, t, r, b };
                            if !all_free(&c) {
                                continue;
                            }
                            let grow = [
                                (l > 0).then(|| CellRect { l: l - 1, ..c }),
                                (t > 0).then(|| CellRect { t: t - 1, ..c }),
                                (r < nx).then(|| CellRect { r: r + 1, ..c }),
                                (b < ny).then(|| CellRect { b: b + 1, ..c }),
                            ];
                            if grow.iter().flatten().all(|g| !all_free(g)) {
                                expected.push(c);
                            }
                        }
                    }
                }
            }
            let key = |c: &CellRect| (c.t, c.l, c.b, c.r);
            let mut got = maximal(&free, nx, ny);
            got.sort_by_key(key);
            expected.sort_by_key(key);
            assert_eq!(got, expected, "{free:?} ({nx}x{ny})");
        }
    }

    /// L 字の空きでは、角で重なる横長と縦長の両方を返す。重なっても、7 割が
    /// 既に返した候補に入らなければ捨てない。
    #[test]
    fn overlapping_bands_of_an_l_shaped_gap_are_both_kept() {
        // 右下 [80, 80, 120, 120] だけが障害物 → 空きは上の帯と左の帯の L 字。
        // 面積も左上も同じなので、幅の狭い縦長が先に来る
        let mut obstacles = Obstacles::new(200, 200);
        obstacles.fill_rect([80.0, 80.0, 120.0, 120.0]);
        let regions = find(&obstacles, [0, 0, 200, 200], 0, [8, 8]);
        assert_eq!(rects(&regions), [[0, 0, 80, 200], [0, 0, 200, 80]]);
    }

    /// 既に残した候補の中に 7 割以上入るものは捨てる。
    #[test]
    fn a_region_mostly_inside_a_kept_one_is_dropped() {
        // 上端の中央に 1 セルの障害物 → 極大矩形は下の大きな 1 つと、障害物の
        // 左右の縦長 2 つ。縦長は 96% が大きな 1 つの中に入るので出ない
        let mut obstacles = Obstacles::new(200, 200);
        obstacles.fill_rect([96.0, 0.0, 8.0, 8.0]);
        let regions = find(&obstacles, [0, 0, 200, 200], 0, [8, 8]);
        assert_eq!(rects(&regions), [[0, 8, 200, 192]]);
    }

    /// 並びは面積の降順、同じ面積なら (y, x) の小さい順。
    #[test]
    fn ties_are_ordered_by_y_then_x() {
        // 中央の縦棒で左右に同じ大きさの空きを作る
        let mut obstacles = Obstacles::new(160, 80);
        obstacles.fill_rect([72.0, 0.0, 16.0, 80.0]);
        let regions = find(&obstacles, [0, 0, 160, 80], 0, [8, 8]);
        assert_eq!(rects(&regions), [[0, 0, 72, 80], [88, 0, 72, 80]]);
    }

    /// 候補は `MAX_REGIONS` 個で止まる。
    #[test]
    fn at_most_eight_regions_are_returned() {
        let mut obstacles = Obstacles::new(400, 40);
        for k in 0..20 {
            obstacles.fill_rect([f64::from(k * 20 + 16), 0.0, 4.0, 40.0]);
        }
        let regions = find(&obstacles, [0, 0, 400, 40], 0, [8, 8]);
        assert_eq!(regions.len(), MAX_REGIONS);
    }

    #[test]
    fn the_defaults_follow_the_canvas() {
        assert_eq!(default_margin(1280), 24);
        assert_eq!(default_margin(1920), 36);
        assert_eq!(default_margin(640), 12);
        let note = crate::compose::profile::named("note").unwrap();
        assert_eq!(min_size((1280, 670), Some(note)), [196, 98]);
        // 短辺の 8%（切り上げ）
        assert_eq!(min_size((1280, 670), None), [108, 54]);
        assert_eq!(min_size((400, 400), None), [64, 32]);
        assert_eq!(margin_from(23.2), 24);
        assert_eq!(margin_from(0.0), 0);
        assert_eq!(margin_from(1e300), u32::MAX);
    }

    #[test]
    fn the_range_is_clipped_and_rounded_inward() {
        assert_eq!(within((1280, 670), None), [0, 0, 1280, 670]);
        assert_eq!(
            within((1280, 670), Some([0.0, 108.0, 1280.0, 454.0])),
            [0, 108, 1280, 454]
        );
        assert_eq!(
            within((100, 100), Some([10.5, -20.0, 200.0, 50.25])),
            [11, 0, 89, 30]
        );
        // f32 から来た 1 ulp の誤差で端を失わない
        assert_eq!(
            within(
                (1280, 670),
                Some([0.0, 107.999_999_9, 1280.0, 454.000_000_2])
            ),
            [0, 108, 1280, 454]
        );
        assert_eq!(within((100, 100), Some([200.0, 0.0, 10.0, 10.0]))[2], 0);
    }

    #[test]
    fn a_text_rect_is_filled_outward_without_the_f32_noise() {
        let mut obstacles = Obstacles::new(20, 20);
        obstacles.fill_rect([2.5, 3.999_98, 3.0, 2.000_01]);
        let filled: Vec<(usize, usize)> = (0..400)
            .filter(|&k| obstacles.get(k % 20, k / 20))
            .map(|k| (k % 20, k / 20))
            .collect();
        let expected: Vec<(usize, usize)> =
            (4..6).flat_map(|y| (2..6).map(move |x| (x, y))).collect();
        assert_eq!(filled, expected);
    }
}
