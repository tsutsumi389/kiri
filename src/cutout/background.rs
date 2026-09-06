//! 背景色の推定。
//!
//! 単色背景を前提とし、画像外周の帯から背景色を求める。同時に「その帯がどれだけ
//! 均一か」を `uniformity` として返す。AI エージェントはこの値を見て、対象画像が
//! kiri の想定する単色背景かどうかを事前に判断できる。

use image::RgbaImage;

use crate::color::lab::delta_e_rgb;

/// 外周サンプルが背景色とみなせる ΔE の上限。
/// CIE76 で 5 前後は「注意すれば違いが分かる」水準にあたる。
const UNIFORM_DELTA_E: f64 = 5.0;

/// 背景推定に使う外周の既定幅(px)。
pub const DEFAULT_BORDER: u32 = 2;

/// 外周サンプルが推定背景色からどれだけ離れているかの分布。
///
/// `uniformity` は「均一か否か」しか言わないため、低かったときに
/// 「わずかなムラが広く出ている」のか「一部だけ大きく外れている」のかを
/// 区別できない。前者は tolerance で吸収できるが、後者は bbox で切るしかない。
/// AI エージェントがその判断を下すための情報である。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DeltaEQuantiles {
    pub p50: f64,
    pub p90: f64,
    pub max: f64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct BackgroundEstimate {
    pub rgb: [u8; 3],
    /// 外周サンプルのうち、推定背景色から ΔE<=5 に収まる割合 (0.0-1.0)
    pub uniformity: f64,
    pub samples: usize,
    /// 外周サンプルの推定背景色からの ΔE 分布
    pub delta_e: DeltaEQuantiles,
}

impl BackgroundEstimate {
    /// 単色背景として扱えるか。切り抜きの成否をおおむねこの値が決める。
    pub fn is_uniform(&self) -> bool {
        self.uniformity >= 0.90
    }
}

/// 画像外周から背景色を推定する。
///
/// 中央値を使うのは、外周に商品がわずかに掛かっている場合に平均だと引きずられるため。
pub fn estimate_background(image: &RgbaImage, border: u32) -> BackgroundEstimate {
    let samples = collect_border_pixels(image, border);
    if samples.is_empty() {
        return BackgroundEstimate {
            rgb: [255, 255, 255],
            uniformity: 0.0,
            samples: 0,
            delta_e: quantiles(&[]),
        };
    }

    let rgb = median_rgb(&samples);
    let mut deltas: Vec<f64> = samples.iter().map(|&s| delta_e_rgb(s, rgb)).collect();
    let within = deltas.iter().filter(|d| **d <= UNIFORM_DELTA_E).count();
    deltas.sort_by(f64::total_cmp);

    BackgroundEstimate {
        rgb,
        uniformity: within as f64 / samples.len() as f64,
        samples: samples.len(),
        delta_e: quantiles(&deltas),
    }
}

/// 昇順に並んだ値から分位を取り出す。
fn quantiles(sorted: &[f64]) -> DeltaEQuantiles {
    if sorted.is_empty() {
        return DeltaEQuantiles {
            p50: 0.0,
            p90: 0.0,
            max: 0.0,
        };
    }
    let at = |q: f64| -> f64 {
        let i = ((sorted.len() as f64 - 1.0) * q).round() as usize;
        sorted[i]
    };
    DeltaEQuantiles {
        p50: at(0.5),
        p90: at(0.9),
        max: sorted[sorted.len() - 1],
    }
}

/// 外周 `border` px の帯にあるピクセルを集める。
///
/// 既に透過している画像（切り抜き済みの再処理など）では透明ピクセルは背景色の
/// 情報を持たないため除外する。全て透明だった場合のみ、やむなく全件を使う。
fn collect_border_pixels(image: &RgbaImage, border: u32) -> Vec<[u8; 3]> {
    let (w, h) = (image.width(), image.height());
    if w == 0 || h == 0 {
        return Vec::new();
    }
    let border = border.max(1).min(w.div_ceil(2)).min(h.div_ceil(2));

    let mut opaque = Vec::new();
    let mut all = Vec::new();
    for y in 0..h {
        for x in 0..w {
            let on_border = x < border || y < border || x >= w - border || y >= h - border;
            if !on_border {
                continue;
            }
            let p = image.get_pixel(x, y).0;
            let rgb = [p[0], p[1], p[2]];
            all.push(rgb);
            if p[3] >= 250 {
                opaque.push(rgb);
            }
        }
    }
    if opaque.is_empty() { all } else { opaque }
}

fn median_rgb(samples: &[[u8; 3]]) -> [u8; 3] {
    let mut out = [0u8; 3];
    let mut channel = Vec::with_capacity(samples.len());
    for (c, slot) in out.iter_mut().enumerate() {
        channel.clear();
        channel.extend(samples.iter().map(|s| s[c]));
        channel.sort_unstable();
        *slot = channel[channel.len() / 2];
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::Rgba;

    fn solid(w: u32, h: u32, rgba: [u8; 4]) -> RgbaImage {
        RgbaImage::from_pixel(w, h, Rgba(rgba))
    }

    #[test]
    fn solid_white_is_perfectly_uniform() {
        let img = solid(20, 20, [255, 255, 255, 255]);
        let est = estimate_background(&img, DEFAULT_BORDER);
        assert_eq!(est.rgb, [255, 255, 255]);
        assert_eq!(est.uniformity, 1.0);
        assert!(est.is_uniform());
    }

    #[test]
    fn studio_gray_is_detected() {
        let img = solid(20, 20, [248, 248, 247, 255]);
        let est = estimate_background(&img, DEFAULT_BORDER);
        assert_eq!(est.rgb, [248, 248, 247]);
        assert!(est.is_uniform());
    }

    #[test]
    fn the_center_does_not_affect_the_estimate() {
        // 中央に商品があっても、外周だけを見ているので背景色は白のまま
        let mut img = solid(20, 20, [255, 255, 255, 255]);
        for y in 5..15 {
            for x in 5..15 {
                img.put_pixel(x, y, Rgba([10, 20, 30, 255]));
            }
        }
        let est = estimate_background(&img, DEFAULT_BORDER);
        assert_eq!(est.rgb, [255, 255, 255]);
        assert_eq!(est.uniformity, 1.0);
    }

    #[test]
    fn quantiles_describe_the_spread_of_a_patchy_border() {
        // 外周の大半が揃っていても一部だけ大きく外れている場合、uniformity は
        // 下がるが p50 は小さいままになる。この差が「bbox で切れば直る」か
        // 「単色背景ではない」かの判断材料になる
        let mut img = solid(20, 20, [250, 250, 250, 255]);
        for x in 0..4 {
            img.put_pixel(x, 0, image::Rgba([10, 10, 10, 255]));
        }
        let est = estimate_background(&img, 1);
        assert!(est.delta_e.p50 < 1.0, "大半は背景色どおり");
        assert!(est.delta_e.max > 50.0, "外れ値は max に出る");
        assert!(est.uniformity < 1.0);
    }

    #[test]
    fn a_uniform_border_has_no_spread() {
        let est = estimate_background(&solid(20, 20, [200, 200, 200, 255]), 2);
        assert_eq!(est.delta_e.p50, 0.0);
        assert_eq!(est.delta_e.p90, 0.0);
        assert_eq!(est.delta_e.max, 0.0);
    }

    #[test]
    fn a_split_border_reports_low_uniformity() {
        // 外周の左半分が黒、右半分が白。単色背景ではないと判定されるべき
        let mut img = solid(20, 20, [255, 255, 255, 255]);
        for y in 0..20 {
            for x in 0..10 {
                img.put_pixel(x, y, Rgba([0, 0, 0, 255]));
            }
        }
        let est = estimate_background(&img, DEFAULT_BORDER);
        assert!(est.uniformity < 0.9, "uniformity={}", est.uniformity);
        assert!(!est.is_uniform());
    }

    #[test]
    fn slight_sensor_noise_still_counts_as_uniform() {
        let mut img = solid(20, 20, [250, 250, 250, 255]);
        for x in 0..20 {
            img.put_pixel(x, 0, Rgba([252, 249, 251, 255]));
        }
        let est = estimate_background(&img, DEFAULT_BORDER);
        assert!(est.is_uniform(), "uniformity={}", est.uniformity);
    }

    #[test]
    fn transparent_borders_are_ignored_when_opaque_pixels_exist() {
        // 外周1pxが透明、その内側が白。透明部は背景色の情報を持たないので除外される
        let mut img = solid(20, 20, [255, 255, 255, 255]);
        for x in 0..20 {
            img.put_pixel(x, 0, Rgba([0, 0, 0, 0]));
            img.put_pixel(x, 19, Rgba([0, 0, 0, 0]));
        }
        for y in 0..20 {
            img.put_pixel(0, y, Rgba([0, 0, 0, 0]));
            img.put_pixel(19, y, Rgba([0, 0, 0, 0]));
        }
        let est = estimate_background(&img, 2);
        assert_eq!(est.rgb, [255, 255, 255]);
    }
}
