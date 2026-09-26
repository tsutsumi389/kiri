//! `kiri batch` — 仕様ファイルに従って複数の画像を一括処理する。
//!
//! 1 件の失敗で全体を止めない。数百点を回すバッチでは、失敗した項目を報告しつつ
//! 残りを処理し切るほうが有用であるため。失敗があれば終了コードで知らせる。

use std::path::Path;
use std::time::Instant;

use rayon::prelude::*;

use crate::batch::{self, BatchItem, ItemSettings};
use crate::cli::{
    BatchArgs, ColorOpts, CutoutArgs, OutputOpts, Polygon, RotateArg, SegmentOpts, parse_hex_color,
    parse_size,
};
use crate::commands::{cutout, output};
use crate::compliance::FailOn;
use crate::cutout::{BackgroundModel, CutoutOptions, DEFAULT_BORDER, Matting, OptimizeFixed};
use crate::error::{Error, ErrorCode, Result};
use crate::image_io::OutputFormat;
use crate::image_io::derive::DeriveSpec;
use crate::profile::{self, ExplicitOptions, PROFILE_NAMES};
use crate::report::{BatchItemReport, BatchReport, ErrorBody, ManifestItem, SCHEMA_VERSION};
use crate::segment::SegmentMode;
use crate::transform::shadow::ShadowMode;
use crate::warning::{Warning, WarningCode};

/// 「処理は成功したが規格に達しなかった」項目の綴り。
///
/// `"ok"` / `"error"` の 2 値に足す 3 つ目で、**`succeeded` と `failed` の
/// どちらの定義も動かさない。**
pub const REJECTED: &str = "rejected";

pub fn run(args: &BatchArgs) -> Result<BatchReport> {
    let started = Instant::now();
    let spec = batch::load(&args.spec)?;
    let base = batch::base_dir(&args.spec, args.base_dir.as_deref());
    // **1 件も処理する前に目録の上書き可否を問う。** 書き終えてから断ると、
    // 数百点を書いた後に `OUTPUT_EXISTS` が `Err` として返り、`BatchReport` が
    // 丸ごと捨てられる——利用者に残るのはエラー 1 行だけで、何枚書かれたのかも
    // どれが成功したのかも返らない。convert / resize / rotate / cutout の 4 つが
    // `run()` の先頭で問うているのと同じ位置に揃える
    let manifest_warnings: Vec<Warning> =
        output::ensure_manifest_writable(args.manifest.as_deref(), args.force, args.dry_run)?
            .into_iter()
            .collect();

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
                // **不合格も `result` を通常どおり入れる。** 処理は成功していて
                // 成果物も書かれているので、`status` の綴りだけで区別する
                status: match report.compliance.as_ref() {
                    Some(c) if !c.passed => REJECTED,
                    _ => "ok",
                },
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
    let rejected = results.iter().filter(|r| r.status == REJECTED).count();
    let with_warnings = results
        .iter()
        .filter(|r| r.result.as_ref().is_some_and(|c| !c.warnings.is_empty()))
        .count();

    let mut warnings = manifest_warnings;
    warnings.extend(write_manifest(args, &results, failed)?);

    Ok(BatchReport {
        schema_version: SCHEMA_VERSION,
        spec: args.spec.display().to_string(),
        total: results.len(),
        // **不合格は成功に数える。** 処理は通っていて成果物も書かれており、
        // `succeeded` を「書けた件数」として読んでいる既存の読み手にとって
        // それは今も正しい。人が見るべき件数は `rejected` が別に言う
        succeeded: results.len() - failed,
        failed,
        rejected,
        with_warnings,
        dry_run: args.dry_run,
        warnings,
        elapsed_ms: started.elapsed().as_millis(),
        results,
    })
}

/// 実行全体で 1 つのマニフェストを書く。
///
/// **成功した項目だけを載せる。** batch は 1 件の失敗で全体を止めない規約なので、
/// 失敗を「書いたもの」の目録へ混ぜようがない。かわりに `MANIFEST_PARTIAL` が
/// 「欠けている」ことを言う——目録だけを見て「これで全部だ」と読まれるのが
/// 最も高くつく誤りで、件数（`data.failed`）まで添えて分岐できる形にする。
///
/// `--dry-run` では 1 バイトも書かない。
///
/// **上書きの可否はここでは問わない。** 検査は `run()` の先頭で済ませてある
/// ——ここまで来た時点で全項目が書き終わっているので、断っても手遅れになる
fn write_manifest(
    args: &BatchArgs,
    results: &[BatchItemReport],
    failed: usize,
) -> Result<Vec<Warning>> {
    let Some(path) = args.manifest.as_deref() else {
        return Ok(Vec::new());
    };
    let mut warnings: Vec<Warning> = Vec::new();
    if failed > 0 {
        warnings.push(
            Warning::new(
                WarningCode::ManifestPartial,
                format!(
                    "{failed} 件が失敗したため、成功した {} 件だけをマニフェストに載せました",
                    results.len() - failed
                ),
            )
            .with_hint("失敗した項目は results[] の status が error のものです")
            .with_data("failed", failed)
            .with_data("manifest", path.display().to_string()),
        );
    }
    if args.dry_run {
        return Ok(warnings);
    }
    let items = results
        .iter()
        .filter_map(|r| {
            r.result.as_ref().map(|c| ManifestItem {
                input: r.input.clone(),
                outputs: c.outputs.clone(),
            })
        })
        .collect();
    output::write_manifest(path, items)?;
    Ok(warnings)
}

/// 仕様の 1 項目を cutout の引数へ落とす。
///
/// cutout コマンドをそのまま呼ぶことで、単体実行とバッチで挙動が食い違わないようにする。
///
/// `base` を受けるのは、指示として渡される画像（`trimap` / `alpha_trimap` / `fg_mask` /
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

    let max_bytes = max_bytes(settings.max_bytes.as_ref())?;

    // **1 バイトも読む前に断る。** CLI では clap が同じ位置で解いている。
    // 切り抜きを全部終えてから綴り違いに気づく形にしない
    let fail_on = settings
        .fail_on
        .as_deref()
        .map(|s| {
            FailOn::parse(s).map_err(|e| {
                Error::new(ErrorCode::InvalidFailOn, e).with_hint(
                    "--fail-on とまったく同じ書式です（kiri cutout --help が綴りを配ります）",
                )
            })
        })
        .transpose()?;

    // **1 バイトも読む前に断る。** CLI では clap の候補が同じ位置で断っている
    // （綴りを外せば code 無しの exit 2）。spec 経由だけが、数百点を切り抜いた
    // 後に「そんな profile は無い」と言う形にはしない。CLI と同じ関門
    // （`profile::named`）を通すので、片方だけが未知の名前を黙って既定へ
    // 落とすこともない
    let profile = settings
        .profile
        .as_deref()
        .map(|name| {
            profile::named(name).ok_or_else(|| {
                Error::new(
                    ErrorCode::UnknownProfile,
                    format!("'{name}' は既知の profile ではありません"),
                )
                .with_hint(format!(
                    "指定できるのは {}（条件と出典は kiri schema の profiles[] が配ります）",
                    PROFILE_NAMES.join(" / ")
                ))
            })
        })
        .transpose()?;

    // **排他は CLI と同じ。** 2 つの組み立て方が混ざると「どちらが勝つか」と
    // いう覚える規則が増える。clap は `--derive` と `--sizes` を構造として
    // 弾くので、spec でも同じ形を断っておかないと片方だけが緩くなる
    if settings.derive.is_some() && (settings.sizes.is_some() || settings.formats.is_some()) {
        return Err(Error::new(
            ErrorCode::InvalidDerivation,
            "derive と sizes / formats は同時に指定できません",
        )
        .with_hint(
            "直積が欲しいなら sizes / formats だけを、1 本ずつ書くなら derive だけを使ってください",
        ));
    }
    let derive = derive(settings.derive.as_deref())?;
    let formats = formats(settings.formats.as_deref())?;

    let seal = capped(settings.seal, 1, crate::cli::MAX_SEAL, "seal")?;
    let cleanup = capped(settings.cleanup, 2, crate::cli::MAX_CLEANUP, "cleanup")?;

    let path = |p: &Option<std::path::PathBuf>| p.as_ref().map(|p| batch::resolve(base, p));

    Ok(CutoutArgs {
        input: input.to_path_buf(),
        bbox: settings.bbox,
        normalized: settings.normalized.unwrap_or(false),
        fg_seed: settings.fg_seeds.clone().unwrap_or_default(),
        trimap: path(&settings.trimap),
        alpha_trimap: path(&settings.alpha_trimap),
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
        smooth_contour: capped_f64(
            settings.smooth_contour,
            crate::cutout::DEFAULT_SMOOTH_CONTOUR,
            crate::cli::MAX_SMOOTH_CONTOUR,
            "smooth_contour",
        )?,
        no_reclassify: !settings.reclassify.unwrap_or(true),
        background_model: value_enum(
            settings.background_model.as_deref(),
            BackgroundModel::Auto,
            "background_model",
        )?,
        optimize: settings.optimize.unwrap_or(false),
        // spec では `Some` がそのまま「明示した」である。CLI 側が clap の
        // `ValueSource` を見て解いているのと同じ問いに、JSON では素直に答えられる
        fixed: OptimizeFixed {
            tolerance: settings.tolerance.is_some(),
            bbox: settings.bbox.is_some(),
            background_model: settings.background_model.is_some(),
        },
        color: ColorOpts {
            no_color_convert: !settings.color_convert.unwrap_or(true),
        },
        // **spec からも segment を受ける。** 断っていたのは 1 件ごとに
        // 176MB を読み直し、ピーク RSS 1.6GB を並列度ぶん積む形だったため
        // である。いまは計画をプロセスで 1 つ持ち（`segment::isnet`）、
        // 推論そのものは 1 本ずつ通すので、`--jobs` を上げてもモデルのぶんは
        // 増えない。**時間は増える**——1 件あたり 1.3 秒は並べられないので、
        // 数百点に一律で付ける値ではないことは変わらない
        segment: SegmentOpts {
            segment: value_enum(settings.segment.as_deref(), SegmentMode::Off, "segment")?,
            model_path: path(&settings.model_path),
        },
        // 未指定は未指定のまま渡す。既定値で埋めてしまうと、テクスチャに応じた
        // 自動調整が spec を書いた人の「8 を指定した」と区別できなくなる
        edge_threshold: checked_opt(settings.edge_threshold, "edge_threshold")?,
        step_tolerance: checked(settings.step_tolerance, 2.2, "step_tolerance")?,
        shadow_tolerance: checked(settings.shadow_tolerance, 35.0, "shadow_tolerance")?,
        shadow: value_enum(settings.shadow.as_deref(), ShadowMode::Off, "shadow")?,
        shadow_offset: offset(settings.shadow_offset, [0.0, 12.0], "shadow_offset")?,
        // spec は clap を通らないので、CLI と同じ上限をここで掛ける。
        // 抜けていると `--shadow-blur` では断る値が spec 経由でだけ通る
        shadow_blur: capped_f64(
            settings.shadow_blur,
            10.0,
            crate::cli::SHADOW_BLUR_MAX,
            "shadow_blur",
        )?,
        shadow_color: settings
            .shadow_color
            .as_deref()
            .map(|s| parse_hex_color(s).map_err(|e| Error::new(ErrorCode::InvalidColor, e)))
            .transpose()?
            .unwrap_or([0, 0, 0]),
        shadow_opacity: ratio(settings.shadow_opacity, 0.25, "shadow_opacity")?,
        seal,
        // **角度だけは負値を通す。** 反時計回りの指定であり、他の設定の
        // ように「負値は機能が黙って消える」種類の誤りではない
        rotate: rotate(settings.rotate.as_ref())?,
        profile,
        // spec では `Some` がそのまま「明示した」である。CLI 側が clap の
        // `ValueSource` を見て解いているのと同じ問いに、JSON では素直に
        // 答えられる（`fixed` とまったく同じ作り）。**`OutputOpts` の側で
        // `unwrap_or` して既定を埋めた後では区別が付かない**ので、
        // 埋める前の `settings` を見る
        explicit: ExplicitOptions {
            canvas: settings.canvas.is_some(),
            fill_ratio: settings.fill_ratio.is_some(),
            format: settings.format.is_some(),
            background: settings.background.is_some(),
            flatten: settings.flatten.is_some(),
            max_bytes: settings.max_bytes.is_some(),
            quality: settings.quality.is_some(),
        },
        canvas,
        fill_ratio: settings.fill_ratio.unwrap_or(0.85),
        fail_on,
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
            max_bytes,
            background,
            flatten: settings.flatten.unwrap_or(false),
            derive,
            sizes: settings.sizes.clone().unwrap_or_default(),
            formats,
            naming: settings.naming.clone(),
            // **マニフェストは項目ごとではなく実行全体で 1 つ。** 数百点が
            // 同じパスへ順に書けば、最後の 1 件だけが残る目録になる。
            // spec のキーにもしていないのはそのためで、入口は
            // `kiri batch --manifest` だけにしてある
            manifest: None,
            force,
            dry_run,
        },
    })
}

/// spec の `derive[]` を 1 本ずつ解く。
///
/// **CLI と同じ関門（`DeriveSpec::set`）を通す。** 値は JSON の素の型
/// （数値・文字列・真偽値）で書かれるので、綴り直してから渡す——そうすることで
/// `"500k"` も `512000` も CLI とまったく同じ規則で読まれる。
///
/// 断るときの code は `INVALID_DERIVATION` である。`INVALID_SETTING` に
/// まとめないのは、**直し方が他の設定と違う**ためで、`INVALID_MAX_BYTES` を
/// 分けたのと同じ理由になる
fn derive(values: Option<&[serde_json::Value]>) -> Result<Vec<DeriveSpec>> {
    let invalid = |message: String| {
        Error::new(ErrorCode::InvalidDerivation, message).with_hint(
            "derive は [{\"width\":1600,\"format\":\"jpeg\"}] のようなオブジェクトの配列です",
        )
    };
    values
        .unwrap_or_default()
        .iter()
        .enumerate()
        .map(|(i, value)| {
            let object = value
                .as_object()
                .ok_or_else(|| invalid(format!("derive[{i}] はオブジェクトではありません")))?;
            let mut spec = DeriveSpec::default();
            for (key, raw) in object {
                let spelled = match raw {
                    serde_json::Value::String(s) => s.clone(),
                    serde_json::Value::Number(n) => n.to_string(),
                    serde_json::Value::Bool(b) => b.to_string(),
                    other => {
                        return Err(invalid(format!(
                            "derive[{i}].{key} を値として読めません（{other} が指定されました）"
                        )));
                    }
                };
                spec.set(key, &spelled)
                    .map_err(|e| invalid(format!("derive[{i}]: {e}")))?;
            }
            if spec == DeriveSpec::default() {
                return Err(invalid(format!("derive[{i}] が空です")));
            }
            Ok(spec)
        })
        .collect()
}

/// spec の `formats` を形式の並びへ落とす。
///
/// 綴り違いの code は `format` と同じ `UNKNOWN_OUTPUT_FORMAT` にする。
/// **直し方が同じ種類の誤り**——避けるべきは「どの形式が書けるか」を 2 つの
/// code で言うことで、受け手はそのどちらにも同じ分岐を書くことになる
fn formats(names: Option<&[String]>) -> Result<Vec<OutputFormat>> {
    names
        .unwrap_or_default()
        .iter()
        .map(|name| {
            OutputFormat::from_name(name).ok_or_else(|| {
                Error::new(
                    ErrorCode::UnknownOutputFormat,
                    format!("formats に未対応の形式 '{name}' があります"),
                )
                .with_hint("avif / png / jpeg / jpg のいずれかを指定してください")
            })
        })
        .collect()
}

/// spec の `matting` を解き方へ落とす。
fn matting(name: Option<&str>) -> Result<Matting> {
    value_enum(name, CutoutOptions::default().matting, "matting")
}

/// spec の文字列を `clap::ValueEnum` の枝へ落とす。
///
/// **綴りを外したら断る。** 未知の値を既定へ落とすと、その項目だけ黙って
/// 別の設定で処理され、数百点を回した後に仕上がりを見るまで気づけない。
///
/// **候補は `clap::ValueEnum` から引く。** 手書きの `match` に catch-all を
/// 置くと、列挙に枝を足したときに spec 側だけが取りこぼす——しかもコンパイラは
/// 何も言わない。CLI と spec が同じ 1 つの列挙を見る。
fn value_enum<T: clap::ValueEnum + Copy>(name: Option<&str>, default: T, key: &str) -> Result<T> {
    let Some(name) = name else {
        return Ok(default);
    };
    let known = || -> Vec<String> {
        T::value_variants()
            .iter()
            .filter_map(|v| v.to_possible_value().map(|p| p.get_name().to_string()))
            .collect()
    };
    T::value_variants()
        .iter()
        .find(|v| {
            v.to_possible_value()
                .is_some_and(|p| p.matches(name, false))
        })
        .copied()
        .ok_or_else(|| {
            Error::new(
                ErrorCode::SpecInvalid,
                format!("'{name}' は未対応の {key} です"),
            )
            .with_hint(format!(
                "{} のいずれかを指定してください",
                known().join(" / ")
            ))
        })
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

/// spec の `max_bytes` を解く。**数値でも文字列でも受ける。**
///
/// エージェントが書く JSON には `512000` と `"500k"` の両方が現れる。片方を
/// 断ると、CLI では通る書き方が spec でだけ通らない道具になる。文字列は CLI と
/// 同じ `parse_max_bytes` へ流し、数値は `u64` として読めて 0 より大きいことだけを
/// 見る——小数・負値・0 はどれも `as_u64()` か大小比較で落ちる。
///
/// 断るときの code は `INVALID_MAX_BYTES` で、`INVALID_SETTING` ではない。
/// **上限バイト数の誤りは「どの値をどう直すか」が他の設定と違う**（単位の綴りか、
/// 小数か、0 か）ので、受け手が同じ分岐でまとめて扱える種類の失敗ではない。
fn max_bytes(value: Option<&serde_json::Value>) -> Result<Option<u64>> {
    let Some(value) = value else {
        return Ok(None);
    };
    let invalid = |message: String| {
        Error::new(ErrorCode::InvalidMaxBytes, message).with_hint(
            "max_bytes は 512000 のような整数か、\"500k\" / \"2mb\" のような文字列で指定してください",
        )
    };
    match value {
        serde_json::Value::String(s) => crate::cli::parse_max_bytes(s)
            .map(Some)
            .map_err(|e| invalid(format!("max_bytes: {e}"))),
        serde_json::Value::Number(n) => match n.as_u64() {
            Some(bytes) if bytes > 0 => Ok(Some(bytes)),
            _ => Err(invalid(format!(
                "max_bytes は 1 以上の整数である必要があります（{n} が指定されました）"
            ))),
        },
        other => Err(invalid(format!(
            "max_bytes を数値としても文字列としても読めません（{other} が指定されました）"
        ))),
    }
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

/// 上限のある実数の設定に CLI と同じ関門を掛ける。
///
/// `capped` の実数版。上限を超えた `smooth_contour` は `RADIUS_CEILING` で
/// 頭打ちになるだけなので、spec に書いた値と効いた値が黙って食い違う。
fn capped_f64(value: Option<f64>, default: f64, max: f64, key: &str) -> Result<f64> {
    let value = validate(value.unwrap_or(default), key)?;
    if value > max {
        return Err(Error::new(
            ErrorCode::InvalidSetting,
            format!("{key} は 0 から {max} の範囲で指定してください（{value} が指定されました）"),
        ));
    }
    Ok(value)
}

/// 0.0-1.0 の設定に CLI と同じ関門を掛ける（`unit_interval` の spec 版）。
///
/// 1.5 は飽和して 1.0 と同じ結果になり、-0.2 は機能が黙って消える。どちらも
/// 数百点を回し終えてから仕上がりで気づく種類の失敗になる。
fn ratio(value: Option<f64>, default: f64, key: &str) -> Result<f64> {
    let value = validate(value.unwrap_or(default), key)?;
    if value > 1.0 {
        return Err(Error::new(
            ErrorCode::InvalidSetting,
            format!("{key} は 0.0 から 1.0 の範囲で指定してください（{value} が指定されました）"),
        ));
    }
    Ok(value)
}

/// spec の `rotate` を解く。**数値でも文字列でも受ける。**
///
/// `max_bytes` とまったく同じ事情である。エージェントが書く JSON には `90` と
/// `"90"` の両方が現れ、そのうえ `auto` は数値では表せない。**CLI と同じ
/// `cli::parse_rotate` へ流す**ので、`auto` の綴りも nan の扱いも片方でだけ
/// 通る／通らないが起きない——数値は `to_string()` で綴り直してから渡す
/// （`derive` が JSON の素の型を綴り直しているのと同じ作法）。
///
/// 断るときの code は `INVALID_ROTATE` で、`INVALID_SETTING` ではない。
/// **角度の誤りは「どの値をどう直すか」が他の設定と違う**（綴りか、auto の
/// 大文字小文字か、そもそも数値でないか）ので、受け手が同じ分岐でまとめて
/// 扱える種類の失敗ではない。`INVALID_MAX_BYTES` を分けたのと同じ理由になる。
///
/// **負値は通す**（反時計回りの指定）。nan と無限大だけを `parse_rotate` が断る。
fn rotate(value: Option<&serde_json::Value>) -> Result<RotateArg> {
    let Some(value) = value else {
        return Ok(RotateArg::Degrees(0.0));
    };
    let invalid = |message: String| {
        Error::new(ErrorCode::InvalidRotate, message).with_hint(
            "rotate は 90 / -3.5 のような度数か、\"auto\"（主体の傾きを測って適用）で指定してください",
        )
    };
    let spelled = match value {
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Number(n) => n.to_string(),
        other => {
            return Err(invalid(format!(
                "rotate を数値としても文字列としても読めません（{other} が指定されました）"
            )));
        }
    };
    crate::cli::parse_rotate(&spelled).map_err(|e| invalid(format!("rotate: {e}")))
}

/// ずらし量に CLI と同じ関門を掛ける。**負値は通す**（影を上や左へ出す指定）。
fn offset(value: Option<[f64; 2]>, default: [f64; 2], key: &str) -> Result<[f64; 2]> {
    let value = value.unwrap_or(default);
    if value.iter().any(|v| !v.is_finite()) {
        return Err(Error::new(
            ErrorCode::InvalidSetting,
            format!("{key} は有限な数値 2 個である必要があります"),
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
