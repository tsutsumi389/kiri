//! `kiri cutout` — 背景を透過して商品を切り抜く。
//!
//! bbox は任意。未指定なら全自動で判定する。単色背景に限定したことで自動判定が
//! 成立するため、AI エージェントは「全件の座標を出す」のではなく「結果の JSON を
//! 見て、失敗した数枚だけを救済する」役割を担える。

use std::path::PathBuf;
use std::time::Instant;

use crate::cli::CutoutArgs;
use crate::commands::output;
use crate::cutout::{CutoutOptions, cutout};
use crate::error::{Error, Result};
use crate::image_io::load;
use crate::report::{BackgroundReport, CutoutReport, Dimensions, MaskReport};

pub fn run(args: &CutoutArgs) -> Result<CutoutReport> {
    let started = Instant::now();
    let format = output::resolve_format(&args.out)?;
    output::ensure_writable(&args.out)?;

    let loaded = load::load(&args.input)?;
    let (w, h) = (loaded.width(), loaded.height());

    let bbox = args
        .bbox
        .map(|b| resolve_bbox(b, args.normalized, w, h))
        .transpose()?;
    let fg_seeds = args
        .fg_seed
        .iter()
        .map(|p| resolve_point(*p, args.normalized, w, h))
        .collect::<Result<Vec<_>>>()?;

    let opts = CutoutOptions {
        tolerance: args.tolerance,
        border: args.border,
        bbox,
        fg_seeds,
        cleanup: args.cleanup,
        feather: args.feather,
        despill: !args.no_despill,
        edge_threshold: args.edge_threshold,
    };
    let result = cutout(&loaded.image, &opts);

    let debug_mask = write_debug_mask(args.debug_mask.as_ref(), &result.mask)?;

    let (output_report, save_warnings) = output::write_image(&result.image, &args.out, format)?;

    let mut warnings = loaded.warnings();
    warnings.extend(result.warnings);
    warnings.extend(save_warnings);

    Ok(CutoutReport {
        input: args.input.display().to_string(),
        source: Dimensions {
            width: w,
            height: h,
        },
        outputs: vec![output_report],
        background: BackgroundReport {
            rgb: result.background.rgb,
            uniformity: round4(result.background.uniformity),
        },
        tolerance: args.tolerance,
        applied_bbox: bbox.map(|(x1, y1, x2, y2)| [x1, y1, x2, y2]),
        mask: MaskReport {
            foreground_ratio: round4(result.stats.foreground_ratio),
            bbox: result.stats.bbox.map(|(x1, y1, x2, y2)| [x1, y1, x2, y2]),
            touches_edge: result.stats.touches_edge,
            debug_mask,
        },
        elapsed_ms: started.elapsed().as_millis(),
        warnings,
    })
}

fn write_debug_mask(path: Option<&PathBuf>, mask: &crate::cutout::Mask) -> Result<Option<String>> {
    let Some(path) = path else {
        return Ok(None);
    };
    mask.to_image().save(path).map_err(|e| {
        Error::general(
            "DEBUG_MASK_WRITE_FAILED",
            format!("{} に書けません: {e}", path.display()),
        )
    })?;
    Ok(Some(path.display().to_string()))
}

/// 指定された bbox を画素座標に落とす。
///
/// ビジョンモデルは 0.0-1.0 の正規化座標を返すことが多いため、`--normalized` で
/// 両方を受け付ける。画像外へはみ出した分は切り詰める。
pub fn resolve_bbox(
    bbox: [f64; 4],
    normalized: bool,
    width: u32,
    height: u32,
) -> Result<(u32, u32, u32, u32)> {
    let scaled = if normalized {
        if bbox.iter().any(|v| *v > 1.0) {
            return Err(Error::argument(
                "INVALID_BBOX",
                "--normalized 指定時、bbox の各値は 0.0-1.0 である必要があります",
            )
            .with_hint("画素座標で指定する場合は --normalized を外してください"));
        }
        [
            bbox[0] * f64::from(width),
            bbox[1] * f64::from(height),
            bbox[2] * f64::from(width),
            bbox[3] * f64::from(height),
        ]
    } else {
        bbox
    };

    if scaled[0] >= f64::from(width) || scaled[1] >= f64::from(height) {
        return Err(Error::argument(
            "INVALID_BBOX",
            format!(
                "bbox の始点 ({:.0},{:.0}) が画像 {width}x{height} の外です",
                scaled[0], scaled[1]
            ),
        ));
    }

    let x1 = scaled[0].floor().max(0.0) as u32;
    let y1 = scaled[1].floor().max(0.0) as u32;
    // 終点は画像内に収める。x1 より小さくならないよう下限も押さえる
    let x2 = (scaled[2].ceil() as u32).min(width - 1).max(x1);
    let y2 = (scaled[3].ceil() as u32).min(height - 1).max(y1);

    Ok((x1, y1, x2, y2))
}

fn resolve_point(point: [f64; 2], normalized: bool, width: u32, height: u32) -> Result<(u32, u32)> {
    let (x, y) = if normalized {
        if point[0] > 1.0 || point[1] > 1.0 {
            return Err(Error::argument(
                "INVALID_SEED",
                "--normalized 指定時、座標は 0.0-1.0 である必要があります",
            ));
        }
        (point[0] * f64::from(width), point[1] * f64::from(height))
    } else {
        (point[0], point[1])
    };

    if x >= f64::from(width) || y >= f64::from(height) {
        return Err(Error::argument(
            "INVALID_SEED",
            format!("座標 ({x:.0},{y:.0}) が画像 {width}x{height} の外です"),
        ));
    }
    Ok((x.floor() as u32, y.floor() as u32))
}

fn round4(v: f64) -> f64 {
    (v * 10_000.0).round() / 10_000.0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pixel_coordinates_pass_through() {
        let b = resolve_bbox([10.0, 20.0, 300.0, 400.0], false, 800, 600).unwrap();
        assert_eq!(b, (10, 20, 300, 400));
    }

    #[test]
    fn normalized_coordinates_are_scaled() {
        let b = resolve_bbox([0.1, 0.25, 0.5, 0.75], true, 1000, 800).unwrap();
        assert_eq!(b, (100, 200, 500, 600));
    }

    #[test]
    fn a_bbox_larger_than_the_image_is_clipped() {
        let b = resolve_bbox([10.0, 10.0, 9999.0, 9999.0], false, 100, 50).unwrap();
        assert_eq!(b, (10, 10, 99, 49));
    }

    #[test]
    fn a_bbox_starting_outside_the_image_is_an_error() {
        let err = resolve_bbox([500.0, 10.0, 600.0, 40.0], false, 100, 50).unwrap_err();
        assert_eq!(err.code, "INVALID_BBOX");
        assert_eq!(err.exit_code(), 2);
    }

    #[test]
    fn normalized_values_above_one_are_rejected() {
        // 画素座標を --normalized で渡す取り違えを検出する
        let err = resolve_bbox([120.0, 80.0, 900.0, 1400.0], true, 1600, 2000).unwrap_err();
        assert_eq!(err.code, "INVALID_BBOX");
        assert!(err.hint.unwrap().contains("--normalized"));
    }

    #[test]
    fn points_resolve_in_both_coordinate_systems() {
        assert_eq!(
            resolve_point([120.0, 80.0], false, 200, 200).unwrap(),
            (120, 80)
        );
        assert_eq!(
            resolve_point([0.5, 0.25], true, 200, 400).unwrap(),
            (100, 100)
        );
    }

    #[test]
    fn a_point_outside_the_image_is_an_error() {
        let err = resolve_point([200.0, 10.0], false, 100, 100).unwrap_err();
        assert_eq!(err.code, "INVALID_SEED");
    }
}
