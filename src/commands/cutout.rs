//! `kiri cutout` — 背景を透過して商品を切り抜く。
//!
//! bbox は任意。未指定なら全自動で判定する。単色背景に限定したことで自動判定が
//! 成立するため、AI エージェントは「全件の座標を出す」のではなく「結果の JSON を
//! 見て、失敗した数枚だけを救済する」役割を担える。

use std::path::PathBuf;
use std::time::Instant;

use crate::cli::CutoutArgs;
use crate::commands::output::{self, round4};
use crate::cutout::{CutoutOptions, cutout};
use crate::error::{Error, Result};
use crate::image_io::{OutputFormat, SaveOptions, load, save};
use crate::preview::{PreviewSpec, contact_sheet};
use crate::report::{
    BackgroundReport, CanvasReport, CutoutReport, Dimensions, MaskReport, PerimeterDeltaE,
};
use crate::transform::canvas::{CanvasSpec, apply as canvas_apply, plan as canvas_plan};

pub fn run(args: &CutoutArgs) -> Result<CutoutReport> {
    let started = Instant::now();
    let format = output::resolve_format(&args.out)?;
    output::ensure_writable(&args.out)?;
    let preview_format = check_side_outputs(args)?;

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
        refine: !args.no_refine,
    };
    let result = cutout(&loaded.image, &opts);

    let debug_mask = write_debug_mask(args.debug_mask.as_ref(), &result.mask)?;

    let mut warnings = loaded.warnings();
    warnings.extend(result.warnings.clone());

    let (final_image, canvas) = match args.canvas {
        Some((cw, ch)) => {
            let (image, report) = place_on_canvas(&result, cw, ch, args, &mut warnings)?;
            (image, Some(report))
        }
        None => (result.image.clone(), None),
    };

    let (output_report, save_warnings) = output::write_image(&final_image, &args.out, format)?;
    warnings.extend(save_warnings);

    let preview = write_preview(
        args,
        preview_format,
        &loaded.image,
        &result.mask,
        &final_image,
        &mut warnings,
    );

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
            perimeter_delta_e: PerimeterDeltaE {
                p50: round4(result.background.delta_e.p50),
                p90: round4(result.background.delta_e.p90),
                max: round4(result.background.delta_e.max),
            },
        },
        tolerance: args.tolerance,
        applied_bbox: bbox.map(|(x1, y1, x2, y2)| [x1, y1, x2, y2]),
        mask: MaskReport {
            foreground_ratio: round4(result.stats.foreground_ratio),
            bbox: result.stats.bbox.map(|(x1, y1, x2, y2)| [x1, y1, x2, y2]),
            touches_edge: result.stats.touches_edge,
            separability: result.separability.map(round4),
            halo_ratio: round4(result.diagnostics.halo_ratio),
            edge_width: round4(result.diagnostics.edge_width),
            debug_mask,
        },
        canvas,
        preview,
        elapsed_ms: started.elapsed().as_millis(),
        warnings,
    })
}

/// 切り抜いた商品を余白ごと切り詰め、指定サイズのキャンバス中央へ配置する。
fn place_on_canvas(
    result: &crate::cutout::CutoutResult,
    width: u32,
    height: u32,
    args: &CutoutArgs,
    warnings: &mut Vec<String>,
) -> Result<(image::RgbaImage, CanvasReport)> {
    // フェザリングされた薄い縁まで含めて切り詰める。前景判定(128以上)で切ると
    // 輪郭の階調が落ちてギザギザに戻ってしまう
    let (x1, y1, x2, y2) = result.mask.bbox_above(0).ok_or_else(|| {
        Error::processing(
            "NO_FOREGROUND",
            "前景が検出されなかったためキャンバスに配置できません",
        )
        .with_hint("--tolerance を下げるか --bbox で対象範囲を指定してください")
    })?;

    let trimmed =
        image::imageops::crop_imm(&result.image, x1, y1, x2 - x1 + 1, y2 - y1 + 1).to_image();

    let spec = CanvasSpec {
        width,
        height,
        fill_ratio: args.fill_ratio,
        // --flatten が指定されていれば下地を塗る。既定は透明のまま
        background: args.out.flatten.then_some(args.out.background),
    };
    let plan = canvas_plan((trimmed.width(), trimmed.height()), &spec)?;
    let placed = canvas_apply(&trimmed, &spec)?;

    if plan.scale > 1.0 {
        warnings.push(format!(
            "商品を {:.2} 倍に拡大して配置しました。元素材以上の解像度にはなりません",
            plan.scale
        ));
    }

    Ok((
        placed,
        CanvasReport {
            width,
            height,
            fill_ratio: args.fill_ratio,
            content: [plan.content.0, plan.content.1],
            offset: [plan.offset.0, plan.offset.1],
            scale: round4(plan.scale),
        },
    ))
}

/// 重い処理に入る前に、付随出力（プレビュー・デバッグマスク）のパスを検証する。
///
/// 付随出力は本出力を書いた後に書かれるため、パスが衝突していると成果物を
/// 上書きしてしまう。しかも結果 JSON は上書き前の寸法とサイズを報告するので、
/// エージェントには検知できない。機械可読なレポートが嘘をつくのは致命的なので、
/// 必ず事前に弾く。
///
/// プレビューの出力形式もここで確定させる。拡張子が解釈できないまま処理を
/// 進めて最後に落ちるより、着手前に断るほうが無駄がない。
fn check_side_outputs(args: &CutoutArgs) -> Result<Option<OutputFormat>> {
    let conflict = |path: &PathBuf, flag: &str| -> Result<()> {
        if path == &args.out.output {
            return Err(Error::argument(
                "SIDE_OUTPUT_CONFLICT",
                format!("{flag} と --output に同じパスは指定できません"),
            )
            .with_hint("付随出力は本出力の後に書かれるため、成果物を壊します"));
        }
        output::ensure_path_writable(path, args.out.force)
    };

    if let Some(mask) = args.debug_mask.as_ref() {
        conflict(mask, "--debug-mask")?;
    }

    let Some(preview) = args.preview.as_ref() else {
        return Ok(None);
    };
    conflict(preview, "--preview")?;
    if let Some(mask) = args.debug_mask.as_ref() {
        if preview == mask {
            return Err(Error::argument(
                "SIDE_OUTPUT_CONFLICT",
                "--preview と --debug-mask に同じパスは指定できません",
            ));
        }
    }

    // 本出力と同じ規約で拡張子から決める。--output は解釈できない拡張子を
    // エラーにするので、こちらだけ黙って PNG にすると契約が不揃いになる
    let format = OutputFormat::from_path(preview).ok_or_else(|| {
        Error::argument(
            "UNKNOWN_OUTPUT_FORMAT",
            format!("{} の拡張子から出力形式を判別できません", preview.display()),
        )
        .with_hint("--preview には avif / png / jpeg のいずれかの拡張子を指定してください")
    })?;
    Ok(Some(format))
}

/// 検証用のコンタクトシートを書き出す。
///
/// 結果パネルにはキャンバス配置まで済んだ最終画像を使う。AI に見せるのは
/// 「実際に書き出されたもの」でなければ、判断が実物とずれるため。
///
/// 書き出しに失敗しても処理全体は失敗させない。プレビューは検証用の付随物で
/// あり、これを理由にエラーを返すと「成果物は書けているのにエラー」となって、
/// エージェントは再実行し、今度は OUTPUT_EXISTS で二重に詰まる。
fn write_preview(
    args: &CutoutArgs,
    format: Option<OutputFormat>,
    original: &image::RgbaImage,
    mask: &crate::cutout::Mask,
    final_image: &image::RgbaImage,
    warnings: &mut Vec<String>,
) -> Option<String> {
    let path = args.preview.as_ref()?;
    let format = format?;

    let spec = PreviewSpec {
        panel: args.preview_size,
        grid: !args.no_preview_grid,
    };
    let opts = SaveOptions {
        format,
        quality: 85.0,
        effort: 6,
        background: [255, 255, 255],
        flatten: false,
    };

    let written = contact_sheet(original, mask, final_image, &spec)
        .and_then(|sheet| save(path, &sheet, &opts));

    match written {
        Ok(_) => Some(path.display().to_string()),
        Err(e) => {
            warnings.push(format!(
                "プレビューを {} に書けませんでした: {}",
                path.display(),
                e.message
            ));
            None
        }
    }
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
