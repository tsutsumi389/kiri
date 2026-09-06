//! `kiri batch` — 仕様ファイルに従って複数の画像を一括処理する。
//!
//! 1 件の失敗で全体を止めない。数百点を回すバッチでは、失敗した項目を報告しつつ
//! 残りを処理し切るほうが有用であるため。失敗があれば終了コードで知らせる。

use std::path::Path;
use std::time::Instant;

use rayon::prelude::*;

use crate::batch::{self, BatchItem, ItemSettings};
use crate::cli::{BatchArgs, CutoutArgs, OutputOpts, parse_hex_color, parse_size};
use crate::commands::cutout;
use crate::cutout::DEFAULT_BORDER;
use crate::error::{Error, Result};
use crate::image_io::OutputFormat;
use crate::report::{BatchItemReport, BatchReport, ErrorBody};

pub fn run(args: &BatchArgs) -> Result<BatchReport> {
    let started = Instant::now();
    let spec = batch::load(&args.spec)?;
    let base = batch::base_dir(&args.spec, args.base_dir.as_deref());

    let process = |item: &BatchItem| -> BatchItemReport {
        let input = batch::resolve(&base, &item.input);
        let output = batch::resolve(&base, &item.output);
        let settings = item.settings.merged_over(&spec.defaults);

        let outcome = to_cutout_args(&input, &output, &settings, args.force)
            .and_then(|args| cutout::run(&args));

        match outcome {
            Ok(report) => BatchItemReport {
                input: input.display().to_string(),
                output: output.display().to_string(),
                status: "ok",
                result: Some(report),
                error: None,
            },
            Err(e) => BatchItemReport {
                input: input.display().to_string(),
                output: output.display().to_string(),
                status: "error",
                result: None,
                error: Some(ErrorBody::from(&e)),
            },
        }
    };

    // par_iter は入力順を保つので、結果の並びは仕様ファイルどおりになる
    let results: Vec<BatchItemReport> = if args.jobs == 1 {
        spec.items.iter().map(process).collect()
    } else {
        rayon::ThreadPoolBuilder::new()
            .num_threads(args.jobs)
            .build()
            .map_err(|e| Error::general("THREAD_POOL_FAILED", e.to_string()))?
            .install(|| spec.items.par_iter().map(process).collect())
    };

    let failed = results.iter().filter(|r| r.status == "error").count();
    let with_warnings = results
        .iter()
        .filter(|r| r.result.as_ref().is_some_and(|c| !c.warnings.is_empty()))
        .count();

    Ok(BatchReport {
        spec: args.spec.display().to_string(),
        total: results.len(),
        succeeded: results.len() - failed,
        failed,
        with_warnings,
        elapsed_ms: started.elapsed().as_millis(),
        results,
    })
}

/// 仕様の 1 項目を cutout の引数へ落とす。
///
/// cutout コマンドをそのまま呼ぶことで、単体実行とバッチで挙動が食い違わないようにする。
fn to_cutout_args(
    input: &Path,
    output: &Path,
    settings: &ItemSettings,
    force: bool,
) -> Result<CutoutArgs> {
    let format = settings
        .format
        .as_deref()
        .map(|name| {
            OutputFormat::from_name(name).ok_or_else(|| {
                Error::argument(
                    "UNKNOWN_OUTPUT_FORMAT",
                    format!("'{name}' は未対応の形式です"),
                )
                .with_hint("avif / png / jpeg のいずれかを指定してください")
            })
        })
        .transpose()?;

    let canvas = settings
        .canvas
        .as_deref()
        .map(|s| parse_size(s).map_err(|e| Error::argument("INVALID_CANVAS", e)))
        .transpose()?;

    let background = settings
        .background
        .as_deref()
        .map(|s| parse_hex_color(s).map_err(|e| Error::argument("INVALID_COLOR", e)))
        .transpose()?
        .unwrap_or([255, 255, 255]);

    Ok(CutoutArgs {
        input: input.to_path_buf(),
        bbox: settings.bbox,
        normalized: settings.normalized.unwrap_or(false),
        fg_seed: settings.fg_seeds.clone().unwrap_or_default(),
        tolerance: settings.tolerance.unwrap_or(12.0),
        border: settings.border.unwrap_or(DEFAULT_BORDER),
        cleanup: settings.cleanup.unwrap_or(2),
        feather: settings.feather.unwrap_or(1),
        no_despill: !settings.despill.unwrap_or(true),
        no_refine: !settings.refine.unwrap_or(true),
        edge_threshold: settings.edge_threshold.unwrap_or(8.0),
        canvas,
        fill_ratio: settings.fill_ratio.unwrap_or(0.85),
        debug_mask: None,
        // バッチは JSON だけで回す。数百点でプレビューを吐くと無駄な I/O になる
        preview: None,
        preview_size: crate::preview::DEFAULT_PANEL,
        no_preview_grid: false,
        out: OutputOpts {
            output: output.to_path_buf(),
            format,
            quality: settings.quality.unwrap_or(75.0),
            effort: settings.effort.unwrap_or(6),
            background,
            flatten: settings.flatten.unwrap_or(false),
            force,
        },
    })
}
