//! 出力処理の共通部分。convert と resize が同じ規約で書き出すために使う。

use std::path::Path;
use std::time::Instant;

use image::RgbaImage;

use crate::cli::OutputOpts;
use crate::error::{Error, Result};
use crate::image_io::{OutputFormat, SaveOptions, save};
use crate::report::{Dimensions, OutputReport, ProcessReport};

/// 明示指定がなければ拡張子から出力形式を決める。
pub fn resolve_format(opts: &OutputOpts) -> Result<OutputFormat> {
    opts.format
        .or_else(|| OutputFormat::from_path(&opts.output))
        .ok_or_else(|| {
            Error::argument(
                "UNKNOWN_OUTPUT_FORMAT",
                format!(
                    "{} の拡張子から出力形式を判別できません",
                    opts.output.display()
                ),
            )
            .with_hint("--format で avif / png / jpeg を明示してください")
        })
}

/// 上書きの可否を確認する。重い処理を走らせる前に呼ぶこと。
pub fn ensure_writable(opts: &OutputOpts) -> Result<()> {
    if opts.output.exists() && !opts.force {
        return Err(Error::argument(
            "OUTPUT_EXISTS",
            format!("{} は既に存在します", opts.output.display()),
        )
        .with_hint("--force を付けると上書きします"));
    }
    Ok(())
}

/// 書き出して結果レポートを組み立てる。
pub fn finish(
    input: &Path,
    source: (u32, u32),
    image: &RgbaImage,
    opts: &OutputOpts,
    format: OutputFormat,
    started: Instant,
    mut warnings: Vec<String>,
) -> Result<ProcessReport> {
    let save_opts = SaveOptions {
        format,
        quality: opts.quality,
        effort: opts.effort,
        background: opts.background,
    };
    let outcome = save(&opts.output, image, &save_opts)?;
    warnings.extend(outcome.warnings);

    Ok(ProcessReport {
        input: input.display().to_string(),
        source: Dimensions {
            width: source.0,
            height: source.1,
        },
        outputs: vec![OutputReport {
            path: opts.output.display().to_string(),
            format: format.as_str().to_string(),
            width: image.width(),
            height: image.height(),
            bytes: outcome.bytes,
        }],
        elapsed_ms: started.elapsed().as_millis(),
        warnings,
    })
}
