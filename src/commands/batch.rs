//! `kiri batch` — 仕様ファイルに従って複数の画像を一括処理する。
//!
//! 1 件の失敗で全体を止めない。数百点を回すバッチでは、失敗した項目を報告しつつ
//! 残りを処理し切るほうが有用であるため。失敗があれば終了コードで知らせる。

use std::path::Path;
use std::time::Instant;

use rayon::prelude::*;

use crate::batch::{self, BatchItem, ItemSettings};
use crate::cli::{
    BatchArgs, ColorOpts, CutoutArgs, OutputOpts, Polygon, parse_hex_color, parse_size,
};
use crate::commands::cutout;
use crate::cutout::{CutoutOptions, DEFAULT_BORDER, Matting};
use crate::error::{Error, ErrorCode, Result};
use crate::image_io::OutputFormat;
use crate::report::{BatchItemReport, BatchReport, ErrorBody, SCHEMA_VERSION};

pub fn run(args: &BatchArgs) -> Result<BatchReport> {
    let started = Instant::now();
    let spec = batch::load(&args.spec)?;
    let base = batch::base_dir(&args.spec, args.base_dir.as_deref());

    let process = |item: &BatchItem| -> BatchItemReport {
        let input = batch::resolve(&base, &item.input);
        let output = batch::resolve(&base, &item.output);
        let settings = item.settings.merged_over(&spec.defaults);

        let outcome = to_cutout_args(&base, &input, &output, &settings, args.force, args.dry_run)
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
            .map_err(|e| Error::new(ErrorCode::ThreadPoolFailed, e.to_string()))?
            .install(|| spec.items.par_iter().map(process).collect())
    };

    let failed = results.iter().filter(|r| r.status == "error").count();
    let with_warnings = results
        .iter()
        .filter(|r| r.result.as_ref().is_some_and(|c| !c.warnings.is_empty()))
        .count();

    Ok(BatchReport {
        schema_version: SCHEMA_VERSION,
        spec: args.spec.display().to_string(),
        total: results.len(),
        succeeded: results.len() - failed,
        failed,
        with_warnings,
        dry_run: args.dry_run,
        elapsed_ms: started.elapsed().as_millis(),
        results,
    })
}

/// 仕様の 1 項目を cutout の引数へ落とす。
///
/// cutout コマンドをそのまま呼ぶことで、単体実行とバッチで挙動が食い違わないようにする。
///
/// `base` を受けるのは、指示として渡される画像（`trimap` / `fg_mask` /
/// `bg_mask`）のパスを `input` と同じ規則で解決するためである。**片方だけ
/// カレントディレクトリ基準にすると、同じ spec が実行場所によって違う
/// マスクを読む。**
fn to_cutout_args(
    base: &Path,
    input: &Path,
    output: &Path,
    settings: &ItemSettings,
    force: bool,
    dry_run: bool,
) -> Result<CutoutArgs> {
    let format = settings
        .format
        .as_deref()
        .map(|name| {
            OutputFormat::from_name(name).ok_or_else(|| {
                Error::new(
                    ErrorCode::UnknownOutputFormat,
                    format!("'{name}' は未対応の形式です"),
                )
                .with_hint("avif / png / jpeg のいずれかを指定してください")
            })
        })
        .transpose()?;

    let canvas = settings
        .canvas
        .as_deref()
        .map(|s| parse_size(s).map_err(|e| Error::new(ErrorCode::InvalidCanvas, e)))
        .transpose()?;

    let background = settings
        .background
        .as_deref()
        .map(|s| parse_hex_color(s).map_err(|e| Error::new(ErrorCode::InvalidColor, e)))
        .transpose()?
        .unwrap_or([255, 255, 255]);

    let seal = capped(settings.seal, 1, crate::cli::MAX_SEAL, "seal")?;
    let cleanup = capped(settings.cleanup, 2, crate::cli::MAX_CLEANUP, "cleanup")?;

    let path = |p: &Option<std::path::PathBuf>| p.as_ref().map(|p| batch::resolve(base, p));

    Ok(CutoutArgs {
        input: input.to_path_buf(),
        bbox: settings.bbox,
        normalized: settings.normalized.unwrap_or(false),
        fg_seed: settings.fg_seeds.clone().unwrap_or_default(),
        trimap: path(&settings.trimap),
        fg_mask: path(&settings.fg_mask),
        bg_mask: path(&settings.bg_mask),
        fg_polygon: polygons(settings.fg_polygons.as_deref(), "fg_polygons")?,
        bg_polygon: polygons(settings.bg_polygons.as_deref(), "bg_polygons")?,
        tolerance: checked(settings.tolerance, 12.0, "tolerance")?,
        border: settings.border.unwrap_or(DEFAULT_BORDER),
        cleanup,
        feather: settings.feather.unwrap_or(1),
        no_despill: !settings.despill.unwrap_or(true),
        no_refine: !settings.refine.unwrap_or(true),
        matting: matting(settings.matting.as_deref())?,
        smooth_contour: checked(
            settings.smooth_contour,
            crate::cutout::DEFAULT_SMOOTH_CONTOUR,
            "smooth_contour",
        )?,
        no_reclassify: !settings.reclassify.unwrap_or(true),
        color: ColorOpts {
            no_color_convert: !settings.color_convert.unwrap_or(true),
        },
        // 未指定は未指定のまま渡す。既定値で埋めてしまうと、テクスチャに応じた
        // 自動調整が spec を書いた人の「8 を指定した」と区別できなくなる
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
            dry_run,
        },
    })
}

/// spec の `matting` を解き方へ落とす。
///
/// **綴りを外したら断る。** 未知の値を既定へ落とすと、その項目だけ黙って
/// 別の解き方で処理され、数百点を回した後に仕上がりを見るまで気づけない。
/// CLI では clap が同じ役目を果たしている。
fn matting(name: Option<&str>) -> Result<Matting> {
    match name {
        None => Ok(CutoutOptions::default().matting),
        Some("projection") => Ok(Matting::Projection),
        Some("guided") => Ok(Matting::Guided),
        Some(other) => Err(Error::new(
            ErrorCode::SpecInvalid,
            format!("'{other}' は未対応の matting です"),
        )
        .with_hint("projection / guided のいずれかを指定してください")),
    }
}

/// spec の `[[x,y,x,y,...], ...]` を多角形へ落とす。
///
/// **検証は CLI と同じ関門（`Polygon::from_values`）を通す。** spec 経由でだけ
/// 2 点の「多角形」や奇数個の座標が通ると、その項目の指示だけが黙って
/// 無視される。数百点を回した後に、仕上がりを目で見るまで気づけない。
fn polygons(values: Option<&[Vec<f64>]>, key: &str) -> Result<Vec<Polygon>> {
    values
        .unwrap_or_default()
        .iter()
        .map(|v| {
            Polygon::from_values(v).map_err(|e| {
                Error::new(ErrorCode::InvalidPolygon, format!("{key}: {e}")).with_hint(
                    "1 つの多角形は [x1,y1,x2,y2,...] の並びで、3 点以上を書いてください",
                )
            })
        })
        .collect()
}

/// 数値の設定に CLI と同じ約束を掛ける。
///
/// バッチは spec の JSON を直接読むので clap の検証を通らない。負値や nan が
/// そのまま通ると、その項目だけ機能が黙って無効化されたまま数百点が処理され、
/// 結果の JSON にも異常が出ない。気づけるのは仕上がりを目で見たときになる。
fn checked(value: Option<f64>, default: f64, key: &str) -> Result<f64> {
    validate(value.unwrap_or(default), key)
}

/// 上限のある整数の設定に CLI と同じ関門を掛ける。
///
/// clap の `value_parser` に相当するものが spec には無い。上限を超えた値を
/// 通すと、`--seal` なら 1MP で秒単位、`--cleanup` なら商品ごと全消しという
/// 形で表れるが、どちらも「数百点を回し終えてから気づく」種類の失敗になる。
fn capped(value: Option<u32>, default: u32, max: u32, key: &str) -> Result<u32> {
    let value = value.unwrap_or(default);
    if value > max {
        return Err(Error::new(
            ErrorCode::InvalidSetting,
            format!("{key} は 0 から {max} の範囲で指定してください（{value} が指定されました）"),
        ));
    }
    Ok(value)
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
        return Err(Error::new(
            ErrorCode::InvalidSetting,
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
        let args = to_cutout_args(
            Path::new("."),
            Path::new("in.png"),
            Path::new("out.png"),
            &settings,
            false,
            false,
        )
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
        // 色変換だけ既定値の持ち主が CutoutOptions ではなく LoadOptions になる。
        // 読み込み側の設定なので、切り抜きの設定に混ぜるとかえって追えない
        assert_eq!(
            !args.color.no_color_convert,
            crate::image_io::LoadOptions::default().convert_color,
            "color_convert の既定値"
        );
    }
}
