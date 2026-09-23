//! 1 枚の最終画像から、書き出す派生を作る。
//!
//! Phase 19（--max-bytes）と Phase 20（多派生）はどちらも「エンコードして書く」を
//! 奪い合う。先に 1 本の道へ畳み、N = 1 で ICC を付けない（`IccPolicy::None`）とき
//! Phase 17 と同じバイト列になることを固定してから、その上へ機能を載せる。既定の
//! `Embed` との差は iCCP / APP2 の 1 つだけで、それは `save.rs` のテストが見ている

use std::path::PathBuf;

use image::RgbaImage;

use super::save::{IccPolicy, OutputFormat, SaveOptions, encode, write_encoded};
use crate::error::Result;
use crate::report::OutputReport;
use crate::warning::Warning;

/// 書き出す 1 本。
///
/// 計画 7.2 が挙げた `role` / `width` / `height` / `fit` / `max_bytes` は、それを読む
/// フェーズで足す。**読まれないフィールドを先に置くと「指定したのに効かない」が
/// 型として作れてしまう。** 寸法は Phase 20 で `transform::resize::ResizeSpec` を
/// 1 つ足す形にする（`allow_upscale` を落とさないため）
#[derive(Debug, Clone)]
pub struct Derivation {
    /// 解決済みの書き出し先。命名や衝突の検査は render より前に済ませる
    pub path: PathBuf,
    pub format: OutputFormat,
    pub quality: f32,
    pub effort: u8,
    pub background: [u8; 3],
    pub flatten: bool,
    pub icc: IccPolicy,
}

impl Derivation {
    pub fn save_options(&self) -> SaveOptions {
        SaveOptions {
            format: self.format,
            quality: self.quality,
            effort: self.effort,
            background: self.background,
            flatten: self.flatten,
            icc: self.icc,
        }
    }
}

#[derive(Debug)]
pub struct Rendered {
    pub report: OutputReport,
    /// 派生ごとに分けて持つ。Phase 20 で全警告の `data` に `output` を付けるため
    pub warnings: Vec<Warning>,
}

/// 派生を順にエンコードし、`dry_run` でなければ書く。
///
/// **ICC はエンコーダの内側で埋まる**ので、`report.bytes` は ICC 込みの大きさで
/// ある（Phase 19 の探索はこの値だけを見ればよい）。dry-run でもエンコードまでは
/// 同じ道を通る。最初の失敗で止まる——部分失敗を扱うのは Phase 20 の仕事
pub fn render(
    image: &RgbaImage,
    derivations: &[Derivation],
    dry_run: bool,
) -> Result<Vec<Rendered>> {
    derivations
        .iter()
        .map(|d| {
            let (bytes, warnings) = encode(image, &d.save_options())?;
            if !dry_run {
                write_encoded(&d.path, &bytes)?;
            }
            Ok(Rendered {
                report: OutputReport {
                    path: d.path.display().to_string(),
                    format: d.format.as_str().to_string(),
                    width: image.width(),
                    height: image.height(),
                    bytes: bytes.len() as u64,
                    icc: d.icc.signal(d.format),
                },
                warnings,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn derivation(path: PathBuf, format: OutputFormat, icc: IccPolicy) -> Derivation {
        Derivation {
            path,
            format,
            quality: 75.0,
            effort: 6,
            background: [255, 255, 255],
            flatten: false,
            icc,
        }
    }

    /// 受け入れ基準 (a)。render は encode の結果に何も足さず、何も引かない。
    /// ここがずれると、Phase 19 の探索が見た大きさと書かれたファイルが食い違う
    #[test]
    fn render_with_one_derivation_writes_what_encode_returns() {
        let img = RgbaImage::from_fn(20, 16, |x, y| {
            image::Rgba([
                (x * 12) as u8,
                (y * 15) as u8,
                90,
                if x < 10 { 255 } else { 128 },
            ])
        });
        let dir = tempfile::tempdir().unwrap();
        for format in [OutputFormat::Png, OutputFormat::Jpeg, OutputFormat::Avif] {
            for icc in [IccPolicy::Embed, IccPolicy::None] {
                let name = format!("{}-{icc:?}.{}", format.as_str(), format.as_str());
                let d = derivation(dir.path().join("out").join(&name), format, icc);
                let (expected, expected_warnings) = encode(&img, &d.save_options()).unwrap();

                let rendered = render(&img, std::slice::from_ref(&d), false).unwrap();
                assert_eq!(rendered.len(), 1);
                let written = std::fs::read(&d.path).unwrap();
                assert_eq!(written, expected, "{name}");
                let report = &rendered[0].report;
                assert_eq!(report.bytes, written.len() as u64, "{name}");
                assert_eq!(report.icc, icc.signal(format), "{name}");
                assert_eq!(report.path, d.path.display().to_string());
                assert_eq!((report.width, report.height), (20, 16));
                let codes = |w: &[Warning]| w.iter().map(|w| w.code).collect::<Vec<_>>();
                assert_eq!(
                    codes(&rendered[0].warnings),
                    codes(&expected_warnings),
                    "{name}"
                );
            }
        }
    }

    /// dry-run は書かないだけで、報告は本番と同じ値になる
    #[test]
    fn dry_run_encodes_but_writes_nothing() {
        let img = RgbaImage::from_pixel(8, 8, image::Rgba([10, 20, 30, 255]));
        let dir = tempfile::tempdir().unwrap();
        for format in [OutputFormat::Png, OutputFormat::Jpeg, OutputFormat::Avif] {
            let d = derivation(
                dir.path().join(format!("dry.{}", format.as_str())),
                format,
                IccPolicy::Embed,
            );
            let rendered = render(&img, std::slice::from_ref(&d), true).unwrap();
            assert!(!d.path.exists(), "{format:?}");
            let (expected, _) = encode(&img, &d.save_options()).unwrap();
            assert_eq!(rendered[0].report.bytes, expected.len() as u64);
            assert_eq!(rendered[0].report.icc, IccPolicy::Embed.signal(format));
        }
    }
}
