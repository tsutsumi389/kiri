//! 入力画像の読み込みと正規化。
//!
//! EXIF Orientation は必ず適用する。これを怠ると、AI が見ている画像の向きと
//! 生ピクセルの座標系がずれ、`--bbox` で渡される座標がすべてずれる。

use std::path::Path;

use exif::{In, Tag};
use image::{DynamicImage, ImageDecoder, ImageFormat, ImageReader, RgbaImage};

use crate::color::icc::{self, Interpretation};
use crate::error::{Error, Result};
use crate::image_io::heif;
use crate::warning::Warning;

/// 読み込み時の振る舞い。
#[derive(Debug, Clone, Copy)]
pub struct LoadOptions {
    /// 埋め込み ICC を解釈して sRGB へ変換する
    pub convert_color: bool,
}

impl Default for LoadOptions {
    /// 既定で変換する。iPhone の素材は Display P3 で入ってくるのが普通で、
    /// 素通しすると彩度が誇張されたまま納品されるため。
    fn default() -> Self {
        Self {
            convert_color: true,
        }
    }
}

pub struct LoadedImage {
    /// EXIF Orientation 適用済み・必要なら sRGB へ変換済みの RGBA 画像
    pub image: RgbaImage,
    pub format: ImageFormat,
    /// 元ファイルの EXIF Orientation 値（1 = 回転なし）
    pub exif_orientation: u16,
    /// 実際に回転・反転を適用したか
    pub orientation_applied: bool,
    pub icc_profile: bool,
    /// 検出した色空間の名前（"sRGB" / "Display P3" / "uncalibrated" など）
    pub color_space: String,
    /// 埋め込み ICC 自身の名乗り。sRGB 相当と判定して素通ししたときも、
    /// どのプロファイルが付いていたのかを残すために持つ（ICC が無ければ None）
    pub color_profile: Option<String>,
    /// 実際に sRGB へ変換したか
    pub color_converted: bool,
    /// 実際に不透明でないピクセルが存在するか
    pub has_alpha: bool,
    /// 色空間に起因する警告
    color_warnings: Vec<Warning>,
}

impl LoadedImage {
    pub fn width(&self) -> u32 {
        self.image.width()
    }

    pub fn height(&self) -> u32 {
        self.image.height()
    }

    /// 色空間に起因する警告を返す。
    ///
    /// 変換できたときは黙る。何が起きたかは `color_space` と `color_converted`
    /// が答えており、成功を警告として流すとエージェントが本当の警告を見落とす。
    pub fn warnings(&self) -> Vec<Warning> {
        self.color_warnings.clone()
    }
}

pub fn load(path: &Path) -> Result<LoadedImage> {
    load_with(path, &LoadOptions::default())
}

pub fn load_with(path: &Path, opts: &LoadOptions) -> Result<LoadedImage> {
    let bytes = std::fs::read(path).map_err(|e| {
        Error::input(
            "INPUT_UNREADABLE",
            format!("{} を読めません: {e}", path.display()),
        )
        .with_hint("パスと読み取り権限を確認してください")
    })?;

    // HEIF 系は `image` が形式すら判別できず「判別不能」に落ちる。素材の出所が
    // iPhone だと分かっているのに手詰まりのメッセージを返すのは不親切なので、
    // 先に自前で見分けて変換手順まで示す
    if let Some(family) = heif::detect(&bytes) {
        return Err(heif::unsupported(family));
    }

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

    let icc = decoder
        .icc_profile()
        .ok()
        .flatten()
        .filter(|p| !p.is_empty());
    let icc_profile = icc.is_some();

    let dynamic = DynamicImage::from_decoder(decoder)
        .map_err(|e| Error::input("INPUT_DECODE_FAILED", e.to_string()))?;

    let (exif_orientation, exif_color_space) = read_exif(&bytes);
    let (image, orientation_applied) = apply_orientation(dynamic, exif_orientation);

    let mut image = image.to_rgba8();
    let color = normalize_color(&mut image, icc.as_deref(), exif_color_space, opts);
    let has_alpha = image.pixels().any(|p| p[3] != 255);

    Ok(LoadedImage {
        image,
        format,
        exif_orientation,
        orientation_applied,
        icc_profile,
        color_space: color.name,
        color_profile: color.profile,
        color_converted: color.converted,
        has_alpha,
        color_warnings: color.warnings,
    })
}

/// 色空間の判定と変換の結果。
struct ColorOutcome {
    name: String,
    /// 埋め込み ICC 自身の名乗り
    profile: Option<String>,
    converted: bool,
    warnings: Vec<Warning>,
}

/// 埋め込み ICC があれば sRGB へ寄せ、無ければ EXIF の申告をそのまま報告する。
///
/// ICC を EXIF より優先するのは、iPhone の JPEG が EXIF では uncalibrated と
/// 名乗りつつ ICC で Display P3 だと明かすため。曖昧な申告より実体を採る。
fn normalize_color(
    image: &mut RgbaImage,
    icc: Option<&[u8]>,
    exif_color_space: Option<u16>,
    opts: &LoadOptions,
) -> ColorOutcome {
    let Some(bytes) = icc else {
        return match exif_color_space {
            Some(0xFFFF) => ColorOutcome {
                name: "uncalibrated".into(),
                profile: None,
                converted: false,
                // ICC が無いので実体を確かめる術がない。sRGB として扱ったことを
                // 伝えるしかなく、次の一手も「変換して渡し直す」以外に無い
                warnings: vec![
                    Warning::new(
                        "COLOR_SPACE_UNCALIBRATED",
                        "入力の色空間が uncalibrated です（AdobeRGB の可能性）。ICC も\
                         埋め込まれていないため sRGB として扱います",
                    )
                    .with_hint(
                        "色が合わない場合は、あらかじめ sRGB へ変換した素材を渡してください",
                    ),
                ],
            },
            _ => ColorOutcome {
                name: "sRGB".into(),
                profile: None,
                converted: false,
                warnings: Vec::new(),
            },
        };
    };

    let icc::Icc {
        name: profile,
        interpretation,
    } = icc::interpret(bytes);
    let label = match &profile {
        Some(n) => format!("'{n}'"),
        None => "（名前なし）".to_string(),
    };
    let reported = || profile.clone().unwrap_or_else(|| "unknown".into());

    match interpretation {
        // 判定は名乗りではなく原色と TRC で決まる。報告する色空間は "sRGB" に
        // 揃えつつ、名乗りは残す。片方だけでは「sRGB と出たが、そう名乗って
        // いただけなのか実体もそうなのか」を後から追えない
        Interpretation::Srgb => ColorOutcome {
            name: "sRGB".into(),
            profile,
            converted: false,
            warnings: Vec::new(),
        },
        Interpretation::Convertible(transform) => {
            let name = reported();
            if opts.convert_color {
                transform.apply(image);
                ColorOutcome {
                    name,
                    profile,
                    converted: true,
                    warnings: Vec::new(),
                }
            } else {
                let mut warning = Warning::new(
                    "COLOR_CONVERSION_SKIPPED",
                    format!(
                        "ICC プロファイル {label} を検出しましたが、--no-color-convert のため \
                         sRGB へ変換していません"
                    ),
                );
                if let Some(n) = &profile {
                    warning = warning.with_data("profile", n.clone());
                }
                ColorOutcome {
                    name,
                    profile,
                    converted: false,
                    warnings: vec![warning],
                }
            }
        }
        Interpretation::Unsupported => {
            let mut warning = Warning::new(
                "COLOR_PROFILE_UNSUPPORTED",
                format!("ICC プロファイル {label} は変換に対応していないため sRGB として扱います"),
            );
            if let Some(n) = &profile {
                warning = warning.with_data("profile", n.clone());
            }
            ColorOutcome {
                name: reported(),
                profile,
                converted: false,
                warnings: vec![warning],
            }
        }
    }
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

    /// 読み込みの入口で HEIC が弾かれること。検出そのものの網羅は `heif` 側にある。
    #[test]
    fn a_heif_input_explains_how_to_convert_it() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("photo.HEIC");
        let mut heic = 24u32.to_be_bytes().to_vec();
        heic.extend_from_slice(b"ftypheic");
        heic.extend_from_slice(&0u32.to_be_bytes());
        heic.extend_from_slice(b"mif1heic");
        std::fs::write(&path, heic).unwrap();

        let err = load(&path).err().expect("HEIC は断るべき");
        assert_eq!(err.code, "UNSUPPORTED_FORMAT");
        assert_eq!(err.exit_code(), 3);
        assert!(err.hint.unwrap().contains("sips"));
    }

    /// ICC 付きの実ファイルを通した経路。単体の変換が正しくても、
    /// デコーダから ICC を取り出せていなければ何も起きない。
    fn jpeg_with_profile(dir: &Path, name: &str, rgb: [u8; 3], icc: &[u8]) -> std::path::PathBuf {
        let img = image::RgbImage::from_pixel(24, 24, image::Rgb(rgb));
        let mut raw = Vec::new();
        image::codecs::jpeg::JpegEncoder::new_with_quality(&mut raw, 100)
            .encode_image(&img)
            .unwrap();
        let path = dir.join(name);
        std::fs::write(&path, crate::color::synthetic::embed_in_jpeg(&raw, icc)).unwrap();
        path
    }

    #[test]
    fn an_embedded_display_p3_profile_is_converted_to_srgb() {
        use crate::color::synthetic::{build, display_p3};
        let dir = tempfile::tempdir().unwrap();
        // P3 の飽和した赤。sRGB では色域外なので 255 に張り付く
        let path = jpeg_with_profile(dir.path(), "p3.jpg", [255, 0, 0], &build(&display_p3()));

        let loaded = load(&path).unwrap();
        assert!(loaded.icc_profile);
        assert_eq!(loaded.color_space, "Display P3");
        assert!(loaded.color_converted);
        assert!(loaded.warnings().is_empty(), "変換できたら黙るべき");

        let px = loaded.image.get_pixel(12, 12);
        assert!(px[0] > 250 && px[1] < 8 && px[2] < 8, "{px:?}");
    }

    #[test]
    fn no_color_convert_leaves_the_pixels_untouched_and_says_so() {
        use crate::color::synthetic::{build, display_p3};
        let dir = tempfile::tempdir().unwrap();
        let path = jpeg_with_profile(dir.path(), "p3.jpg", [200, 60, 40], &build(&display_p3()));

        let converted = load(&path).unwrap();
        let raw = load_with(
            &path,
            &LoadOptions {
                convert_color: false,
            },
        )
        .unwrap();

        assert!(!raw.color_converted);
        assert_eq!(raw.color_space, "Display P3");
        assert_eq!(raw.warnings().len(), 1, "変換していないことは伝えるべき");
        assert_ne!(
            raw.image.get_pixel(12, 12),
            converted.image.get_pixel(12, 12),
            "変換の有無で結果が変わらないのはおかしい"
        );
    }

    /// sRGB 相当と判定して素通ししても、どのプロファイルが付いていたかは残す。
    ///
    /// `color_space` だけでは「sRGB と出たが、名乗りだけだったのか実体もそう
    /// だったのか」を後から追えない。色を疑ったときに手がかりが消えている。
    #[test]
    fn an_embedded_srgb_profile_is_reported_but_not_converted() {
        use crate::color::synthetic::{build, srgb_v2};
        let dir = tempfile::tempdir().unwrap();
        let path = jpeg_with_profile(dir.path(), "srgb.jpg", [190, 70, 55], &build(&srgb_v2()));

        let loaded = load(&path).unwrap();
        assert!(loaded.icc_profile);
        assert_eq!(loaded.color_space, "sRGB");
        assert_eq!(loaded.color_profile.as_deref(), Some("sRGB IEC61966-2.1"));
        assert!(!loaded.color_converted, "sRGB を変換してはいけない");
        assert!(loaded.warnings().is_empty());
    }

    /// ICC が無ければ名乗りも無い。「sRGB として扱った」と「sRGB を名乗って
    /// いた」は別のことなので、埋まっていないものを埋まっていたことにしない。
    #[test]
    fn an_image_without_a_profile_reports_no_profile_name() {
        let dir = tempfile::tempdir().unwrap();
        let img = image::RgbImage::from_pixel(8, 8, image::Rgb([190, 70, 55]));
        let path = dir.path().join("plain.png");
        img.save(&path).unwrap();

        let loaded = load(&path).unwrap();
        assert_eq!(loaded.color_space, "sRGB");
        assert_eq!(loaded.color_profile, None);
    }

    #[test]
    fn an_unsupported_profile_warns_instead_of_converting() {
        use crate::color::synthetic::{build, display_p3};
        let dir = tempfile::tempdir().unwrap();
        let mut spec = display_p3();
        spec.include_matrix = false;
        let path = jpeg_with_profile(dir.path(), "lut.jpg", [190, 70, 55], &build(&spec));

        let loaded = load(&path).unwrap();
        assert!(!loaded.color_converted);
        assert_eq!(loaded.color_space, "Display P3");
        let warnings = loaded.warnings();
        assert_eq!(warnings.len(), 1);
        assert_eq!(warnings[0].code, "COLOR_PROFILE_UNSUPPORTED");
        assert!(
            warnings[0].message.contains("変換に対応していない"),
            "{warnings:?}"
        );
    }

    /// ICC が無い画像は今までどおり触らない。
    #[test]
    fn an_image_without_a_profile_is_passed_through() {
        let dir = tempfile::tempdir().unwrap();
        let img = image::RgbImage::from_pixel(8, 8, image::Rgb([190, 70, 55]));
        let path = dir.path().join("plain.png");
        img.save(&path).unwrap();

        let loaded = load(&path).unwrap();
        assert!(!loaded.icc_profile);
        assert_eq!(loaded.color_space, "sRGB");
        assert!(!loaded.color_converted);
        let px = loaded.image.get_pixel(4, 4);
        assert_eq!([px[0], px[1], px[2]], [190, 70, 55]);
    }
}
