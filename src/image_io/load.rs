//! 入力画像の読み込みと正規化。
//!
//! EXIF Orientation は必ず適用する。これを怠ると、AI が見ている画像の向きと
//! 生ピクセルの座標系がずれ、`--bbox` で渡される座標がすべてずれる。

use std::path::Path;

use exif::{In, Tag};
use image::{DynamicImage, ImageDecoder, ImageReader, RgbaImage};

use crate::color::icc::{self, Interpretation};
use crate::error::{Error, ErrorCode, Result};
use crate::image_io::heif;
use crate::image_io::save::OutputFormat;
use crate::warning::{Warning, WarningCode};

/// 読み込み時の振る舞い。
#[derive(Debug, Clone, Copy)]
pub struct LoadOptions {
    /// 埋め込み ICC を解釈して sRGB へ変換する
    pub convert_color: bool,
    /// EXIF Orientation を適用する。
    ///
    /// **素材の画像では必ず適用する。** 切る側と指す側で座標系がずれると
    /// bbox がすべて狂う。切るのは `--trimap` / `--fg-mask` / `--bg-mask` の
    /// ように**画素そのものが指示である**入力だけで、そちらは
    /// 「EXIF 適用後の入力画像と同じ寸法」と約束しているため、向きを
    /// 勝手に直すとその約束のほうが崩れる
    pub apply_orientation: bool,
}

impl Default for LoadOptions {
    /// 既定で変換する。iPhone の素材は Display P3 で入ってくるのが普通で、
    /// 素通しすると彩度が誇張されたまま納品されるため。
    fn default() -> Self {
        Self {
            convert_color: true,
            apply_orientation: true,
        }
    }
}

/// `LoadedImage::color_space` が sRGB を名乗るときの綴り。
///
/// **定数にしてあるのは `kiri lint` が照合するためである。** 綴りを
/// 書き写した側が `"srgb"` と打った日から、lint は sRGB の画像を
/// 「別の色空間を名乗っている」として落とし続ける——しかも落ちるのは
/// 実行してみたときだけで、型は何も言わない。
pub const COLOR_SPACE_SRGB: &str = "sRGB";

/// ICC も EXIF の申告も無く、色空間を名乗っていないときの綴り。
///
/// **「別の色空間」ではなく「名乗っていない」である。** `kiri lint` は
/// この 2 つを別の `status` で返す（前者は `fail`、こちらは `unmeasurable`）。
pub const COLOR_SPACE_UNCALIBRATED: &str = "uncalibrated";

pub struct LoadedImage {
    /// EXIF Orientation 適用済み・必要なら sRGB へ変換済みの RGBA 画像
    pub image: RgbaImage,
    /// 読めた形式。**読める形式だけが値を持つ型**にしてあるので、使う側に
    /// 「それ以外」の枝が要らない（`OutputFormat::from_decoded`）
    pub format: OutputFormat,
    /// 元ファイルの EXIF Orientation 値（1 = 回転なし）
    pub exif_orientation: u16,
    /// 実際に回転・反転を適用したか
    pub orientation_applied: bool,
    pub icc_profile: bool,
    /// ファイルが色空間を**1 つでも名乗っていたか**（ICC か EXIF ColorSpace）。
    ///
    /// **`color_space` からは読めない事実である。** ICC も EXIF の申告も無い
    /// 入力に対して `color_space` は `COLOR_SPACE_SRGB` を返す——kiri がその
    /// 画素を sRGB として扱うのは正しく、`info` の出力はそのままでよい。
    /// だが「sRGB として扱った」と「ファイルが sRGB を名乗っていた」は
    /// 別の事実で、**規格が sRGB を要求しているかを検査する側が要るのは
    /// 後者**である（`kiri lint` の `color_space`）。
    ///
    /// ここが偽なら kiri には材料が 1 つも無い。AVIF の CICP が unspecified の
    /// ときに `unmeasurable` を返すのとまったく同じ状態で、**形式が違うだけで
    /// 合否が変わる枝を作らない**ためにこの 1 つを持つ。
    pub color_named: bool,
    /// 検出した色空間の名前（"sRGB" / "Display P3" / "uncalibrated" など）
    pub color_space: String,
    /// 埋め込み ICC 自身の名乗り。sRGB 相当と判定して素通ししたときも、
    /// どのプロファイルが付いていたのかを残すために持つ（ICC が無ければ None）
    pub color_profile: Option<String>,
    /// 実際に sRGB へ変換したか
    pub color_converted: bool,
    /// 画素を sRGB として扱えるか。出力に sRGB を名乗らせてよいかをこれで決める。
    /// false になるのは、変換できるプロファイルを `--no-color-convert` で変換しなかったときだけ——
    /// LUT 型や uncalibrated は既に「sRGB として扱う」と宣言しているので true
    pub srgb_pixels: bool,
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

/// 入力形式を断るときの hint。2 箇所で同じ一覧を言うので 1 つにしておく。
///
/// WebP は lossy / lossless のどちらも読む（読むのは pure Rust の `image-webp`）。
/// 書けるのが lossless だけなのは出力側の事情で、入力には関係しない
const SUPPORTED_INPUTS: &str = "対応入力形式は JPEG / PNG / WebP（静止画）です";

pub fn load(path: &Path) -> Result<LoadedImage> {
    load_with(path, &LoadOptions::default())
}

pub fn load_with(path: &Path, opts: &LoadOptions) -> Result<LoadedImage> {
    let bytes = std::fs::read(path).map_err(|e| {
        Error::new(
            ErrorCode::InputUnreadable,
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
        .map_err(|e| Error::new(ErrorCode::InputUnreadable, e.to_string()))?;

    let image_format = reader.format().ok_or_else(|| {
        Error::new(
            ErrorCode::UnsupportedFormat,
            format!("{} の形式を判別できません", path.display()),
        )
        .with_hint(SUPPORTED_INPUTS)
    })?;

    let format = OutputFormat::from_decoded(image_format).ok_or_else(|| {
        Error::new(
            ErrorCode::UnsupportedFormat,
            format!("{image_format:?} は入力として未対応です"),
        )
        .with_hint(SUPPORTED_INPUTS)
    })?;
    if format == OutputFormat::WebP {
        refuse_animated_webp(&bytes, path)?;
    }

    let mut decoder = reader
        .into_decoder()
        .map_err(|e| Error::new(ErrorCode::InputDecodeFailed, e.to_string()))?;

    let icc = decoder
        .icc_profile()
        .ok()
        .flatten()
        .filter(|p| !p.is_empty());
    let icc_profile = icc.is_some();
    // WebP の EXIF はデコーダからチャンクの中身を受け取って自分で読む
    // （`read_webp_exif` の doc）。JPEG / PNG は従来どおりコンテナから読む
    let webp_exif = if format == OutputFormat::WebP {
        decoder.exif_metadata().ok().flatten()
    } else {
        None
    };

    let dynamic = DynamicImage::from_decoder(decoder)
        .map_err(|e| Error::new(ErrorCode::InputDecodeFailed, e.to_string()))?;

    let (exif_orientation, exif_color_space) = match format {
        OutputFormat::WebP => webp_exif.as_deref().map_or((1, None), read_webp_exif),
        _ => read_exif(&bytes),
    };
    let (image, orientation_applied) = if opts.apply_orientation {
        apply_orientation(dynamic, exif_orientation)
    } else {
        (dynamic, false)
    };

    let mut image = image.to_rgba8();
    let color = normalize_color(&mut image, icc.as_deref(), exif_color_space, opts);
    let has_alpha = image.pixels().any(|p| p[3] != 255);

    Ok(LoadedImage {
        image,
        format,
        exif_orientation,
        orientation_applied,
        icc_profile,
        // **名乗りの有無は「何があったか」だけで決まる。** ICC が付いていれば
        // 中身が何であれ名乗っており、EXIF ColorSpace はその値（1 = sRGB、
        // 0xFFFF = uncalibrated）が何であれ申告そのものである
        color_named: icc_profile || exif_color_space.is_some(),
        color_space: color.name,
        color_profile: color.profile,
        color_converted: color.converted,
        srgb_pixels: color.srgb,
        has_alpha,
        color_warnings: color.warnings,
    })
}

/// アニメーション WebP を断る。
///
/// **黙って 1 枚目を使わない。** `image` はアニメーションでも静止画として
/// デコードでき、そのときは 1 枚目のフレームを返す。だが 1 枚目が商品を
/// 代表している保証は無く（フェードインの真っ白な 1 枚目はよくある）、
/// 切り抜いた結果を見て初めて「別の絵だった」と気づくことになる。
/// どのフレームを使うかは呼び出し側にしか決められないので、取り出し方を
/// 示して断る。
///
/// **フレームが 1 枚でも断る。** 判定は旗で行い枚数を数えない。1 枚なら「どれが
/// 代表か」の問題は無いが、そのフレームはキャンバスの一部に置かれた矩形で
/// ありうる（ANMF のオフセット）。静止画の契約（寸法 = 画素の矩形）に載せるには
/// 取り出してもらうのが確実で、手順も複数枚のときと同じである。
///
/// 判定は `image-webp` のヘッダ解析（VP8X のアニメーションフラグ）に任せる。
/// RIFF を自前で読むと、壊れたファイルの扱いがデコーダ本体と割れうる
fn refuse_animated_webp(bytes: &[u8], path: &Path) -> Result<()> {
    let decoder = image::codecs::webp::WebPDecoder::new(std::io::Cursor::new(bytes))
        .map_err(|e| Error::new(ErrorCode::InputDecodeFailed, e.to_string()))?;
    if decoder.has_animation() {
        return Err(Error::new(
            ErrorCode::UnsupportedFormat,
            format!(
                "{} はアニメーション WebP です。静止画だけを入力として受け付けます",
                path.display()
            ),
        )
        .with_hint(
            "使うフレームを静止画として取り出してから渡してください\
             （例: webpmux -get frame 1 in.webp -o frame.webp）",
        ));
    }
    Ok(())
}

/// 色空間の判定と変換の結果。
struct ColorOutcome {
    name: String,
    /// 埋め込み ICC 自身の名乗り
    profile: Option<String>,
    converted: bool,
    /// 読み終えた画素を sRGB として扱えるか
    srgb: bool,
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
                name: COLOR_SPACE_UNCALIBRATED.into(),
                profile: None,
                converted: false,
                srgb: true,
                // ICC が無いので実体を確かめる術がない。sRGB として扱ったことを
                // 伝えるしかなく、次の一手も「変換して渡し直す」以外に無い
                warnings: vec![
                    Warning::new(
                        WarningCode::ColorSpaceUncalibrated,
                        "入力の色空間が uncalibrated です（AdobeRGB の可能性）。ICC も\
                         埋め込まれていないため sRGB として扱います",
                    )
                    .with_hint(
                        "色が合わない場合は、あらかじめ sRGB へ変換した素材を渡してください",
                    ),
                ],
            },
            _ => ColorOutcome {
                name: COLOR_SPACE_SRGB.into(),
                profile: None,
                converted: false,
                srgb: true,
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
            name: COLOR_SPACE_SRGB.into(),
            profile,
            converted: false,
            srgb: true,
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
                    srgb: true,
                    warnings: Vec::new(),
                }
            } else {
                let mut warning = Warning::new(
                    WarningCode::ColorConversionSkipped,
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
                    srgb: false,
                    warnings: vec![warning],
                }
            }
        }
        Interpretation::Unsupported => {
            let mut warning = Warning::new(
                WarningCode::ColorProfileUnsupported,
                format!("ICC プロファイル {label} は変換に対応していないため sRGB として扱います"),
            );
            if let Some(n) = &profile {
                warning = warning.with_data("profile", n.clone());
            }
            ColorOutcome {
                name: reported(),
                profile,
                converted: false,
                srgb: true,
                warnings: vec![warning],
            }
        }
    }
}

/// EXIF から Orientation と ColorSpace を読む。EXIF がなければ既定値を返す。
fn read_exif(bytes: &[u8]) -> (u16, Option<u16>) {
    let mut cursor = std::io::Cursor::new(bytes);
    match exif::Reader::new().read_from_container(&mut cursor) {
        Ok(exif) => exif_fields(&exif),
        Err(_) => (1, None),
    }
}

/// WebP の EXIF チャンクの中身から Orientation と ColorSpace を読む。
///
/// **`Exif\0\0` の接頭辞を剥がしてから読む。** 規格は TIFF 構造をそのまま入れると
/// 定めているが、JPEG の APP1 の中身を丸ごと写す道具がこの接頭辞を残す。
/// kamadak-exif 0.6.1 の `read_from_container` はそれを TIFF として読めずに
/// 失敗し、**向きが黙って 1 になる**——`--bbox` の座標がすべてずれる。
/// 接頭辞の無い正しい形はそのまま読むので、どちらの書き手でも同じ結果になる
fn read_webp_exif(raw: &[u8]) -> (u16, Option<u16>) {
    let tiff = raw.strip_prefix(b"Exif\0\0").unwrap_or(raw);
    match exif::Reader::new().read_raw(tiff.to_vec()) {
        Ok(exif) => exif_fields(&exif),
        Err(_) => (1, None),
    }
}

fn exif_fields(exif: &exif::Exif) -> (u16, Option<u16>) {
    let field = |tag| {
        exif.get_field(tag, In::PRIMARY)
            .and_then(|f| f.value.get_uint(0))
            .map(|v| v as u16)
    };
    (field(Tag::Orientation).unwrap_or(1), field(Tag::ColorSpace))
}

/// EXIF Orientation に従って回転・反転を適用する。
/// 変換自体は image クレートの実装に委ねる（8 通りの取り違えは事故のもとであるため）。
fn apply_orientation(mut image: DynamicImage, exif_orientation: u16) -> (DynamicImage, bool) {
    let orientation = u8::try_from(exif_orientation)
        .ok()
        .filter(|v| (2..=8).contains(v))
        .and_then(image::metadata::Orientation::from_exif);
    match orientation {
        Some(orientation) => {
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

    /// Orientation ただ 1 つを持つ TIFF 構造（リトルエンディアン）
    fn orientation_tiff(orientation: u16) -> Vec<u8> {
        let [lo, hi] = orientation.to_le_bytes();
        let mut tiff = vec![0x49, 0x49, 0x2A, 0x00, 0x08, 0x00, 0x00, 0x00];
        tiff.extend_from_slice(&[0x01, 0x00, 0x12, 0x01, 0x03, 0x00]);
        tiff.extend_from_slice(&[0x01, 0x00, 0x00, 0x00, lo, hi, 0x00, 0x00]);
        tiff.extend_from_slice(&[0x00, 0x00, 0x00, 0x00]);
        tiff
    }

    /// 規格どおりの素の TIFF も、JPEG 流の `Exif\0\0` 付きも同じ向きになる
    #[test]
    fn webp_exif_is_read_with_or_without_the_jpeg_style_prefix() {
        let tiff = orientation_tiff(6);
        assert_eq!(read_webp_exif(&tiff), (6, None));
        let mut prefixed = b"Exif\0\0".to_vec();
        prefixed.extend_from_slice(&tiff);
        assert_eq!(read_webp_exif(&prefixed), (6, None));
        // 読めない中身は既定値へ倒れる（読み込み全体は止めない。JPEG と同じ）
        assert_eq!(read_webp_exif(b"garbage"), (1, None));
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
        assert_eq!(err.code.as_str(), "UNSUPPORTED_FORMAT");
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
        assert!(loaded.srgb_pixels, "変換した画素は sRGB を名乗ってよい");
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
                ..Default::default()
            },
        )
        .unwrap();

        assert!(!raw.color_converted);
        assert!(
            !raw.srgb_pixels,
            "P3 のままの画素に sRGB を名乗らせてはいけない"
        );
        assert!(converted.srgb_pixels);
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
        assert_eq!(warnings[0].code.as_str(), "COLOR_PROFILE_UNSUPPORTED");
        assert!(
            warnings[0].message.contains("変換に対応していない"),
            "{warnings:?}"
        );
    }

    /// ICC が無い画像の画素には触らない。
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
