//! 出力画像のエンコードと書き出し。
//!
//! 常に生ピクセルから再エンコードするため、EXIF などのメタデータは自然に落ちる。
//! Web 配信ではサイズと個人情報の両面で不要なため、これを既定の挙動とする。

use std::path::Path;

use image::{ExtendedColorType, ImageEncoder, RgbaImage};
use ravif::{AlphaColorMode, Encoder as AvifEncoder, RGBA8};

use crate::error::{Error, ErrorCode, Result};
use crate::warning::{Warning, WarningCode};

#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum OutputFormat {
    Avif,
    Png,
    Jpeg,
}

impl OutputFormat {
    pub fn as_str(self) -> &'static str {
        match self {
            OutputFormat::Avif => "avif",
            OutputFormat::Png => "png",
            OutputFormat::Jpeg => "jpeg",
        }
    }

    /// アルファチャンネルを保持できる形式か。
    pub fn supports_alpha(self) -> bool {
        !matches!(self, OutputFormat::Jpeg)
    }

    /// 名前から出力形式を得る。仕様ファイルの "format" 用。
    pub fn from_name(name: &str) -> Option<Self> {
        match name.to_ascii_lowercase().as_str() {
            "avif" => Some(OutputFormat::Avif),
            "png" => Some(OutputFormat::Png),
            "jpeg" | "jpg" => Some(OutputFormat::Jpeg),
            _ => None,
        }
    }

    /// 拡張子から出力形式を推論する。
    pub fn from_path(path: &Path) -> Option<Self> {
        let ext = path.extension()?.to_str()?.to_ascii_lowercase();
        match ext.as_str() {
            "avif" => Some(OutputFormat::Avif),
            "png" => Some(OutputFormat::Png),
            "jpg" | "jpeg" => Some(OutputFormat::Jpeg),
            _ => None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct SaveOptions {
    pub format: OutputFormat,
    /// AVIF / JPEG の品質 (0-100)
    pub quality: f32,
    /// AVIF のエンコード速度 (1-10)。小さいほど高品質・低速。
    /// 実測では 1 は 7.7MP で 40 秒に達するため既定は 6。
    pub effort: u8,
    /// アルファを保持できない形式へ出力する際の合成色
    pub background: [u8; 3],
    /// 形式が透過を保持できる場合でも、背景色で塗り潰して不透明にする
    pub flatten: bool,
}

impl Default for SaveOptions {
    fn default() -> Self {
        Self {
            format: OutputFormat::Avif,
            quality: 75.0,
            effort: 6,
            background: [255, 255, 255],
            flatten: false,
        }
    }
}

pub struct SaveOutcome {
    pub bytes: u64,
    pub warnings: Vec<Warning>,
}

/// エンコードだけを行い、書き出さない。
///
/// `--dry-run` はここで止まる。「書かない」を「何もしない」にはしない。
/// エンコードまで通しておかないと `bytes` が見積もりになり、品質やサイズを
/// 決めるための実行に使えなくなる。**書き出しの直前までは同じ道を通る**ので、
/// `ALPHA_FLATTENED` のような書き出し由来の警告も本番と同じに出る。
pub fn encode(image: &RgbaImage, opts: &SaveOptions) -> Result<(Vec<u8>, Vec<Warning>)> {
    let mut warnings = Vec::new();

    if !(0.0..=100.0).contains(&opts.quality) {
        return Err(Error::new(
            ErrorCode::InvalidQuality,
            "quality は 0-100 の範囲で指定してください",
        ));
    }
    if !(1..=10).contains(&opts.effort) {
        return Err(Error::new(
            ErrorCode::InvalidEffort,
            "effort は 1-10 の範囲で指定してください",
        ));
    }

    let has_alpha = image.pixels().any(|p| p[3] != 255);
    // 形式が透過を扱えない場合は否応なく、--flatten 指定時は要求として塗り潰す
    let must_flatten = has_alpha && (opts.flatten || !opts.format.supports_alpha());
    if has_alpha && !opts.format.supports_alpha() {
        let [r, g, b] = opts.background;
        warnings.push(
            Warning::new(
                WarningCode::AlphaFlattened,
                format!(
                    "{} は透過を保持できないため #{r:02X}{g:02X}{b:02X} で合成しました",
                    opts.format.as_str()
                ),
            )
            .with_hint("透過を残すには --format png を指定してください")
            .with_data("format", opts.format.as_str())
            .with_data("background", vec![r, g, b]),
        );
    }

    let flattened;
    let target = if must_flatten {
        flattened = flatten_image(image, opts.background);
        &flattened
    } else {
        image
    };

    let encoded = match opts.format {
        OutputFormat::Avif => encode_avif(target, opts)?,
        OutputFormat::Png => encode_png(target)?,
        OutputFormat::Jpeg => encode_jpeg(target, opts)?,
    };

    Ok((encoded, warnings))
}

pub fn save(path: &Path, image: &RgbaImage, opts: &SaveOptions) -> Result<SaveOutcome> {
    let (encoded, warnings) = encode(image, opts)?;

    // let-chain は Rust 1.88 以降。MSRV 1.85 を保つためネストで書く
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)?;
        }
    }
    std::fs::write(path, &encoded).map_err(|e| {
        Error::new(
            ErrorCode::OutputWriteFailed,
            format!("{} に書き出せません: {e}", path.display()),
        )
    })?;

    Ok(SaveOutcome {
        bytes: encoded.len() as u64,
        warnings,
    })
}

fn encode_avif(image: &RgbaImage, opts: &SaveOptions) -> Result<Vec<u8>> {
    let pixels: Vec<RGBA8> = image
        .pixels()
        .map(|p| RGBA8 {
            r: p[0],
            g: p[1],
            b: p[2],
            a: p[3],
        })
        .collect();
    let src = imgref::Img::new(
        pixels.as_slice(),
        image.width() as usize,
        image.height() as usize,
    );

    // UnassociatedClean は完全透明部の RGB を捨てる。切り抜き後の画像は透明領域が
    // 広いため、ここでサイズを削れる。
    let encoded = AvifEncoder::new()
        .with_quality(opts.quality)
        .with_alpha_quality(opts.quality.max(80.0))
        .with_speed(opts.effort)
        .with_alpha_color_mode(AlphaColorMode::UnassociatedClean)
        .encode_rgba(src)
        .map_err(|e| Error::new(ErrorCode::AvifEncodeFailed, e.to_string()))?;

    Ok(encoded.avif_file)
}

fn encode_png(image: &RgbaImage) -> Result<Vec<u8>> {
    let mut buf = Vec::new();
    image::codecs::png::PngEncoder::new(&mut buf)
        .write_image(
            image.as_raw(),
            image.width(),
            image.height(),
            ExtendedColorType::Rgba8,
        )
        .map_err(|e| Error::new(ErrorCode::PngEncodeFailed, e.to_string()))?;
    Ok(buf)
}

fn encode_jpeg(image: &RgbaImage, opts: &SaveOptions) -> Result<Vec<u8>> {
    let rgb = flatten_onto(image, opts.background);
    let mut buf = Vec::new();
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut buf, opts.quality.round() as u8)
        .write_image(&rgb, image.width(), image.height(), ExtendedColorType::Rgb8)
        .map_err(|e| Error::new(ErrorCode::JpegEncodeFailed, e.to_string()))?;
    Ok(buf)
}

/// アルファを指定色の上に合成し、不透明な RGBA を返す。
fn flatten_image(image: &RgbaImage, background: [u8; 3]) -> RgbaImage {
    let rgb = flatten_onto(image, background);
    let mut out = RgbaImage::new(image.width(), image.height());
    for (i, pixel) in out.pixels_mut().enumerate() {
        *pixel = image::Rgba([rgb[i * 3], rgb[i * 3 + 1], rgb[i * 3 + 2], 255]);
    }
    out
}

/// アルファを指定色の上に合成して RGB に落とす。
fn flatten_onto(image: &RgbaImage, background: [u8; 3]) -> Vec<u8> {
    let mut out = Vec::with_capacity(image.width() as usize * image.height() as usize * 3);
    for p in image.pixels() {
        let a = p[3] as u32;
        for c in 0..3 {
            let fg = p[c] as u32;
            let bg = background[c] as u32;
            // 四捨五入つきの線形合成
            out.push(((fg * a + bg * (255 - a) + 127) / 255) as u8);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_is_inferred_from_extension() {
        assert_eq!(
            OutputFormat::from_path(Path::new("a.avif")),
            Some(OutputFormat::Avif)
        );
        assert_eq!(
            OutputFormat::from_path(Path::new("a.PNG")),
            Some(OutputFormat::Png)
        );
        assert_eq!(
            OutputFormat::from_path(Path::new("a.jpg")),
            Some(OutputFormat::Jpeg)
        );
        assert_eq!(
            OutputFormat::from_path(Path::new("a.jpeg")),
            Some(OutputFormat::Jpeg)
        );
        assert_eq!(OutputFormat::from_path(Path::new("a.webp")), None);
        assert_eq!(OutputFormat::from_path(Path::new("noext")), None);
    }

    #[test]
    fn format_names_are_parsed_case_insensitively() {
        assert_eq!(OutputFormat::from_name("avif"), Some(OutputFormat::Avif));
        assert_eq!(OutputFormat::from_name("JPEG"), Some(OutputFormat::Jpeg));
        assert_eq!(OutputFormat::from_name("jpg"), Some(OutputFormat::Jpeg));
        assert_eq!(OutputFormat::from_name("webp"), None);
    }

    #[test]
    fn only_jpeg_lacks_alpha_support() {
        assert!(OutputFormat::Avif.supports_alpha());
        assert!(OutputFormat::Png.supports_alpha());
        assert!(!OutputFormat::Jpeg.supports_alpha());
    }

    #[test]
    fn flatten_image_removes_all_transparency() {
        let mut img = RgbaImage::new(2, 1);
        img.put_pixel(0, 0, image::Rgba([10, 20, 30, 0]));
        img.put_pixel(1, 0, image::Rgba([10, 20, 30, 255]));
        let out = flatten_image(&img, [255, 255, 255]);
        assert_eq!(out.get_pixel(0, 0).0, [255, 255, 255, 255]);
        assert_eq!(out.get_pixel(1, 0).0, [10, 20, 30, 255]);
    }

    #[test]
    fn fully_transparent_pixels_become_the_background_color() {
        let mut img = RgbaImage::new(1, 2);
        img.put_pixel(0, 0, image::Rgba([10, 20, 30, 0]));
        img.put_pixel(0, 1, image::Rgba([10, 20, 30, 255]));
        let rgb = flatten_onto(&img, [255, 255, 255]);
        assert_eq!(&rgb[0..3], &[255, 255, 255]);
        assert_eq!(&rgb[3..6], &[10, 20, 30]);
    }

    #[test]
    fn half_transparent_pixels_blend_evenly() {
        let mut img = RgbaImage::new(1, 1);
        img.put_pixel(0, 0, image::Rgba([0, 0, 0, 128]));
        let rgb = flatten_onto(&img, [255, 255, 255]);
        // 128/255 の黒を白に載せるとほぼ中間になる
        assert!((127..=128).contains(&rgb[0]), "got {}", rgb[0]);
    }
}
