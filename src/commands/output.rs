//! 出力処理の共通部分。convert と resize が同じ規約で書き出すために使う。

use std::path::Path;
use std::time::Instant;

use image::RgbaImage;

use crate::cli::OutputOpts;
use crate::cutout::{BackgroundEstimate, SubjectHint};
use crate::error::{Error, ErrorCode, Result};
use crate::image_io::{LoadedImage, OutputFormat, SaveOptions, encode, save};
use crate::report::{
    BackgroundReport, Dimensions, OutputReport, PerimeterDeltaE, PerimeterTexture, ProcessReport,
    SCHEMA_VERSION, SubjectReport,
};
use crate::warning::{Warning, WarningCode};

/// 明示指定がなければ拡張子から出力形式を決める。
pub fn resolve_format(opts: &OutputOpts) -> Result<OutputFormat> {
    opts.format
        .or_else(|| OutputFormat::from_path(&opts.output))
        .ok_or_else(|| {
            Error::new(
                ErrorCode::UnknownOutputFormat,
                format!(
                    "{} の拡張子から出力形式を判別できません",
                    opts.output.display()
                ),
            )
            .with_hint("--format で avif / png / jpeg を明示してください")
        })
}

/// 上書きの可否を確認する。重い処理を走らせる前に呼ぶこと。
///
/// `--dry-run` では検査しない。上書き検査は成果物を守るためのもので、
/// 1 バイトも書かない実行を止める理由が無いためである。**代わりに、本番実行なら
/// ここで落ちていた事実を警告で返す。** 黙って通すと、エージェントは dry-run の
/// 成功を見て本番へ進み、`OUTPUT_EXISTS` で二度手間になる。
pub fn ensure_writable(opts: &OutputOpts) -> Result<Option<Warning>> {
    if !opts.dry_run {
        ensure_path_writable(&opts.output, opts.force)?;
        return Ok(None);
    }
    if !opts.output.exists() || opts.force {
        return Ok(None);
    }
    Ok(Some(
        Warning::new(
            WarningCode::DryRunOutputExists,
            format!(
                "{} は既に存在します。dry-run なので書いていませんが、本番実行は上書きを拒みます",
                opts.output.display()
            ),
        )
        .with_hint("本番実行には --force が要ります")
        .with_data("path", opts.output.display().to_string()),
    ))
}

/// 本出力以外（プレビュー・デバッグマスク）にも同じ上書き規約を適用する。
///
/// 付随物だからと素通しにすると、利用者のファイルを黙って壊しうる。
pub fn ensure_path_writable(path: &Path, force: bool) -> Result<()> {
    if path.exists() && !force {
        return Err(Error::new(
            ErrorCode::OutputExists,
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

/// 主体の推定を JSON のレポートへ落とす。
///
/// `background_report` と同じ理由で組み立てを 1 箇所に置く。`info` と `cutout` が
/// 別々に組むと、片方だけ丸め方や項目が食い違っていても誰も気づかない。
pub fn subject_report(subject: &SubjectHint) -> SubjectReport {
    SubjectReport {
        bbox: subject.bbox,
        normalized_bbox: [
            round4(subject.normalized_bbox[0]),
            round4(subject.normalized_bbox[1]),
            round4(subject.normalized_bbox[2]),
            round4(subject.normalized_bbox[3]),
        ],
        area_ratio: round4(subject.area_ratio),
        capture_ratio: round4(subject.capture_ratio),
        delta_e: round4(subject.delta_e),
        leftover_ratio: round4(subject.leftover_ratio),
        touches_edge: subject.touches_edge,
        confidence: subject.confidence,
    }
}

/// 画像を書き出し、出力レポートと警告を返す。
pub fn write_image(
    image: &RgbaImage,
    opts: &OutputOpts,
    format: OutputFormat,
) -> Result<(OutputReport, Vec<Warning>)> {
    let save_opts = SaveOptions {
        format,
        quality: opts.quality,
        effort: opts.effort,
        background: opts.background,
        flatten: opts.flatten,
    };
    // dry-run でもエンコードは通す。`bytes` を見積もりにすると、
    // 品質と形式の判断が本番実行を挟まないと下せなくなる
    let (bytes, warnings) = if opts.dry_run {
        let (encoded, warnings) = encode(image, &save_opts)?;
        (encoded.len() as u64, warnings)
    } else {
        let outcome = save(&opts.output, image, &save_opts)?;
        (outcome.bytes, outcome.warnings)
    };
    Ok((
        OutputReport {
            path: opts.output.display().to_string(),
            format: format.as_str().to_string(),
            width: image.width(),
            height: image.height(),
            bytes,
        },
        warnings,
    ))
}

/// 書き出して結果レポートを組み立てる。
///
/// 元寸法と色空間はどちらも読み込み結果が持っているので、ばらして渡さず
/// `LoadedImage` のまま受ける。書き出す画像だけは加工後のものが来る。
pub fn finish(
    input: &Path,
    loaded: &LoadedImage,
    image: &RgbaImage,
    opts: &OutputOpts,
    format: OutputFormat,
    started: Instant,
    mut warnings: Vec<Warning>,
) -> Result<ProcessReport> {
    let (output, save_warnings) = write_image(image, opts, format)?;
    warnings.extend(save_warnings);

    Ok(ProcessReport {
        schema_version: SCHEMA_VERSION,
        input: input.display().to_string(),
        source: Dimensions {
            width: loaded.width(),
            height: loaded.height(),
        },
        outputs: vec![output],
        color_space: loaded.color_space.clone(),
        color_profile: loaded.color_profile.clone(),
        color_converted: loaded.color_converted,
        dry_run: opts.dry_run,
        // 回転は rotate コマンドだけが後から埋める
        rotate: None,
        elapsed_ms: started.elapsed().as_millis(),
        warnings,
    })
}
