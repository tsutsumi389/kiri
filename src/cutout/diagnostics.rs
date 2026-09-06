//! 境界品質の診断値。
//!
//! `separability` は「輪郭が色の違いによって引かれたか」を境界の**内側**で測る。
//! そのため、前景の外側に背景色のままの縁が残っていても検出できない。実際、
//! エッジ堤防が残す 1px の縁は separability を何ら悪化させないまま、黒い下地に
//! 載せたときの白い光輪として現れていた。
//!
//! ここでは縁そのものを測る `halo_ratio` と、境界の階調の広がりを測る
//! `edge_width` を用意する。どちらも画像を開かずに失敗を検出するための値である。

use image::RgbaImage;

use crate::color::lab::{delta_e76, srgb_to_lab};
use crate::cutout::mask::Mask;

/// 境界近傍とみなす距離(px)。
const NEAR_BOUNDARY: i64 = 3;
/// 局所背景色を集める窓の半径(px)。
const LOCAL_BG_WINDOW: i64 = 8;
/// 「背景色のまま」とみなす色差。CIE76 で 2.3 前後が見分けの限界。
const SAME_AS_BACKGROUND: f64 = 3.0;
/// アルファ遷移を追う距離の上限(px)。
const MAX_TRANSITION: f32 = 16.0;
/// 遷移幅を測る刻み(px)。
const TRANSITION_STEP: f32 = 0.5;

/// この値を超える `halo_ratio` は目視確認に値する。
pub const HALO_WARN: f64 = 0.10;

#[derive(Debug, Clone, PartialEq)]
pub struct Diagnostics {
    /// 境界近傍で不透明なのに、元画素の色が局所背景と見分けがつかない画素の割合。
    /// 大きいほど、切り抜きの縁に背景色が残って光輪になる。
    ///
    /// 境界近傍の前景画素が1つも無ければ None。`separability` と同じ理由で
    /// 0.0 とは区別する。「縁が残っていない」と「そもそも測れていない」は
    /// まったく別の状態であり、0 と報告すると前者に見えてしまう
    pub halo_ratio: Option<f64>,
    /// 境界法線方向にアルファが 0.9 から 0.1 へ落ちるまでの幅(px)の中央値。
    /// 小さいほど輪郭が鮮鋭で、大きすぎればぼやけている。
    ///
    /// 遷移を1本も追えなければ None。前景が無い場合と、見切れて輪郭が
    /// 画像の中に存在しない場合がこれに当たる
    pub edge_width: Option<f64>,
}

/// 元画像とマスクから診断値を求める。
///
/// 元画像を使うのが要点。デスピル後の画像で測ると「背景色を消したから縁が
/// 見えなくなった」だけの状態を good と誤判定する。知りたいのは
/// 「背景色のままの画素を不透明にしていないか」である。
pub fn diagnose(image: &RgbaImage, mask: &Mask, background: [u8; 3]) -> Diagnostics {
    Diagnostics {
        halo_ratio: halo_ratio(image, mask, background),
        edge_width: edge_width(mask),
    }
}

/// 境界近傍で「背景色のままなのに不透明」な画素の割合。測る対象が無ければ None。
pub fn halo_ratio(image: &RgbaImage, mask: &Mask, background: [u8; 3]) -> Option<f64> {
    let (w, h) = (mask.width(), mask.height());
    if image.width() != w || image.height() != h {
        return None;
    }
    let fallback = srgb_to_lab(background);
    let (mut halo, mut total) = (0u64, 0u64);

    for y in 0..h {
        for x in 0..w {
            if !mask.is_foreground(x, y) || !near_boundary(mask, x, y) {
                continue;
            }
            total += 1;
            let local = local_background(image, mask, x, y);
            let reference = local.map_or(fallback, srgb_to_lab);
            let p = image.get_pixel(x, y).0;
            if delta_e76(srgb_to_lab([p[0], p[1], p[2]]), reference) <= SAME_AS_BACKGROUND {
                halo += 1;
            }
        }
    }
    (total > 0).then(|| halo as f64 / total as f64)
}

fn near_boundary(mask: &Mask, x: u32, y: u32) -> bool {
    let (w, h) = (mask.width(), mask.height());
    let x0 = (x as i64 - NEAR_BOUNDARY).max(0) as u32;
    let x1 = ((x as i64 + NEAR_BOUNDARY) as u32).min(w - 1);
    let y0 = (y as i64 - NEAR_BOUNDARY).max(0) as u32;
    let y1 = ((y as i64 + NEAR_BOUNDARY) as u32).min(h - 1);
    for ny in y0..=y1 {
        for nx in x0..=x1 {
            if !mask.is_foreground(nx, ny) {
                return true;
            }
        }
    }
    false
}

/// 窓内の完全に透明な画素の平均色。1つも無ければ None。
///
/// 大域の背景色ではなく局所の色を使うのは、照明ムラや落ち影のある場所で
/// 「大域の背景色とは違うが、その場所の背景ではある」画素を見逃さないため。
fn local_background(image: &RgbaImage, mask: &Mask, x: u32, y: u32) -> Option<[u8; 3]> {
    let (w, h) = (mask.width(), mask.height());
    let x0 = (x as i64 - LOCAL_BG_WINDOW).max(0) as u32;
    let x1 = ((x as i64 + LOCAL_BG_WINDOW) as u32).min(w - 1);
    let y0 = (y as i64 - LOCAL_BG_WINDOW).max(0) as u32;
    let y1 = ((y as i64 + LOCAL_BG_WINDOW) as u32).min(h - 1);
    let mut sum = [0u64; 3];
    let mut n = 0u64;
    for ny in y0..=y1 {
        for nx in x0..=x1 {
            if mask.get(nx, ny) != 0 {
                continue;
            }
            let p = image.get_pixel(nx, ny).0;
            for (k, slot) in sum.iter_mut().enumerate() {
                *slot += u64::from(p[k]);
            }
            n += 1;
        }
    }
    (n > 0).then(|| [(sum[0] / n) as u8, (sum[1] / n) as u8, (sum[2] / n) as u8])
}

/// 境界法線方向にアルファが 0.9 から 0.1 へ落ちるまでの幅(px)の中央値。
/// 遷移を1本も追えなければ None。
pub fn edge_width(mask: &Mask) -> Option<f64> {
    let (w, h) = (mask.width(), mask.height());
    let mut widths: Vec<f32> = Vec::new();

    for y in 0..h {
        for x in 0..w {
            if !mask.is_foreground(x, y) || !mask.touches_background(x, y) {
                continue;
            }
            let Some(normal) = mask.outward_normal(x, y) else {
                continue;
            };
            if let Some(width) = transition_span(mask, x, y, normal) {
                widths.push(width);
            }
        }
    }

    if widths.is_empty() {
        return None;
    }
    widths.sort_by(f32::total_cmp);
    Some(f64::from(widths[widths.len() / 2]))
}

/// 法線に沿ってアルファを追い、0.9 を最後に上回った位置から 0.1 を最初に
/// 下回った位置までの距離を返す。
fn transition_span(mask: &Mask, x: u32, y: u32, normal: [f32; 2]) -> Option<f32> {
    let mut high: Option<f32> = None;
    let mut t = -MAX_TRANSITION;
    while t <= MAX_TRANSITION {
        let a = mask.sample(x as f32 + normal[0] * t, y as f32 + normal[1] * t);
        if a >= 0.9 {
            high = Some(t);
        } else if a <= 0.1 {
            // 0.9 を通過する前に 0.1 まで落ちていたら、この向きは輪郭ではない
            return high.map(|start| t - start);
        }
        t += TRANSITION_STEP;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::Rgba;

    /// 左半分が商品、右半分が背景の画像と、それに対するマスク。
    ///
    /// `rim` は「背景色のままなのに不透明」な縁の厚さ(px)。マスクだけを外へ
    /// `rim` px 広げるので、堤防が残す縁をそのまま模している。
    /// `ramp` はマスクの階調の幅(px)。
    fn scene(rim: u32, ramp: u32) -> (RgbaImage, Mask) {
        let (w, h) = (60u32, 20u32);
        let bg = [250u8, 250, 249];
        let product = [40u8, 40, 45];
        let mut image = RgbaImage::from_pixel(w, h, Rgba([bg[0], bg[1], bg[2], 255]));
        let mut mask = Mask::new(w, h, 0);
        for y in 0..h {
            for x in 0..w {
                if x < 30 {
                    image.put_pixel(x, y, Rgba([product[0], product[1], product[2], 255]));
                }
                // 階調はマスクの端の内側に置く。正しく引かれたマスクの階調は
                // 輪郭をまたぐので、外側に置くとそれ自体が縁になってしまう
                let edge = 30 + rim;
                let value = if x + ramp < edge {
                    255
                } else if x < edge {
                    ((edge - x) as f32 / (ramp + 1) as f32 * 255.0) as u8
                } else {
                    0
                };
                mask.set(x, y, value);
            }
        }
        (image, mask)
    }

    #[test]
    fn a_clean_edge_has_no_halo() {
        let (image, mask) = scene(0, 2);
        let r = halo_ratio(&image, &mask, [250, 250, 249]).expect("境界があるので測れる");
        assert!(r < 0.05, "縁が無いのに halo_ratio が高い: {r:.3}");
    }

    #[test]
    fn a_background_coloured_rim_is_detected() {
        let (image, mask) = scene(3, 2);
        let r = halo_ratio(&image, &mask, [250, 250, 249]).expect("境界があるので測れる");
        assert!(r > 0.5, "背景色の縁を検出できていない: {r:.3}");
    }

    #[test]
    fn a_wider_rim_scores_higher() {
        let narrow = {
            let (i, m) = scene(1, 2);
            halo_ratio(&i, &m, [250, 250, 249]).unwrap()
        };
        let wide = {
            let (i, m) = scene(3, 2);
            halo_ratio(&i, &m, [250, 250, 249]).unwrap()
        };
        assert!(
            wide > narrow,
            "縁の厚みに反応していない: {wide} <= {narrow}"
        );
    }

    #[test]
    fn edge_width_grows_with_the_ramp() {
        let (_, sharp) = scene(0, 1);
        let (_, soft) = scene(0, 8);
        let a = edge_width(&sharp).unwrap();
        let b = edge_width(&soft).unwrap();
        assert!(a < b, "遷移幅が階調の広さに追従していない: {a} >= {b}");
        assert!(b > 3.0, "8px の階調が幅として出ていない: {b}");
    }

    #[test]
    fn a_binary_edge_is_narrow() {
        let (_, mask) = scene(0, 0);
        let width = edge_width(&mask).unwrap();
        assert!(width <= 1.5, "二値の境界が広く測られている: {width}");
    }

    #[test]
    fn an_empty_mask_is_not_a_panic() {
        let image = RgbaImage::from_pixel(8, 8, Rgba([250, 250, 249, 255]));
        let mask = Mask::new(8, 8, 0);
        let d = diagnose(&image, &mask, [250, 250, 249]);
        // 「縁が無い」ではなく「測れなかった」。0 と報告すると良い結果に見える
        assert_eq!(d.halo_ratio, None);
        assert_eq!(d.edge_width, None);
    }

    #[test]
    fn mismatched_dimensions_are_refused_rather_than_panicking() {
        let image = RgbaImage::from_pixel(8, 8, Rgba([0, 0, 0, 255]));
        let mut mask = Mask::new(10, 10, 0);
        mask.set(5, 5, 255);
        assert_eq!(halo_ratio(&image, &mask, [0; 3]), None);
    }
}
