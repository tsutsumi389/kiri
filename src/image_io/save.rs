//! 出力画像のエンコードと書き出し。
//!
//! 常に生ピクセルから再エンコードするため、EXIF などのメタデータは自然に落ちる。
//! Web 配信ではサイズと個人情報の両面で不要なため、これを既定の挙動とする。
//! ただし sRGB の ICC だけは例外で、既定で埋める（出力の色の名乗り。`IccPolicy`）。

use std::path::Path;

use image::{ExtendedColorType, ImageEncoder, RgbaImage};
use ravif::{AlphaColorMode, Encoder as AvifEncoder, RGBA8};
use serde::Serialize;

use crate::color::srgb_profile::srgb_icc;
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

    /// この形式のエンコーダが**実際に受け取る**品質。持たない形式では `None`。
    ///
    /// **JPEG はここで丸める。** `image` の JPEG エンコーダは `u8` しか受けず、
    /// `--quality 33.3` は 33 として効く。丸めを `encode_jpeg` の中に閉じていた
    /// ときは、報告の `quality_used` が 33.3 を名乗って「実際に使った品質」が
    /// 嘘になっていた。**丸める場所は 1 つだけにする**——`encode_jpeg` も
    /// `--max-bytes` の報告もここを引く。
    ///
    /// AVIF は `ravif` が f32 をそのまま受けるので手を入れない。PNG は無損失で、
    /// `SaveOptions::quality` を何に変えてもバイト列は 1 バイトも動かない
    pub fn effective_quality(self, quality: f32) -> Option<f32> {
        match self {
            OutputFormat::Avif => Some(quality),
            OutputFormat::Jpeg => Some(quality.round()),
            OutputFormat::Png => None,
        }
    }

    /// 品質というつまみを持つ形式か。**PNG だけが持たない。**
    ///
    /// `--max-bytes` の探索が段を降りられるかはこれで決まる。「試したが
    /// 変わらなかった」と「試す意味が無い」は結果が同じでも報告が違う
    /// （前者は attempts が伸び、後者は 1 のまま）ので、形式の側で答える。
    /// **答えは `effective_quality` から引く。** 2 つの述語を別々に持つと、
    /// 形式が増えたときに片方だけが更新されうる
    pub fn has_quality(self) -> bool {
        self.effective_quality(0.0).is_some()
    }

    /// ファイル名に付ける拡張子。**`as_str()` とは別物である。**
    ///
    /// `as_str()` は報告の `outputs[].format` に出る名乗りで、JPEG は `"jpeg"`。
    /// 一方ファイル名の慣習は `.jpg` なので、`--naming` の `{ext}` はこちらを使う。
    /// 2 つが食い違って見えるのは今日の `--output out.jpg` も同じで
    /// （`format` は `"jpeg"` を返している）、**新しい不揃いを作ってはいない**。
    ///
    /// `from_path(extension())` が必ず自分へ戻ることは
    /// `the_extension_round_trips_through_from_path` が固定する
    pub fn extension(self) -> &'static str {
        match self {
            OutputFormat::Avif => "avif",
            OutputFormat::Png => "png",
            OutputFormat::Jpeg => "jpg",
        }
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

/// 出力に sRGB を名乗らせるか。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IccPolicy {
    /// PNG は iCCP、JPEG は APP2 に sRGB の ICC を埋める。AVIF は ICC を入れず、
    /// ravif が AV1 の色情報で名乗る（外す口が無い）
    Embed,
    /// 埋めない。PNG / JPEG のバイト列は Phase 17 と 1 バイトも変わらない
    None,
}

impl IccPolicy {
    pub fn signal(self, format: OutputFormat) -> IccSignal {
        match (format, self) {
            (OutputFormat::Avif, _) => IccSignal::Nclx,
            (_, IccPolicy::Embed) => IccSignal::Embedded,
            (_, IccPolicy::None) => IccSignal::None,
        }
    }
}

/// 出力が実際にどう名乗ったか（`outputs[].icc`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum IccSignal {
    Embedded,
    /// ICC ではなく AV1 シーケンスヘッダの色情報（CICP 1/13/6/full）。
    /// avif-serialize は既定値と同じ `colr` を省くので、コンテナに `colr` は無い
    Nclx,
    None,
}

impl IccSignal {
    pub fn as_str(self) -> &'static str {
        match self {
            IccSignal::Embedded => "embedded",
            IccSignal::Nclx => "nclx",
            IccSignal::None => "none",
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
    /// 既定は `Embed`。CLI の既定と同じ。preview は None を明示する
    pub icc: IccPolicy,
}

impl Default for SaveOptions {
    fn default() -> Self {
        Self {
            format: OutputFormat::Avif,
            quality: 75.0,
            effort: 6,
            background: [255, 255, 255],
            flatten: false,
            icc: IccPolicy::Embed,
        }
    }
}

pub struct SaveOutcome {
    pub bytes: u64,
    pub warnings: Vec<Warning>,
}

/// エンコードへ渡す直前の画素と、そこまでで出た警告。
///
/// **品質に依らない前処理をここで抱える。** `--max-bytes` の探索は同じ画像を
/// 品質だけ変えて何度もエンコードするので、アルファの全画素走査と合成を段ごとに
/// やり直すと、24.5MP の JPEG では 1 段あたり 98MB の複製と全画素走査が積む。
/// 合成が要らなければ入力を借りたままにする（`Cow::Borrowed`）ので、
/// **単発のエンコードでは今までと 1 バイトも 1 回の複製も変わらない。**
pub struct Prepared<'a> {
    image: std::borrow::Cow<'a, RgbaImage>,
    pub warnings: Vec<Warning>,
}

/// 品質に依らない下ごしらえ。**値域の検査・アルファの走査・合成はここ 1 回だけ。**
///
/// `opts` のうちここが見るのは `format` / `flatten` / `background` / `effort` で、
/// **`quality` は見ない**（値域の検査を除く）。だから探索が段を降りても結果は
/// 変わらず、1 回で済む。
pub fn prepare<'a>(image: &'a RgbaImage, opts: &SaveOptions) -> Result<Prepared<'a>> {
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

    let image = if must_flatten {
        std::borrow::Cow::Owned(flatten_image(image, opts.background))
    } else {
        std::borrow::Cow::Borrowed(image)
    };
    Ok(Prepared { image, warnings })
}

/// 下ごしらえ済みの画素を、いまの `opts.quality` でエンコードする。
///
/// **探索が何度も呼ぶのはここだけ。** 警告は `Prepared` が 1 組だけ持っている
/// ので、段の数だけ同じ警告が積むこともない。
pub fn encode_prepared(prepared: &Prepared, opts: &SaveOptions) -> Result<Vec<u8>> {
    let target = prepared.image.as_ref();
    match opts.format {
        OutputFormat::Avif => encode_avif(target, opts),
        OutputFormat::Png => encode_png(target, opts.icc),
        OutputFormat::Jpeg => encode_jpeg(target, opts),
    }
}

/// エンコードだけを行い、書き出さない。
///
/// `--dry-run` はここで止まる。「書かない」を「何もしない」にはしない。
/// エンコードまで通しておかないと `bytes` が見積もりになり、品質やサイズを
/// 決めるための実行に使えなくなる。**書き出しの直前までは同じ道を通る**ので、
/// `ALPHA_FLATTENED` のような書き出し由来の警告も本番と同じに出る。
pub fn encode(image: &RgbaImage, opts: &SaveOptions) -> Result<(Vec<u8>, Vec<Warning>)> {
    let prepared = prepare(image, opts)?;
    let encoded = encode_prepared(&prepared, opts)?;
    Ok((encoded, prepared.warnings))
}

pub fn save(path: &Path, image: &RgbaImage, opts: &SaveOptions) -> Result<SaveOutcome> {
    let (encoded, warnings) = encode(image, opts)?;
    write_encoded(path, &encoded)?;
    Ok(SaveOutcome {
        bytes: encoded.len() as u64,
        warnings,
    })
}

/// エンコード済みのバイト列を書く。親ディレクトリが無ければ作る。
pub fn write_encoded(path: &Path, bytes: &[u8]) -> Result<()> {
    // let-chain は Rust 1.88 以降。MSRV 1.85 を保つためネストで書く
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)?;
        }
    }
    std::fs::write(path, bytes).map_err(|e| {
        Error::new(
            ErrorCode::OutputWriteFailed,
            format!("{} に書き出せません: {e}", path.display()),
        )
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

fn encode_png(image: &RgbaImage, icc: IccPolicy) -> Result<Vec<u8>> {
    let mut buf = Vec::new();
    let mut encoder = image::codecs::png::PngEncoder::new(&mut buf);
    if icc == IccPolicy::Embed {
        // sRGB チャンクは併記しない。PNG 3 が iCCP との併存を should not とし、
        // png クレートも 2 つを排他にしている
        encoder
            .set_icc_profile(srgb_icc().to_vec())
            .map_err(|e| Error::new(ErrorCode::PngEncodeFailed, e.to_string()))?;
    }
    encoder
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
    // 丸めはここでは決めない。`effective_quality` が 1 箇所で決め、報告も同じ値を出す
    let quality = OutputFormat::Jpeg
        .effective_quality(opts.quality)
        .unwrap_or(opts.quality) as u8;
    let mut encoder = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut buf, quality);
    if opts.icc == IccPolicy::Embed {
        // APP2 は image が APP0(JFIF) の直後へ書く。SOI の直後に置くと JFIF に反する
        encoder
            .set_icc_profile(srgb_icc().to_vec())
            .map_err(|e| Error::new(ErrorCode::JpegEncodeFailed, e.to_string()))?;
    }
    encoder
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

    /// `{ext}` で綴った拡張子は、読み戻すと同じ形式になる。
    ///
    /// **ここが割れると `--naming` が嘘のファイル名を作る。** `{ext}` を
    /// `as_str()` から取ると JPEG が `.jpeg` になり、`--output` の拡張子から
    /// 形式を決める既存の規約と綴りが揃わない
    #[test]
    fn the_extension_round_trips_through_from_path() {
        for format in [OutputFormat::Avif, OutputFormat::Png, OutputFormat::Jpeg] {
            let name = format!("out.{}", format.extension());
            assert_eq!(
                OutputFormat::from_path(Path::new(&name)),
                Some(format),
                "{name}"
            );
        }
        assert_eq!(OutputFormat::Jpeg.extension(), "jpg", "慣習は .jpg");
        assert_eq!(OutputFormat::Jpeg.as_str(), "jpeg", "名乗りは jpeg のまま");
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

    /// PNG だけが品質を持たない。ここが反転すると `--max-bytes` が
    /// 無損失の形式で梯子を降り、同じバイト列を 8 回作る
    #[test]
    fn only_png_lacks_a_quality_knob() {
        assert!(OutputFormat::Avif.has_quality());
        assert!(OutputFormat::Jpeg.has_quality());
        assert!(!OutputFormat::Png.has_quality());
    }

    /// **JPEG の丸めは 1 箇所にしかない。** エンコーダへ渡る値と
    /// `effective_quality` が名乗る値が割れると、`quality_used` が嘘になる
    #[test]
    fn the_effective_quality_is_what_the_encoder_receives() {
        assert_eq!(OutputFormat::Jpeg.effective_quality(33.3), Some(33.0));
        assert_eq!(OutputFormat::Jpeg.effective_quality(33.7), Some(34.0));
        assert_eq!(OutputFormat::Avif.effective_quality(33.3), Some(33.3));
        assert_eq!(OutputFormat::Png.effective_quality(33.3), None);

        // 丸めた先と同じ値を渡したエンコードは 1 バイトも違わない
        let img = gradient(false);
        let at = |quality: f32| {
            encode(
                &img,
                &SaveOptions {
                    format: OutputFormat::Jpeg,
                    quality,
                    ..Default::default()
                },
            )
            .unwrap()
            .0
        };
        assert_eq!(at(33.3), at(33.0));
    }

    /// PNG の「品質を持たない」は主張であって、実装が追随していなければ嘘になる。
    /// 梯子の両端で同じバイト列が出ることをここで固定する
    #[test]
    fn png_bytes_do_not_move_with_quality() {
        let img = gradient(true);
        let at = |quality: f32| {
            encode(
                &img,
                &SaveOptions {
                    format: OutputFormat::Png,
                    quality,
                    ..Default::default()
                },
            )
            .unwrap()
            .0
        };
        assert_eq!(at(85.0), at(25.0));
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

    /// 半透明を含むグラデーション。単色だとエンコーダの分岐を素通りしかねない
    fn gradient(translucent: bool) -> RgbaImage {
        RgbaImage::from_fn(32, 24, |x, y| {
            let a = if translucent && x >= 16 {
                (y * 10) as u8
            } else {
                255
            };
            image::Rgba([(x * 8) as u8, (y * 10) as u8, 128, a])
        })
    }

    fn encoded(img: &RgbaImage, format: OutputFormat, icc: IccPolicy) -> Vec<u8> {
        let opts = SaveOptions {
            format,
            icc,
            ..Default::default()
        };
        encode(img, &opts).unwrap().0
    }

    /// PNG のチャンクを (型, データ) で並べる。CRC は読み飛ばす
    fn png_chunks(bytes: &[u8]) -> Vec<([u8; 4], &[u8])> {
        assert_eq!(&bytes[..8], b"\x89PNG\r\n\x1a\n");
        let mut out = Vec::new();
        let mut i = 8;
        while i < bytes.len() {
            let len = u32::from_be_bytes(bytes[i..i + 4].try_into().unwrap()) as usize;
            let kind: [u8; 4] = bytes[i + 4..i + 8].try_into().unwrap();
            out.push((kind, &bytes[i + 8..i + 8 + len]));
            i += 12 + len;
        }
        assert_eq!(i, bytes.len(), "チャンクの長さがファイル末尾と合わない");
        out
    }

    /// 指定した型のチャンクを CRC ごと落とす
    fn png_without(bytes: &[u8], kind: &[u8; 4]) -> Vec<u8> {
        let mut out = bytes[..8].to_vec();
        let mut i = 8;
        while i < bytes.len() {
            let len = u32::from_be_bytes(bytes[i..i + 4].try_into().unwrap()) as usize;
            if &bytes[i + 4..i + 8] != kind {
                out.extend_from_slice(&bytes[i..i + 12 + len]);
            }
            i += 12 + len;
        }
        out
    }

    /// SOS より前のマーカーセグメントを (マーカー, ファイル内の位置, ペイロード) で並べる
    fn jpeg_segments(bytes: &[u8]) -> Vec<(u8, usize, &[u8])> {
        assert_eq!(&bytes[..2], &[0xFF, 0xD8], "SOI で始まっていない");
        let mut out = Vec::new();
        let mut i = 2;
        loop {
            assert_eq!(bytes[i], 0xFF, "{i} にマーカーが無い");
            let marker = bytes[i + 1];
            let len = u16::from_be_bytes([bytes[i + 2], bytes[i + 3]]) as usize;
            out.push((marker, i, &bytes[i + 4..i + 2 + len]));
            if marker == 0xDA {
                return out;
            }
            i += 2 + len;
        }
    }

    fn is_sof(marker: u8) -> bool {
        (0xC0..=0xCF).contains(&marker) && ![0xC4, 0xC8, 0xCC].contains(&marker)
    }

    /// ISOBMFF の子ボックスを (型, 中身) で並べる。size 0 / 1 は ravif が書かないので扱わない
    fn boxes(bytes: &[u8]) -> Vec<([u8; 4], &[u8])> {
        let mut out = Vec::new();
        let mut i = 0;
        while i < bytes.len() {
            let size = u32::from_be_bytes(bytes[i..i + 4].try_into().unwrap()) as usize;
            assert!(size >= 8, "size {size} のボックスは想定していない");
            let kind: [u8; 4] = bytes[i + 4..i + 8].try_into().unwrap();
            out.push((kind, &bytes[i + 8..i + size]));
            i += size;
        }
        assert_eq!(i, bytes.len(), "ボックスの長さが親と合わない");
        out
    }

    fn child<'a>(parent: &'a [u8], kind: &[u8; 4]) -> &'a [u8] {
        let found: Vec<_> = boxes(parent)
            .into_iter()
            .filter(|(k, _)| k == kind)
            .collect();
        assert_eq!(
            found.len(),
            1,
            "{} がちょうど 1 個ではない",
            String::from_utf8_lossy(kind)
        );
        found[0].1
    }

    /// meta は full box なので、子の前に version / flags の 4 バイトがある
    fn meta(avif: &[u8]) -> &[u8] {
        &child(avif, b"meta")[4..]
    }

    fn ipco_kinds(avif: &[u8]) -> Vec<[u8; 4]> {
        let ipco = child(child(meta(avif), b"iprp"), b"ipco");
        boxes(ipco).into_iter().map(|(k, _)| k).collect()
    }

    fn be(bytes: &[u8]) -> u64 {
        bytes.iter().fold(0, |v, &b| (v << 8) | b as u64)
    }

    /// 主画像（pitm が指す項目）の AV1 ビットストリームを iloc から切り出す。
    /// 半透明の画像は mdat にアルファの AV1 も入るので、先頭の OBU を読むだけでは足りない
    fn primary_item(avif: &[u8]) -> &[u8] {
        let meta = meta(avif);
        let pitm = child(meta, b"pitm");
        assert_eq!(pitm[0], 0, "pitm は version 0 のみ想定");
        let primary = be(&pitm[4..6]);

        let iloc = child(meta, b"iloc");
        assert_eq!(iloc[0], 0, "iloc は version 0 のみ想定");
        let offset_size = (iloc[4] >> 4) as usize;
        let length_size = (iloc[4] & 0xF) as usize;
        let base_size = (iloc[5] >> 4) as usize;
        let count = be(&iloc[6..8]);
        let mut p = 8;
        for _ in 0..count {
            let id = be(&iloc[p..p + 2]);
            p += 4; // item_ID と data_reference_index
            let base = be(&iloc[p..p + base_size]);
            p += base_size;
            let extents = be(&iloc[p..p + 2]);
            p += 2;
            let mut found = None;
            for _ in 0..extents {
                let offset = (base + be(&iloc[p..p + offset_size])) as usize;
                p += offset_size;
                let len = be(&iloc[p..p + length_size]) as usize;
                p += length_size;
                if id == primary {
                    assert!(
                        found.is_none(),
                        "主画像が複数の extent に分かれるのは想定していない"
                    );
                    found = Some(&avif[offset..offset + len]);
                }
            }
            if let Some(data) = found {
                return data;
            }
        }
        panic!("pitm の項目 {primary} が iloc に無い");
    }

    struct Bits<'a> {
        bytes: &'a [u8],
        pos: usize,
    }

    impl Bits<'_> {
        fn f(&mut self, n: usize) -> u32 {
            let mut v = 0;
            for _ in 0..n {
                let bit = (self.bytes[self.pos >> 3] >> (7 - (self.pos & 7))) & 1;
                v = (v << 1) | bit as u32;
                self.pos += 1;
            }
            v
        }
    }

    #[derive(Debug, PartialEq, Eq)]
    struct Cicp {
        cp: u32,
        tc: u32,
        mc: u32,
        full_range: bool,
    }

    /// AV1 の sequence_header_obu を color_config まで読む（AV1 仕様 5.5.1 / 5.5.2）
    fn parse_seq(payload: &[u8]) -> Cicp {
        let mut b = Bits {
            bytes: payload,
            pos: 0,
        };
        let profile = b.f(3);
        b.f(1); // still_picture
        // 静止画の簡略ヘッダだけを読む。通常のヘッダは timing_info などの枝が多く、
        // ravif が書かない形を検証されないまま抱えることになる
        assert_eq!(
            b.f(1),
            1,
            "reduced_still_picture_header ではない（ravif が変わった）"
        );
        b.f(5); // seq_level_idx[0]
        let width_bits = b.f(4) as usize + 1;
        let height_bits = b.f(4) as usize + 1;
        b.f(width_bits);
        b.f(height_bits);
        // use_128x128_superblock / filter_intra / intra_edge / superres / cdef / restoration
        b.f(6);

        let high_bitdepth = b.f(1) == 1;
        if profile == 2 && high_bitdepth {
            b.f(1);
        }
        let mono = profile != 1 && b.f(1) == 1;
        let (cp, tc, mc) = if b.f(1) == 1 {
            (b.f(8), b.f(8), b.f(8))
        } else {
            (2, 2, 2) // unspecified
        };
        let full_range = if !mono && cp == 1 && tc == 13 && mc == 0 {
            true // sRGB の恒等行列は range を読まずに full と決まる
        } else {
            b.f(1) == 1
        };
        Cicp {
            cp,
            tc,
            mc,
            full_range,
        }
    }

    fn leb128(bytes: &[u8], p: &mut usize) -> usize {
        let mut v = 0;
        for i in 0..8 {
            let c = bytes[*p];
            *p += 1;
            v |= ((c & 0x7F) as usize) << (i * 7);
            if c < 0x80 {
                break;
            }
        }
        v
    }

    /// 主画像のシーケンスヘッダが名乗る CICP。無ければ None
    fn color_cicp(avif: &[u8]) -> Option<Cicp> {
        let av1 = primary_item(avif);
        let mut p = 0;
        while p < av1.len() {
            let h = av1[p];
            assert!(
                h & 0b10 != 0,
                "obu_has_size_field が無い（ravif が変わった）"
            );
            let kind = (h >> 3) & 0xF;
            p += 1 + ((h >> 2) & 1) as usize; // extension ヘッダの 1 バイト
            let size = leb128(av1, &mut p);
            if kind == 1 {
                return Some(parse_seq(&av1[p..p + size]));
            }
            p += size;
        }
        None
    }

    /// 受け入れ基準 (a)。ICC を抜けば Phase 17 の `encode_png` と 1 バイトも違わない
    #[test]
    fn icc_none_png_is_the_phase17_encoder_output() {
        let img = gradient(true);
        let mut phase17 = Vec::new();
        image::codecs::png::PngEncoder::new(&mut phase17)
            .write_image(
                img.as_raw(),
                img.width(),
                img.height(),
                ExtendedColorType::Rgba8,
            )
            .unwrap();
        assert_eq!(encoded(&img, OutputFormat::Png, IccPolicy::None), phase17);
    }

    #[test]
    fn icc_none_jpeg_is_the_phase17_encoder_output() {
        let img = gradient(true);
        let rgb = flatten_onto(&img, [255, 255, 255]);
        let mut phase17 = Vec::new();
        image::codecs::jpeg::JpegEncoder::new_with_quality(&mut phase17, 75)
            .write_image(&rgb, img.width(), img.height(), ExtendedColorType::Rgb8)
            .unwrap();
        assert_eq!(encoded(&img, OutputFormat::Jpeg, IccPolicy::None), phase17);
    }

    /// AVIF は ICC の口を持たない。ポリシーで何かが変わったら、名乗りの説明が嘘になる
    #[test]
    fn avif_bytes_do_not_depend_on_the_icc_policy() {
        for translucent in [false, true] {
            let img = gradient(translucent);
            assert_eq!(
                encoded(&img, OutputFormat::Avif, IccPolicy::Embed),
                encoded(&img, OutputFormat::Avif, IccPolicy::None),
                "translucent={translucent}"
            );
        }
    }

    /// 受け入れ基準 (c)。iCCP は 1 個だけ、IHDR の直後（PLTE / IDAT より前が規格の要求）
    #[test]
    fn png_carries_exactly_one_iccp_right_after_ihdr() {
        let png = encoded(&gradient(true), OutputFormat::Png, IccPolicy::Embed);
        let chunks = png_chunks(&png);
        let kinds: Vec<[u8; 4]> = chunks.iter().map(|(k, _)| *k).collect();

        assert_eq!(kinds[0], *b"IHDR");
        assert_eq!(kinds[1], *b"iCCP");
        assert_eq!(kinds.last(), Some(b"IEND"));
        // `all` は空の範囲で真になる。IDAT が 1 つも無い並びを通さないよう、有ることも言う
        assert!(kinds.contains(b"IDAT"), "{kinds:?}");
        assert!(
            kinds[2..kinds.len() - 1].iter().all(|k| k == b"IDAT"),
            "{kinds:?}"
        );
        assert_eq!(kinds.iter().filter(|k| *k == b"iCCP").count(), 1);
        // 併記すると読み手によって優先が割れる
        assert!(!kinds.contains(b"sRGB"));

        let data = chunks[1].1;
        let nul = data.iter().position(|&b| b == 0).unwrap();
        assert!((1..=79).contains(&nul), "プロファイル名の長さ {nul}");
        assert_eq!(data[nul + 1], 0, "圧縮方式は deflate(0) だけが規格にある");

        use image::ImageDecoder;
        let mut decoder = image::codecs::png::PngDecoder::new(std::io::Cursor::new(&png)).unwrap();
        assert_eq!(decoder.icc_profile().unwrap().as_deref(), Some(srgb_icc()));
    }

    #[test]
    fn png_with_icc_differs_from_none_only_by_the_iccp_chunk() {
        let img = gradient(true);
        let with = encoded(&img, OutputFormat::Png, IccPolicy::Embed);
        let plain = encoded(&img, OutputFormat::Png, IccPolicy::None);
        assert_ne!(with, plain);
        assert_eq!(png_without(&with, b"iCCP"), plain);
    }

    /// APP2 は APP0(JFIF) の直後。JFIF は APP0 が SOI の直後であることを求める
    #[test]
    fn jpeg_carries_exactly_one_icc_app2_right_after_jfif() {
        let jpeg = encoded(&gradient(true), OutputFormat::Jpeg, IccPolicy::Embed);
        let segs = jpeg_segments(&jpeg);

        assert_eq!(segs[0].0, 0xE0);
        assert!(segs[0].2.starts_with(b"JFIF\0"));
        assert_eq!(segs[1].0, 0xE2);
        assert_eq!(segs[1].1, 20, "APP0 の直後");
        assert!(segs.iter().all(|s| s.0 != 0xE1), "APP1 を書く理由は無い");
        assert_eq!(segs.iter().filter(|s| s.0 == 0xE2).count(), 1);

        let p = segs[1].2;
        assert_eq!(&p[..12], b"ICC_PROFILE\0");
        assert_eq!((p[12], p[13]), (1, 1), "516 バイトは 1 セグメントに収まる");
        assert_eq!(&p[14..], srgb_icc());

        let sof = segs.iter().position(|s| is_sof(s.0)).expect("SOF が無い");
        assert!(1 < sof, "APP2 は SOF より前に置く");
    }

    #[test]
    fn jpeg_with_icc_differs_from_none_only_by_the_app2_segment() {
        let img = gradient(true);
        let with = encoded(&img, OutputFormat::Jpeg, IccPolicy::Embed);
        let plain = encoded(&img, OutputFormat::Jpeg, IccPolicy::None);
        let seg_len = 2 + u16::from_be_bytes([with[22], with[23]]) as usize;
        assert_eq!(&with[20..22], &[0xFF, 0xE2]);

        let mut stripped = with[..20].to_vec();
        stripped.extend_from_slice(&with[20 + seg_len..]);
        assert_eq!(stripped, plain);
        // マーカー 2 + 長さ 2 + "ICC_PROFILE\0" 12 + 通し番号 2
        assert_eq!(with.len(), plain.len() + 18 + srgb_icc().len());
    }

    /// AVIF の名乗りは AV1 の中だけにある。`colr` が出るようになったら、
    /// schema の notes と README の説明を書き直す合図
    #[test]
    fn avif_names_srgb_only_through_the_av1_sequence_header() {
        for translucent in [false, true] {
            let avif = encoded(&gradient(translucent), OutputFormat::Avif, IccPolicy::Embed);
            let kinds = ipco_kinds(&avif);
            assert!(kinds.contains(b"av1C"), "{kinds:?}");
            assert!(!kinds.contains(b"colr"), "translucent={translucent}");
            assert_eq!(
                color_cicp(&avif),
                Some(Cicp {
                    cp: 1,
                    tc: 13,
                    mc: 6,
                    full_range: true
                }),
                "translucent={translucent}"
            );
        }
    }

    #[test]
    fn icc_signal_follows_the_format() {
        let cases = [
            (OutputFormat::Png, IccPolicy::Embed, IccSignal::Embedded),
            (OutputFormat::Png, IccPolicy::None, IccSignal::None),
            (OutputFormat::Jpeg, IccPolicy::Embed, IccSignal::Embedded),
            (OutputFormat::Jpeg, IccPolicy::None, IccSignal::None),
            // ravif に名乗りを外す口が無いので、None でも nclx
            (OutputFormat::Avif, IccPolicy::Embed, IccSignal::Nclx),
            (OutputFormat::Avif, IccPolicy::None, IccSignal::Nclx),
        ];
        for (format, policy, signal) in cases {
            assert_eq!(policy.signal(format), signal, "{format:?} {policy:?}");
        }
        assert_eq!(
            serde_json::to_value(IccSignal::Embedded).unwrap(),
            IccSignal::Embedded.as_str()
        );
        assert_eq!(serde_json::to_value(IccSignal::Nclx).unwrap(), "nclx");
        assert_eq!(serde_json::to_value(IccSignal::None).unwrap(), "none");
    }
}
