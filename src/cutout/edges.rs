//! 輪郭強度の算出。フラッドフィルの「堤防」として使う。
//!
//! 色差だけで背景を判定すると、淡い色の商品（白背景の白い箱など）は背景ごと
//! 飲み込まれる。一方で落ち影を消すには許容量を広く取る必要があり、両者は衝突する。
//!
//! この衝突は勾配で解ける。商品の輪郭は 1px で急峻に変化するのに対し、
//! 落ち影は数十 px かけてなだらかに変化する。1px あたりの変化量を見れば
//! 両者は明確に区別できる。

use image::RgbaImage;

/// Sobel による勾配強度を求める。
///
/// 値は「1px あたりの輝度変化量」に正規化してある。輝度 34 の段差なら
/// おおよそ 34 が返るため、しきい値を輝度の差として直感的に指定できる。
pub fn gradient_magnitude(image: &RgbaImage) -> Vec<f32> {
    ridges(image, false)
}

/// 非極大抑制で稜線を細線化した勾配強度。堤防にはこちらを使う。
///
/// Sobel の応答は 1px の段差に対しても輪郭の**両側**に立つ。JPEG の滲みが
/// 加わると厚さは 3-4px に達し、その帯の画素がすべてフィルの侵入を拒む。
/// 結果として
///
/// - フィルの停止位置が真の輪郭より 1px 外側になる（境界の 1px が
///   「色は完全に背景なのに不透明」という縁として残る）
/// - 商品の直下に落ちた影のうち、輪郭に近い数 px が背景として消せずに残る
///
/// という 2 つの副作用が出ていた。勾配の向きに沿って隣と比べ、極大でない画素を
/// 落とすことで、堤防を輪郭上の 1px の線に絞る。線が 1px でも 4 近傍のフィルは
/// 越えられない（斜めにつながっていれば 4 近傍の経路は必ず遮られる）ので、
/// 堤防としての働きは失われない。
///
/// 平坦な傾斜（落ち影のような一定勾配）は極大を持たないため、まるごと落ちる。
/// これは望ましい。堤防が守るべきなのは段差であって傾斜ではない。
pub fn edge_ridges(image: &RgbaImage) -> Vec<f32> {
    ridges(image, true)
}

fn ridges(image: &RgbaImage, suppress: bool) -> Vec<f32> {
    let (w, h) = (image.width() as usize, image.height() as usize);
    let luma: Vec<f32> = image
        .pixels()
        .map(|p| 0.2126 * f32::from(p[0]) + 0.7152 * f32::from(p[1]) + 0.0722 * f32::from(p[2]))
        .collect();

    let mut magnitude = vec![0.0f32; w * h];
    let mut direction = vec![0u8; w * h];
    if w < 3 || h < 3 {
        return magnitude;
    }

    for y in 1..h - 1 {
        for x in 1..w - 1 {
            let at = |dx: usize, dy: usize| luma[(y + dy - 1) * w + (x + dx - 1)];
            let gx = -at(0, 0) + at(2, 0) - 2.0 * at(0, 1) + 2.0 * at(2, 1) - at(0, 2) + at(2, 2);
            let gy = -at(0, 0) - 2.0 * at(1, 0) - at(2, 0) + at(0, 2) + 2.0 * at(1, 2) + at(2, 2);
            // Sobel は理想的な段差に対して 4 倍の値を返すため、4 で割って戻す
            magnitude[y * w + x] = (gx * gx + gy * gy).sqrt() / 4.0;
            direction[y * w + x] = octant(gx, gy);
        }
    }
    if !suppress {
        return magnitude;
    }

    // 勾配の向きに沿った両隣と比べ、極大でなければ落とす。
    // 手前側は「以上」、奥側は「より大きい」で比べる。段差の応答は 2px の
    // 平坦な山になるので、両側とも「以上」にすると 2px のまま残り、両側とも
    // 「より大きい」にすると山がまるごと消えてしまう
    let mut out = vec![0.0f32; w * h];
    for y in 1..h - 1 {
        for x in 1..w - 1 {
            let i = y * w + x;
            let m = magnitude[i];
            if m <= 0.0 {
                continue;
            }
            let (dx, dy) = OFFSETS[direction[i] as usize];
            let back = magnitude[(y as isize - dy) as usize * w + (x as isize - dx) as usize];
            let ahead = magnitude[(y as isize + dy) as usize * w + (x as isize + dx) as usize];
            if m >= back && m > ahead {
                out[i] = m;
            }
        }
    }
    out
}

/// 勾配の向きを 4 方向へ量子化した際の隣接オフセット。
const OFFSETS: [(isize, isize); 4] = [(1, 0), (1, 1), (0, 1), (-1, 1)];

/// 勾配ベクトルを 4 方向（0°, 45°, 90°, 135°）のどれかへ丸める。
fn octant(gx: f32, gy: f32) -> u8 {
    // tan(22.5°) と tan(67.5°)。atan2 を避けるため比較で分ける
    const LOW: f32 = 0.414_213_56;
    const HIGH: f32 = 2.414_213_6;
    let (ax, ay) = (gx.abs(), gy.abs());
    if ay <= ax * LOW {
        0 // 横向きの勾配 = 縦の輪郭
    } else if ay >= ax * HIGH {
        2 // 縦向きの勾配 = 横の輪郭
    } else if (gx > 0.0) == (gy > 0.0) {
        1
    } else {
        3
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::Rgba;

    fn gray_step(width: u32, left: u8, right: u8) -> RgbaImage {
        let mut img = RgbaImage::new(width, 9);
        for y in 0..9 {
            for x in 0..width {
                let v = if x < width / 2 { left } else { right };
                img.put_pixel(x, y, Rgba([v, v, v, 255]));
            }
        }
        img
    }

    #[test]
    fn a_flat_image_has_no_gradient() {
        let img = RgbaImage::from_pixel(9, 9, Rgba([200, 200, 200, 255]));
        let g = gradient_magnitude(&img);
        assert!(g.iter().all(|&v| v < 0.001), "平坦なのに勾配が出ている");
    }

    #[test]
    fn a_sharp_step_reports_the_step_size() {
        // 輝度差 34 の段差なら、正規化により 34 前後が返るはず
        let img = gray_step(9, 250, 216);
        let g = gradient_magnitude(&img);
        let peak = g.iter().cloned().fold(0.0f32, f32::max);
        assert!(
            (peak - 34.0).abs() < 2.0,
            "段差 34 に対して {peak} が返った"
        );
    }

    #[test]
    fn a_gentle_ramp_reports_a_small_gradient() {
        // 同じ 34 の変化を 34px かけて起こすと、1px あたりは 1 程度になる
        let mut img = RgbaImage::new(40, 9);
        for y in 0..9 {
            for x in 0..40 {
                let v = (250 - x.min(34)) as u8;
                img.put_pixel(x, y, Rgba([v, v, v, 255]));
            }
        }
        let g = gradient_magnitude(&img);
        let peak = g.iter().cloned().fold(0.0f32, f32::max);
        assert!(peak < 3.0, "なだらかな傾斜で {peak} と大きく出ている");
    }

    #[test]
    fn a_sharp_edge_and_a_soft_shadow_are_separable() {
        // 商品の輪郭(急峻)と落ち影(なだらか)を1枚に置き、しきい値で分けられること
        let sharp = gradient_magnitude(&gray_step(9, 250, 216));
        let mut ramp = RgbaImage::new(40, 9);
        for y in 0..9 {
            for x in 0..40 {
                let v = (250 - x.min(34)) as u8;
                ramp.put_pixel(x, y, Rgba([v, v, v, 255]));
            }
        }
        let soft = gradient_magnitude(&ramp);
        let threshold = 8.0;
        assert!(sharp.iter().cloned().fold(0.0f32, f32::max) > threshold);
        assert!(soft.iter().cloned().fold(0.0f32, f32::max) < threshold);
    }

    #[test]
    fn tiny_images_do_not_panic() {
        for (w, h) in [(1u32, 1u32), (2, 2), (1, 9), (9, 1)] {
            let img = RgbaImage::from_pixel(w, h, Rgba([10, 10, 10, 255]));
            assert_eq!(gradient_magnitude(&img).len(), (w * h) as usize);
            assert_eq!(edge_ridges(&img).len(), (w * h) as usize);
        }
    }

    /// 稜線は輪郭の片側 1px に絞られる。両側に立つと、背景側の画素まで
    /// 「侵入禁止」になって前景の外に縁が残る。
    #[test]
    fn suppression_thins_the_response_to_a_single_column() {
        let img = gray_step(9, 250, 216);
        let raw = gradient_magnitude(&img);
        let thin = edge_ridges(&img);
        let count = |g: &[f32]| (0..9).filter(|&x| g[4 * 9 + x] > 8.0).count();
        assert_eq!(count(&raw), 2, "前提が崩れている: 素の Sobel は 2px に立つ");
        assert_eq!(count(&thin), 1, "細線化できていない");
    }

    #[test]
    fn suppression_keeps_the_peak_value() {
        // しきい値は「輝度の段差」として指定する契約なので、強度は変えない
        let thin = edge_ridges(&gray_step(9, 250, 216));
        let peak = thin.iter().cloned().fold(0.0f32, f32::max);
        assert!(
            (peak - 34.0).abs() < 2.0,
            "段差 34 に対して {peak} が返った"
        );
    }

    /// 一定の傾斜はほとんど落ちる。堤防が守るべきなのは段差であって傾斜ではない。
    ///
    /// 完全にゼロにはならない。傾斜も 8bit へ量子化されると階段になり、
    /// 段の縁が極大として残るためである。残るのは点在する画素であって
    /// 連なった壁にはならないので、4 近傍のフィルは迂回できる。
    #[test]
    fn a_constant_ramp_is_mostly_suppressed() {
        let mut img = RgbaImage::new(40, 9);
        for y in 0..9 {
            for x in 0..40 {
                let v = (250 - x.min(34)) as u8;
                img.put_pixel(x, y, Rgba([v, v, v, 255]));
            }
        }
        let raw = gradient_magnitude(&img);
        let thin = edge_ridges(&img);
        let count = |g: &[f32]| {
            (1..8u32)
                .flat_map(|y| (5..30u32).map(move |x| (y * 40 + x) as usize))
                .filter(|&i| g[i] > 0.5)
                .count()
        };
        let (before, after) = (count(&raw), count(&thin));
        assert!(before > 100, "前提が崩れている: 傾斜全体に勾配が出るはず");
        assert!(
            after * 3 < before,
            "傾斜の勾配がほとんど落ちていない: {before} -> {after}"
        );
    }

    #[test]
    fn the_ridge_of_a_diagonal_edge_stays_connected() {
        // 斜めの輪郭で稜線が途切れると、4 近傍のフィルがそこから抜けてしまう
        let mut img = RgbaImage::new(20, 20);
        for y in 0..20 {
            for x in 0..20 {
                let v = if x + y < 20 { 250 } else { 216 };
                img.put_pixel(x, y, Rgba([v, v, v, 255]));
            }
        }
        let thin = edge_ridges(&img);
        for y in 2..18u32 {
            let on_row = (1..19u32).any(|x| thin[(y * 20 + x) as usize] > 8.0);
            assert!(on_row, "y={y} の行に稜線が無い");
        }
    }
}
