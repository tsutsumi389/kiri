//! 出力処理の共通部分。convert と resize が同じ規約で書き出すために使う。

use std::path::Path;
use std::time::Instant;

use image::RgbaImage;

use crate::cli::OutputOpts;
use crate::cutout::{BackgroundEstimate, DeltaEQuantiles, ResolvedModel, SubjectHint};
use crate::error::{Error, ErrorCode, Result};
use crate::image_io::derive::{self, Derivation, DeriveSpec, Rendered, render};
use crate::image_io::naming::{self, Naming};
use crate::image_io::{IccPolicy, IccSignal, LoadedImage, OutputFormat};
use crate::report::{
    BackgroundReport, Dimensions, Manifest, ManifestItem, OutputReport, PerimeterDeltaE,
    PerimeterTexture, ProcessReport, SCHEMA_VERSION, SubjectReport,
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

/// 多派生の指定があるか。
///
/// **1 本しか書かない実行を今までとまったく同じ道へ通すための門である。**
/// `--derive` / `--sizes` / `--formats` / `--naming` のどれも無ければ、パスは
/// `--output` そのもので、上書き検査も着手前の 1 回きり（Phase 19 と同じ）。
/// どれかがあれば、パスは最終画像の寸法が決まるまで綴れないので、検査は
/// 書き出しの直前へ寄せる
pub fn has_derivations(opts: &OutputOpts) -> bool {
    !opts.derive.is_empty()
        || !opts.sizes.is_empty()
        || !opts.formats.is_empty()
        || opts.naming.is_some()
}

/// 上書きの可否を確認する。重い処理を走らせる前に呼ぶこと。
///
/// `--dry-run` では検査しない。上書き検査は成果物を守るためのもので、
/// 1 バイトも書かない実行を止める理由が無いためである。**代わりに、本番実行なら
/// ここで落ちていた事実を警告で返す。** 黙って通すと、エージェントは dry-run の
/// 成功を見て本番へ進み、`OUTPUT_EXISTS` で二度手間になる。
///
/// 多派生では何もしない。`--output` は書き出し先ではなく名前の雛形になるので、
/// ここでその存在を問うても意味が無い——実際のパスは `resolve` が派生ごとに
/// 同じ規約で検査する
pub fn ensure_writable(opts: &OutputOpts) -> Result<Option<Warning>> {
    if has_derivations(opts) {
        return Ok(None);
    }
    writable(&opts.output, opts.force, opts.dry_run)
}

/// `--manifest` にも本出力と同じ上書きの規約を当てる。
///
/// マニフェストは成果物の目録であって検証用の付随物ではないので、黙って
/// 壊してよいものではない。`--dry-run` で書かないことも本出力と揃える
pub fn ensure_manifest_writable(
    manifest: Option<&Path>,
    force: bool,
    dry_run: bool,
) -> Result<Option<Warning>> {
    match manifest {
        Some(path) => writable(path, force, dry_run),
        None => Ok(None),
    }
}

/// 1 つのパスに上書きの規約を当てる。本番なら断り、dry-run なら警告で知らせる。
fn writable(path: &Path, force: bool, dry_run: bool) -> Result<Option<Warning>> {
    if !dry_run {
        ensure_path_writable(path, force)?;
        return Ok(None);
    }
    if !path.exists() || force {
        return Ok(None);
    }
    Ok(Some(
        Warning::new(
            WarningCode::DryRunOutputExists,
            format!(
                "{} は既に存在します。dry-run なので書いていませんが、本番実行は上書きを拒みます",
                path.display()
            ),
        )
        .with_hint("本番実行には --force が要ります")
        // **キーは `output` である**（`SCHEMA_VERSION` 2 で `path` から改名した）。
        // 派生に紐づく警告が揃って `data.output` で「どの出力の話か」を名乗る
        // 規約に合わせたもので、同じ意味のキーを 2 つ並べるほうが害が大きい
        .with_data("output", path.display().to_string()),
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

/// `subject.source` の既定値。背景色から遠い画素の最大の塊として測ったもの。
pub const SUBJECT_FROM_COLOUR: &str = "colour";
/// `subject.source` がセグメンテーションモデル由来であることを表す綴り。
pub const SUBJECT_FROM_SEGMENT: &str = "segment";

/// 背景推定を JSON のレポートへ落とす。
///
/// `info` と `cutout` の両方が同じ形を返す約束なので、組み立てを 1 箇所に置く。
/// 片方にだけ項目を足すと、エージェントは「この画像では測れなかった」のか
/// 「このコマンドは報告しない」のかを区別できない。
pub fn background_report(
    background: &BackgroundEstimate,
    model: ResolvedModel,
    field_range: [f64; 2],
    residual: &DeltaEQuantiles,
) -> BackgroundReport {
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
        model: model.as_str(),
        field_range: field_range.map(round4),
        residual: PerimeterDeltaE {
            p50: round4(residual.p50),
            p90: round4(residual.p90),
            max: round4(residual.max),
        },
    }
}

/// 主体の推定を JSON のレポートへ落とす。
///
/// `background_report` と同じ理由で組み立てを 1 箇所に置く。`info` と `cutout` が
/// 別々に組むと、片方だけ丸め方や項目が食い違っていても誰も気づかない。
///
/// `source` は「この矩形が何から出たか」である。**キーを足すだけで既存の値の
/// 意味は変えない**——色から測ったものは今までどおり `"colour"` で、
/// `info --segment` でモデルから測ったときだけ `"segment"` になる。
pub fn subject_report(subject: &SubjectHint, source: &'static str) -> SubjectReport {
    SubjectReport {
        source,
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
        level_rotation: subject.level_rotation.map(round4),
        border: subject.border,
        confidence: subject.confidence,
    }
}

/// 本出力と衝突してはいけない、既に用途の決まっているパス。
///
/// `--preview` / `--debug-mask` / `--manifest` の 3 つで、どれも本出力より後に
/// 書かれる。衝突を通すと成果物を上書きしたうえ、結果 JSON は上書き前の寸法と
/// サイズを報告する——**機械可読なレポートが嘘をつく**ので、必ず事前に弾く
pub struct Reserved<'a> {
    pub path: &'a Path,
    /// 衝突の文面に出すフラグ名（`--preview` など）
    pub flag: &'static str,
}

/// 指定から派生の並びを組む。
///
/// **3 通りの入口を 1 つの並びへ畳む。** `--derive` を書いたならそれ、
/// `--sizes` / `--formats` を書いたならその直積（size が外、format が内）、
/// どれも無ければ「何も上書きしない 1 本」になる。最後の枝が Phase 19 までと
/// 同じ道で、**`DeriveSpec::default()` は 1 つも値を持たない**ので、下流は
/// すべて `OutputOpts` の値をそのまま継ぐ
fn specs(opts: &OutputOpts) -> Vec<DeriveSpec> {
    if !opts.derive.is_empty() {
        return opts.derive.clone();
    }
    if opts.sizes.is_empty() && opts.formats.is_empty() {
        return vec![DeriveSpec::default()];
    }
    // 片方だけの指定は「その軸は 1 通り」として扱う。--sizes だけなら形式は
    // 解決済みの 1 つ、--formats だけなら幅は最終画像のまま（リサイズしない）
    let widths: Vec<Option<u32>> = if opts.sizes.is_empty() {
        vec![None]
    } else {
        opts.sizes.iter().map(|w| Some(*w)).collect()
    };
    let formats: Vec<Option<OutputFormat>> = if opts.formats.is_empty() {
        vec![None]
    } else {
        opts.formats.iter().map(|f| Some(*f)).collect()
    };
    let mut out = Vec::with_capacity(widths.len() * formats.len());
    for width in &widths {
        for format in &formats {
            out.push(DeriveSpec {
                width: *width,
                format: *format,
                ..DeriveSpec::default()
            });
        }
    }
    out
}

/// 書き始める前に、全派生のパスと寸法を決めて検査する。
///
/// **ここを通り抜けたら、あとは書くだけである。** 1 枚でも書いた後にエラーで
/// 落ちると半端な成果物が残り、しかも結果 JSON は返らないので何が書けたのかを
/// 追う手段が無い（計画 7.2）。検査は 3 つ——派生どうしの衝突、付随出力との衝突、
/// 上書きの可否——で、どれも重いエンコードの前に済ませる。
///
/// `source` は最終画像の寸法である。`{width}` も `outputs[].width` も
/// `Derivation::dimensions` という 1 つの答えを引くので、名前と報告が食い違わない
fn resolve(
    opts: &OutputOpts,
    format: OutputFormat,
    icc: IccPolicy,
    source: (u32, u32),
    reserved: &[Reserved],
) -> Result<(Vec<Derivation>, Vec<Warning>)> {
    let specs = specs(opts);
    // 明示した --naming は派生が 1 本でも効かせる。**予測可能性を優先した**
    // ——「2 本以上のときだけ効く」にすると、同じテンプレートが派生の数で
    // 効いたり効かなかったりする
    let naming = match opts.naming.as_deref() {
        Some(template) => Some(Naming::parse(template)?),
        None if specs.len() > 1 => Some(Naming::parse(naming::DEFAULT_TEMPLATE)?),
        None => None,
    };
    if let Some(naming) = &naming {
        if naming.uses_role() && specs.iter().any(|s| s.role.is_none()) {
            return Err(Error::new(
                ErrorCode::InvalidNamingTemplate,
                "--naming が {role} を使っていますが、role を持たない派生があります",
            )
            .with_hint("すべての --derive に role を書くか、{role} を外してください"));
        }
    }

    let stem = naming::stem_of(&opts.output).to_string();
    let mut derivations = Vec::with_capacity(specs.len());
    let mut warnings = Vec::new();

    for (index, spec) in specs.iter().enumerate() {
        let format = spec.format.unwrap_or(format);
        let derivation = Derivation {
            // 仮の置き場。寸法が決まらないと名前を綴れないので、下で入れ替える
            path: opts.output.clone(),
            format,
            quality: spec.quality.unwrap_or(opts.quality),
            effort: spec.effort.unwrap_or(opts.effort),
            background: opts.background,
            flatten: opts.flatten,
            icc,
            max_bytes: spec.max_bytes.or(opts.max_bytes),
            resize: spec.resize(),
            role: spec.role.clone(),
        };
        let (width, height) = derivation.dimensions(source)?;
        let path = match &naming {
            Some(naming) => naming::beside(
                &opts.output,
                &naming.render(&naming::Name {
                    stem: &stem,
                    index,
                    width,
                    height,
                    ext: format.extension(),
                    role: spec.role.as_deref(),
                }),
            ),
            None => opts.output.clone(),
        };
        // 許した拡大だけをここで言う。許していない拡大は `dimensions` が
        // `UPSCALE_NOT_ALLOWED` で既に断っている
        if width > source.0 || height > source.1 {
            warnings.push(derive::tag(upscaled(source, (width, height)), &path));
        }
        derivations.push(Derivation { path, ..derivation });
    }

    check_collisions(&derivations)?;
    check_reserved(&derivations, reserved)?;
    // 1 本しか書かない実行の上書き検査は、着手前に `ensure_writable` が
    // 済ませてある。ここで二重に掛けると `DRY_RUN_OUTPUT_EXISTS` が 2 度出る
    if has_derivations(opts) {
        for d in &derivations {
            if let Some(warning) = writable(&d.path, opts.force, opts.dry_run)? {
                warnings.push(warning);
            }
        }
    }
    Ok((derivations, warnings))
}

/// 2 本の派生が同じパスへ落ちていないか。
///
/// **`--force` では許さない。** 上書きの可否は「利用者の既存のファイルを壊して
/// よいか」の話で、こちらは 1 回の実行が自分の成果物を自分で潰す指定である。
/// 後に書いたほうだけが残り、結果 JSON は 2 本とも書いたと報告する
fn check_collisions(derivations: &[Derivation]) -> Result<()> {
    let mut seen: Vec<(&Path, usize)> = Vec::with_capacity(derivations.len());
    for (index, d) in derivations.iter().enumerate() {
        if let Some((_, first)) = seen.iter().find(|(path, _)| *path == d.path.as_path()) {
            return Err(Error::new(
                ErrorCode::OutputNameCollision,
                format!(
                    "派生 {first} と {index} がどちらも {} になります",
                    d.path.display()
                ),
            )
            .with_hint(
                "--naming に {index} か {role} を入れると、幅や形式が同じ派生でも名前が分かれます",
            ));
        }
        seen.push((d.path.as_path(), index));
    }
    Ok(())
}

/// 派生のパスが付随出力と衝突していないか。
fn check_reserved(derivations: &[Derivation], reserved: &[Reserved]) -> Result<()> {
    for d in derivations {
        for r in reserved {
            if d.path == r.path {
                return Err(Error::new(
                    ErrorCode::SideOutputConflict,
                    format!(
                        "{} と出力 {} に同じパスは指定できません",
                        r.flag,
                        d.path.display()
                    ),
                )
                .with_hint("付随出力は本出力の後に書かれるため、成果物を壊します"));
            }
        }
    }
    Ok(())
}

/// 派生のリサイズで元画像より大きくした。
///
/// `kiri resize --allow-upscale` が出すものと同じ code だが、**こちらは
/// `data.output` を持つ**——1 実行で複数の派生を書く以上、どの出力の話かが
/// 分からない警告は分岐の材料にならない
fn upscaled(from: (u32, u32), to: (u32, u32)) -> Warning {
    Warning::new(
        WarningCode::Upscaled,
        format!(
            "{}x{} から {}x{} へ拡大しました。画質は元素材を超えません",
            from.0, from.1, to.0, to.1
        ),
    )
    .with_data("from", vec![from.0, from.1])
    .with_data("to", vec![to.0, to.1])
}

/// 画像を派生の数だけ書き出し、出力レポートと警告を返す。
///
/// 読み込み結果を受けるのは、**出力が何を名乗るかを画素の素性で決める**ため。
/// `--no-color-convert` で変換しなかった画素に sRGB の ICC を付けると、名乗りが嘘になる。
///
/// 派生を 1 つも指定しなければ戻りは 1 要素で、そのバイト列も出力パスも
/// Phase 19 と 1 バイトも変わらない
pub fn write_images(
    image: &RgbaImage,
    loaded: &LoadedImage,
    opts: &OutputOpts,
    format: OutputFormat,
    reserved: &[Reserved],
) -> Result<(Vec<OutputReport>, Vec<Warning>)> {
    let icc = if loaded.srgb_pixels {
        IccPolicy::Embed
    } else {
        IccPolicy::None
    };
    let source = (image.width(), image.height());
    let (derivations, mut warnings) = resolve(opts, format, icc, source, reserved)?;

    let rendered = render(image, &derivations, opts.dry_run)?;
    let mut reports = Vec::with_capacity(rendered.len());
    for (
        d,
        Rendered {
            report,
            warnings: w,
        },
    ) in derivations.iter().zip(rendered)
    {
        warnings.extend(w);
        // **派生ごとに 1 回出す。** 名乗りは形式で変わるので、AVIF と PNG を
        // 同時に書く実行では言うべきことが 2 通りある
        if icc == IccPolicy::None {
            warnings.push(derive::tag(
                icc_not_embedded(loaded, d.format, report.icc),
                &d.path,
            ));
        }
        reports.push(report);
    }
    Ok((reports, warnings))
}

/// マニフェストを書く。**tmp + rename で置き換える。**
///
/// 同じディレクトリに一時ファイルを作ってから `rename` するので、途中で落ちても
/// 半端な JSON が残らない（`rename` は同一ファイルシステム内で原子的である）。
/// 一時ファイルの名前に**プロセス ID を混ぜる**のは、batch を複数走らせたときに
/// 互いの tmp を踏まないようにするため。中身は決定的なので、この名前が
/// 成果物に残ることは無い
pub fn write_manifest(path: &Path, items: Vec<ManifestItem>) -> Result<()> {
    let manifest = Manifest {
        schema_version: SCHEMA_VERSION,
        kiri_version: env!("CARGO_PKG_VERSION"),
        items,
    };
    let failed = |message: String| Error::new(ErrorCode::ManifestWriteFailed, message);
    let mut json = serde_json::to_vec_pretty(&manifest)
        .map_err(|e| failed(format!("マニフェストを JSON にできません: {e}")))?;
    json.push(b'\n');

    if let Some(dir) = path.parent() {
        if !dir.as_os_str().is_empty() && !dir.exists() {
            std::fs::create_dir_all(dir)
                .map_err(|e| failed(format!("{} を作成できません: {e}", dir.display())))?;
        }
    }
    let temp = path.with_file_name(format!(
        ".{}.{}.tmp",
        path.file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("manifest"),
        std::process::id()
    ));
    std::fs::write(&temp, &json)
        .map_err(|e| failed(format!("{} を書き出せません: {e}", temp.display())))?;
    std::fs::rename(&temp, path).map_err(|e| {
        // 置き換えに失敗したら一時ファイルを残さない。次の実行が古い中身を
        // 拾うことは無いが、成果物の隣にごみが積む
        let _ = std::fs::remove_file(&temp);
        failed(format!("{} へ置き換えられません: {e}", path.display()))
    })
}

/// 名乗りを外した（AVIF では外せなかった）ことを伝える。
///
/// 読み込み時の `COLOR_CONVERSION_SKIPPED` は「変換しなかった」までしか言わない。
/// 出力の側で何が起きたかは形式で違うので、ここで別に言う
fn icc_not_embedded(loaded: &LoadedImage, format: OutputFormat, signal: IccSignal) -> Warning {
    let label = match &loaded.color_profile {
        Some(n) => format!("'{n}'"),
        None => "（名前なし）".to_string(),
    };
    let message = match format {
        OutputFormat::Avif => format!(
            "入力の ICC プロファイル {label} を sRGB へ変換していませんが、AVIF は AV1 の\
             色情報で sRGB を名乗ったままです（外す手段がありません）"
        ),
        OutputFormat::Png | OutputFormat::Jpeg => format!(
            "入力の ICC プロファイル {label} を sRGB へ変換していないため、sRGB の ICC を\
             埋め込みませんでした"
        ),
    };
    let mut warning = Warning::new(WarningCode::IccNotEmbedded, message)
        .with_hint("--no-color-convert を外すと sRGB へ変換し、名乗りと画素が一致します")
        .with_data("format", format.as_str())
        .with_data("icc", signal.as_str());
    if let Some(n) = &loaded.color_profile {
        warning = warning.with_data("profile", n.clone());
    }
    warning
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
    // **マニフェストだけが本出力と衝突しうる。** convert / resize / rotate は
    // `--preview` も `--debug-mask` も受けない
    let reserved: Vec<Reserved> = opts
        .manifest
        .as_deref()
        .map(|path| Reserved {
            path,
            flag: "--manifest",
        })
        .into_iter()
        .collect();
    let (outputs, save_warnings) = write_images(image, loaded, opts, format, &reserved)?;
    warnings.extend(save_warnings);

    // **書き出しが全部済んでから書く。** 目録が先にできると、途中で落ちた実行の
    // 後に「あるはずのファイル」を並べた JSON が残る
    if !opts.dry_run {
        if let Some(path) = opts.manifest.as_deref() {
            write_manifest(
                path,
                vec![ManifestItem {
                    input: input.display().to_string(),
                    outputs: outputs.clone(),
                }],
            )?;
        }
    }

    Ok(ProcessReport {
        schema_version: SCHEMA_VERSION,
        input: input.display().to_string(),
        source: Dimensions {
            width: loaded.width(),
            height: loaded.height(),
        },
        outputs,
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::image_io::load::{LoadOptions, load_with};
    use image::ImageEncoder;

    fn opts(output: std::path::PathBuf) -> OutputOpts {
        OutputOpts {
            output,
            format: None,
            quality: 75.0,
            effort: 6,
            max_bytes: None,
            background: [255, 255, 255],
            flatten: false,
            derive: Vec::new(),
            sizes: Vec::new(),
            formats: Vec::new(),
            naming: None,
            manifest: None,
            force: false,
            dry_run: true,
        }
    }

    fn not_embedded(warnings: &[Warning]) -> Vec<&Warning> {
        warnings
            .iter()
            .filter(|w| w.code == WarningCode::IccNotEmbedded)
            .collect()
    }

    /// 名乗りを外すのは画素が sRGB でないときだけ。sRGB へ変換した画素に
    /// 警告を出すと、既定の実行で毎回鳴ってしまう
    #[test]
    fn icc_not_embedded_only_when_pixels_are_not_srgb() {
        use crate::color::synthetic::{build, display_p3};
        let dir = tempfile::tempdir().unwrap();
        let input = dir.path().join("p3.jpg");
        let img = image::RgbImage::from_pixel(16, 16, image::Rgb([200, 60, 40]));
        let mut raw = Vec::new();
        let mut encoder = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut raw, 95);
        encoder.set_icc_profile(build(&display_p3())).unwrap();
        encoder
            .write_image(img.as_raw(), 16, 16, image::ExtendedColorType::Rgb8)
            .unwrap();
        std::fs::write(&input, raw).unwrap();

        let raw_pixels = load_with(
            &input,
            &LoadOptions {
                convert_color: false,
                ..Default::default()
            },
        )
        .unwrap();
        assert!(!raw_pixels.srgb_pixels);
        let cases = [
            (OutputFormat::Png, IccSignal::None),
            (OutputFormat::Jpeg, IccSignal::None),
            (OutputFormat::Avif, IccSignal::Nclx),
        ];
        for (format, signal) in cases {
            let out = dir.path().join(format!("out.{}", format.as_str()));
            let (reports, warnings) =
                write_images(&raw_pixels.image, &raw_pixels, &opts(out), format, &[]).unwrap();
            let report = &reports[0];
            assert_eq!(report.icc, signal, "{format:?}");
            let found = not_embedded(&warnings);
            assert_eq!(found.len(), 1, "{format:?}");
            assert_eq!(found[0].data["format"], format.as_str());
            assert_eq!(found[0].data["icc"], signal.as_str());
            assert_eq!(found[0].data["profile"], "Display P3");
        }

        let converted = load_with(&input, &LoadOptions::default()).unwrap();
        assert!(converted.srgb_pixels);
        for format in [OutputFormat::Png, OutputFormat::Jpeg, OutputFormat::Avif] {
            let out = dir.path().join(format!("out.{}", format.as_str()));
            let (reports, warnings) =
                write_images(&converted.image, &converted, &opts(out), format, &[]).unwrap();
            assert!(not_embedded(&warnings).is_empty(), "{format:?}");
            assert_eq!(
                reports[0].icc,
                IccPolicy::Embed.signal(format),
                "{format:?}"
            );
        }
    }
}
