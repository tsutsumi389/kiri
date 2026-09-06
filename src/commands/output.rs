//! 出力処理の共通部分。convert と resize が同じ規約で書き出すために使う。

use std::path::Path;
use std::time::Instant;

use image::RgbaImage;

use crate::cli::OutputOpts;
use crate::cutout::BackgroundEstimate;
use crate::error::{Error, Result};
use crate::image_io::{OutputFormat, SaveOptions, save};
use crate::report::{
    BackgroundReport, Dimensions, OutputReport, PerimeterDeltaE, PerimeterTexture, ProcessReport,
};

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
    ensure_path_writable(&opts.output, opts.force)
}

/// 本出力以外（プレビュー・デバッグマスク）にも同じ上書き規約を適用する。
///
/// 付随物だからと素通しにすると、利用者のファイルを黙って壊しうる。
pub fn ensure_path_writable(path: &Path, force: bool) -> Result<()> {
    if path.exists() && !force {
        return Err(Error::argument(
            "OUTPUT_EXISTS",
            format!("{} は既に存在します", path.display()),
        )
        .with_hint("--force を付けると上書きします"));
    }
    Ok(())
}

/// 小数第4位で丸める。実質的な情報量はそこまでで、無用な桁は
/// エージェントの差分比較を汚すだけであるため。
pub fn round4(v: f64) -> f64 {
    (v * 10_000.0).round() / 10_000.0
}

/// 背景推定を JSON のレポートへ落とす。
///
/// `info` と `cutout` の両方が同じ形を返す約束なので、組み立てを 1 箇所に置く。
/// 片方にだけ項目を足すと、エージェントは「この画像では測れなかった」のか
/// 「このコマンドは報告しない」のかを区別できない。
pub fn background_report(background: &BackgroundEstimate) -> BackgroundReport {
    BackgroundReport {
        rgb: background.rgb,
        uniformity: round4(background.uniformity),
        perimeter_delta_e: PerimeterDeltaE {
            p50: round4(background.delta_e.p50),
            p90: round4(background.delta_e.p90),
            max: round4(background.delta_e.max),
        },
        texture: PerimeterTexture {
            p50: round4(background.texture.p50),
            p90: round4(background.texture.p90),
        },
    }
}

/// 画像を書き出し、出力レポートと警告を返す。
pub fn write_image(
    image: &RgbaImage,
    opts: &OutputOpts,
    format: OutputFormat,
) -> Result<(OutputReport, Vec<String>)> {
    let save_opts = SaveOptions {
        format,
        quality: opts.quality,
        effort: opts.effort,
        background: opts.background,
        flatten: opts.flatten,
    };
    let outcome = save(&opts.output, image, &save_opts)?;
    Ok((
        OutputReport {
            path: opts.output.display().to_string(),
            format: format.as_str().to_string(),
            width: image.width(),
            height: image.height(),
            bytes: outcome.bytes,
        },
        outcome.warnings,
    ))
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
    let (output, save_warnings) = write_image(image, opts, format)?;
    warnings.extend(save_warnings);

    Ok(ProcessReport {
        input: input.display().to_string(),
        source: Dimensions {
            width: source.0,
            height: source.1,
        },
        outputs: vec![output],
        elapsed_ms: started.elapsed().as_millis(),
        warnings,
    })
}
