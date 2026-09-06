//! `kiri info` — 画像の情報と背景推定を返す。
//!
//! AI エージェントが座標を出す前に寸法を知る必要があるため、また `uniformity` で
//! 対象画像が処理可能かを事前判断できるようにするために存在する。

use image::ImageFormat;

use crate::cli::InfoArgs;
use crate::commands::output::background_report;
use crate::cutout::estimate_background;
use crate::error::Result;
use crate::image_io::load;
use crate::report::InfoReport;

pub fn run(args: &InfoArgs) -> Result<InfoReport> {
    let loaded = load::load_with(&args.input, &args.color.to_load_options())?;
    let background = estimate_background(&loaded.image, args.border);

    let mut warnings = loaded.warnings();
    if !background.is_uniform() {
        warnings.push(format!(
            "背景の均一度が {:.2} と低く、単色背景ではない可能性があります。\
             kiri が対象とするのは単色背景の画像です",
            background.uniformity
        ));
    }

    Ok(InfoReport {
        input: args.input.display().to_string(),
        width: loaded.width(),
        height: loaded.height(),
        format: format_name(loaded.format).to_string(),
        exif_orientation: loaded.exif_orientation,
        orientation_applied: loaded.orientation_applied,
        color_space: loaded.color_space.clone(),
        color_profile: loaded.color_profile.clone(),
        color_converted: loaded.color_converted,
        icc_profile: loaded.icc_profile,
        has_alpha: loaded.has_alpha,
        background: background_report(&background),
        warnings,
    })
}

fn format_name(format: ImageFormat) -> &'static str {
    match format {
        ImageFormat::Jpeg => "jpeg",
        ImageFormat::Png => "png",
        _ => "unknown",
    }
}
