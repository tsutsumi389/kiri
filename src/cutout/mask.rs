//! 前景マスクとその統計。
//!
//! 統計値は AI エージェントが切り抜きの成否を判定するために使う。
//! 画像を見なくても `foreground_ratio` や `touches_edge` から失敗を検出できる。

/// 前景の度合いを画素ごとに持つ。0 = 背景、255 = 前景。
/// フェザリング後は中間値も取る。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Mask {
    width: u32,
    height: u32,
    data: Vec<u8>,
}

/// 前景とみなす下限。フェザリングされた境界を前景に数えないための閾値。
///
/// `pub(crate)` なのは、全画素を生のスライスで舐める処理（診断値）が
/// `is_foreground` の添字計算を挟まずに同じ判定をしたいからである。
/// **しきい値の定義はここ 1 箇所にしかない。**
pub(crate) const FOREGROUND_THRESHOLD: u8 = 128;

#[derive(Debug, Clone, PartialEq)]
pub struct MaskStats {
    /// 前景画素が画像全体に占める割合。極端な値(0.0付近/1.0付近)は失敗の兆候
    pub foreground_ratio: f64,
    /// 前景の外接矩形 (x1, y1, x2, y2)。前景が無ければ None
    pub bbox: Option<(u32, u32, u32, u32)>,
    /// 前景が画像の外周に接しているか。商品の見切れを示す
    pub touches_edge: bool,
}

impl Mask {
    pub fn new(width: u32, height: u32, value: u8) -> Self {
        Self {
            width,
            height,
            data: vec![value; (width as usize) * (height as usize)],
        }
    }

    pub fn width(&self) -> u32 {
        self.width
    }

    pub fn height(&self) -> u32 {
        self.height
    }

    pub fn as_slice(&self) -> &[u8] {
        &self.data
    }

    /// 走査順を自分で決めたい呼び出し側のための可変ビュー。
    ///
    /// `set` は画素ごとに添字を計算し直すので、全画素を舐める処理
    /// （診断値の箱ぼかしなど）では桁が変わるほど遅い。
    pub fn as_mut_slice(&mut self) -> &mut [u8] {
        &mut self.data
    }

    /// 前景判定(128以上)だけを残した 0/255 のマスク。
    ///
    /// 階調を落として**形だけ**を見たいときに使う。しきい値をここに閉じ込めて
    /// おくのは、呼び出し側が 128 を書き写すと `is_foreground` を動かしたときに
    /// 黙って食い違うためである。
    pub fn binarized(&self) -> Mask {
        Mask {
            width: self.width,
            height: self.height,
            data: self
                .data
                .iter()
                .map(|&v| {
                    if v >= FOREGROUND_THRESHOLD {
                        u8::MAX
                    } else {
                        0
                    }
                })
                .collect(),
        }
    }

    #[inline]
    fn index(&self, x: u32, y: u32) -> usize {
        (y as usize) * (self.width as usize) + (x as usize)
    }

    #[inline]
    pub fn get(&self, x: u32, y: u32) -> u8 {
        self.data[self.index(x, y)]
    }

    #[inline]
    pub fn set(&mut self, x: u32, y: u32, value: u8) {
        let i = self.index(x, y);
        self.data[i] = value;
    }

    #[inline]
    pub fn is_foreground(&self, x: u32, y: u32) -> bool {
        self.get(x, y) >= FOREGROUND_THRESHOLD
    }

    /// 4 近傍に背景があるか。前景かどうかは見ないので、境界の前景側だけを
    /// 拾いたい呼び出し側は `is_foreground` と併せて使う。
    ///
    /// 画像の外は背景として数えない。見切れている商品の縁を境界と見なすと、
    /// そこに存在しない輪郭を測ることになる。
    pub fn touches_background(&self, x: u32, y: u32) -> bool {
        (x > 0 && !self.is_foreground(x - 1, y))
            || (y > 0 && !self.is_foreground(x, y - 1))
            || (x + 1 < self.width && !self.is_foreground(x + 1, y))
            || (y + 1 < self.height && !self.is_foreground(x, y + 1))
    }

    /// 前景から背景へ向かう単位法線。5x5 の窓で背景側の重心方向を採る。
    ///
    /// 画像の外は背景として数えない。見切れている商品の縁を輪郭と見なすと、
    /// 存在しない遷移をそこで測ることになる。
    pub fn outward_normal(&self, x: u32, y: u32) -> Option<[f32; 2]> {
        let (mut vx, mut vy) = (0f32, 0f32);
        for dy in -2i64..=2 {
            for dx in -2i64..=2 {
                if dx == 0 && dy == 0 {
                    continue;
                }
                let (nx, ny) = (x as i64 + dx, y as i64 + dy);
                if nx < 0
                    || ny < 0
                    || nx >= i64::from(self.width)
                    || ny >= i64::from(self.height)
                    || self.is_foreground(nx as u32, ny as u32)
                {
                    continue;
                }
                let len = ((dx * dx + dy * dy) as f32).sqrt();
                vx += dx as f32 / len;
                vy += dy as f32 / len;
            }
        }
        let len = (vx * vx + vy * vy).sqrt();
        (len > 1e-3).then(|| [vx / len, vy / len])
    }

    /// マスクを 0.0-1.0 の実数として双線形にサンプルする。範囲外は 0 とみなす。
    pub fn sample(&self, fx: f32, fy: f32) -> f32 {
        let (w, h) = (i64::from(self.width), i64::from(self.height));
        let (bx, by) = (fx.floor(), fy.floor());
        let (tx, ty) = (fx - bx, fy - by);
        let at = |ix: i64, iy: i64| -> f32 {
            if ix < 0 || iy < 0 || ix >= w || iy >= h {
                return 0.0;
            }
            f32::from(self.get(ix as u32, iy as u32)) / 255.0
        };
        let (bx, by) = (bx as i64, by as i64);
        let top = at(bx, by) * (1.0 - tx) + at(bx + 1, by) * tx;
        let bottom = at(bx, by + 1) * (1.0 - tx) + at(bx + 1, by + 1) * tx;
        top * (1.0 - ty) + bottom * ty
    }

    /// 真偽値の並びからマスクを作る（true = 前景）。
    pub fn from_bools(width: u32, height: u32, values: &[bool]) -> Self {
        Self {
            width,
            height,
            data: values.iter().map(|&v| if v { 255 } else { 0 }).collect(),
        }
    }

    /// 指定した値を超える画素の外接矩形を返す。
    ///
    /// `stats` の bbox は前景判定(128以上)に基づくが、キャンバス配置では
    /// フェザリングされた薄い縁まで含めたいので閾値を分けられるようにしている。
    pub fn bbox_above(&self, threshold: u8) -> Option<(u32, u32, u32, u32)> {
        let mut min = (u32::MAX, u32::MAX);
        let mut max = (0u32, 0u32);
        let mut found = false;
        for y in 0..self.height {
            for x in 0..self.width {
                if self.get(x, y) <= threshold {
                    continue;
                }
                found = true;
                min.0 = min.0.min(x);
                min.1 = min.1.min(y);
                max.0 = max.0.max(x);
                max.1 = max.1.max(y);
            }
        }
        found.then_some((min.0, min.1, max.0, max.1))
    }

    pub fn stats(&self) -> MaskStats {
        let mut count = 0usize;
        let mut min = (u32::MAX, u32::MAX);
        let mut max = (0u32, 0u32);
        let mut touches_edge = false;

        for y in 0..self.height {
            for x in 0..self.width {
                if !self.is_foreground(x, y) {
                    continue;
                }
                count += 1;
                min.0 = min.0.min(x);
                min.1 = min.1.min(y);
                max.0 = max.0.max(x);
                max.1 = max.1.max(y);
                if x == 0 || y == 0 || x == self.width - 1 || y == self.height - 1 {
                    touches_edge = true;
                }
            }
        }

        let total = (self.width as usize) * (self.height as usize);
        MaskStats {
            foreground_ratio: if total == 0 {
                0.0
            } else {
                count as f64 / total as f64
            },
            bbox: (count > 0).then_some((min.0, min.1, max.0, max.1)),
            touches_edge,
        }
    }

    /// デバッグ用にマスクをグレースケール画像として書き出す。
    pub fn to_image(&self) -> image::GrayImage {
        image::GrayImage::from_raw(self.width, self.height, self.data.clone())
            .expect("マスクの寸法とバッファ長は常に一致する")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_empty_mask_has_no_bbox() {
        let mask = Mask::new(10, 10, 0);
        let s = mask.stats();
        assert_eq!(s.foreground_ratio, 0.0);
        assert_eq!(s.bbox, None);
        assert!(!s.touches_edge);
    }

    #[test]
    fn a_full_mask_covers_everything_and_touches_the_edge() {
        let mask = Mask::new(10, 10, 255);
        let s = mask.stats();
        assert_eq!(s.foreground_ratio, 1.0);
        assert_eq!(s.bbox, Some((0, 0, 9, 9)));
        assert!(s.touches_edge);
    }

    #[test]
    fn bbox_covers_only_the_foreground() {
        let mut mask = Mask::new(10, 10, 0);
        for y in 3..=5 {
            for x in 2..=6 {
                mask.set(x, y, 255);
            }
        }
        let s = mask.stats();
        assert_eq!(s.bbox, Some((2, 3, 6, 5)));
        assert_eq!(s.foreground_ratio, 15.0 / 100.0);
        assert!(!s.touches_edge, "外周に接していない");
    }

    #[test]
    fn touching_any_border_is_detected() {
        for (x, y) in [(0u32, 5u32), (9, 5), (5, 0), (5, 9)] {
            let mut mask = Mask::new(10, 10, 0);
            mask.set(x, y, 255);
            assert!(
                mask.stats().touches_edge,
                "({x},{y}) が外周として検出されない"
            );
        }
    }

    #[test]
    fn bbox_above_includes_faint_edges() {
        let mut mask = Mask::new(10, 10, 0);
        mask.set(5, 5, 255);
        // 薄い縁。前景judgeでは拾われないが、キャンバス配置では含めたい
        mask.set(3, 3, 20);
        assert_eq!(mask.stats().bbox, Some((5, 5, 5, 5)));
        assert_eq!(mask.bbox_above(0), Some((3, 3, 5, 5)));
        assert_eq!(mask.bbox_above(64), Some((5, 5, 5, 5)));
    }

    #[test]
    fn bbox_above_is_none_for_an_empty_mask() {
        assert_eq!(Mask::new(4, 4, 0).bbox_above(0), None);
    }

    #[test]
    fn the_boundary_is_the_foreground_side_of_the_edge() {
        let mut mask = Mask::new(5, 5, 0);
        for y in 1..4 {
            for x in 1..4 {
                mask.set(x, y, 255);
            }
        }
        assert!(mask.touches_background(1, 1), "縁が境界とみなされていない");
        assert!(!mask.touches_background(2, 2), "中心が境界とみなされている");
        // 前景かどうかは見ない契約。呼び出し側が is_foreground と併せて使う
        assert!(
            mask.touches_background(0, 1),
            "背景側からも境界に見えるはず"
        );
    }

    #[test]
    fn a_product_filling_the_frame_has_no_boundary() {
        // 画像の外を背景として数えると、見切れた商品の縁が輪郭になってしまう
        let mask = Mask::new(5, 5, 255);
        assert!(!mask.touches_background(0, 0));
        assert_eq!(mask.outward_normal(0, 0), None);
    }

    #[test]
    fn the_normal_points_away_from_the_foreground() {
        // 左半分が前景。境界の法線は右（背景側）を向く
        let mut mask = Mask::new(10, 10, 0);
        for y in 0..10 {
            for x in 0..5 {
                mask.set(x, y, 255);
            }
        }
        let n = mask.outward_normal(4, 5).unwrap();
        assert!(n[0] > 0.9, "法線が背景側を向いていない: {n:?}");
        assert!(n[1].abs() < 0.1, "法線が境界に沿って傾いている: {n:?}");
    }

    #[test]
    fn sampling_interpolates_between_neighbours() {
        let mut mask = Mask::new(2, 1, 0);
        mask.set(0, 0, 255);
        assert_eq!(mask.sample(0.0, 0.0), 1.0);
        assert_eq!(mask.sample(1.0, 0.0), 0.0);
        assert!((mask.sample(0.5, 0.0) - 0.5).abs() < 0.01);
        assert_eq!(mask.sample(-5.0, 0.0), 0.0, "範囲外は 0");
    }

    #[test]
    fn feathered_values_below_the_threshold_are_not_foreground() {
        let mut mask = Mask::new(4, 1, 0);
        mask.set(0, 0, 0);
        mask.set(1, 0, 127);
        mask.set(2, 0, 128);
        mask.set(3, 0, 255);
        assert!(!mask.is_foreground(0, 0));
        assert!(!mask.is_foreground(1, 0));
        assert!(mask.is_foreground(2, 0));
        assert!(mask.is_foreground(3, 0));
        assert_eq!(mask.stats().foreground_ratio, 0.5);
    }
}
