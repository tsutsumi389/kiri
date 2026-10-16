//! 図形の層を描く（13-phase-32.md §13.2.3）。**情報を持たない層である。**
//!
//! 文字の下に敷く帯と、文字の上に置く札がこれで書ける。測るものは無い——図形が
//! 何かを伝えることは無いので、コントラストも重なりも測る理由が無い。測りに
//! 入るのは**図形が他の層に与える影響**のほうで、それは何もしなくても入る：
//! 下に敷けば文字の背後（原寸の `text_contrast`）と最終のキャンバス（縮小の
//! 測り）が変わり、上に置けば `painter` を塗って `text_obscured` が言う。
//!
//! # resvg で描く
//!
//! 矩形くらい自分で塗れるが、**縁のアンチエイリアスを文字とそろえる**ために
//! 文字と同じ経路を通す。小数の枠を自前で塗ると、端の画素の扱いが文字の側と
//! 食い違い、同じ位置に置いた帯と字の縁が半画素ずれて見える。

use resvg::usvg;

use crate::compose::text::{escape, rasterize};
use crate::compose::{Canvas, ShapeLayer};
use crate::error::{Error, ErrorCode, Result};
use image::RgbaImage;

/// 1 つの図形の層を、キャンバスと同じ大きさの画像（straight alpha）に描く。
pub fn render(layer: &ShapeLayer, canvas: &Canvas) -> Result<RgbaImage> {
    let svg = to_svg(layer, canvas);
    let tree = usvg::Tree::from_str(&svg, &usvg::Options::default()).map_err(|e| {
        // 文字の層と同じく、ここへ来るのは kiri の組み立ての誤りである
        Error::new(
            ErrorCode::SpecInvalid,
            format!("図形 '{}' を描けませんでした: {e}", layer.id),
        )
    })?;
    rasterize(&tree, canvas)
}

/// spec の 1 層を SVG の `<rect>` にする。
///
/// **`rx` と `ry` に同じ値を渡す。** 幅か高さの半分を超えた分は SVG の規則で
/// 辺ごとに半分で止まる——細長い帯に大きな radius を書けば、短い辺の側が
/// 半円になる。kiri の側で切り詰めないのは、規則を 2 箇所に持たないためである。
fn to_svg(layer: &ShapeLayer, canvas: &Canvas) -> String {
    let [x, y, width, height] = layer.rect;
    format!(
        "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"{w}\" height=\"{h}\" \
         viewBox=\"0 0 {w} {h}\"><rect x=\"{x}\" y=\"{y}\" width=\"{width}\" \
         height=\"{height}\" rx=\"{r}\" ry=\"{r}\" fill=\"{color}\" \
         fill-opacity=\"{opacity}\"/></svg>",
        w = canvas.width,
        h = canvas.height,
        r = layer.radius,
        color = escape(&layer.color),
        opacity = layer.opacity,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn band(opacity: f64, radius: f64) -> ShapeLayer {
        ShapeLayer {
            id: "band".into(),
            rect: [0.0, 10.0, 40.0, 20.0],
            color: "#000000".into(),
            opacity,
            radius,
        }
    }

    const CANVAS: Canvas = Canvas {
        width: 40,
        height: 40,
        background: None,
    };

    /// 枠の中だけが塗られ、不透明度がそのまま α になる。
    #[test]
    fn a_band_fills_its_rect_with_the_given_opacity() {
        let pixels = render(&band(0.6, 0.0), &CANVAS).unwrap();
        assert_eq!(pixels.get_pixel(20, 5).0[3], 0, "枠の外は塗らない");
        let inside = pixels.get_pixel(20, 20).0;
        assert_eq!(&inside[..3], &[0, 0, 0]);
        assert!((i32::from(inside[3]) - 153).abs() <= 1, "{inside:?}");
    }

    /// 角の丸みは高さの半分で止まり、角の画素は塗られない。
    #[test]
    fn a_large_radius_rounds_the_corners() {
        let pixels = render(&band(1.0, 1000.0), &CANVAS).unwrap();
        assert_eq!(pixels.get_pixel(0, 10).0[3], 0, "角は丸く抜ける");
        assert_eq!(pixels.get_pixel(20, 20).0[3], 255, "中央は塗る");
        let square = render(&band(1.0, 0.0), &CANVAS).unwrap();
        assert_eq!(square.get_pixel(0, 10).0[3], 255, "radius 0 は角まで塗る");
    }
}
