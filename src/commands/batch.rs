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

    let seal = settings.seal.unwrap_or(1);
    if seal > crate::cli::MAX_SEAL {
        return Err(Error::argument(
            "INVALID_SETTING",
            format!(
                "seal は 0 から {} の範囲で指定してください（{seal} が指定されました）",
                crate::cli::MAX_SEAL
            ),
        ));
    }

    Ok(CutoutArgs {
        input: input.to_path_buf(),
        bbox: settings.bbox,
        normalized: settings.normalized.unwrap_or(false),
        fg_seed: settings.fg_seeds.clone().unwrap_or_default(),
        tolerance: checked(settings.tolerance, 12.0, "tolerance")?,
        border: settings.border.unwrap_or(DEFAULT_BORDER),
        cleanup: settings.cleanup.unwrap_or(2),
        feather: settings.feather.unwrap_or(1),
        no_despill: !settings.despill.unwrap_or(true),
        no_refine: !settings.refine.unwrap_or(true),
        // 未指定は未指定のまま渡す。既定値で埋めてしまうと、テクスチャに応じた
        // 自動調整が spec を書いた人の「8 を指定した」と区別できなくなる
        // 未指定は未指定のまま渡す。既定値で埋めてしまうと、テクスチャに応じた
        // 自動調整が「spec に 8 と書いた」と区別できなくなる
        edge_threshold: checked_opt(settings.edge_threshold, "edge_threshold")?,
        step_tolerance: checked(settings.step_tolerance, 2.2, "step_tolerance")?,
        shadow_tolerance: checked(settings.shadow_tolerance, 35.0, "shadow_tolerance")?,
        seal,
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

/// 数値の設定に CLI と同じ約束を掛ける。
///
/// バッチは spec の JSON を直接読むので clap の検証を通らない。負値や nan が
/// そのまま通ると、その項目だけ機能が黙って無効化されたまま数百点が処理され、
/// 結果の JSON にも異常が出ない。気づけるのは仕上がりを目で見たときになる。
fn checked(value: Option<f64>, default: f64, key: &str) -> Result<f64> {
    validate(value.unwrap_or(default), key)
}

/// 既定値を持たない設定用。未指定は未指定のまま返す。
///
/// 「未指定」と「既定値を明示」を区別する設定（edge_threshold）では、ここで
/// 埋めてしまうと下流の自動調整が働かなくなる。
fn checked_opt(value: Option<f64>, key: &str) -> Result<Option<f64>> {
    value.map(|v| validate(v, key)).transpose()
}

fn validate(v: f64, key: &str) -> Result<f64> {
    if !v.is_finite() || v < 0.0 {
        return Err(Error::argument(
            "INVALID_SETTING",
            format!("{key} は 0 以上の有限な数値である必要があります（{v} が指定されました）"),
        ));
    }
    Ok(v)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cutout::CutoutOptions;

    /// 何も書かれていない仕様項目の既定値が、ライブラリの既定値と食い違わないこと。
    ///
    /// 同じ数字が clap の `default_value_t`、`CutoutOptions::default()`、そして
    /// ここの `unwrap_or` の 3 箇所に書かれている。片方だけ動かしてもコンパイルは
    /// 通り、テストも「その値でたまたま通る」ので誰も気づかない。CLI と
    /// ライブラリの突き合わせは tests/cli.rs にあるが、`to_cutout_args` は
    /// 非公開なのでそちらからは触れない。同じモジュール内なら呼べる。
    #[test]
    fn the_batch_defaults_match_the_library_defaults() {
        let settings = ItemSettings::default();
        let args = to_cutout_args(Path::new("in.png"), Path::new("out.png"), &settings, false)
            .expect("既定値だけの項目は解釈できるはず");
        let defaults = CutoutOptions::default();

        assert_eq!(args.tolerance, defaults.tolerance, "tolerance の既定値");
        assert_eq!(args.border, defaults.border, "border の既定値");
        assert_eq!(args.cleanup, defaults.cleanup, "cleanup の既定値");
        assert_eq!(args.feather, defaults.feather, "feather の既定値");
        assert_eq!(
            args.edge_threshold, defaults.edge_threshold,
            "edge_threshold の既定値（どちらも未指定）"
        );
        assert_eq!(
            args.step_tolerance, defaults.step_tolerance,
            "step_tolerance の既定値"
        );
        assert_eq!(
            args.shadow_tolerance, defaults.shadow_tolerance,
            "shadow_tolerance の既定値"
        );
        assert_eq!(args.seal, defaults.seal, "seal の既定値");
        assert_eq!(!args.no_despill, defaults.despill, "デスピルの既定");
        assert_eq!(!args.no_refine, defaults.refine, "アルファ再推定の既定");
    }
}
