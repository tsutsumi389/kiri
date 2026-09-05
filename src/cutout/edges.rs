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
    let (w, h) = (image.width() as usize, image.height() as usize);
    let luma: Vec<f32> = image
        .pixels()
        .map(|p| 0.2126 * f32::from(p[0]) + 0.7152 * f32::from(p[1]) + 0.0722 * f32::from(p[2]))
        .collect();

    let mut out = vec![0.0f32; w * h];
    if w < 3 || h < 3 {
        return out;
    }

    for y in 1..h - 1 {
        for x in 1..w - 1 {
            let at = |dx: usize, dy: usize| luma[(y + dy - 1) * w + (x + dx - 1)];
            let gx = -at(0, 0) + at(2, 0) - 2.0 * at(0, 1) + 2.0 * at(2, 1) - at(0, 2) + at(2, 2);
            let gy = -at(0, 0) - 2.0 * at(1, 0) - at(2, 0) + at(0, 2) + 2.0 * at(1, 2) + at(2, 2);
            // Sobel は理想的な段差に対して 4 倍の値を返すため、4 で割って戻す
            out[y * w + x] = (gx * gx + gy * gy).sqrt() / 4.0;
        }
    }
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
        }
    }
}
