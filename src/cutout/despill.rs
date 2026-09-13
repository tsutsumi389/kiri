//! 境界の色かぶり除去。
//!
//! 商品の輪郭では、撮影された画素そのものが商品色と背景色の混色になっている。
//! アルファを与えただけでは背景色が残り、白背景なら輪郭が白っぽく浮く。
//!
//! 観測色を C、商品色を F、背景色を B、被覆率を a とすると C = aF + (1-a)B が
//! 成り立つので、F = (C - (1-a)B) / a で商品色を復元できる。
//!
//! これは `--no-refine` のときだけ使う簡易版である。正は
//! [`crate::cutout::refine::refine`] 側の復元で、あちらは線形光で解き、
//! 背景色を局所推定に置き換え、増幅した誤差を色相ごと縮める。ここでは
//! sRGB 8bit のまま大域の背景色で解き、結果をチャンネルごとに切り詰めるので、
//! **ざらついた背景では輪郭に色相のねじれ（緑や紫の縁）が残る**。
//! `--no-refine` は「淡い色の商品で新方式が不安定なときの逃げ道」であり、
//! その代償としてこの崩れを受け入れる。

use image::RgbaImage;

use crate::cutout::background::BackgroundField;
use crate::cutout::mask::Mask;

/// これを下回るアルファでは復元式の分母が小さすぎて色が暴れるため触らない。
/// ほぼ透明で見えない画素なので実害はない。
const MIN_ALPHA: u8 = 16;

/// マスクの中間値を持つ画素から背景色の寄与を取り除く。
///
/// 引くのは**その場の背景色**である。照明の勾配がある背景で大域の 1 色を引くと、
/// 明るい側では引きすぎ、暗い側では引き足りず、境界の 1px が画像の左右で
/// 逆向きにずれる。`flat` の場では `rgb_at` が大域の 1 色を返すので、
/// 1 色で引いていた頃と 1 ビットも変わらない。
pub fn despill(image: &mut RgbaImage, mask: &Mask, field: &BackgroundField) {
    for y in 0..image.height() {
        for x in 0..image.width() {
            let a = mask.get(x, y);
            if a < MIN_ALPHA || a == 255 {
                continue;
            }
            let alpha = f32::from(a) / 255.0;
            let background = field.rgb_at(x, y);
            let pixel = image.get_pixel_mut(x, y);
            for c in 0..3 {
                let observed = f32::from(pixel[c]);
                let bg = f32::from(background[c]);
                let recovered = (observed - (1.0 - alpha) * bg) / alpha;
                pixel[c] = recovered.round().clamp(0.0, 255.0) as u8;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::Rgba;

    const WHITE: [u8; 3] = [255, 255, 255];

    fn single(color: [u8; 4]) -> RgbaImage {
        RgbaImage::from_pixel(1, 1, Rgba(color))
    }

    fn mask_with(value: u8) -> Mask {
        Mask::new(1, 1, value)
    }

    #[test]
    fn opaque_pixels_are_left_alone() {
        let mut img = single([190, 70, 55, 255]);
        despill(&mut img, &mask_with(255), &BackgroundField::flat(WHITE));
        assert_eq!(img.get_pixel(0, 0).0, [190, 70, 55, 255]);
    }

    #[test]
    fn nearly_transparent_pixels_are_left_alone() {
        // 分母が小さすぎて色が暴れる領域には触れない
        let mut img = single([250, 250, 250, 255]);
        despill(&mut img, &mask_with(8), &BackgroundField::flat(WHITE));
        assert_eq!(img.get_pixel(0, 0).0[0], 250);
    }

    #[test]
    fn a_half_covered_pixel_recovers_the_product_color() {
        // 商品色(60,20,10)が白背景と半々で混ざった画素を作り、元の色に戻せるか見る
        let product = [60.0f32, 20.0, 10.0];
        let observed: [u8; 4] = [
            (0.5 * product[0] + 0.5 * 255.0).round() as u8,
            (0.5 * product[1] + 0.5 * 255.0).round() as u8,
            (0.5 * product[2] + 0.5 * 255.0).round() as u8,
            255,
        ];
        let mut img = single(observed);
        despill(&mut img, &mask_with(128), &BackgroundField::flat(WHITE));

        let out = img.get_pixel(0, 0).0;
        for c in 0..3 {
            let diff = (f32::from(out[c]) - product[c]).abs();
            assert!(
                diff <= 3.0,
                "ch{c}: {} が {} に戻っていない",
                out[c],
                product[c]
            );
        }
    }

    #[test]
    fn a_pixel_that_is_pure_background_becomes_dark_not_garbage() {
        // 背景そのものの色に中途半端なアルファが付いた場合、復元結果は
        // 0 に張り付く。少なくとも範囲外の値やオーバーフローは起こさない
        let mut img = single([255, 255, 255, 255]);
        despill(&mut img, &mask_with(64), &BackgroundField::flat(WHITE));
        let out = img.get_pixel(0, 0).0;
        assert_eq!([out[0], out[1], out[2]], [255, 255, 255]);
    }

    #[test]
    fn works_on_a_dark_background_too() {
        let background = [10u8, 10, 10];
        let product = [200.0f32, 180.0, 160.0];
        let observed: [u8; 4] = [
            (0.5 * product[0] + 0.5 * 10.0).round() as u8,
            (0.5 * product[1] + 0.5 * 10.0).round() as u8,
            (0.5 * product[2] + 0.5 * 10.0).round() as u8,
            255,
        ];
        let mut img = single(observed);
        despill(
            &mut img,
            &mask_with(128),
            &BackgroundField::flat(background),
        );
        let out = img.get_pixel(0, 0).0;
        for c in 0..3 {
            assert!(
                (f32::from(out[c]) - product[c]).abs() <= 3.0,
                "ch{c}: {}",
                out[c]
            );
        }
    }
}
