//! `kiri convert` — 形式変換のみを行う。
//!
//! 切り抜きを伴わないケース（既に透過済みの素材を AVIF にする等）のための入口。

use std::time::Instant;

use crate::cli::ConvertArgs;
use crate::error::{Error, Result};
use crate::image_io::{OutputFormat, SaveOptions, load, save};
use crate::report::{ConvertReport, Dimensions, OutputReport};

pub fn run(args: &ConvertArgs) -> Result<ConvertReport> {
    let start = Instant::now();

    let format = args
        .format
        .or_else(|| OutputFormat::from_path(&args.output))
        .ok_or_else(|| {
            Error::argument(
                "UNKNOWN_OUTPUT_FORMAT",
                format!(
                    "{} の拡張子から出力形式を判別できません",
                    args.output.display()
                ),
            )
            .with_hint("--format で avif / png / jpeg を明示してください")
        })?;

    // 読み込みより先に確認する。重い処理を走らせてから弾くのは無駄であるため。
    if args.output.exists() && !args.force {
        return Err(Error::argument(
            "OUTPUT_EXISTS",
            format!("{} は既に存在します", args.output.display()),
        )
        .with_hint("--force を付けると上書きします"));
    }

    let loaded = load::load(&args.input)?;

    let opts = SaveOptions {
        format,
        quality: args.quality,
        effort: args.effort,
        background: args.background,
    };
    let outcome = save(&args.output, &loaded.image, &opts)?;

    let mut warnings = loaded.warnings();
    warnings.extend(outcome.warnings);

    Ok(ConvertReport {
        input: args.input.display().to_string(),
        source: Dimensions {
            width: loaded.width(),
            height: loaded.height(),
        },
        outputs: vec![OutputReport {
            path: args.output.display().to_string(),
            format: format.as_str().to_string(),
            width: loaded.width(),
            height: loaded.height(),
            bytes: outcome.bytes,
        }],
        elapsed_ms: start.elapsed().as_millis(),
        warnings,
    })
}
