//! リサイズ。
//!
//! 出力寸法の決定（`plan`）と実際の画素処理（`apply`）を分けている。寸法計算は
//! 取り違えが起きやすく、かつ画像なしで網羅的に検証できるためである。

use fast_image_resize::images::{Image as FirImage, ImageRef as FirImageRef};
use fast_image_resize::{FilterType, PixelType, ResizeAlg, ResizeOptions, Resizer};
use image::RgbaImage;

use crate::error::{Error, ErrorCode, Result};

/// 指定した枠に対する当てはめ方。
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum FitMode {
    /// 枠に収まるよう縮小する。アスペクト比を保つ。結果は枠以下になる
    Contain,
    /// 枠を覆うよう縮小し、はみ出した分を中央で切り取る。結果は枠ちょうどになる
    Cover,
    /// アスペクト比を無視して枠ちょうどに変形する
    Exact,
}

#[derive(Debug, Clone)]
pub struct ResizeSpec {
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub fit: FitMode,
    /// 元画像より大きくすることを許すか。既定は不許可（ぼけた素材を作らないため）
    pub allow_upscale: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResizePlan {
    /// リサイズ後の寸法
    pub scaled: (u32, u32),
    /// cover で中央を切り取る場合の領域 (x, y, width, height)
    pub crop: Option<(u32, u32, u32, u32)>,
    /// 最終的な出力寸法
    pub output: (u32, u32),
}

/// 元寸法と指定から出力寸法を決める。
pub fn plan(source: (u32, u32), spec: &ResizeSpec) -> Result<ResizePlan> {
    let (sw, sh) = source;
    if sw == 0 || sh == 0 {
        return Err(Error::new(ErrorCode::EmptyImage, "画像の寸法が 0 です"));
    }
    if let Some(0) = spec.width {
        return Err(Error::new(
            ErrorCode::InvalidDimension,
            "width に 0 は指定できません",
        ));
    }
    if let Some(0) = spec.height {
        return Err(Error::new(
            ErrorCode::InvalidDimension,
            "height に 0 は指定できません",
        ));
    }

    let scaled = match (spec.width, spec.height) {
        (None, None) => {
            return Err(Error::new(
                ErrorCode::MissingDimension,
                "--width か --height の少なくとも一方を指定してください",
            )
            .with_hint("例: --width 1000"));
        }
        // 片方だけの指定は fit に関係なくアスペクト比を保つ
        (Some(w), None) => (w, scale_other(sh, w, sw)),
        (None, Some(h)) => (scale_other(sw, h, sh), h),
        (Some(w), Some(h)) => match spec.fit {
            FitMode::Exact => (w, h),
            FitMode::Contain | FitMode::Cover => {
                let rw = w as f64 / sw as f64;
                let rh = h as f64 / sh as f64;
                let ratio = if spec.fit == FitMode::Contain {
                    rw.min(rh)
                } else {
                    rw.max(rh)
                };
                (round_dim(sw as f64 * ratio), round_dim(sh as f64 * ratio))
            }
        },
    };

    if !spec.allow_upscale && (scaled.0 > sw || scaled.1 > sh) {
        return Err(Error::new(
            ErrorCode::UpscaleNotAllowed,
            format!(
                "{}x{} から {}x{} への拡大が必要です",
                sw, sh, scaled.0, scaled.1
            ),
        )
        .with_hint("--allow-upscale を付けると拡大しますが、画質は劣化します"));
    }

    // cover のときだけ、枠からはみ出た分を中央で切り取る
    let (crop, output) = match (spec.fit, spec.width, spec.height) {
        (FitMode::Cover, Some(w), Some(h)) => {
            let cw = w.min(scaled.0);
            let ch = h.min(scaled.1);
            let x = (scaled.0 - cw) / 2;
            let y = (scaled.1 - ch) / 2;
            (Some((x, y, cw, ch)), (cw, ch))
        }
        _ => (None, scaled),
    };

    Ok(ResizePlan {
        scaled,
        crop,
        output,
    })
}

/// 片方の辺だけ指定された場合に、もう片方をアスペクト比から求める。
fn scale_other(other: u32, target: u32, base: u32) -> u32 {
    round_dim(other as f64 * target as f64 / base as f64)
}

/// 寸法は 1px を下回らせない。0 は後段の全処理を壊すため。
fn round_dim(v: f64) -> u32 {
    (v.round() as u32).max(1)
}

/// 計画に従って実際にリサイズする。
pub fn apply(image: &RgbaImage, plan: &ResizePlan) -> Result<RgbaImage> {
    let resized = if (image.width(), image.height()) == plan.scaled {
        image.clone()
    } else {
        scale(image, plan.scaled)?
    };

    match plan.crop {
        Some((x, y, w, h)) => Ok(image::imageops::crop_imm(&resized, x, y, w, h).to_image()),
        None => Ok(resized),
    }
}

fn scale(image: &RgbaImage, to: (u32, u32)) -> Result<RgbaImage> {
    // 借用ビューで渡す。`from_vec_u8` に `as_raw().clone()` を渡すと、縮小の
    // ためだけに元画像のフル RGBA をもう 1 枚持つことになる。20MP で 98MB、
    // `info` のピーク RSS がそれだけで 1.7 倍になっていた。**`info` は
    // 「着手前の安い見立て」であり、そこが重くなるのは設計意図と食い違う。**
    // 読み出すだけなので所有権は要らない
    let src = FirImageRef::new(
        image.width(),
        image.height(),
        image.as_raw().as_slice(),
        PixelType::U8x4,
    )
    .map_err(|e| Error::new(ErrorCode::ResizeFailed, e.to_string()))?;

    let mut dst = FirImage::new(to.0, to.1, PixelType::U8x4);

    // use_alpha により事前乗算つきで補間される。これを怠ると透過の境界に
    // 背景色がにじむ（切り抜き後の画像で顕著に出る）。
    let options = ResizeOptions::new()
        .resize_alg(ResizeAlg::Convolution(FilterType::Lanczos3))
        .use_alpha(true);

    Resizer::new()
        .resize(&src, &mut dst, &options)
        .map_err(|e| Error::new(ErrorCode::ResizeFailed, e.to_string()))?;

    RgbaImage::from_raw(to.0, to.1, dst.into_vec())
        .ok_or_else(|| Error::new(ErrorCode::ResizeFailed, "リサイズ結果を復元できません"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(width: Option<u32>, height: Option<u32>, fit: FitMode) -> ResizeSpec {
        ResizeSpec {
            width,
            height,
            fit,
            allow_upscale: false,
        }
    }

    // --- 片方だけ指定 ---

    #[test]
    fn width_only_preserves_the_aspect_ratio() {
        let p = plan((1600, 2000), &spec(Some(800), None, FitMode::Contain)).unwrap();
        assert_eq!(p.output, (800, 1000));
        assert_eq!(p.crop, None);
    }

    #[test]
    fn height_only_preserves_the_aspect_ratio() {
        let p = plan((1600, 2000), &spec(None, Some(500), FitMode::Contain)).unwrap();
        assert_eq!(p.output, (400, 500));
    }

    #[test]
    fn a_single_dimension_ignores_the_fit_mode() {
        // 枠が片方しかない以上、当てはめようがないのでアスペクト比保持に倒す
        for fit in [FitMode::Contain, FitMode::Cover, FitMode::Exact] {
            let p = plan((1600, 2000), &spec(Some(800), None, fit)).unwrap();
            assert_eq!(p.output, (800, 1000), "fit={fit:?}");
        }
    }

    // --- fit モード ---

    #[test]
    fn contain_fits_inside_the_box() {
        // 縦長画像を正方形の枠に収めると、高さが枠いっぱいになる
        let p = plan((1600, 2000), &spec(Some(800), Some(800), FitMode::Contain)).unwrap();
        assert_eq!(p.output, (640, 800));
        assert!(p.output.0 <= 800 && p.output.1 <= 800);
    }

    #[test]
    fn cover_fills_the_box_and_crops_the_overflow() {
        let p = plan((1600, 2000), &spec(Some(800), Some(800), FitMode::Cover)).unwrap();
        assert_eq!(p.scaled, (800, 1000), "枠を覆うまで縮小される");
        assert_eq!(p.output, (800, 800), "結果は枠ちょうど");
        assert_eq!(p.crop, Some((0, 100, 800, 800)), "上下から均等に切られる");
    }

    #[test]
    fn exact_ignores_the_aspect_ratio() {
        let p = plan((1600, 2000), &spec(Some(800), Some(800), FitMode::Exact)).unwrap();
        assert_eq!(p.output, (800, 800));
        assert_eq!(p.crop, None);
    }

    #[test]
    fn a_square_source_is_unchanged_by_fit_mode() {
        for fit in [FitMode::Contain, FitMode::Cover, FitMode::Exact] {
            let p = plan((1000, 1000), &spec(Some(500), Some(500), fit)).unwrap();
            assert_eq!(p.output, (500, 500), "fit={fit:?}");
        }
    }

    // --- 拡大の禁止 ---

    #[test]
    fn upscaling_is_rejected_by_default() {
        let err = plan((800, 600), &spec(Some(1600), None, FitMode::Contain)).unwrap_err();
        assert_eq!(err.code.as_str(), "UPSCALE_NOT_ALLOWED");
        assert_eq!(err.exit_code(), 2);
        assert!(err.hint.unwrap().contains("--allow-upscale"));
    }

    #[test]
    fn upscaling_is_allowed_when_requested() {
        let mut s = spec(Some(1600), None, FitMode::Contain);
        s.allow_upscale = true;
        let p = plan((800, 600), &s).unwrap();
        assert_eq!(p.output, (1600, 1200));
    }

    #[test]
    fn shrinking_one_axis_while_growing_the_other_is_still_rejected() {
        // contain なら縮小に倒れるが、exact は片側が伸びるので弾かれるべき
        let err = plan((1000, 1000), &spec(Some(500), Some(2000), FitMode::Exact)).unwrap_err();
        assert_eq!(err.code.as_str(), "UPSCALE_NOT_ALLOWED");
    }

    #[test]
    fn contain_never_upscales_so_it_passes() {
        // 枠が元より大きくても contain なら縮小率が 1 未満に決まらない…わけではない。
        // 枠 2000x100 に 1000x1000 を収めると 100x100 になるため通る
        let p = plan((1000, 1000), &spec(Some(2000), Some(100), FitMode::Contain)).unwrap();
        assert_eq!(p.output, (100, 100));
    }

    // --- 異常系 ---

    #[test]
    fn missing_both_dimensions_is_an_argument_error() {
        let err = plan((100, 100), &spec(None, None, FitMode::Contain)).unwrap_err();
        assert_eq!(err.code.as_str(), "MISSING_DIMENSION");
        assert_eq!(err.exit_code(), 2);
    }

    #[test]
    fn zero_dimensions_are_rejected() {
        assert_eq!(
            plan((100, 100), &spec(Some(0), None, FitMode::Contain))
                .unwrap_err()
                .code
                .as_str(),
            "INVALID_DIMENSION"
        );
        assert_eq!(
            plan((100, 100), &spec(None, Some(0), FitMode::Contain))
                .unwrap_err()
                .code
                .as_str(),
            "INVALID_DIMENSION"
        );
    }

    #[test]
    fn extreme_shrink_never_reaches_zero() {
        // 1px を下回ると後続の処理が全て壊れるため 1 で止める
        let p = plan((4000, 10), &spec(Some(2), None, FitMode::Contain)).unwrap();
        assert_eq!(p.output, (2, 1));
    }

    // --- 実際の画素処理 ---

    #[test]
    fn apply_produces_the_planned_dimensions() {
        let img = RgbaImage::from_pixel(400, 200, image::Rgba([120, 60, 30, 255]));
        let p = plan((400, 200), &spec(Some(100), None, FitMode::Contain)).unwrap();
        let out = apply(&img, &p).unwrap();
        assert_eq!((out.width(), out.height()), (100, 50));
    }

    #[test]
    fn apply_crops_to_the_box_in_cover_mode() {
        let img = RgbaImage::from_pixel(400, 200, image::Rgba([120, 60, 30, 255]));
        let p = plan((400, 200), &spec(Some(100), Some(100), FitMode::Cover)).unwrap();
        let out = apply(&img, &p).unwrap();
        assert_eq!((out.width(), out.height()), (100, 100));
    }

    #[test]
    fn a_flat_color_survives_resizing() {
        let img = RgbaImage::from_pixel(400, 400, image::Rgba([120, 60, 30, 255]));
        let p = plan((400, 400), &spec(Some(100), None, FitMode::Contain)).unwrap();
        let out = apply(&img, &p).unwrap();
        let center = out.get_pixel(50, 50).0;
        assert!(center[0].abs_diff(120) <= 1 && center[1].abs_diff(60) <= 1);
        assert_eq!(center[3], 255);
    }

    #[test]
    fn transparent_areas_do_not_bleed_into_the_subject() {
        // 左半分が不透明な赤、右半分が完全透明。事前乗算なしで補間すると
        // 境界に黒(0,0,0)がにじむので、それが起きていないことを確かめる
        let mut img = RgbaImage::new(200, 20);
        for y in 0..20 {
            for x in 0..200 {
                let p = if x < 100 {
                    image::Rgba([255, 0, 0, 255])
                } else {
                    image::Rgba([0, 0, 0, 0])
                };
                img.put_pixel(x, y, p);
            }
        }
        let p = plan((200, 20), &spec(Some(100), None, FitMode::Contain)).unwrap();
        let out = apply(&img, &p).unwrap();

        // 不透明側の内部は赤のまま
        let inside = out.get_pixel(20, 5).0;
        assert_eq!(inside[3], 255);
        assert!(
            inside[0] > 240 && inside[1] < 15,
            "赤がくすんでいる: {inside:?}"
        );
    }

    #[test]
    fn resizing_to_the_same_size_is_a_no_op() {
        let img = RgbaImage::from_pixel(64, 64, image::Rgba([1, 2, 3, 255]));
        let p = plan((64, 64), &spec(Some(64), None, FitMode::Contain)).unwrap();
        let out = apply(&img, &p).unwrap();
        assert_eq!(out.as_raw(), img.as_raw());
    }
}
