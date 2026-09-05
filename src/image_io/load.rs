//! 入力画像の読み込みと正規化。
//!
//! EXIF Orientation は必ず適用する。これを怠ると、AI が見ている画像の向きと
//! 生ピクセルの座標系がずれ、`--bbox` で渡される座標がすべてずれる。

use std::path::Path;

use exif::{In, Tag};
use image::{DynamicImage, ImageDecoder, ImageFormat, ImageReader, RgbaImage};

use crate::error::{Error, Result};

/// 入力画像の色空間。sRGB 以外は色がくすむ可能性があるため警告の対象にする。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColorSpace {
    /// EXIF が sRGB を明示している、または情報がなく sRGB とみなせる
    Srgb,
    /// EXIF が uncalibrated（AdobeRGB 等の可能性が高い）
    Uncalibrated,
    /// ICC プロファイルはあるが sRGB かどうか判別できない
    Unknown,
}

impl ColorSpace {
    pub fn as_str(self) -> &'static str {
        match self {
            ColorSpace::Srgb => "sRGB",
            ColorSpace::Uncalibrated => "uncalibrated",
            ColorSpace::Unknown => "unknown",
        }
    }
}

pub struct LoadedImage {
    /// EXIF Orientation 適用済みの RGBA 画像
    pub image: RgbaImage,
    pub format: ImageFormat,
    /// 元ファイルの EXIF Orientation 値（1 = 回転なし）
    pub exif_orientation: u16,
    /// 実際に回転・反転を適用したか
    pub orientation_applied: bool,
    pub icc_profile: bool,
    pub color_space: ColorSpace,
    /// 実際に不透明でないピクセルが存在するか
    pub has_alpha: bool,
}

impl LoadedImage {
    pub fn width(&self) -> u32 {
        self.image.width()
    }

    pub fn height(&self) -> u32 {
        self.image.height()
    }

    /// 色空間に起因する警告を返す。
    pub fn warnings(&self) -> Vec<String> {
        let mut w = Vec::new();
        match self.color_space {
            ColorSpace::Uncalibrated => w.push(
                "入力の色空間が uncalibrated です（AdobeRGB の可能性）。sRGB として\
                 扱うため色がくすむ場合があります"
                    .into(),
            ),
            ColorSpace::Unknown => w.push(
                "ICC プロファイルが埋め込まれていますが sRGB か判別できません。\
                 sRGB として扱います"
                    .into(),
            ),
            ColorSpace::Srgb => {}
        }
        w
    }
}

pub fn load(path: &Path) -> Result<LoadedImage> {
    let bytes = std::fs::read(path).map_err(|e| {
        Error::input(
            "INPUT_UNREADABLE",
            format!("{} を読めません: {e}", path.display()),
        )
        .with_hint("パスと読み取り権限を確認してください")
    })?;

    let reader = ImageReader::new(std::io::Cursor::new(&bytes))
        .with_guessed_format()
        .map_err(|e| Error::input("INPUT_UNREADABLE", e.to_string()))?;

    let format = reader.format().ok_or_else(|| {
        Error::input(
            "UNSUPPORTED_FORMAT",
            format!("{} の形式を判別できません", path.display()),
        )
        .with_hint("対応入力形式は JPEG と PNG です")
    })?;

    if !matches!(format, ImageFormat::Jpeg | ImageFormat::Png) {
        return Err(Error::input(
            "UNSUPPORTED_FORMAT",
            format!("{format:?} は入力として未対応です"),
        )
        .with_hint("対応入力形式は JPEG と PNG です"));
    }

    let mut decoder = reader
        .into_decoder()
        .map_err(|e| Error::input("INPUT_DECODE_FAILED", e.to_string()))?;

    let icc_profile = decoder
        .icc_profile()
        .ok()
        .flatten()
        .is_some_and(|p| !p.is_empty());

    let dynamic = DynamicImage::from_decoder(decoder)
        .map_err(|e| Error::input("INPUT_DECODE_FAILED", e.to_string()))?;

    let (exif_orientation, exif_color_space) = read_exif(&bytes);
    let (image, orientation_applied) = apply_orientation(dynamic, exif_orientation);

    let color_space = match exif_color_space {
        Some(1) => ColorSpace::Srgb,
        Some(0xFFFF) => ColorSpace::Uncalibrated,
        _ if icc_profile => ColorSpace::Unknown,
        _ => ColorSpace::Srgb,
    };

    let image = image.to_rgba8();
    let has_alpha = image.pixels().any(|p| p[3] != 255);

    Ok(LoadedImage {
        image,
        format,
        exif_orientation,
        orientation_applied,
        icc_profile,
        color_space,
        has_alpha,
    })
}

/// EXIF から Orientation と ColorSpace を読む。EXIF がなければ既定値を返す。
fn read_exif(bytes: &[u8]) -> (u16, Option<u16>) {
    let mut cursor = std::io::Cursor::new(bytes);
    let Ok(exif) = exif::Reader::new().read_from_container(&mut cursor) else {
        return (1, None);
    };
    let field = |tag| {
        exif.get_field(tag, In::PRIMARY)
            .and_then(|f| f.value.get_uint(0))
            .map(|v| v as u16)
    };
    (field(Tag::Orientation).unwrap_or(1), field(Tag::ColorSpace))
}

/// EXIF Orientation に従って回転・反転を適用する。
/// 変換自体は image クレートの実装に委ねる（8 通りの取り違えは事故のもとであるため）。
fn apply_orientation(image: DynamicImage, exif_orientation: u16) -> (DynamicImage, bool) {
    if exif_orientation <= 1 || exif_orientation > 8 {
        return (image, false);
    }
    let Ok(value) = u8::try_from(exif_orientation) else {
        return (image, false);
    };
    match image::metadata::Orientation::from_exif(value) {
        Some(orientation) => {
            let mut image = image;
            image.apply_orientation(orientation);
            (image, true)
        }
        None => (image, false),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::Rgba;

    /// 左上に目印を置いた 2x3 の画像を作る。回転・反転の追跡に使う。
    fn marked() -> DynamicImage {
        let mut img = RgbaImage::from_pixel(2, 3, Rgba([0, 0, 0, 255]));
        img.put_pixel(0, 0, Rgba([255, 0, 0, 255]));
        DynamicImage::ImageRgba8(img)
    }

    /// 目印(赤)の座標を返す。
    fn mark_at(img: &DynamicImage) -> (u32, u32) {
        let rgba = img.to_rgba8();
        rgba.enumerate_pixels()
            .find(|(_, _, p)| p[0] == 255)
            .map(|(x, y, _)| (x, y))
            .expect("目印が消えている")
    }

    #[test]
    fn orientation_1_leaves_the_image_untouched() {
        let (img, applied) = apply_orientation(marked(), 1);
        assert!(!applied);
        assert_eq!((img.width(), img.height()), (2, 3));
        assert_eq!(mark_at(&img), (0, 0));
    }

    #[test]
    fn orientation_2_mirrors_horizontally() {
        let (img, applied) = apply_orientation(marked(), 2);
        assert!(applied);
        assert_eq!((img.width(), img.height()), (2, 3));
        assert_eq!(mark_at(&img), (1, 0), "左上の目印が右上に移るべき");
    }

    #[test]
    fn orientation_3_rotates_180_degrees() {
        let (img, _) = apply_orientation(marked(), 3);
        assert_eq!((img.width(), img.height()), (2, 3));
        assert_eq!(mark_at(&img), (1, 2), "左上の目印が右下に移るべき");
    }

    #[test]
    fn orientation_4_mirrors_vertically() {
        let (img, _) = apply_orientation(marked(), 4);
        assert_eq!((img.width(), img.height()), (2, 3));
        assert_eq!(mark_at(&img), (0, 2), "左上の目印が左下に移るべき");
    }

    #[test]
    fn orientation_6_rotates_90_clockwise() {
        let (img, applied) = apply_orientation(marked(), 6);
        assert!(applied);
        assert_eq!((img.width(), img.height()), (3, 2), "縦横が入れ替わるべき");
        assert_eq!(mark_at(&img), (2, 0), "左上の目印が右上に移るべき");
    }

    #[test]
    fn orientation_8_rotates_270_clockwise() {
        let (img, _) = apply_orientation(marked(), 8);
        assert_eq!((img.width(), img.height()), (3, 2), "縦横が入れ替わるべき");
        assert_eq!(mark_at(&img), (0, 1), "左上の目印が左下に移るべき");
    }

    #[test]
    fn transposed_orientations_swap_dimensions() {
        for value in [5, 7] {
            let (img, applied) = apply_orientation(marked(), value);
            assert!(applied, "orientation {value} が適用されていない");
            assert_eq!((img.width(), img.height()), (3, 2), "orientation {value}");
        }
    }

    #[test]
    fn out_of_range_orientations_are_ignored() {
        for value in [0, 9, 65535] {
            let (img, applied) = apply_orientation(marked(), value);
            assert!(!applied, "orientation {value} を適用してはいけない");
            assert_eq!((img.width(), img.height()), (2, 3));
        }
    }
}
