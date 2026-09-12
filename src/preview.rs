//! 検証用プレビュー。
//!
//! kiri の出力は原寸（数千 px・数十 MB）で、視覚モデルにそのまま渡せない。
//! 「元画像 | マスク | 結果」を 1 枚に並べた小さな画像を書き出すことで、AI が
//! 結果を見て次の調整を決められるようにする。外部ツールを挟まないのは、
//! 依存ゼロの単一バイナリという前提を崩さないためである。
//!
//! 3面に分けるのは、結果だけを見ても「なぜ失敗したか」が分からないためである。
//! マスクを併置すれば、商品が消えたのか背景が残ったのかを一目で切り分けられる。

use image::{Rgba, RgbaImage};

use crate::cutout::{Constraint, Constraints, Mask};
use crate::error::Result;
use crate::transform::resize::{ResizePlan, apply as resize_apply};

/// パネル1枚の長辺の既定値(px)。
/// 512 は視覚モデルが商品の輪郭を判別できる下限に近く、かつトークン消費が軽い。
pub const DEFAULT_PANEL: u32 = 512;

const MARGIN: u32 = 8;
const GUTTER: u32 = 8;
/// 台紙の色。白い商品と白背景の両方が縁として見えるよう暗く取る
const SHEET_BG: [u8; 3] = [60, 60, 60];
const CHECKER_LIGHT: [u8; 3] = [255, 255, 255];
const CHECKER_DARK: [u8; 3] = [204, 204, 204];
const CHECKER_SIZE: u32 = 8;
/// グリッド線の色。商品写真に自然には出にくい色を選ぶ
const GRID_RGB: [u8; 3] = [255, 51, 102];
/// 確定前景の重ね色。確定背景と補色の関係にして、縮小しても取り違えないようにする
const FORCED_FG_RGB: [u8; 3] = [0, 220, 60];
/// 確定背景の重ね色。
const FORCED_BG_RGB: [u8; 3] = [230, 30, 30];
/// 指示の重ね具合。**元の画素が読めなくなってはいけない。** 置き場所を
/// 確かめるための重ね描きなので、下の商品と背景が透けて見える必要がある
const CONSTRAINT_ALPHA: f64 = 0.35;

#[derive(Debug, Clone)]
pub struct PreviewSpec {
    /// パネル1枚の長辺(px)
    pub panel: u32,
    /// 元画像パネルに 0.1 刻みの座標グリッドを重ねるか。
    /// AI が --bbox --normalized の値を読み取るために使う
    pub grid: bool,
}

impl Default for PreviewSpec {
    fn default() -> Self {
        Self {
            panel: DEFAULT_PANEL,
            grid: true,
        }
    }
}

/// 「元画像 | マスク | 結果」を横に並べた1枚を組み立てる。
///
/// 結果パネルは市松模様の上に合成する。透過と白い商品を区別できないと
/// 「切り抜けた」のか「商品ごと消えた」のかが見て分からないため。
///
/// `constraints` を渡すと、元画像パネルに確定前景を緑・確定背景を赤で重ねる。
/// **エージェントが自分の指示の置き場所を目で確かめられる唯一の手段である。**
/// 座標は自分で書いたものなので数字では検算できず、ずれていても結果の数値には
/// 「切り抜きが下手」としか出ない。指示が無ければ何も描かない（既存の
/// プレビューのバイト列を動かさないため）。
pub fn contact_sheet(
    original: &RgbaImage,
    mask: &Mask,
    result: &RgbaImage,
    constraints: Option<&Constraints>,
    spec: &PreviewSpec,
) -> Result<RgbaImage> {
    let mut source = fit(original, spec.panel)?;
    if spec.grid {
        draw_grid(&mut source);
    }
    // グリッドの上に重ねる。指示のほうが後から確かめたいものだからで、
    // 下に敷くと 0.1 刻みの線が指示を横切って読み取りを邪魔する
    if let Some(c) = constraints {
        draw_constraints(&mut source, c, original.width(), original.height());
    }
    let mask_panel = fit_mask(mask, spec.panel);
    let result_panel = over_checkerboard(&fit(result, spec.panel)?);

    Ok(compose(&[source, mask_panel, result_panel]))
}

/// 長辺が `panel` に収まるよう縮小する。元が小さければ拡大せずそのまま使う。
fn fit(image: &RgbaImage, panel: u32) -> Result<RgbaImage> {
    let (w, h) = (image.width(), image.height());
    let long = w.max(h);
    if w == 0 || h == 0 || long <= panel {
        return Ok(image.clone());
    }
    let scale = f64::from(panel) / f64::from(long);
    let to = (
        ((f64::from(w) * scale).round() as u32).max(1),
        ((f64::from(h) * scale).round() as u32).max(1),
    );
    // resize::apply を通すことで事前乗算つきの補間が効く。素朴に縮小すると
    // 切り抜き済み画像の境界に背景色がにじみ、プレビューが実物と食い違う
    resize_apply(
        image,
        &ResizePlan {
            scaled: to,
            crop: None,
            output: to,
        },
    )
}

/// マスクを縮小しながら RGBA に広げる。
///
/// 原寸で RGBA 化してから縮小すると、12MP のマスク(12MB)が 48MB の RGBA になり、
/// さらに縮小器の内部でもう一度複製される。マスクは 1 byte/px なので、
/// 縮めてから広げれば大半を節約できる。
///
/// 面積平均で縮める。マスクは輪郭の鋭い画像なので、Lanczos のような
/// リンギングを持つ補間よりこちらが適する。
fn fit_mask(mask: &Mask, panel: u32) -> RgbaImage {
    let (w, h) = (mask.width(), mask.height());
    if w == 0 || h == 0 {
        return RgbaImage::new(w, h);
    }
    let long = w.max(h);
    let (tw, th) = if long <= panel {
        (w, h)
    } else {
        let scale = f64::from(panel) / f64::from(long);
        (
            ((f64::from(w) * scale).round() as u32).max(1),
            ((f64::from(h) * scale).round() as u32).max(1),
        )
    };

    let mut out = RgbaImage::new(tw, th);
    for oy in 0..th {
        let y0 = (u64::from(oy) * u64::from(h) / u64::from(th)) as u32;
        let y1 = ((u64::from(oy) + 1) * u64::from(h) / u64::from(th)).max(u64::from(y0) + 1) as u32;
        for ox in 0..tw {
            let x0 = (u64::from(ox) * u64::from(w) / u64::from(tw)) as u32;
            let x1 =
                ((u64::from(ox) + 1) * u64::from(w) / u64::from(tw)).max(u64::from(x0) + 1) as u32;

            let mut sum = 0u64;
            let mut count = 0u64;
            for y in y0..y1.min(h) {
                for x in x0..x1.min(w) {
                    sum += u64::from(mask.get(x, y));
                    count += 1;
                }
            }
            let v = sum.checked_div(count).unwrap_or(0) as u8;
            out.put_pixel(ox, oy, Rgba([v, v, v, 255]));
        }
    }
    out
}

/// 市松模様の上にアルファ合成する。縮小後に敷くので、格子の大きさは
/// 元画像の寸法によらず一定になる。
fn over_checkerboard(image: &RgbaImage) -> RgbaImage {
    let mut out = RgbaImage::new(image.width(), image.height());
    for y in 0..image.height() {
        for x in 0..image.width() {
            let p = image.get_pixel(x, y).0;
            let base = if (x / CHECKER_SIZE + y / CHECKER_SIZE) % 2 == 0 {
                CHECKER_LIGHT
            } else {
                CHECKER_DARK
            };
            let a = f64::from(p[3]) / 255.0;
            let mut px = [0u8; 4];
            for c in 0..3 {
                px[c] = (f64::from(p[c]) * a + f64::from(base[c]) * (1.0 - a)).round() as u8;
            }
            px[3] = 255;
            out.put_pixel(x, y, Rgba(px));
        }
    }
    out
}

/// 0.1 刻みのグリッドを重ねる。0.5 の線だけ濃くして、AI が線を数えずとも
/// 中央を基準に座標を読めるようにする。
fn draw_grid(image: &mut RgbaImage) {
    let (w, h) = (image.width(), image.height());
    if w < 2 || h < 2 {
        return;
    }
    for i in 1..10u32 {
        let strength = if i == 5 { 0.75 } else { 0.35 };
        let x = ((f64::from(w) * f64::from(i) / 10.0).round() as u32).min(w - 1);
        for y in 0..h {
            blend_toward(image, x, y, GRID_RGB, strength);
        }
        let y = ((f64::from(h) * f64::from(i) / 10.0).round() as u32).min(h - 1);
        for x in 0..w {
            blend_toward(image, x, y, GRID_RGB, strength);
        }
    }
}

/// 確定前景を緑、確定背景を赤で重ねる。
///
/// パネルは縮小済みなので、画素ごとに元の座標へ引き戻して問う。`fit_mask` と
/// 同じ最近傍の対応にしてある——指示の縁が半画素ずれて見えるより、
/// 「どのあたりを塗ったか」が読めることのほうが要る。
///
/// 寸法が食い違う指示は描かない。`foreground_mask` と同じ規約で、
/// 公開 API に届いた食い違いで panic させない。
fn draw_constraints(panel: &mut RgbaImage, constraints: &Constraints, width: u32, height: u32) {
    if constraints.width() != width || constraints.height() != height {
        return;
    }
    let (pw, ph) = (panel.width(), panel.height());
    if pw == 0 || ph == 0 || width == 0 || height == 0 {
        return;
    }
    for py in 0..ph {
        let sy = (u64::from(py) * u64::from(height) / u64::from(ph)) as u32;
        for px in 0..pw {
            let sx = (u64::from(px) * u64::from(width) / u64::from(pw)) as u32;
            let rgb = match constraints.at(sx.min(width - 1), sy.min(height - 1)) {
                Constraint::Free => continue,
                Constraint::ForcedFg => FORCED_FG_RGB,
                Constraint::ForcedBg => FORCED_BG_RGB,
            };
            blend_toward(panel, px, py, rgb, CONSTRAINT_ALPHA);
        }
    }
}

fn blend_toward(image: &mut RgbaImage, x: u32, y: u32, rgb: [u8; 3], amount: f64) {
    let p = image.get_pixel_mut(x, y);
    for c in 0..3 {
        p[c] = (f64::from(p[c]) * (1.0 - amount) + f64::from(rgb[c]) * amount).round() as u8;
    }
    // グリッドは透過部にも見えている必要がある
    p[3] = 255;
}

/// パネルを横に並べ、高さ方向は中央で揃える。
fn compose(panels: &[RgbaImage]) -> RgbaImage {
    // contact_sheet は公開 API で panel は任意の u32 を取りうる。
    // 台紙の寸法計算で溢れさせない
    let total_w: u32 = panels
        .iter()
        .map(RgbaImage::width)
        .fold(0u32, u32::saturating_add);
    let gutters = GUTTER.saturating_mul(panels.len().saturating_sub(1) as u32);
    let width = total_w.saturating_add(gutters).saturating_add(MARGIN * 2);
    let height = panels
        .iter()
        .map(RgbaImage::height)
        .max()
        .unwrap_or(0)
        .saturating_add(MARGIN * 2);

    let mut sheet = RgbaImage::from_pixel(
        width.max(1),
        height.max(1),
        Rgba([SHEET_BG[0], SHEET_BG[1], SHEET_BG[2], 255]),
    );

    let mut x = MARGIN;
    for panel in panels {
        let y = MARGIN + (height - MARGIN * 2 - panel.height()) / 2;
        image::imageops::overlay(&mut sheet, panel, i64::from(x), i64::from(y));
        x += panel.width() + GUTTER;
    }
    sheet
}

#[cfg(test)]
mod tests {
    use super::*;

    fn solid(w: u32, h: u32, rgba: [u8; 4]) -> RgbaImage {
        RgbaImage::from_pixel(w, h, Rgba(rgba))
    }

    #[test]
    fn a_large_image_is_scaled_down_to_the_panel() {
        let img = solid(4032, 3024, [10, 20, 30, 255]);
        let fitted = fit(&img, 512).unwrap();
        assert_eq!(fitted.width(), 512, "長辺がパネル幅に一致する");
        assert_eq!(fitted.height(), 384, "アスペクト比が保たれる");
    }

    #[test]
    fn a_small_image_is_not_upscaled() {
        let img = solid(100, 60, [10, 20, 30, 255]);
        let fitted = fit(&img, 512).unwrap();
        assert_eq!((fitted.width(), fitted.height()), (100, 60));
    }

    #[test]
    fn the_sheet_holds_three_panels_side_by_side() {
        let original = solid(200, 100, [255, 0, 0, 255]);
        let result = solid(200, 100, [0, 255, 0, 128]);
        let mask = Mask::new(200, 100, 255);
        let sheet =
            contact_sheet(&original, &mask, &result, None, &PreviewSpec::default()).unwrap();

        assert_eq!(sheet.width(), MARGIN * 2 + 200 * 3 + GUTTER * 2);
        assert_eq!(sheet.height(), MARGIN * 2 + 100);
    }

    #[test]
    fn transparent_areas_show_the_checkerboard() {
        // 完全透明の画像を敷くと市松模様がそのまま出る。
        // 白い商品と透過を取り違えないための最重要の性質
        let clear = solid(16, 16, [0, 0, 0, 0]);
        let out = over_checkerboard(&clear);
        assert_eq!(out.get_pixel(0, 0).0, [255, 255, 255, 255]);
        assert_eq!(out.get_pixel(CHECKER_SIZE, 0).0, [204, 204, 204, 255]);
        assert_eq!(out.get_pixel(0, CHECKER_SIZE).0, [204, 204, 204, 255]);
        assert!(
            out.pixels().all(|p| p.0[3] == 255),
            "合成後に透過が残ってはいけない"
        );
    }

    #[test]
    fn opaque_areas_survive_the_checkerboard() {
        let opaque = solid(16, 16, [12, 34, 56, 255]);
        let out = over_checkerboard(&opaque);
        assert_eq!(out.get_pixel(3, 3).0, [12, 34, 56, 255]);
    }

    #[test]
    fn the_grid_marks_tenths_and_emphasises_the_centre() {
        let mut img = solid(100, 100, [0, 0, 0, 255]);
        draw_grid(&mut img);

        let untouched = img.get_pixel(3, 3).0[0];
        let tenth = img.get_pixel(10, 3).0[0];
        let centre = img.get_pixel(50, 3).0[0];

        assert_eq!(untouched, 0, "線の無い場所は変わらない");
        assert!(tenth > 0, "0.1 の位置に線が引かれる");
        assert!(centre > tenth, "0.5 の線は他より濃い");
    }

    #[test]
    fn the_mask_panel_is_scaled_without_a_full_size_rgba_copy() {
        let mut mask = Mask::new(1000, 500, 0);
        for y in 100..400 {
            for x in 200..800 {
                mask.set(x, y, 255);
            }
        }
        let panel = fit_mask(&mask, 100);
        assert_eq!((panel.width(), panel.height()), (100, 50));
        assert_eq!(
            panel.get_pixel(50, 25).0,
            [255, 255, 255, 255],
            "内部は前景"
        );
        assert_eq!(panel.get_pixel(2, 2).0, [0, 0, 0, 255], "外側は背景");
        assert!(panel.pixels().all(|p| p.0[3] == 255));
    }

    #[test]
    fn a_small_mask_keeps_its_size() {
        let mask = Mask::new(40, 20, 128);
        let panel = fit_mask(&mask, 512);
        assert_eq!((panel.width(), panel.height()), (40, 20));
        assert_eq!(panel.get_pixel(0, 0).0, [128, 128, 128, 255]);
    }

    #[test]
    fn the_grid_does_not_panic_on_a_tiny_image() {
        let mut img = solid(1, 1, [0, 0, 0, 255]);
        draw_grid(&mut img);
    }
}
