//! キャンバス配置。
//!
//! EC では商品画像のサイズを統一することが求められ、さらに「商品がフレームの
//! 何割を占めるか」まで規定されていることが多い。切り抜いた商品を指定サイズの
//! 中央に、指定した占有率で配置する。
//!
//! 複数商品を同じ設定で処理すれば、元画像の構図がばらついていても並べたときの
//! 見た目が揃う。これが `--fill-ratio` の目的。

use image::RgbaImage;

use crate::error::{Error, ErrorCode, Result};
use crate::transform::resize::{FitMode, ResizeSpec, apply as resize_apply, plan as resize_plan};

#[derive(Debug, Clone)]
pub struct CanvasSpec {
    pub width: u32,
    pub height: u32,
    /// 商品がキャンバスの何割を占めるか (0.0-1.0)
    pub fill_ratio: f64,
    /// キャンバスの下地。None なら透明のまま
    pub background: Option<[u8; 3]>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct CanvasPlan {
    /// 配置される商品の寸法
    pub content: (u32, u32),
    /// キャンバス左上からの配置位置
    pub offset: (u32, u32),
    /// 元の商品寸法に対する倍率
    pub scale: f64,
}

/// 商品をキャンバス中央に、指定の占有率で収める配置を決める。
pub fn plan(content: (u32, u32), spec: &CanvasSpec) -> Result<CanvasPlan> {
    if spec.width == 0 || spec.height == 0 {
        return Err(Error::new(
            ErrorCode::InvalidCanvas,
            "キャンバスの寸法に 0 は指定できません",
        ));
    }
    if !(spec.fill_ratio > 0.0 && spec.fill_ratio <= 1.0) {
        return Err(Error::new(
            ErrorCode::InvalidFillRatio,
            format!(
                "fill-ratio は 0.0 より大きく 1.0 以下である必要があります（指定: {}）",
                spec.fill_ratio
            ),
        ));
    }
    if content.0 == 0 || content.1 == 0 {
        return Err(Error::new(
            ErrorCode::EmptyContent,
            "配置する内容の寸法が 0 です",
        ));
    }

    // 縦横どちらも占有率の枠に収まるよう、小さいほうの倍率を採る
    let box_w = f64::from(spec.width) * spec.fill_ratio;
    let box_h = f64::from(spec.height) * spec.fill_ratio;
    let scale = (box_w / f64::from(content.0)).min(box_h / f64::from(content.1));

    let w = ((f64::from(content.0) * scale).round() as u32).clamp(1, spec.width);
    let h = ((f64::from(content.1) * scale).round() as u32).clamp(1, spec.height);

    Ok(CanvasPlan {
        content: (w, h),
        offset: ((spec.width - w) / 2, (spec.height - h) / 2),
        scale,
    })
}

/// 商品画像をキャンバスへ配置する。`image` は既に商品の範囲へ切り詰めてあること。
pub fn apply(content: &RgbaImage, spec: &CanvasSpec) -> Result<RgbaImage> {
    let plan = plan((content.width(), content.height()), spec)?;

    let scaled = {
        let resize = ResizeSpec {
            width: Some(plan.content.0),
            height: Some(plan.content.1),
            fit: FitMode::Exact,
            // 配置に必要な拡縮は要求そのものなので、ここでは拡大を禁じない。
            // 呼び出し側が倍率を見て警告を出す
            allow_upscale: true,
        };
        let p = resize_plan((content.width(), content.height()), &resize)?;
        resize_apply(content, &p)?
    };

    let mut canvas = match spec.background {
        Some([r, g, b]) => {
            RgbaImage::from_pixel(spec.width, spec.height, image::Rgba([r, g, b, 255]))
        }
        None => RgbaImage::new(spec.width, spec.height),
    };

    composite(&mut canvas, &scaled, plan.offset);
    Ok(canvas)
}

/// アルファ合成でキャンバスへ載せる。
fn composite(canvas: &mut RgbaImage, src: &RgbaImage, offset: (u32, u32)) {
    for y in 0..src.height() {
        for x in 0..src.width() {
            let (cx, cy) = (offset.0 + x, offset.1 + y);
            if cx >= canvas.width() || cy >= canvas.height() {
                continue;
            }
            let s = *src.get_pixel(x, y);
            let d = canvas.get_pixel_mut(cx, cy);
            let sa = u32::from(s[3]);
            if sa == 255 {
                *d = s;
                continue;
            }
            if sa == 0 {
                continue;
            }
            let da = u32::from(d[3]);
            let out_a = sa + da * (255 - sa) / 255;
            for c in 0..3 {
                let sc = u32::from(s[c]) * sa;
                let dc = u32::from(d[c]) * da * (255 - sa) / 255;
                // sa == 0 は上で弾いているので out_a は 0 にならないが、念のため守る
                d[c] = (sc + dc).checked_div(out_a).unwrap_or(0) as u8;
            }
            d[3] = out_a as u8;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(w: u32, h: u32, fill: f64) -> CanvasSpec {
        CanvasSpec {
            width: w,
            height: h,
            fill_ratio: fill,
            background: None,
        }
    }

    #[test]
    fn a_square_product_is_centred_at_the_requested_ratio() {
        let p = plan((500, 500), &spec(1000, 1000, 0.85)).unwrap();
        assert_eq!(p.content, (850, 850));
        assert_eq!(p.offset, (75, 75), "左右上下の余白が均等でない");
    }

    #[test]
    fn a_tall_product_is_limited_by_the_canvas_height() {
        // 縦長の商品は高さが先に枠へ届く
        let p = plan((200, 400), &spec(1000, 1000, 0.8)).unwrap();
        assert_eq!(p.content, (400, 800));
        assert_eq!(p.offset, (300, 100));
    }

    #[test]
    fn a_wide_product_is_limited_by_the_canvas_width() {
        let p = plan((400, 200), &spec(1000, 1000, 0.8)).unwrap();
        assert_eq!(p.content, (800, 400));
        assert_eq!(p.offset, (100, 300));
    }

    #[test]
    fn the_aspect_ratio_is_preserved() {
        let p = plan((300, 900), &spec(1000, 1000, 0.9)).unwrap();
        let before = 300.0 / 900.0;
        let after = f64::from(p.content.0) / f64::from(p.content.1);
        assert!(
            (before - after).abs() < 0.01,
            "比率が崩れている: {before} -> {after}"
        );
    }

    #[test]
    fn a_full_ratio_touches_the_canvas_edges() {
        let p = plan((500, 500), &spec(1000, 1000, 1.0)).unwrap();
        assert_eq!(p.content, (1000, 1000));
        assert_eq!(p.offset, (0, 0));
    }

    #[test]
    fn a_small_product_is_scaled_up_to_fill_the_canvas() {
        // キャンバス配置では拡大が要求そのものなので通る。倍率は呼び出し側が見る
        let p = plan((100, 100), &spec(1000, 1000, 0.85)).unwrap();
        assert_eq!(p.content, (850, 850));
        assert!(
            (p.scale - 8.5).abs() < 0.01,
            "倍率が報告されていない: {}",
            p.scale
        );
    }

    #[test]
    fn a_non_square_canvas_works() {
        let p = plan((100, 100), &spec(800, 1200, 0.5)).unwrap();
        assert_eq!(p.content, (400, 400), "短辺側の枠に収まるべき");
        assert_eq!(p.offset, (200, 400));
    }

    #[test]
    fn invalid_ratios_are_rejected() {
        for bad in [0.0, -0.5, 1.5] {
            let err = plan((100, 100), &spec(500, 500, bad)).unwrap_err();
            assert_eq!(err.code.as_str(), "INVALID_FILL_RATIO", "fill_ratio={bad}");
            assert_eq!(err.exit_code(), 2);
        }
    }

    #[test]
    fn a_zero_sized_canvas_is_rejected() {
        assert_eq!(
            plan((100, 100), &spec(0, 500, 0.9))
                .unwrap_err()
                .code
                .as_str(),
            "INVALID_CANVAS"
        );
        assert_eq!(
            plan((100, 100), &spec(500, 0, 0.9))
                .unwrap_err()
                .code
                .as_str(),
            "INVALID_CANVAS"
        );
    }

    // --- 実際の配置 ---

    #[test]
    fn apply_produces_the_canvas_size_with_transparent_margins() {
        let product = RgbaImage::from_pixel(50, 50, image::Rgba([200, 60, 40, 255]));
        let out = apply(&product, &spec(200, 200, 0.5)).unwrap();

        assert_eq!((out.width(), out.height()), (200, 200));
        assert_eq!(out.get_pixel(2, 2).0[3], 0, "余白が透明でない");
        assert_eq!(out.get_pixel(100, 100).0[3], 255, "中央に商品が無い");
    }

    #[test]
    fn apply_fills_the_canvas_when_a_background_is_given() {
        let product = RgbaImage::from_pixel(50, 50, image::Rgba([200, 60, 40, 255]));
        let mut s = spec(200, 200, 0.5);
        s.background = Some([255, 255, 255]);
        let out = apply(&product, &s).unwrap();

        assert_eq!(
            out.get_pixel(2, 2).0,
            [255, 255, 255, 255],
            "余白が白で埋まっていない"
        );
        assert_eq!(out.get_pixel(100, 100).0[3], 255);
    }

    #[test]
    fn the_product_is_centred_within_one_pixel() {
        let product = RgbaImage::from_pixel(40, 40, image::Rgba([10, 20, 30, 255]));
        let out = apply(&product, &spec(101, 101, 0.5)).unwrap();
        // 奇数サイズでも左右の余白差が 1px を超えないこと
        let opaque_x: Vec<u32> = (0..101)
            .filter(|&x| out.get_pixel(x, 50).0[3] > 0)
            .collect();
        let left = opaque_x[0];
        let right = 100 - opaque_x[opaque_x.len() - 1];
        assert!(
            left.abs_diff(right) <= 1,
            "中央からずれている: 左{left} 右{right}"
        );
    }

    #[test]
    fn transparency_in_the_product_survives_placement() {
        let mut product = RgbaImage::from_pixel(40, 40, image::Rgba([10, 20, 30, 255]));
        product.put_pixel(20, 20, image::Rgba([0, 0, 0, 0]));
        let out = apply(&product, &spec(40, 40, 1.0)).unwrap();
        assert_eq!(out.get_pixel(20, 20).0[3], 0, "商品内部の透過が潰れている");
    }
}
