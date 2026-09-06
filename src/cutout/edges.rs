//! 輪郭強度の算出。フラッドフィルの「堤防」として使う。
//!
//! 色差だけで背景を判定すると、淡い色の商品（白背景の白い箱など）は背景ごと
//! 飲み込まれる。一方で落ち影を消すには許容量を広く取る必要があり、両者は衝突する。
//!
//! この衝突は勾配で解ける。商品の輪郭は 1px で急峻に変化するのに対し、
//! 落ち影は数十 px かけてなだらかに変化する。1px あたりの変化量を見れば
//! 両者は明確に区別できる。

use image::RgbaImage;

/// 非極大抑制で稜線を細線化し、`threshold` を超えた画素だけを立てた表。
/// 堤防にはこれを使う。
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
///
/// 強度ではなく真偽値を返すのは、呼び出し側がしきい値との比較しかしないため。
/// 12MP では f32 の表というだけで 48MB を積むので、返した先で捨てられる
/// 精度に払う値段としては高すぎる。
pub fn edge_ridges(image: &RgbaImage, threshold: f32) -> Vec<bool> {
    let (w, h) = (image.width() as usize, image.height() as usize);
    let mut out = vec![false; w * h];
    if w < 3 || h < 3 {
        return out;
    }
    let (magnitude, direction) = sobel(image, w, h);
    suppress(w, h, &magnitude, &direction, |i, m| out[i] = m > threshold);
    out
}

/// 外周の帯で測った勾配強度の分位。値の意味は `edge_ridges` のしきい値と同じ
/// 「1px あたりの輝度変化量」で、そのまま比較できる。
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct GradientQuantiles {
    pub p50: f64,
    pub p90: f64,
}

/// 画像外周 `band` px の帯で、勾配強度の分位を測る。
///
/// **堤防を張ってよいかを事前に知るためにある。** 織り目のある布や段ボールを
/// 背景にすると、素材そのものが 1px あたり十数の変化を持つ。堤防は背景の中で
/// 壁になり、フィルは商品まで届かない。壁になるかどうかは「輪郭がどれだけ
/// 急峻か」ではなく「背景がどれだけざらついているか」で決まるので、
/// 商品の写っていない外周で測るのが筋になる。
///
/// 帯だけを走査するのは費用の問題である。12MP で画像全体の Sobel を取ると
/// 輝度・強度・向きの表で 150MB を積むが、この関数は帯の画素ぶんの f32 一本
/// （12MP・帯 90px で 5MB）で済む。輝度の表も持たず、3x3 をその場で読む。
///
/// 帯に商品が写り込んでも p90 はほとんど動かない。商品の輪郭は曲線なので帯の
/// 面積に対して 1 次元でしか効かず、商品の内側は一様で勾配を下げるためである。
/// 逆に、**外周まで織り目で埋まった商品**（畳んだ布そのものを撮る等）では
/// 区別が付かない。その場合は `--edge-threshold` を明示すること。
pub fn border_gradient_quantiles(image: &RgbaImage, band: u32) -> GradientQuantiles {
    let (w, h) = (image.width() as usize, image.height() as usize);
    // Sobel は 3x3 を読むので、外周 1px は測れない。帯として意味を持つのは
    // 2px から。半分を超えると「外周の帯」ではなくなるので、そこで頭を打つ
    if w.min(h) < 4 {
        return GradientQuantiles::default();
    }
    let band = (band as usize).clamp(2, w.min(h) / 2);
    let mut samples: Vec<f32> = Vec::new();

    let push = |x: usize, y: usize, out: &mut Vec<f32>| {
        let at = |dx: usize, dy: usize| -> f32 {
            let p = image.get_pixel((x + dx - 1) as u32, (y + dy - 1) as u32).0;
            0.2126 * f32::from(p[0]) + 0.7152 * f32::from(p[1]) + 0.0722 * f32::from(p[2])
        };
        let gx = -at(0, 0) + at(2, 0) - 2.0 * at(0, 1) + 2.0 * at(2, 1) - at(0, 2) + at(2, 2);
        let gy = -at(0, 0) - 2.0 * at(1, 0) - at(2, 0) + at(0, 2) + 2.0 * at(1, 2) + at(2, 2);
        out.push((gx * gx + gy * gy).sqrt() / 4.0);
    };

    // 上下の帯は全幅、左右の帯はその間の行だけを見る。角を二重に数えないため。
    // band <= 短辺/2 なので、どの範囲も重ならず、隙間も空かない
    for y in (1..band).chain(h - band..h - 1) {
        for x in 1..w - 1 {
            push(x, y, &mut samples);
        }
    }
    for y in band..h - band {
        for x in (1..band).chain(w - band..w - 1) {
            push(x, y, &mut samples);
        }
    }

    quantiles(&mut samples)
}

/// 分位を取り出す。全体を並べ替えず、必要な順位だけを確定させる。
///
/// 12MP・帯 90px では標本が 126 万個になる。完全な整列は 100ms 級で、
/// 切り抜き全体（760ms）に対して無視できない。
fn quantiles(samples: &mut [f32]) -> GradientQuantiles {
    if samples.is_empty() {
        return GradientQuantiles::default();
    }
    let last = samples.len() - 1;
    let index = |q: f64| -> usize { ((last as f64) * q).round() as usize };
    let (i90, i50) = (index(0.9), index(0.5));
    let (_, p90, _) = samples.select_nth_unstable_by(i90, f32::total_cmp);
    let p90 = f64::from(*p90);
    let (head, _) = samples.split_at_mut(i90);
    let p50 = if i50 < head.len() {
        f64::from(*head.select_nth_unstable_by(i50, f32::total_cmp).1)
    } else {
        p90
    };
    GradientQuantiles { p50, p90 }
}

/// Sobel の勾配強度と向き。
///
/// 値は「1px あたりの輝度変化量」に正規化してある。輝度 34 の段差なら
/// おおよそ 34 が返るため、しきい値を輝度の差として直感的に指定できる。
///
/// 輝度の表はこの関数の中で捨てる。12MP では輝度・強度・向き・出力の 4 本が
/// 同時に生きると 156MB になり、フラッドフィルの Lab 表と重なった瞬間に
/// ピーク RSS を押し上げていた。
fn sobel(image: &RgbaImage, w: usize, h: usize) -> (Vec<f32>, Vec<u8>) {
    let luma: Vec<f32> = image
        .pixels()
        .map(|p| 0.2126 * f32::from(p[0]) + 0.7152 * f32::from(p[1]) + 0.0722 * f32::from(p[2]))
        .collect();

    let mut magnitude = vec![0.0f32; w * h];
    let mut direction = vec![0u8; w * h];
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
    (magnitude, direction)
}

/// 勾配の向きに沿った両隣と比べ、極大でなければ落とす。
///
/// 手前側は「以上」、奥側は「より大きい」で比べる。段差の応答は 2px の
/// 平坦な山になるので、両側とも「以上」にすると 2px のまま残り、両側とも
/// 「より大きい」にすると山がまるごと消えてしまう。
///
/// 結果を溜める器は呼び出し側に決めさせる。本番は真偽値だけ、テストは強度を
/// そのまま見たい、という違いのために f32 の表を作らずに済む。
fn suppress(
    w: usize,
    h: usize,
    magnitude: &[f32],
    direction: &[u8],
    mut keep: impl FnMut(usize, f32),
) {
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
                keep(i, m);
            }
        }
    }
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

/// 細線化する前の勾配強度。
///
/// 本番からは使わない。堤防は真偽値しか要らないので、強度の表を作るだけ無駄に
/// なる。しきい値の意味（輝度の段差そのもの）が保たれているかを確かめるのは
/// この関数の役目で、テストからのみ呼ぶ。
#[cfg(test)]
fn gradient_magnitude(image: &RgbaImage) -> Vec<f32> {
    let (w, h) = (image.width() as usize, image.height() as usize);
    if w < 3 || h < 3 {
        return vec![0.0; w * h];
    }
    sobel(image, w, h).0
}

/// 細線化した後の勾配強度。`edge_ridges` がしきい値を掛ける前の値。
#[cfg(test)]
fn ridge_magnitude(image: &RgbaImage) -> Vec<f32> {
    let (w, h) = (image.width() as usize, image.height() as usize);
    let mut out = vec![0.0f32; w * h];
    if w < 3 || h < 3 {
        return out;
    }
    let (magnitude, direction) = sobel(image, w, h);
    suppress(w, h, &magnitude, &direction, |i, m| out[i] = m);
    out
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
            assert_eq!(edge_ridges(&img, 8.0).len(), (w * h) as usize);
        }
    }

    /// 稜線は輪郭の片側 1px に絞られる。両側に立つと、背景側の画素まで
    /// 「侵入禁止」になって前景の外に縁が残る。
    #[test]
    fn suppression_thins_the_response_to_a_single_column() {
        let img = gray_step(9, 250, 216);
        let raw = gradient_magnitude(&img);
        let thin = edge_ridges(&img, 8.0);
        assert_eq!(
            (0..9).filter(|&x| raw[4 * 9 + x] > 8.0).count(),
            2,
            "前提が崩れている: 素の Sobel は 2px に立つ"
        );
        assert_eq!(
            (0..9).filter(|&x| thin[4 * 9 + x]).count(),
            1,
            "細線化できていない"
        );
    }

    #[test]
    fn suppression_keeps_the_peak_value() {
        // しきい値は「輝度の段差」として指定する契約なので、強度は変えない
        let thin = ridge_magnitude(&gray_step(9, 250, 216));
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
        let thin = ridge_magnitude(&img);
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

    /// 織り目状のテクスチャを乗せた背景。振幅 `amp`・周期 `period` px。
    fn woven(size: u32, amp: f32, period: f32) -> RgbaImage {
        let mut img = RgbaImage::new(size, size);
        let k = std::f32::consts::TAU / period;
        for y in 0..size {
            for x in 0..size {
                let t = amp * (x as f32 * k).sin() * (y as f32 * k).sin();
                let v = (177.0 + t).clamp(0.0, 255.0) as u8;
                img.put_pixel(x, y, Rgba([v, v, v, 255]));
            }
        }
        img
    }

    #[test]
    fn a_flat_border_reports_no_gradient() {
        let img = RgbaImage::from_pixel(40, 40, Rgba([200, 200, 200, 255]));
        let q = border_gradient_quantiles(&img, 4);
        assert_eq!(q.p50, 0.0);
        assert_eq!(q.p90, 0.0);
    }

    /// 織り目のある背景は既定の堤防（8）を超える勾配を持つ。
    /// この値が出るからこそ「堤防が背景の中で壁になる」と判断できる。
    #[test]
    fn a_woven_border_reports_a_gradient_above_the_default_dam() {
        let q = border_gradient_quantiles(&woven(120, 12.0, 8.0), 8);
        assert!(q.p90 > 8.0, "織り目の p90 が堤防を超えていない: {:?}", q);
        assert!(q.p50 > 2.0, "織り目の p50 が小さすぎる: {:?}", q);
    }

    /// 帯の外にある構造は結果に影響しない。中央に真っ二つの段差を置いても、
    /// 外周が平坦なら「堤防を張ってよい背景」と報告されるべきである。
    #[test]
    fn structure_outside_the_band_is_not_measured() {
        let mut img = RgbaImage::from_pixel(60, 60, Rgba([200, 200, 200, 255]));
        for y in 20..40 {
            for x in 20..40 {
                img.put_pixel(x, y, Rgba([20, 20, 20, 255]));
            }
        }
        let q = border_gradient_quantiles(&img, 5);
        assert_eq!(q.p90, 0.0, "帯の外の段差を拾っている: {:?}", q);
    }

    /// 帯に商品が掛かっても p90 はほとんど動かない。輪郭は曲線なので帯の面積に
    /// 対して 1 次元でしか効かず、商品の内側は一様で勾配を下げるためである。
    #[test]
    fn a_product_touching_the_border_barely_moves_the_quantiles() {
        let mut img = RgbaImage::from_pixel(80, 80, Rgba([200, 200, 200, 255]));
        // 左上の角から 20x20 だけ商品がはみ出している
        for y in 0..20 {
            for x in 0..20 {
                img.put_pixel(x, y, Rgba([20, 20, 20, 255]));
            }
        }
        let q = border_gradient_quantiles(&img, 6);
        assert_eq!(q.p90, 0.0, "輪郭の線が p90 を動かしている: {:?}", q);
    }

    #[test]
    fn tiny_images_report_nothing_rather_than_panicking() {
        for (w, h) in [(1u32, 1u32), (2, 2), (3, 3), (1, 40), (40, 1), (4, 4)] {
            let img = RgbaImage::from_pixel(w, h, Rgba([10, 10, 10, 255]));
            for band in [0u32, 1, 2, 1000] {
                assert_eq!(border_gradient_quantiles(&img, band).p90, 0.0);
            }
        }
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
        let thin = edge_ridges(&img, 8.0);
        for y in 2..18u32 {
            let on_row = (1..19u32).any(|x| thin[(y * 20 + x) as usize]);
            assert!(on_row, "y={y} の行に稜線が無い");
        }
    }
}
