//! `kiri info` — 画像の情報と背景推定を返す。
//!
//! AI エージェントが座標を出す前に寸法を知る必要があるため、また `uniformity` で
//! 対象画像が処理可能かを事前判断できるようにするために存在する。

use image::ImageFormat;

use crate::cli::InfoArgs;
use crate::commands::output::round4;
use crate::cutout::estimate_background;
use crate::error::Result;
use crate::image_io::load;
use crate::report::{BackgroundReport, InfoReport, PerimeterDeltaE};

pub fn run(args: &InfoArgs) -> Result<InfoReport> {
    let loaded = load::load(&args.input)?;
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
        color_space: loaded.color_space.as_str().to_string(),
        icc_profile: loaded.icc_profile,
        has_alpha: loaded.has_alpha,
        background: BackgroundReport {
            rgb: background.rgb,
            uniformity: round4(background.uniformity),
            perimeter_delta_e: PerimeterDeltaE {
                p50: round4(background.delta_e.p50),
                p90: round4(background.delta_e.p90),
                max: round4(background.delta_e.max),
            },
        },
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
