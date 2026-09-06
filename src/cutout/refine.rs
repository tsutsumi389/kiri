//! 境界帯でのアルファ再推定と色の復元。
//!
//! 従来は二値マスクを箱ぼかしして階調を作っていた（`feather`）。しかしそれは
//! **マスクの形**から作ったアルファであって、画像に写っている本当のエッジ位置とは
//! 無関係である。実際に次の2つが起きていた。
//!
//! 1. エッジ堤防（`floodfill`）は Sobel 勾配が境界の両側 1px に立つため、背景側の
//!    画素まで背景候補から外して前景にしてしまう。この 1px の縁は色が完全に背景
//!    なので、フェザリングで半透明にしても `despill` の復元式は背景色を返す。
//!    白以外の下地に載せると光輪（ハロー）になる。
//! 2. 柔らかい輪郭では堤防が混色のかなり背景寄りで止まるため、遷移全体がほぼ
//!    不透明のまま残る。
//!
//! ここでは代わりに、境界帯の各画素で「近傍の確定前景色 F」と「近傍の確定背景色 B」を
//! 求め、観測色 C を F–B 直線へ射影してアルファを決める。合成式 C = aF + (1-a)B を
//! そのまま解くので、アルファは画像の中身から決まる。
//!
//! ```text
//! a = clamp( (C-B)・(F-B) / |F-B|² , 0, 1 )
//! ```
//!
//! 計算は線形 RGB で行う。合成は光の量の足し算であり、ガンマの掛かった sRGB 値の
//! ままでは混色が直線に乗らないためである。
//!
//! F と B の色差が小さい画素（淡い商品 × 白背景）では、この射影は雑音を拾うだけで
//! 何も決められない。そこだけ従来の幾何的フェザーへ落とす。

use image::RgbaImage;

use crate::color::lab::delta_e_rgb;
use crate::cutout::feather;
use crate::cutout::mask::Mask;

/// 帯幅の下限(px)。くっきりした輪郭でも、堤防が残す 1px の縁と JPEG の滲みを
/// 跨げるだけの幅が要る。
pub const DEFAULT_MIN_RADIUS: u32 = 2;
/// 帯幅の上限(px)。柔らかい輪郭に追従させるが、青天井にすると計算量が跳ね、
/// 商品内部まで帯に飲み込まれる。
pub const DEFAULT_MAX_RADIUS: u32 = 10;
/// 線形 RGB のユークリッド距離で、この値を下回る F–B は「色では決められない」。
pub const DEFAULT_MIN_SEPARATION: f32 = 0.06;

/// 窓の半径の上限(px)。帯幅から決まる窓が大きくなりすぎると、確定前景・確定背景の
/// 平均が局所性を失ううえ、走査量が二乗で効いてくる。
const MAX_WINDOW: u32 = 16;

/// 観測色が参照色に「収束した」とみなす色差。CIE76 で 2 前後が見分けの限界。
const CONVERGED: f64 = 2.0;

/// 復元した前景色を局所前景色と観測色の範囲からどれだけはみ出させるか（線形値）。
const RECOVER_SLACK: f32 = 0.05;

/// 復元式の分母の下限。これ以下では誤差が何十倍にも増幅されて色が暴れる。
const MIN_RECOVER_ALPHA: f32 = 0.05;

#[derive(Debug, Clone)]
pub struct RefineOptions {
    pub min_radius: u32,
    pub max_radius: u32,
    pub min_separation: f32,
    /// 色で決められない画素に使う幾何的フェザーの半径
    pub feather: u32,
    /// 境界画素の色から背景色の寄与を取り除くか
    pub despill: bool,
}

impl Default for RefineOptions {
    fn default() -> Self {
        Self {
            min_radius: DEFAULT_MIN_RADIUS,
            max_radius: DEFAULT_MAX_RADIUS,
            min_separation: DEFAULT_MIN_SEPARATION,
            feather: 1,
            despill: true,
        }
    }
}

pub struct Refined {
    /// 境界画素の色を復元した画像。アルファは書き換えていない
    pub image: RgbaImage,
    pub mask: Mask,
}

/// sRGB 8bit → 線形 RGB の変換表。境界帯では同じ変換を何十回も引くため。
fn srgb_lut() -> [f32; 256] {
    let mut lut = [0f32; 256];
    for (i, slot) in lut.iter_mut().enumerate() {
        let c = i as f32 / 255.0;
        *slot = if c <= 0.040_45 {
            c / 12.92
        } else {
            ((c + 0.055) / 1.055).powf(2.4)
        };
    }
    lut
}

fn linear_to_srgb(v: f32) -> u8 {
    let c = v.clamp(0.0, 1.0);
    let s = if c <= 0.003_130_8 {
        c * 12.92
    } else {
        1.055 * c.powf(1.0 / 2.4) - 0.055
    };
    (s * 255.0).round() as u8
}

/// 境界帯のアルファを画像の色から推定し直す。
pub fn refine(
    image: &RgbaImage,
    binary: &Mask,
    background: [u8; 3],
    opts: &RefineOptions,
) -> Refined {
    let (w, h) = (image.width(), image.height());
    let mut out = image.clone();
    let mut mask = binary.clone();
    if w == 0 || h == 0 || w != binary.width() || h != binary.height() {
        return Refined { image: out, mask };
    }

    let band = band_map(image, binary, background, opts);
    if band.iter().all(|&r| r == 0) {
        return Refined { image: out, mask };
    }

    let lut = srgb_lut();
    let fallback = feather::feather(binary, opts.feather);
    let bg_linear = [
        lut[background[0] as usize],
        lut[background[1] as usize],
        lut[background[2] as usize],
    ];
    let separation_sq = opts.min_separation * opts.min_separation;

    let stride = w as usize;
    for y in 0..h {
        for x in 0..w {
            let i = (y as usize) * stride + (x as usize);
            let radius = band[i];
            if radius == 0 {
                continue;
            }

            // 帯の外側の端から確定前景まで届く窓が要る。帯幅 r の帯の一番外側の
            // 画素から確定前景までは 2r+1 あるため、窓はそれ以上に取る
            let window = (2 * u32::from(radius) + 3).min(MAX_WINDOW);
            let s = sample_window(image, binary, &band, &lut, x, y, window);

            let observed = pixel_linear(image, &lut, x, y);
            let b = s.background.unwrap_or(bg_linear);
            let Some(f) = s.foreground else {
                mask.set(x, y, fallback.get(x, y));
                continue;
            };

            let d = [f[0] - b[0], f[1] - b[1], f[2] - b[2]];
            let dd = d[0] * d[0] + d[1] * d[1] + d[2] * d[2];
            if dd < separation_sq {
                // 色では決められない。幾何的フェザーへ落とす
                mask.set(x, y, fallback.get(x, y));
                continue;
            }

            let projected = ((observed[0] - b[0]) * d[0]
                + (observed[1] - b[1]) * d[1]
                + (observed[2] - b[2]) * d[2])
                / dd;
            let alpha = projected.clamp(0.0, 1.0);
            let alpha8 = (alpha * 255.0).round() as u8;
            mask.set(x, y, alpha8);

            if !opts.despill || alpha8 == 0 {
                continue;
            }
            let pixel = out.get_pixel_mut(x, y);
            for k in 0..3 {
                // 合成式を F について解く。線形光で行うのが要点
                let recovered = b[k] + (observed[k] - b[k]) / alpha.max(MIN_RECOVER_ALPHA);
                // 局所前景色と観測色が挟む範囲から大きく外れさせない。
                // 分母が小さいところで誤差が増幅されて色が飛ぶのを抑える
                let lo = f[k].min(observed[k]) - RECOVER_SLACK;
                let hi = f[k].max(observed[k]) + RECOVER_SLACK;
                pixel[k] = linear_to_srgb(recovered.clamp(lo, hi));
            }
        }
    }

    Refined { image: out, mask }
}

fn pixel_linear(image: &RgbaImage, lut: &[f32; 256], x: u32, y: u32) -> [f32; 3] {
    let p = image.get_pixel(x, y).0;
    [lut[p[0] as usize], lut[p[1] as usize], lut[p[2] as usize]]
}

#[derive(Default)]
struct WindowSamples {
    foreground: Option<[f32; 3]>,
    background: Option<[f32; 3]>,
}

/// 窓内の確定前景色と確定背景色を集める。
///
/// 確定前景が1つも無い場合（幅が帯より細い構造）は、窓の中でマスクに入っている
/// 画素のうち背景から最も離れた色を前景の代わりにする。細いストラップを erosion で
/// 失うと F の推定元が遠くなり、そこだけハローが残るためである。
fn sample_window(
    image: &RgbaImage,
    binary: &Mask,
    band: &[u8],
    lut: &[f32; 256],
    x: u32,
    y: u32,
    window: u32,
) -> WindowSamples {
    let (w, h) = (image.width(), image.height());
    let stride = w as usize;
    let r = window as i64;
    let x0 = (x as i64 - r).max(0) as u32;
    let x1 = ((x as i64 + r) as u32).min(w - 1);
    let y0 = (y as i64 - r).max(0) as u32;
    let y1 = ((y as i64 + r) as u32).min(h - 1);

    let mut fg_sum = [0f32; 3];
    let mut fg_n = 0u32;
    let mut bg_sum = [0f32; 3];
    let mut bg_n = 0u32;
    // 確定前景が無かったときの代役を探すための、背景から最も遠い色
    let mut far_best = 0f32;

    for ny in y0..=y1 {
        for nx in x0..=x1 {
            let i = (ny as usize) * stride + (nx as usize);
            let inside = binary.is_foreground(nx, ny);
            if band[i] != 0 {
                continue;
            }
            let c = pixel_linear(image, lut, nx, ny);
            if inside {
                for k in 0..3 {
                    fg_sum[k] += c[k];
                }
                fg_n += 1;
            } else {
                for k in 0..3 {
                    bg_sum[k] += c[k];
                }
                bg_n += 1;
            }
        }
    }

    let background = (bg_n > 0).then(|| {
        let n = bg_n as f32;
        [bg_sum[0] / n, bg_sum[1] / n, bg_sum[2] / n]
    });

    if fg_n > 0 {
        let n = fg_n as f32;
        return WindowSamples {
            foreground: Some([fg_sum[0] / n, fg_sum[1] / n, fg_sum[2] / n]),
            background,
        };
    }

    // 代役探し。まず最遠距離を測り、次にそれに近い画素だけを平均する。
    // 1画素だけを採るとノイズと JPEG のリンギングをそのまま拾うため
    let reference = background.unwrap_or([1.0, 1.0, 1.0]);
    let distance = |c: [f32; 3]| -> f32 {
        let d = [
            c[0] - reference[0],
            c[1] - reference[1],
            c[2] - reference[2],
        ];
        d[0] * d[0] + d[1] * d[1] + d[2] * d[2]
    };
    for ny in y0..=y1 {
        for nx in x0..=x1 {
            if !binary.is_foreground(nx, ny) {
                continue;
            }
            far_best = far_best.max(distance(pixel_linear(image, lut, nx, ny)));
        }
    }
    if far_best <= 0.0 {
        return WindowSamples {
            foreground: None,
            background,
        };
    }
    let cut = far_best * 0.64;
    let mut sum = [0f32; 3];
    let mut n = 0u32;
    for ny in y0..=y1 {
        for nx in x0..=x1 {
            if !binary.is_foreground(nx, ny) {
                continue;
            }
            let c = pixel_linear(image, lut, nx, ny);
            if distance(c) >= cut {
                for k in 0..3 {
                    sum[k] += c[k];
                }
                n += 1;
            }
        }
    }
    let foreground = (n > 0).then(|| {
        let n = n as f32;
        [sum[0] / n, sum[1] / n, sum[2] / n]
    });
    WindowSamples {
        foreground,
        background,
    }
}

/// 画素ごとの帯幅を返す。0 は帯の外。
///
/// 帯幅は輪郭ごとに測る。くっきりした輪郭に 10px の帯を張れば商品の内側まで
/// 巻き込むし、8px かけて溶ける輪郭に 2px の帯では遷移を跨げない。
fn band_map(
    image: &RgbaImage,
    binary: &Mask,
    background: [u8; 3],
    opts: &RefineOptions,
) -> Vec<u8> {
    let (w, h) = (binary.width(), binary.height());
    let stride = w as usize;
    let mut band = vec![0u8; stride * (h as usize)];
    let min_r = opts.min_radius.max(1);
    let max_r = opts.max_radius.max(min_r);

    let mut edges: Vec<(u32, u32, u32)> = Vec::new();
    for y in 0..h {
        for x in 0..w {
            if !binary.is_foreground(x, y) || !binary.touches_background(x, y) {
                continue;
            }
            let Some(normal) = binary.outward_normal(x, y) else {
                continue;
            };
            let width = transition_width(image, binary, background, x, y, normal, min_r, max_r);
            edges.push((x, y, width));
        }
    }

    for (ex, ey, width) in edges {
        let r = width as i64;
        let x0 = (ex as i64 - r).max(0) as u32;
        let x1 = ((ex as i64 + r) as u32).min(w - 1);
        let y0 = (ey as i64 - r).max(0) as u32;
        let y1 = ((ey as i64 + r) as u32).min(h - 1);
        for ny in y0..=y1 {
            for nx in x0..=x1 {
                let dx = nx as i64 - ex as i64;
                let dy = ny as i64 - ey as i64;
                if dx * dx + dy * dy > r * r {
                    continue;
                }
                let i = (ny as usize) * stride + (nx as usize);
                band[i] = band[i].max(width as u8);
            }
        }
    }
    band
}

/// 境界画素での遷移幅。法線方向に外へ／内へ進み、色が収束するまでの距離を測る。
///
/// 外側の参照色は「帯の外端で実際に観測される色」を使う。大域の背景色を使うと、
/// 落ち影や照明ムラのある場所で永久に収束せず帯が最大幅に張り付いてしまう。
/// 内側も同様に、その方向で最も深い前景画素の色を参照にする。
#[allow(clippy::too_many_arguments)]
fn transition_width(
    image: &RgbaImage,
    binary: &Mask,
    background: [u8; 3],
    x: u32,
    y: u32,
    normal: [f32; 2],
    min_r: u32,
    max_r: u32,
) -> u32 {
    let (w, h) = (image.width(), image.height());
    let at = |t: f32| -> Option<(u32, u32)> {
        let sx = (x as f32 + normal[0] * t).round();
        let sy = (y as f32 + normal[1] * t).round();
        if sx < 0.0 || sy < 0.0 || sx >= w as f32 || sy >= h as f32 {
            return None;
        }
        Some((sx as u32, sy as u32))
    };
    let rgb = |(px, py): (u32, u32)| -> [u8; 3] {
        let p = image.get_pixel(px, py).0;
        [p[0], p[1], p[2]]
    };

    // 外側: 背景の参照色を最外から拾い、そこへ収束する距離を測る
    let mut outer_ref = background;
    for t in (1..=max_r).rev() {
        if let Some(q) = at(t as f32) {
            if !binary.is_foreground(q.0, q.1) {
                outer_ref = rgb(q);
                break;
            }
        }
    }
    let mut out_width = max_r;
    for t in 1..=max_r {
        if let Some(q) = at(t as f32) {
            if delta_e_rgb(rgb(q), outer_ref) <= CONVERGED {
                out_width = t;
                break;
            }
        }
    }

    // 内側: 最も深い前景画素を参照にする
    let mut inner_ref = None;
    for t in (1..=max_r).rev() {
        if let Some(q) = at(-(t as f32)) {
            if binary.is_foreground(q.0, q.1) {
                inner_ref = Some(rgb(q));
                break;
            }
        }
    }
    let in_width = match inner_ref {
        None => min_r,
        Some(reference) => {
            let mut found = max_r;
            for t in 1..=max_r {
                if let Some(q) = at(-(t as f32)) {
                    if delta_e_rgb(rgb(q), reference) <= CONVERGED {
                        found = t;
                        break;
                    }
                }
            }
            found
        }
    };

    out_width.max(in_width).clamp(min_r, max_r)
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::Rgba;

    /// 光の量で混色する。撮像素子が画素の面積で光を積分した結果を作るので、
    /// ガンマの掛かった sRGB 値のまま線形補間してはいけない。
    fn mix(product: [u8; 3], background: [u8; 3], coverage: f32) -> [u8; 4] {
        let lut = srgb_lut();
        let mut out = [255u8; 4];
        for k in 0..3 {
            let f = lut[product[k] as usize];
            let b = lut[background[k] as usize];
            out[k] = linear_to_srgb(f * coverage + b * (1.0 - coverage));
        }
        out
    }

    /// 左半分が商品、右半分が背景。境界の 1 列だけが指定した被覆率で混ざる。
    fn ramp(product: [u8; 3], background: [u8; 3], coverage: f32) -> (RgbaImage, Mask) {
        let (w, h) = (40u32, 12u32);
        let mut img = RgbaImage::from_pixel(
            w,
            h,
            Rgba([background[0], background[1], background[2], 255]),
        );
        let mut mask = Mask::new(w, h, 0);
        for y in 0..h {
            for x in 0..w {
                if x < 20 {
                    img.put_pixel(x, y, Rgba([product[0], product[1], product[2], 255]));
                    mask.set(x, y, 255);
                } else if x == 20 {
                    img.put_pixel(x, y, Rgba(mix(product, background, coverage)));
                    // マスクは混色画素まで前景に含めている（堤防が作る 1px の縁を模す）
                    mask.set(x, y, 255);
                }
            }
        }
        (img, mask)
    }

    #[test]
    fn a_half_covered_pixel_gets_about_half_alpha() {
        let (img, mask) = ramp([40, 40, 45], [250, 250, 249], 0.5);
        let out = refine(&img, &mask, [250, 250, 249], &RefineOptions::default());
        let a = out.mask.get(20, 6);
        assert!(
            (100..=155).contains(&a),
            "混色画素のアルファが半分になっていない: {a}"
        );
    }

    #[test]
    fn a_pure_background_pixel_inside_the_mask_becomes_transparent() {
        // 堤防が前景に含めてしまった、色が完全に背景の縁。ここが不透明で残ると
        // 白以外の下地でハローになる
        let (img, mask) = ramp([40, 40, 45], [250, 250, 249], 0.0);
        let out = refine(&img, &mask, [250, 250, 249], &RefineOptions::default());
        assert_eq!(
            out.mask.get(20, 6),
            0,
            "背景色のままの画素が透明になっていない"
        );
    }

    #[test]
    fn the_interior_and_the_exterior_are_left_alone() {
        let (img, mask) = ramp([40, 40, 45], [250, 250, 249], 0.5);
        let out = refine(&img, &mask, [250, 250, 249], &RefineOptions::default());
        assert_eq!(out.mask.get(2, 6), 255, "内部が薄くなっている");
        assert_eq!(out.mask.get(38, 6), 0, "外部に色が漏れている");
    }

    #[test]
    fn the_recovered_colour_drops_the_background_tint() {
        let (img, mask) = ramp([40, 40, 45], [250, 250, 249], 0.5);
        let out = refine(&img, &mask, [250, 250, 249], &RefineOptions::default());
        let p = out.image.get_pixel(20, 6).0;
        assert!(
            p[0] < 120,
            "境界画素に背景色が残っている: {:?}",
            [p[0], p[1], p[2]]
        );
    }

    #[test]
    fn despill_can_be_switched_off() {
        let (img, mask) = ramp([40, 40, 45], [250, 250, 249], 0.5);
        let opts = RefineOptions {
            despill: false,
            ..Default::default()
        };
        let out = refine(&img, &mask, [250, 250, 249], &opts);
        assert_eq!(
            out.image.get_pixel(20, 6).0,
            img.get_pixel(20, 6).0,
            "--no-despill でも色が書き換わっている"
        );
    }

    #[test]
    fn a_product_the_same_colour_as_the_background_falls_back_to_the_feather() {
        // F と B が近すぎて射影が雑音を拾うだけの場合。従来の幾何的フェザーの
        // 値がそのまま出ることを確かめる
        let (img, mask) = ramp([249, 249, 248], [250, 250, 249], 0.5);
        let out = refine(&img, &mask, [250, 250, 249], &RefineOptions::default());
        let expected = feather::feather(&mask, 1);
        assert_eq!(out.mask.get(20, 6), expected.get(20, 6));
    }

    #[test]
    fn an_empty_mask_is_untouched() {
        let img = RgbaImage::from_pixel(8, 8, Rgba([250, 250, 249, 255]));
        let mask = Mask::new(8, 8, 0);
        let out = refine(&img, &mask, [250, 250, 249], &RefineOptions::default());
        assert_eq!(out.mask, mask);
        assert_eq!(out.image.as_raw(), img.as_raw());
    }

    #[test]
    fn a_full_mask_is_untouched() {
        // 見切れて画像いっぱいに広がった商品。境界が無いので帯も立たない
        let img = RgbaImage::from_pixel(8, 8, Rgba([40, 40, 45, 255]));
        let mask = Mask::new(8, 8, 255);
        let out = refine(&img, &mask, [250, 250, 249], &RefineOptions::default());
        assert_eq!(out.mask, mask);
    }

    #[test]
    fn mismatched_dimensions_are_refused_rather_than_panicking() {
        let img = RgbaImage::from_pixel(8, 8, Rgba([0, 0, 0, 255]));
        let mask = Mask::new(10, 10, 255);
        let out = refine(&img, &mask, [0; 3], &RefineOptions::default());
        assert_eq!(out.mask, mask);
    }

    #[test]
    fn a_soft_contour_gets_a_wider_band_than_a_hard_one() {
        // 帯幅が輪郭の柔らかさに追従すること。追従しなければ、柔らかい輪郭では
        // 遷移の外側だけを見て不透明のまま残す
        let make = |softness: f32| -> Vec<u8> {
            let (w, h) = (60u32, 12u32);
            let mut img = RgbaImage::new(w, h);
            let mut mask = Mask::new(w, h, 0);
            for y in 0..h {
                for x in 0..w {
                    let t = ((x as f32 - 30.0) / softness + 0.5).clamp(0.0, 1.0);
                    let v = (40.0 * (1.0 - t) + 250.0 * t).round() as u8;
                    img.put_pixel(x, y, Rgba([v, v, v, 255]));
                    // 遷移の背景寄りで止まったマスクを模す
                    mask.set(x, y, if (x as f32) < 30.0 + softness { 255 } else { 0 });
                }
            }
            band_map(&img, &mask, [250, 250, 250], &RefineOptions::default())
        };
        let hard = make(1.0).iter().filter(|&&r| r > 0).count();
        let soft = make(8.0).iter().filter(|&&r| r > 0).count();
        assert!(
            soft > hard,
            "柔らかい輪郭で帯が広がっていない: {soft} <= {hard}"
        );
    }
}
