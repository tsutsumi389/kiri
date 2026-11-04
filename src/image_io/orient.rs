//! EXIF Orientation の適用（RGBA 化と同時に、並列に）。
//!
//! **`image` の `DynamicImage::apply_orientation` と画素単位で同じ結果を返す。**
//! 8 通りの取り違えは事故のもとなので、テストでは `image` の実装を正解として
//! 全 8 通り・複数の画素形式・タイルの端をまたぐ寸法で突き合わせる。
//!
//! 自前で持つ理由は速さだけである。`image` の `rotate90` は 1 本のスレッドで
//! 元画像を列方向に読むので、iPhone の縦写真（24.5MP、Orientation 6）で
//! 約 70ms かかり、読み込みの中でデコードに次ぐ逐次の山だった。ここでは
//! 出力を行の帯に割って rayon で回し、帯の中をタイルに区切って元画像の
//! 読みをキャッシュに収める。さらに RGB8 / RGBA8 の入力は RGBA への変換と
//! 並べ替えを 1 回の走査で済ませる（`to_rgba8` の全面コピーを 1 回減らす）。

use image::{DynamicImage, RgbaImage};
use rayon::prelude::*;

/// 出力の帯の行数であり、帯の中のタイルの幅でもある。
///
/// 64x64 画素の RGBA は 16KB で、転置のときに読む元画像の 64 行ぶんの断片も
/// L1 に収まる。値を変えても結果は変わらない（速さだけが変わる）。
const TILE: usize = 64;

/// 有効な Orientation（2〜8）なら RGBA へ変換しつつ適用する。
///
/// 戻り値の 2 つ目は適用したかどうか。1 や範囲外の値は何もせず RGBA にするだけ。
pub fn to_rgba8_oriented(image: DynamicImage, exif_orientation: u16) -> (RgbaImage, bool) {
    let orientation = match u8::try_from(exif_orientation) {
        Ok(v @ 2..=8) => v,
        _ => return (into_rgba8(image), false),
    };
    let out = match image {
        DynamicImage::ImageRgb8(src) => {
            remap::<3>(src.as_raw(), src.width(), src.height(), orientation)
        }
        DynamicImage::ImageRgba8(src) => {
            remap::<4>(src.as_raw(), src.width(), src.height(), orientation)
        }
        // そのほかの形式（グレー・16bit など）は先に RGBA へ変換し、元を手放してから
        // 並べ替える。ピークは「元 + RGBA」か「RGBA 2 枚」の大きいほうで、Luma8 では
        // 以前（元 2 枚 → RGBA）より大きい。並べ替えは画素ごとの変換と入れ替えても
        // 結果が同じなので、出力は変わらない
        other => {
            let rgba = other.to_rgba8();
            drop(other);
            remap::<4>(rgba.as_raw(), rgba.width(), rgba.height(), orientation)
        }
    };
    (out, true)
}

/// 回さないときの RGBA 化。RGBA8 ならコピーせずに中身を受け取る。
fn into_rgba8(image: DynamicImage) -> RgbaImage {
    match image {
        DynamicImage::ImageRgba8(rgba) => rgba,
        other => other.to_rgba8(),
    }
}

/// 出力の画素 (x, y) がどの元画素から来るかを、Orientation ごとに返す。
///
/// `w` / `h` は**元画像**の寸法。5〜8 は縦横が入れ替わる。対応は `image` の
/// `Orientation` の定義（例: 6 = `Rotate90` は時計回り 90 度）に従い、正しさは
/// テストが `image` の実装と突き合わせて保証する。
#[inline(always)]
fn source_of(orientation: u8, w: usize, h: usize, x: usize, y: usize) -> (usize, usize) {
    match orientation {
        2 => (w - 1 - x, y),
        3 => (w - 1 - x, h - 1 - y),
        4 => (x, h - 1 - y),
        5 => (y, x),
        6 => (y, h - 1 - x),
        7 => (w - 1 - y, h - 1 - x),
        8 => (w - 1 - y, x),
        _ => (x, y),
    }
}

/// `C` チャンネル（3 = RGB、4 = RGBA）の元画素を並べ替えて RGBA の出力を作る。
fn remap<const C: usize>(src: &[u8], w: u32, h: u32, orientation: u8) -> RgbaImage {
    let (w, h) = (w as usize, h as usize);
    let (ow, oh) = if orientation >= 5 { (h, w) } else { (w, h) };
    let mut out = vec![0u8; ow * oh * 4];
    if ow > 0 && oh > 0 {
        // 分岐を画素ごとに評価しないよう、Orientation ごとに単相化する
        match orientation {
            2 => fill::<C, 2>(src, w, h, ow, &mut out),
            3 => fill::<C, 3>(src, w, h, ow, &mut out),
            4 => fill::<C, 4>(src, w, h, ow, &mut out),
            5 => fill::<C, 5>(src, w, h, ow, &mut out),
            6 => fill::<C, 6>(src, w, h, ow, &mut out),
            7 => fill::<C, 7>(src, w, h, ow, &mut out),
            _ => fill::<C, 8>(src, w, h, ow, &mut out),
        }
    }
    RgbaImage::from_raw(ow as u32, oh as u32, out).expect("寸法と長さは一致している")
}

fn fill<const C: usize, const O: u8>(src: &[u8], w: usize, h: usize, ow: usize, out: &mut [u8]) {
    out.par_chunks_mut(ow * 4 * TILE)
        .enumerate()
        .for_each(|(band, rows)| {
            let y0 = band * TILE;
            let band_rows = rows.len() / (ow * 4);
            for x0 in (0..ow).step_by(TILE) {
                let x1 = (x0 + TILE).min(ow);
                for dy in 0..band_rows {
                    let y = y0 + dy;
                    let row = &mut rows[dy * ow * 4..(dy + 1) * ow * 4];
                    for x in x0..x1 {
                        let (sx, sy) = source_of(O, w, h, x, y);
                        let s = (sy * w + sx) * C;
                        let d = &mut row[x * 4..x * 4 + 4];
                        d[..3].copy_from_slice(&src[s..s + 3]);
                        d[3] = if C == 4 { src[s + 3] } else { 255 };
                    }
                }
            }
        });
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{GrayImage, Luma, Rgb, RgbImage, Rgba, metadata::Orientation};

    /// 画素ごとに値が違う画像（取り違えがあれば必ず食い違う）
    fn rgba(w: u32, h: u32) -> RgbaImage {
        RgbaImage::from_fn(w, h, |x, y| {
            Rgba([
                (x * 7 + y) as u8,
                (y * 13 + x * 3) as u8,
                (x ^ y) as u8,
                (x * 31 + y * 17) as u8,
            ])
        })
    }

    fn oracle(image: &DynamicImage, orientation: u16) -> RgbaImage {
        let mut expected = image.clone();
        if let Some(o) = Orientation::from_exif(orientation as u8) {
            expected.apply_orientation(o);
        }
        expected.to_rgba8()
    }

    /// タイルの端をまたぐ寸法・1 画素幅・正方形を含めて、全 8 通りを
    /// `image` の実装と突き合わせる
    #[test]
    fn matches_the_image_crate_for_every_orientation_and_format() {
        let sizes = [
            (1, 1),
            (1, 5),
            (5, 1),
            (64, 64),
            (65, 63),
            (130, 67),
            (3, 200),
        ];
        for (w, h) in sizes {
            let base = rgba(w, h);
            let rgb = RgbImage::from_fn(w, h, |x, y| {
                let p = base.get_pixel(x, y);
                Rgb([p[0], p[1], p[2]])
            });
            let gray = GrayImage::from_fn(w, h, |x, y| Luma([base.get_pixel(x, y)[1]]));
            let images = [
                DynamicImage::ImageRgba8(base.clone()),
                DynamicImage::ImageRgb8(rgb),
                DynamicImage::ImageLuma8(gray),
                DynamicImage::ImageLumaA8(DynamicImage::ImageRgba8(base.clone()).to_luma_alpha8()),
                DynamicImage::ImageRgb16(DynamicImage::ImageRgba8(base.clone()).to_rgb16()),
                DynamicImage::ImageRgba16(DynamicImage::ImageRgba8(base.clone()).to_rgba16()),
            ];
            for image in &images {
                for orientation in 1..=8u16 {
                    let (got, applied) = to_rgba8_oriented(image.clone(), orientation);
                    assert_eq!(applied, orientation != 1, "{w}x{h} o={orientation}");
                    let expected = oracle(image, orientation);
                    assert_eq!(
                        got.dimensions(),
                        expected.dimensions(),
                        "{w}x{h} o={orientation}"
                    );
                    assert!(
                        got.as_raw() == expected.as_raw(),
                        "{w}x{h} {:?} o={orientation} が image の実装と食い違う",
                        image.color()
                    );
                }
            }
        }
    }

    #[test]
    fn out_of_range_orientations_only_convert() {
        let image = DynamicImage::ImageRgba8(rgba(3, 2));
        for value in [0, 1, 9, 255, 256, 65535] {
            let (got, applied) = to_rgba8_oriented(image.clone(), value);
            assert!(!applied, "orientation {value}");
            assert_eq!(got.as_raw(), image.to_rgba8().as_raw());
        }
    }
}
