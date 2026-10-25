//! 警告が「どこ」を言うための局所の集計。
//!
//! `halo_ratio` / `contour_roughness` / `rim_contamination` は画像全体で 1 つの
//! 数なので、`CONTOUR_ROUGH` が出てもエージェントには**どこが悪いのか**が
//! 分からない。1 周の輪郭のうち右下の 1 辺だけが布の織り目を拾っているのか、
//! 全周が均して悪いのかで、次の一手（`--bg-polygon` で指す、`--trimap` を
//! 作る範囲を絞る）が変わる。
//!
//! # 同じ指標を格子セルごとに数え直す
//!
//! 3 つの指標はどれも「画素ごとの分子 / 画素ごとの分母」の形をしている
//! （縁の画素のうち背景色のままのもの、輪郭画素ごとの参照からの距離、帯の
//! 画素のうち背景寄りのもの）。**全体の値を出すのと同じ走査の中で、分子と分母を
//! 画素の属するセルへも足す。** 新しい物差しを作らないので、局所の値は
//! 全体の値と同じ定義で、しきい値もそのまま当てられる——セルの値を全セルで
//! 分母の重みつき平均にすると、全体の値と 1 ビットも違わない。
//!
//! 足し込みはどれも整数で行う。局所の値も全体の値も、並列の分け方や走査の
//! 順で動かない。
//!
//! # 区間はセルを束ねて作る
//!
//! しきい値を越えたセル（標本の足りないセルは除く）を、悪い順に種として
//! 8 近傍で束ねる。**束ねる大きさに上限を置く**（縦横 `REGION_SPAN` セル）——
//! 輪郭が全周で悪いと、上限なしの連結は商品の外接矩形 1 つになって、
//! 「全体が悪い」と言うのと変わらなくなる。上限があれば、全周が悪くても
//! いちばん悪い 5 か所を名指しできる。

/// 局所の集計を取る格子の一辺(px, 長辺 1000px 換算)。
///
/// 64px は納品寸法（長辺 1000px 前後）で商品の 1 辺の 1/5〜1/10 にあたる。
/// これより細かいと 1 セルあたりの輪郭画素が減って値が揺れ（輪郭が 1 本
/// 横切るだけのセルでは 64 × scale 画素しか無い）、粗いと「右下の角」と
/// 「下辺の右半分」を言い分けられなくなる。
///
/// 実写のリモコン（24.5MP、`--optimize`）で振った。欠陥は目視で上辺・左上の角・
/// 右辺・右下の角にあり、どこまでを名指せるかで比べる。
///
/// | 一辺 | `CONTOUR_ROUGH` | `RIM_CONTAMINATED` |
/// |---|---|---|
/// | 32 | 5 個とも上辺 | 5 個とも上辺（右下の角は 6 番目以降へ落ちる） |
/// | **64（採用）** | 上辺 3 個 + 左上の角 | 左上の角・上辺 2 個・右下の角・右辺 |
/// | 128 | 上辺の左半分と右半分の 2 個 | 2 個目が上辺の右半分から右下の角まで（商品の半分） |
///
/// **細かすぎると上限の 5 個が 1 辺に吸われる。** 32 ではセルが 183px と短く、
/// 上辺だけでセルが 20 以上しきい値を越えるので、悪い順の上位がそこで埋まる。
pub const CELL_AT_1000: f64 = 64.0;

/// 1 つの区間が広がってよいセルの数（縦横それぞれ）。
///
/// 3 セルは換算 192px、納品寸法で商品の 1 辺の半分ほどである。
pub const REGION_SPAN: u32 = 3;

/// 返す区間の上限。
pub const MAX_REGIONS: usize = 5;

/// 1 セルぶんの分子と分母。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Tally {
    /// 分子（指標ごとに意味が違う。`Measure` を参照）
    pub num: u64,
    /// 分母
    pub den: u64,
    /// 調べた画素の数。`rim_contamination` だけが分母と違う値を持つ
    /// （判定できなかった画素は分母に入らないが、ここには入る）
    pub seen: u64,
    /// 調べた画素の外接矩形
    pub x0: u32,
    pub y0: u32,
    pub x1: u32,
    pub y1: u32,
}

impl Default for Tally {
    fn default() -> Self {
        Self {
            num: 0,
            den: 0,
            seen: 0,
            x0: u32::MAX,
            y0: u32::MAX,
            x1: 0,
            y1: 0,
        }
    }
}

impl Tally {
    /// 画素を 1 つ見たことを記す。分子と分母は呼び出し側が足す。
    #[inline]
    pub fn see(&mut self, x: u32, y: u32) {
        self.seen += 1;
        self.x0 = self.x0.min(x);
        self.y0 = self.y0.min(y);
        self.x1 = self.x1.max(x);
        self.y1 = self.y1.max(y);
    }

    fn merge(&mut self, other: &Tally) {
        self.num += other.num;
        self.den += other.den;
        self.seen += other.seen;
        self.x0 = self.x0.min(other.x0);
        self.y0 = self.y0.min(other.y0);
        self.x1 = self.x1.max(other.x1);
        self.y1 = self.y1.max(other.y1);
    }
}

/// 画像を覆う格子。セルの一辺は長辺 1000px 換算で `CELL_AT_1000`。
///
/// `Default` は 0 個のセルを持つ格子で、何も測らなかった集計に使う。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Grid {
    pub width: u32,
    pub height: u32,
    /// セルの一辺(px)
    pub cell: u32,
    pub cols: u32,
    pub rows: u32,
}

impl Grid {
    pub fn new(width: u32, height: u32, scale: f64) -> Self {
        let cell = ((CELL_AT_1000 * scale).ceil() as u32).max(1);
        Self {
            width,
            height,
            cell,
            cols: width.div_ceil(cell).max(1),
            rows: height.div_ceil(cell).max(1),
        }
    }

    /// 空の集計（セルの数だけ）。
    pub fn tallies(&self) -> Vec<Tally> {
        vec![Tally::default(); (self.cols as usize) * (self.rows as usize)]
    }

    #[inline]
    pub fn index(&self, x: u32, y: u32) -> usize {
        ((y / self.cell) as usize) * (self.cols as usize) + (x / self.cell) as usize
    }
}

/// どの指標の集計か。セルの値の出し方と、標本が足りるかの基準が違う。
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Measure {
    /// 分子は背景色のままの画素、分母は境界近傍の前景画素
    Halo,
    /// 分子は輪郭画素ごとの参照からの距離（チャンファー単位）の和、分母は輪郭画素の数。
    /// 値は `num / den / step / scale`
    Roughness { step: f64, scale: f64 },
    /// 分子は背景寄りの画素、分母は判定できた画素、`seen` は帯の画素
    Rim,
}

impl Measure {
    /// セル（または束ねたセル）の値。測れなければ None。
    ///
    /// `Rim` は全体の値と同じく、**帯の半分以上で判定できなければ値を出さない**。
    fn value(self, t: &Tally) -> Option<f64> {
        if t.den == 0 {
            return None;
        }
        let ratio = t.num as f64 / t.den as f64;
        match self {
            Measure::Halo => Some(ratio),
            Measure::Roughness { step, scale } => Some(ratio / step / scale),
            Measure::Rim => (t.den * 2 >= t.seen).then_some(ratio),
        }
    }

    /// 値を信じてよいだけの画素がセルにあるか。
    ///
    /// 輪郭がセルを 1 本横切れば、輪郭画素はおよそセルの一辺ぶん、縁の帯の
    /// 画素はその数倍ある。角をかすめただけのセルは数画素しか持たず、
    /// 1 画素の蛇行で値が跳ねるので種にしない。
    fn enough(self, t: &Tally, cell: u32) -> bool {
        let side = u64::from(cell);
        match self {
            Measure::Halo => t.den >= side,
            Measure::Roughness { .. } | Measure::Rim => t.den * 2 >= side,
        }
    }
}

/// 名指しする 1 区間。座標は集計した画像の画素（`[x1, y1, x2, y2]`、両端を含む）。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Region {
    pub bbox: (u32, u32, u32, u32),
    /// 区間の中で測り直した同じ指標の値
    pub score: f64,
}

/// しきい値を越えたセルを束ね、悪い順に最大 `MAX_REGIONS` 個を返す。
///
/// `pad` は区間の矩形を外へ広げる幅(px)。集計した画素の外接矩形は輪郭の
/// 片側にしか届かないことがあるので、遷移の帯がまるごと入るよう広げる。
///
/// **決定的である。** 種の順は（値の降順、セルの添字の昇順）、束ねる順は
/// 幅優先で近傍を固定の順に見る。返す順も（値の降順、矩形の上端、左端）で、
/// 同じ集計からは必ず同じ並びが出る。
pub fn regions(
    grid: &Grid,
    tallies: &[Tally],
    measure: Measure,
    threshold: f64,
    pad: u32,
) -> Vec<Region> {
    let (cols, rows) = (grid.cols as usize, grid.rows as usize);
    if tallies.len() != cols * rows {
        return Vec::new();
    }
    // しきい値を越え、かつ標本の足りるセル
    let hot: Vec<Option<f64>> = tallies
        .iter()
        .map(|t| {
            measure
                .value(t)
                .filter(|&v| v > threshold && measure.enough(t, grid.cell))
        })
        .collect();
    let mut seeds: Vec<usize> = (0..hot.len()).filter(|&i| hot[i].is_some()).collect();
    seeds.sort_by(|&a, &b| hot[b].unwrap().total_cmp(&hot[a].unwrap()).then(a.cmp(&b)));

    let mut taken = vec![false; hot.len()];
    let mut found = Vec::new();
    for seed in seeds {
        if taken[seed] {
            continue;
        }
        taken[seed] = true;
        let (sx, sy) = (seed % cols, seed / cols);
        // 束ねたセルの範囲（セルの添字）。縦横とも `REGION_SPAN` を越えない
        let (mut cx0, mut cy0, mut cx1, mut cy1) = (sx, sy, sx, sy);
        let mut sum = tallies[seed];
        let mut queue = std::collections::VecDeque::from([seed]);
        while let Some(at) = queue.pop_front() {
            let (ax, ay) = ((at % cols) as i64, (at / cols) as i64);
            for (dx, dy) in [
                (-1i64, -1i64),
                (0, -1),
                (1, -1),
                (-1, 0),
                (1, 0),
                (-1, 1),
                (0, 1),
                (1, 1),
            ] {
                let (nx, ny) = (ax + dx, ay + dy);
                if nx < 0 || ny < 0 || nx >= cols as i64 || ny >= rows as i64 {
                    continue;
                }
                let (nx, ny) = (nx as usize, ny as usize);
                let n = ny * cols + nx;
                if taken[n] || hot[n].is_none() {
                    continue;
                }
                let span = REGION_SPAN as usize;
                if cx1.max(nx) - cx0.min(nx) >= span || cy1.max(ny) - cy0.min(ny) >= span {
                    continue;
                }
                taken[n] = true;
                cx0 = cx0.min(nx);
                cy0 = cy0.min(ny);
                cx1 = cx1.max(nx);
                cy1 = cy1.max(ny);
                sum.merge(&tallies[n]);
                queue.push_back(n);
            }
        }
        // 束ねたセルはどれもしきい値を越えているので、重みつき平均も越える
        let Some(score) = measure.value(&sum) else {
            continue;
        };
        found.push(Region {
            bbox: (
                sum.x0.saturating_sub(pad),
                sum.y0.saturating_sub(pad),
                (sum.x1 + pad).min(grid.width - 1),
                (sum.y1 + pad).min(grid.height - 1),
            ),
            score,
        });
    }
    found.sort_by(|a, b| {
        b.score
            .total_cmp(&a.score)
            .then(a.bbox.1.cmp(&b.bbox.1))
            .then(a.bbox.0.cmp(&b.bbox.0))
    });
    found.truncate(MAX_REGIONS);
    found
}

/// 区間の矩形を `--normalized` で渡せる形へ。
///
/// **外側へ丸める。** 始点は小数第 4 位で切り捨て、終点は切り上げる。
/// `--bbox --normalized` へ戻すと元の矩形を必ず含む（`bbox_to_pixels` が
/// 外側へ丸めるのと同じ向きで、内側へ丸めた 1px はそのまま指し漏れになる）。
/// 終点は `(x2 + 1) / 幅`——画素の右端であり、`subject.normalized_bbox` と
/// 同じ規約である。
pub fn normalized(bbox: (u32, u32, u32, u32), width: u32, height: u32) -> [f64; 4] {
    let floor4 = |v: f64| (v * 10_000.0).floor() / 10_000.0;
    let ceil4 = |v: f64| ((v * 10_000.0).ceil() / 10_000.0).min(1.0);
    let (w, h) = (f64::from(width), f64::from(height));
    [
        floor4(f64::from(bbox.0) / w),
        floor4(f64::from(bbox.1) / h),
        ceil4(f64::from(bbox.2 + 1) / w),
        ceil4(f64::from(bbox.3 + 1) / h),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn grid() -> Grid {
        // 640x640 を 64px のセルで 10x10
        Grid::new(640, 640, 1.0)
    }

    fn cell(num: u64, den: u64, x0: u32, y0: u32) -> Tally {
        Tally {
            num,
            den,
            seen: den,
            x0,
            y0,
            x1: x0 + 63,
            y1: y0 + 63,
        }
    }

    #[test]
    fn the_grid_follows_the_scale() {
        let g = Grid::new(5712, 4284, 5.712);
        assert_eq!(g.cell, 366);
        assert_eq!((g.cols, g.rows), (16, 12));
        assert_eq!(g.index(5711, 4283), 11 * 16 + 15);
    }

    #[test]
    fn nothing_hot_means_no_region() {
        let g = grid();
        let mut t = g.tallies();
        t[0] = cell(1, 100, 0, 0);
        assert!(regions(&g, &t, Measure::Halo, 0.1, 0).is_empty());
    }

    #[test]
    fn neighbouring_hot_cells_are_bundled_and_scored_together() {
        let g = grid();
        let mut t = g.tallies();
        t[g.index(64, 64)] = cell(50, 100, 64, 64);
        t[g.index(128, 64)] = cell(30, 100, 128, 64);
        let r = regions(&g, &t, Measure::Halo, 0.1, 0);
        assert_eq!(r.len(), 1, "{r:?}");
        assert_eq!(r[0].bbox, (64, 64, 191, 127));
        assert!((r[0].score - 0.4).abs() < 1e-12, "{r:?}");
    }

    /// 全周が悪くても、上限なしに 1 つへ束ねない。
    #[test]
    fn a_long_bad_edge_is_cut_into_bounded_regions() {
        let g = grid();
        let mut t = g.tallies();
        for cx in 0..10u32 {
            // 右へ行くほど悪い
            t[g.index(cx * 64, 0)] = cell(20 + u64::from(cx), 100, cx * 64, 0);
        }
        let r = regions(&g, &t, Measure::Halo, 0.1, 0);
        assert!(r.len() >= 3, "{r:?}");
        for region in &r {
            assert!(region.bbox.2 - region.bbox.0 < 64 * REGION_SPAN, "{r:?}");
        }
        // 悪い順。最初の区間はいちばん右の端を含む
        assert_eq!(r[0].bbox.2, 639, "{r:?}");
        assert!(r.windows(2).all(|p| p[0].score >= p[1].score), "{r:?}");
    }

    #[test]
    fn at_most_five_regions_are_returned() {
        let g = grid();
        let mut t = g.tallies();
        // 離れたセルを 8 つ
        for i in 0..8u32 {
            let (cx, cy) = ((i % 4) * 2, (i / 4) * 4);
            t[g.index(cx * 64, cy * 64)] = cell(50, 100, cx * 64, cy * 64);
        }
        assert_eq!(regions(&g, &t, Measure::Halo, 0.1, 0).len(), MAX_REGIONS);
    }

    #[test]
    fn a_cell_with_too_few_samples_is_not_named() {
        let g = grid();
        let mut t = g.tallies();
        // 5 画素のうち 5 画素が背景色——値は 1.0 だが、角をかすめただけ
        t[0] = cell(5, 5, 0, 0);
        assert!(regions(&g, &t, Measure::Halo, 0.1, 0).is_empty());
    }

    #[test]
    fn rim_needs_half_the_band_decided_like_the_whole_value() {
        let g = grid();
        let mut t = g.tallies();
        let mut c = cell(40, 100, 0, 0);
        c.seen = 300;
        t[0] = c;
        assert!(regions(&g, &t, Measure::Rim, 0.02, 0).is_empty());
        t[0].seen = 200;
        assert_eq!(regions(&g, &t, Measure::Rim, 0.02, 0).len(), 1);
    }

    #[test]
    fn the_pad_stays_inside_the_image() {
        let g = grid();
        let mut t = g.tallies();
        t[0] = cell(50, 100, 0, 0);
        let r = regions(&g, &t, Measure::Halo, 0.1, 10);
        assert_eq!(r[0].bbox, (0, 0, 73, 73));
    }

    #[test]
    fn normalised_coordinates_round_outward() {
        let n = normalized((1234, 2000, 3000, 4000), 4284, 5712);
        assert!(n[0] * 4284.0 <= 1234.0 && n[1] * 5712.0 <= 2000.0, "{n:?}");
        assert!(n[2] * 4284.0 >= 3001.0 && n[3] * 5712.0 >= 4001.0, "{n:?}");
        assert_eq!(normalized((0, 0, 99, 49), 100, 50), [0.0, 0.0, 1.0, 1.0]);
    }
}
