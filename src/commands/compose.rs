//! `kiri compose` — spec を 1 枚へ組む。
//!
//! **層は spec の順に重ねる。** 文字を最後にまとめて重ねると、spec が書いた
//! 順番が消える（`compose::text` の doc）。順に重ねることには測りの側の利得も
//! あって、**重ねる直前のキャンバスがその文字の「背後」そのもの**になるので、
//! コントラストを測るための描き直しが要らない。
//!
//! # 後から覆われた文字を見逃さない
//!
//! 背後だけを見ると、**その文字の上に不透明な層が来た場合に気づけない。**
//! 「背後は白、文字は黒、よってコントラスト 21」と報告した絵の中で、文字が
//! 1 画素も見えていないことが起こりうる。
//!
//! だから画素ごとに**最後に書いた層**を覚えておき、文字が覆う画素のうち自分より
//! 後の層に塗られた割合を数える。重ねる順に 1 回歩くだけで済み、層ごとの
//! 画像を持ち回らずに後から言える。
//!
//! # `--profile` の縮小表示は重ね終えてから測る
//!
//! 縮めるのは**最終のキャンバス**である（`compose::display` の doc）。文字ごとに
//! 要るのはその文字の被覆だけで、被覆は後の層に依らないので、`--profile` が
//! あるときに限り**組んだその場で表示の大きさへ縮めて**取っておく（原寸の
//! グリフを層の数だけ抱えない）。無いときは何もしない——**渡さない実行の
//! 費用も出力も変えない。**
//!
//! # `--preview` は測った絵を書く
//!
//! 縮小の測りは数を返すが、数だけでは「どこが潰れたか」が分からない。
//! **測りが縮めた絵そのもの**を `<出力の stem>.<表示名>.png` へ書く
//! （13-phase-32.md §13.2.4）。別の縮小で作り直すと、見た絵と測った絵が
//! 食い違う。
//!
//! # 書く約束
//!
//! `--preview` を渡した実行は、出力とプレビューの全部の先を**画素を 1 つも描く前に**
//! 確かめ（既存のファイルは `--force` が無ければ断り、ディレクトリは `--force`
//! があっても断る）、**全部をエンコードし終えてから**書き始める。書く途中で
//! 失敗したら、この実行が新しく作ったファイルを消す。**`--force` で上書きした
//! ファイルは戻せない**——前の中身を持っていないからである（13-phase-32.md §13.7）。
//!
//! # `--free-regions` は重ねながら障害物を集める
//!
//! 空きを求めるのに要るのは、`decoration` でない画像の不透明な画素と、文字の
//! `placed` だけである（`compose::free` の doc）。画像は当てはめた絵を手放す前に
//! 原寸のマスクへ押し、文字は重ね終えてから `placed` を塗る。**渡さない実行では
//! マスクを確保しない**——費用も出力も変えない（14-phase-33.md §14.1 の 4）。

use std::path::{Path, PathBuf};
use std::time::Instant;

use image::RgbaImage;
use image::buffer::ConvertBuffer;

use crate::cli::ComposeArgs;
use crate::compose::font::LayerFont;
use crate::compose::free::{self, Obstacles};
use crate::compose::measure::{self, COVERAGE, LayerReport};
use crate::compose::profile::{ComposeProfile, Display};
use crate::compose::{
    self, ComposeSpec, ImageLayer, Layer, Role, ShapeLayer, TextLayer, Trim, display,
};
use crate::error::{Error, ErrorCode, Result};
use crate::image_io::{load, save};
use crate::report::{
    ComposeCanvasReport, ComposeOutputReport, ComposePreviewReport, ComposeProfileRef,
    ComposeReport, SCHEMA_VERSION,
};
use crate::transform::{ResizeSpec, canvas as canvas_mod};
use crate::warning::{Warning, WarningCode};

/// 覆いの記録で「まだ誰も塗っていない」を表す印。
///
/// 層の番号は `u16` に収まる（`compose::MAX_LAYERS` が 1024 で断る）ので、
/// 上端を番号として使うことは無い。
const UNPAINTED: u16 = u16::MAX;

pub fn run(args: &ComposeArgs) -> Result<ComposeReport> {
    let started = Instant::now();
    let profile = args.profile;
    let spec = compose::load(&args.spec, profile)?;
    // 素材のパスは **spec からの相対**で解く。実行時の作業ディレクトリに
    // 依らせると、同じ spec が置き場所によって別のものを読む
    let base = args.spec.parent().unwrap_or(Path::new(".")).to_path_buf();

    let format = save::OutputFormat::from_path(&args.output).ok_or_else(|| {
        Error::new(
            ErrorCode::UnknownOutputFormat,
            format!("{} の拡張子から形式を判別できません", args.output.display()),
        )
        .with_hint("avif / png / jpg / webp のいずれかの拡張子を付けてください")
    })?;
    crate::commands::output::ensure_path_writable(&args.output, args.force)?;
    // 縮小表示で測る表示（`display::measurable`）。**ここで 1 度だけ選ぶ**——
    // プレビューの書く先、文字の被覆の縮小、測りが同じ一覧を見るので、確かめた
    // 先と書く先と測った表示が食い違わない
    let measured: Option<Vec<&Display>> = profile.map(|p| {
        p.displays
            .iter()
            .filter(|d| display::measurable(spec.canvas.width, d))
            .collect()
    });
    // **プレビューの先も、描く前に確かめる。** 出力を書いてからプレビューで
    // 断ると、半端な成果物が残る（モジュールの doc の「書く約束」）
    if let Some(measured) = measured.as_deref().filter(|_| args.preview) {
        let previews = measured.iter().map(|d| preview_path(&args.output, d.name));
        for path in previews {
            crate::commands::output::ensure_path_writable(&path, args.force)?;
            refuse_directory(&path)?;
        }
        refuse_directory(&args.output)?;
    }

    // **フォントは 1 画素も描く前に決める。** 見つからなければここで断るので、
    // 半端な成果物が残らない
    let mut warnings: Vec<Warning> = Vec::new();
    if let Some(profile) = profile {
        warnings.extend(aspect_warning(profile, &spec));
    }
    let fonts = compose::font::resolve(&spec, &base, &mut warnings)?;

    // **透過のまま書けるか。** コントラストを測るときに、下地が透明な画素を
    // 何色として扱うかがここで決まる（`measure::contrast` の doc）
    let flatten_to = (args.flatten || !format.supports_alpha()).then_some(args.background);

    let built = build(
        &spec,
        &base,
        &fonts.layers,
        flatten_to,
        measured.as_deref(),
        args.free_regions,
        &mut warnings,
    )?;
    let canvas = built.canvas;
    let mut layers = built.layers;
    // **文字は `placed` の矩形で避ける。** 行間ごと 1 つの塊として読まれるので、
    // グリフの画素だけを避けると行間へ別の文字を差し込む候補が出る（§14.2.2）
    let free_regions = built.obstacles.map(|mut obstacles| {
        for layer in layers.iter().filter(|l| l.kind == "text") {
            obstacles.fill_rect(layer.placed);
        }
        let (w, h) = (spec.canvas.width, spec.canvas.height);
        let within = free::within((w, h), spec.safe_area);
        let margin = args
            .free_margin
            .map_or_else(|| free::default_margin(w), free::margin_from);
        free::report(&obstacles, within, margin, free::min_size((w, h), profile))
    });
    let mut shrunk: Vec<(&'static str, display::Shrunk)> = Vec::new();

    if let Some(measured) = measured.as_deref() {
        // **透過は白として縮める**——note の一覧の地は白で、透過の PNG を上げた
        // ときに見えるのはその白である（12-phase-31.md §12.6 の 2）。透過を
        // 保てない出力なら、実際に潰される色のほうが見える
        let base = flatten_to.unwrap_or([255, 255, 255]);
        shrunk = measure_displays(measured, &canvas, base, &built.glyphs, &spec, &mut layers);
    }

    for layer in &layers {
        warnings.extend(measure::warnings(layer));
        for report in layer.display.iter().flatten() {
            warnings.extend(display::warning(&layer.id, report, spec.canvas.width));
        }
    }

    let compliance = args
        .fail_on
        .as_ref()
        .map(|f| f.evaluate(&layers, profile.is_some()));
    // **全部をエンコードしてから書く。** エンコードで断られる（形式の制約）のを
    // 書いた後に知ると、出力だけが残る
    let (output, bytes) = encode_output(&canvas, &args.output, format, args, &mut warnings)?;
    let encoded_previews = if args.preview {
        encode_previews(&args.output, &shrunk, args, &mut warnings)?
    } else {
        Vec::new()
    };
    if !args.dry_run {
        let mut files: Vec<(&Path, &[u8])> = vec![(&args.output, &bytes)];
        files.extend(
            encoded_previews
                .iter()
                .map(|(path, bytes, _)| (path.as_path(), bytes.as_slice())),
        );
        write_all(&files)?;
    }
    let previews = args.preview.then(|| {
        encoded_previews
            .into_iter()
            .map(|(_, _, report)| report)
            .collect()
    });

    // **`--dry-run` でも判定する。** dry-run はエンコードまで通し、書いたはずの
    // バイト数を `output.bytes` に返している（`encode_output`）——数が決まって
    // いるのに黙るのは、本番で初めて上限に当たる形を残すだけである
    if let Some(max) = profile.and_then(|p| p.max_bytes) {
        if output.bytes > max {
            warnings.push(
                Warning::new(
                    WarningCode::ProfileMaxBytesExceeded,
                    format!(
                        "{} バイトは --profile の上限 {max} バイトを超えています",
                        output.bytes
                    ),
                )
                .with_hint(max_bytes_hint(format))
                .with_data("bytes", output.bytes)
                .with_data("max_bytes", max),
            );
        }
    }

    Ok(ComposeReport {
        schema_version: SCHEMA_VERSION,
        spec: args.spec.display().to_string(),
        profile: profile.map(|p| ComposeProfileRef {
            name: p.name,
            revision: p.revision,
            source: p.source,
        }),
        canvas: ComposeCanvasReport {
            width: spec.canvas.width,
            height: spec.canvas.height,
            background: spec.canvas.background.clone(),
        },
        font: fonts.top,
        output,
        previews,
        dry_run: args.dry_run,
        layers,
        free_regions,
        compliance,
        elapsed_ms: started.elapsed().as_millis(),
        warnings,
    })
}

/// `PROFILE_MAX_BYTES_EXCEEDED` の hint。**いまの形式で効く手だけを言う。**
///
/// JPEG / AVIF に「JPEG か AVIF で書け」と言っても次の手にならない。PNG と kiri の
/// WebP は可逆で `--quality` が効かないので、そちらには形式を変える手だけを言う
/// （12-phase-31.md §12.9 の R5）。
fn max_bytes_hint(format: save::OutputFormat) -> &'static str {
    match format {
        save::OutputFormat::Jpeg | save::OutputFormat::Avif => {
            "--quality を下げてください。それでも収まらなければ canvas を小さくしてください"
        }
        save::OutputFormat::Png | save::OutputFormat::WebP => {
            "出力先の拡張子を .jpg か .avif にしてください（PNG と WebP は可逆なので、品質を下げても小さくなりません）"
        }
    }
}

/// キャンバスの比が profile の比から外れていたら言う。
///
/// **spec が書いた寸法にだけ当たる。** profile が埋めた寸法は profile の比
/// そのものなので、ここで外れることは無い。
fn aspect_warning(profile: &ComposeProfile, spec: &ComposeSpec) -> Option<Warning> {
    let (w, h) = (spec.canvas.width, spec.canvas.height);
    if !profile.aspect_mismatch(w, h) {
        return None;
    }
    let (pw, ph) = profile.canvas;
    Some(
        Warning::new(
            WarningCode::ProfileAspectMismatch,
            format!(
                "canvas {w}x{h} の比が {} の {pw}x{ph} と違います。表示側でトリミングされます",
                profile.name
            ),
        )
        .with_hint(format!(
            "canvas の width / height を消すと {pw}x{ph} で組みます"
        ))
        .with_data("expected", vec![pw, ph])
        .with_data("actual", vec![w, h]),
    )
}

/// `--preview` が書く先。出力の隣に `<出力の stem>.<表示名>.png`。
///
/// 形式は出力に依らず PNG に固定する——非可逆で書くと、画素から計算し直した
/// 輝度が測りの輝度と合わなくなる。**名前は `OsString` のまま組む。** UTF-8 で
/// ない stem を `to_string_lossy` で通すと `�` に化け、出力と違う名前の隣に書く。
fn preview_path(output: &Path, display: &str) -> PathBuf {
    let mut name = output
        .file_stem()
        .map(std::ffi::OsStr::to_os_string)
        .unwrap_or_default();
    name.push(".");
    name.push(display);
    name.push(".png");
    output.with_file_name(name)
}

/// 書く先が既にあるディレクトリなら断る。**`--force` でも断る**——ディレクトリを
/// ファイルで置き換えることは無く、書く段で必ず失敗する。描く前に言えば、
/// 何も書かずに済む。
fn refuse_directory(path: &Path) -> Result<()> {
    if path.is_dir() {
        return Err(Error::new(
            ErrorCode::OutputWriteFailed,
            format!(
                "{} はディレクトリなので、ファイルとして書けません",
                path.display()
            ),
        )
        .with_hint("出力先の名前を変えるか、そのディレクトリを移してください"));
    }
    Ok(())
}

/// 測りが縮めた絵を PNG にエンコードする。**縮め直さない**——`Shrunk` が持つ
/// RGB をそのまま渡す。
///
/// **縮めた絵の一覧だけから組む。** 書く先の確かめ（`run`）と同じく
/// `display::measurable` で選ばれた表示の一覧なので、確かめた先と書く先が
/// 食い違わない。
fn encode_previews(
    output: &Path,
    shrunk: &[(&'static str, display::Shrunk)],
    args: &ComposeArgs,
    warnings: &mut Vec<Warning>,
) -> Result<Vec<(PathBuf, Vec<u8>, ComposePreviewReport)>> {
    let options = save::SaveOptions {
        format: save::OutputFormat::Png,
        quality: args.quality,
        effort: args.effort,
        background: args.background,
        flatten: false,
        icc: save::IccPolicy::Embed,
    };
    let mut out = Vec::with_capacity(shrunk.len());
    for (name, image) in shrunk {
        let rgba: RgbaImage = image.rgb().convert();
        let (bytes, encode_warnings) = save::encode(&rgba, &options)?;
        warnings.extend(encode_warnings);
        let path = preview_path(output, name);
        let report = ComposePreviewReport {
            display: name,
            path: path.display().to_string(),
            width: image.width,
            height: image.height,
        };
        out.push((path, bytes, report));
    }
    Ok(out)
}

/// エンコード済みのファイルを順に書く。**途中で失敗したら、この呼び出しが
/// 新しく作ったファイルを消す。**
///
/// 上書きしたファイルは戻せない（前の中身を持っていない）。そのときは hint で
/// そう言う——黙ると、消えた前の版を利用者が探すことになる。
fn write_all(files: &[(&Path, &[u8])]) -> Result<()> {
    let mut created: Vec<&Path> = Vec::new();
    let mut overwritten: Vec<&Path> = Vec::new();
    for &(path, bytes) in files {
        let fresh = !path.exists();
        if let Err(error) = save::write_encoded(path, bytes) {
            // 書きかけのファイルが残っていることがある。新しく作った先なら消す
            if fresh {
                created.push(path);
            }
            for p in &created {
                let _ = std::fs::remove_file(p);
            }
            if overwritten.is_empty() {
                return Err(error);
            }
            let names: Vec<String> = overwritten
                .iter()
                .map(|p| p.display().to_string())
                .collect();
            return Err(error.with_hint(format!(
                "この実行が新しく作ったファイルは消しました。--force で上書きした {} は\
                 元に戻せません",
                names.join(", ")
            )));
        }
        if fresh {
            created.push(path);
        } else {
            overwritten.push(path);
        }
    }
    Ok(())
}

/// 全層を重ねたキャンバスを測る表示ごとに縮め、文字の層ごとに測る。
///
/// **表示ごとに 1 回だけ縮める。** 縮めた絵は文字の層をまたいで同じなので、
/// 層ごとに縮め直すと同じ計算を層の数だけ払う。縮めた絵は `--preview` の
/// ために返す（表示 1 つで高々 192x101 の RGB である）。
///
/// `measured` は `display::measurable` で選んだ表示だけの一覧である。表示幅が
/// キャンバスより広い表示は測らず、`display[]` に行ごと出さない。
fn measure_displays(
    measured: &[&Display],
    canvas: &RgbaImage,
    base: [u8; 3],
    glyphs: &[Glyphs],
    spec: &ComposeSpec,
    layers: &mut [LayerReport],
) -> Vec<(&'static str, display::Shrunk)> {
    let mut kept = Vec::with_capacity(measured.len());
    let (w, h) = (spec.canvas.width, spec.canvas.height);
    let mut reports: Vec<Vec<display::DisplayReport>> = glyphs
        .iter()
        .map(|_| Vec::with_capacity(measured.len()))
        .collect();
    for (d, target) in measured.iter().enumerate() {
        let height = display::display_height(w, h, target.width);
        let shrunk = display::shrink_canvas(canvas, base, target.width, height);
        for (entry, list) in glyphs.iter().zip(&mut reports) {
            let halo = entry.halo.as_ref().map(|halo| halo[d].as_slice());
            list.push(display::measure(
                &shrunk,
                &entry.coverage[d],
                halo,
                layers[entry.index].placed,
                entry.size,
                (w, h),
                target,
            ));
        }
        kept.push((target.name, shrunk));
    }
    for (entry, list) in glyphs.iter().zip(reports) {
        layers[entry.index].display = Some(list);
    }
    kept
}

/// 組み上がったもの。
struct Built {
    canvas: RgbaImage,
    layers: Vec<LayerReport>,
    /// 縮小表示で測る文字の層のグリフ。`--profile` が無ければ空
    glyphs: Vec<Glyphs>,
    /// `--free-regions` の障害物のうち、画像の不透明な画素。文字の `placed` は
    /// まだ塗っていない（`run` が塗る）。渡さなければ `None`
    obstacles: Option<Obstacles>,
}

/// 縮小表示で測るために取っておく、1 つの文字の層。
struct Glyphs {
    /// `layers` の中での位置
    index: usize,
    /// spec の `size`。表示上の大きさはここから計算で出す
    size: f64,
    /// この文字の**塗りの**被覆を表示ごとに縮めて区分へ畳んだもの。並びは
    /// 測る表示（`display::measurable` で選んだ一覧）の並び
    coverage: Vec<Vec<u8>>,
    /// 縁取りのある層だけ、**塗り + 縁の**被覆を同じく縮めたもの。縮小での
    /// 背景を縁と地から選ぶのに使う（`display::measure` の doc）
    halo: Option<Vec<Vec<u8>>>,
}

/// 文字の層について、後から覆われた割合を数えるために取っておくもの。
struct Covered {
    /// `layers` の中での位置。これより後に塗られていたら覆われている
    index: u16,
    /// その文字が覆うキャンバスの画素（通し番号）
    pixels: Vec<u32>,
}

/// 下地を作り、層を順に重ね、各層を測る。
///
/// `measured` は縮小表示で測る表示の一覧で、`--profile` が無ければ `None`。
/// `free` が真なら、`--free-regions` の障害物を集める。
fn build(
    spec: &ComposeSpec,
    base: &Path,
    fonts: &[Option<LayerFont>],
    flatten_to: Option<[u8; 3]>,
    measured: Option<&[&Display]>,
    free: bool,
    warnings: &mut Vec<Warning>,
) -> Result<Built> {
    let (w, h) = (spec.canvas.width, spec.canvas.height);
    let area = (w as usize) * (h as usize);
    let mut canvas = match &spec.canvas.background {
        Some(color) => {
            let [r, g, b] = compose::parse_color(color)?;
            RgbaImage::from_pixel(w, h, image::Rgba([r, g, b, 255]))
        }
        None => RgbaImage::new(w, h),
    };

    // `subject` の不透明部分。**文字の重なりはこれで数える**——外接矩形で数えると
    // 行間の空白まで商品に重なったことになる
    let mut subject: Vec<u8> = vec![0; area];
    let mut has_subject = false;
    // 画素ごとに最後に塗った層。**`has_subject` と違い、実際に 1 画素でも
    // 書けたときにしか動かない**
    let mut painter: Vec<u16> = vec![UNPAINTED; area];
    let mut obstacles = free.then(|| Obstacles::new(w, h));

    let mut reports = Vec::with_capacity(spec.layers.len());
    let mut covered: Vec<Covered> = Vec::new();
    let mut glyphs: Vec<Glyphs> = Vec::new();

    for (i, layer) in spec.layers.iter().enumerate() {
        let index = i as u16;
        let report = match layer {
            Layer::Image(image) => {
                let (placed, opaque_rect) = draw_image(
                    &mut canvas,
                    image,
                    base,
                    index,
                    &mut subject,
                    &mut has_subject,
                    &mut painter,
                    obstacles.as_mut(),
                )?;
                LayerReport {
                    opaque_rect,
                    // **不透明な画素で判定する。** 透過の余白が外へ出ても、表示で
                    // 切られるものは無い。描く画素が無ければ外へは出ようがない。
                    // **decoration は測らない**——全面に敷いた背景の写真は外へ出る
                    // のが当然で、図形の層と同じく切られても欠ける情報は無い
                    outside_safe_area: spec
                        .safe_area
                        .filter(|_| image.role != Role::Decoration)
                        .map(|a| opaque_rect.is_some_and(|r| measure::outside(a, r))),
                    ..LayerReport::bare(
                        image.id.clone(),
                        layer.kind(),
                        image.role,
                        image.rect,
                        placed,
                    )
                }
            }
            Layer::Shape(shape) => draw_shape(&mut canvas, shape, spec, index, &mut painter)?,
            Layer::Text(text) => {
                // `compose::validate` が「文字があるのに font が無い」を既に断っているので、
                // ここへ `None` で来ることは無い。**それでも unwrap しない**——届かない
                // はずの経路が届いたときに、panic ではなく code で言えるほうがよい
                let font = fonts.get(i).and_then(Option::as_ref).ok_or_else(|| {
                    Error::new(
                        ErrorCode::FontNotFound,
                        format!("'{}' を組むための字体が決まっていません", text.id),
                    )
                })?;
                let (report, pixels, drawn) = draw_text(
                    &mut canvas,
                    text,
                    spec,
                    font,
                    flatten_to,
                    index,
                    &subject,
                    has_subject,
                    &mut painter,
                    warnings,
                )?;
                // **原寸で 1 画素も描かれなかった層は測らない。** 縮めた前景が
                // 0 画素になるのは当然で、それを `strokes_lost` と呼ぶと
                // `TEXT_NOT_RENDERED` と並んで「weight を上げる」と嘘の手を
                // 勧める（12-phase-31.md §12.9 の R1）。`text_contrast` が null に
                // なるのと同じく、`display` を持たない
                let drawn_at_full_size = !pixels.is_empty();
                covered.push(Covered { index, pixels });
                // **読めることを要求しない層は測らない**（`text_contrast` と同じ線引き）
                if let Some(measured) =
                    measured.filter(|_| text.role.wants_contrast() && drawn_at_full_size)
                {
                    let shrink = |pixels: &RgbaImage| -> Vec<Vec<u8>> {
                        measured
                            .iter()
                            .map(|d| {
                                let height = display::display_height(w, h, d.width);
                                display::shrink_coverage(pixels, d.width, height)
                            })
                            .collect()
                    };
                    // **縁取りのある層は、塗りだけを組み直して前景を取る。** 前景は
                    // 塗りの被覆で選ぶ（縁は線を太らせない）ので、塗り + 縁の絵からは
                    // 取れない。組み直すのは `--profile` を渡した実行だけである
                    let (coverage, halo) = match text.stroke {
                        None => (shrink(&drawn), None),
                        Some(_) => {
                            let fill = fill_only(text, spec, font)?;
                            (shrink(&fill), Some(shrink(&drawn)))
                        }
                    };
                    glyphs.push(Glyphs {
                        index: i,
                        size: text.size,
                        coverage,
                        halo,
                    });
                }
                report
            }
        };
        reports.push(report);
    }

    // **全部重ね終わってから数える。** 覆った層は後から来るので、重ねる途中では
    // 答えが出ない
    for entry in covered {
        let report = &mut reports[entry.index as usize];
        let ratio = obscured(&entry, &painter);
        report.text_obscured = ratio;
        if let Some(ratio) = ratio {
            if ratio > measure::MAX_OBSCURED {
                warnings.push(
                    Warning::new(
                        WarningCode::TextObscured,
                        format!(
                            "'{}' の {:.1}% が後の層に覆われています",
                            report.id,
                            ratio * 100.0
                        ),
                    )
                    .with_hint(
                        "text_contrast は背後を測った値なので、覆われた文字でも高い値を\
                         返します。層の順番を見直してください",
                    )
                    .with_data("text_obscured", ratio)
                    .with_data("maximum", measure::MAX_OBSCURED),
                );
            }
        }
    }

    Ok(Built {
        canvas,
        layers: reports,
        glyphs,
        obstacles,
    })
}

/// 縁取りのある文字の、**塗りだけ**を描いた画像。縁付きと同じ位置に組み
/// （`text::layout_fill`）、縁の属性だけを落とす——塗りの形は縁の有無で
/// 変わらないので、縁付きの絵の塗りと画素まで一致する。
fn fill_only(layer: &TextLayer, spec: &ComposeSpec, font: &LayerFont) -> Result<RgbaImage> {
    let family = &font.used.font.family;
    Ok(compose::text::layout_fill(layer, &spec.canvas, family, &font.db)?.pixels)
}

/// 図形を描いて重ね、層の測りを返す（13-phase-32.md §13.2.3）。
///
/// **`painter` を塗り、`subject` は塗らない。** 後から置いた図形は文字を覆う
/// （`text_obscured`）が、図形は商品ではないので文字が重なってもよい相手である。
/// 測りのキーは全部 null で、`outside_safe_area` も null——端まで敷いた帯が
/// 表示で切られても欠ける情報は無く、測ると普通の構図が毎回
/// `OUTSIDE_SAFE_AREA` を出す。
fn draw_shape(
    canvas: &mut RgbaImage,
    layer: &ShapeLayer,
    spec: &ComposeSpec,
    index: u16,
    painter: &mut [u16],
) -> Result<LayerReport> {
    let pixels = compose::shape::render(layer, &spec.canvas)?;
    paint(
        painter,
        canvas.width(),
        canvas.height(),
        &pixels,
        (0, 0),
        index,
    );
    canvas_mod::composite_clipped(canvas, &pixels, (0, 0));
    Ok(LayerReport::bare(
        layer.id.clone(),
        "shape",
        Role::Decoration,
        layer.rect,
        layer.rect,
    ))
}

/// 文字が覆う画素のうち、自分より後の層に塗られた割合。
fn obscured(entry: &Covered, painter: &[u16]) -> Option<f64> {
    if entry.pixels.is_empty() {
        return None;
    }
    let hidden = entry
        .pixels
        .iter()
        .filter(|&&p| {
            painter
                .get(p as usize)
                .is_some_and(|&who| who != UNPAINTED && who > entry.index)
        })
        .count();
    Some(hidden as f64 / entry.pixels.len() as f64)
}

/// 画像を枠へ当てはめて重ね、**実際に置かれた矩形**と、そのうち不透明な画素が
/// 占める矩形を返す。
///
/// **原点を 0 で止めない。** 止めると「枠の外へ置いた」という事実が結果から
/// 消え、`placed` が spec と違う位置を名乗る。はみ出した分は
/// 切り落とし、返す矩形は要求どおりの位置を言う。
///
/// `obstacles` があれば、`decoration` でない画像の不透明な画素をそこへ押す
/// （`--free-regions`。`subject` だけでなく、ロゴのような `heading` の画像も避ける）。
#[allow(clippy::too_many_arguments)]
fn draw_image(
    canvas: &mut RgbaImage,
    layer: &ImageLayer,
    base: &Path,
    index: u16,
    subject: &mut [u8],
    has_subject: &mut bool,
    painter: &mut [u16],
    obstacles: Option<&mut Obstacles>,
) -> Result<([f64; 4], Option<[f64; 4]>)> {
    let path = base.join(&layer.source);
    let loaded = load::load_with(&path, &Default::default())?;
    // 透過の余白を落としてから当てはめる（`compose::Trim` の doc）。1 画素も
    // 不透明でない素材は切り詰める先が無いので、そのまま当てはめる
    //
    // 元の絵はこのブロックで手放す。切り詰めた複製と当てはめた絵が並ぶ間まで
    // 持つと、24MP で 100MB 近くを無駄に抱える
    let (source, bounds) = {
        let original = loaded.image;
        let bounds = measure::opaque_bounds(&original);
        match (layer.trim, bounds) {
            (Trim::Alpha, Some([bx, by, bw, bh])) if (bw, bh) != original.dimensions() => (
                image::imageops::crop_imm(&original, bx, by, bw, bh).to_image(),
                Some([0, 0, bw, bh]),
            ),
            _ => (original, bounds),
        }
    };

    let [x, y, width, height] = layer.rect;
    // 枠の寸法は `compose::check_rect` が `MAX_CANVAS_SIDE` 以下だと確かめている
    // ので、ここでのキャストは飽和しない
    let spec = ResizeSpec {
        width: Some(width.round().max(1.0) as u32),
        height: Some(height.round().max(1.0) as u32),
        fit: layer.fit,
        // **枠へ収めるための拡大は要求そのものである。** 倍率を見て言うのは
        // 呼ぶ側の仕事という分担は `canvas::apply` と同じ
        allow_upscale: true,
    };
    let plan = crate::transform::plan((source.width(), source.height()), &spec)?;
    let fitted = crate::transform::apply(&source, &plan)?;

    // 枠の中央へ寄せる。**枠より小さく収まるのは `contain` の約束**で、
    // そのぶんの余白をどちらへ寄せるかは決めなければならない
    let (fw, fh) = (fitted.width(), fitted.height());
    let at = (
        (x + (width - f64::from(fw)) / 2.0).round() as i64,
        (y + (height - f64::from(fh)) / 2.0).round() as i64,
    );

    if layer.role == Role::Subject {
        // **実際に 1 画素でも書けたときだけ立てる。** 枠ごとキャンバスの外に
        // ある商品で立ててしまうと、以降の文字が「重なり 0.0」という**測れた
        // 合格**を返す。測れていないなら `null` でなければならない
        if stamp_alpha(subject, canvas.width(), canvas.height(), &fitted, at) {
            *has_subject = true;
        }
    }
    if let Some(obstacles) = obstacles.filter(|_| layer.role != Role::Decoration) {
        obstacles.stamp(&fitted, at);
    }
    paint(painter, canvas.width(), canvas.height(), &fitted, at, index);
    canvas_mod::composite_clipped(canvas, &fitted, at);

    let opaque = bounds
        .and_then(|b| opaque_in_fitted(b, source.dimensions(), &plan))
        .map(|[ox, oy, ow, oh]| [at.0 as f64 + ox, at.1 as f64 + oy, ow, oh]);
    Ok((
        [at.0 as f64, at.1 as f64, f64::from(fw), f64::from(fh)],
        opaque,
    ))
}

/// 素材の不透明な外接矩形を、当てはめた後の絵の座標へ写す。`cover` で
/// 切り落とされて何も残らなければ `None`。
///
/// **縮めた絵の画素を数え直さない。** Lanczos は縁の外へ不透明度 1〜3 の裾を
/// 数画素ぶん作り、それを数えると見えない画素のせいで矩形が膨らむ——
/// `safe_area` の偽陽性を直すための値が、別の偽陽性を作る。
///
/// 写した矩形は**外向きに画素の格子へ丸める**。`placed` と同じく整数で返し、
/// 広がるのは 1px 未満なので裾の問題は戻らない。
fn opaque_in_fitted(
    bounds: [u32; 4],
    source: (u32, u32),
    plan: &crate::transform::ResizePlan,
) -> Option<[f64; 4]> {
    let sx = f64::from(plan.scaled.0) / f64::from(source.0);
    let sy = f64::from(plan.scaled.1) / f64::from(source.1);
    let (cx, cy, cw, ch) = plan.crop.unwrap_or((0, 0, plan.scaled.0, plan.scaled.1));
    let [bx, by, bw, bh] = bounds.map(f64::from);
    let x0 = (bx * sx - f64::from(cx)).floor().max(0.0);
    let y0 = (by * sy - f64::from(cy)).floor().max(0.0);
    let x1 = ((bx + bw) * sx - f64::from(cx)).ceil().min(f64::from(cw));
    let y1 = ((by + bh) * sy - f64::from(cy)).ceil().min(f64::from(ch));
    (x0 < x1 && y0 < y1).then_some([x0, y0, x1 - x0, y1 - y0])
}

/// 文字を組んで重ね、測りと「覆った画素」と、この文字だけを描いた画像を返す。
#[allow(clippy::too_many_arguments)]
fn draw_text(
    canvas: &mut RgbaImage,
    layer: &TextLayer,
    spec: &ComposeSpec,
    font: &LayerFont,
    flatten_to: Option<[u8; 3]>,
    index: u16,
    subject: &[u8],
    has_subject: bool,
    painter: &mut [u16],
    warnings: &mut Vec<Warning>,
) -> Result<(LayerReport, Vec<u32>, RgbaImage)> {
    let layout = compose::text::layout(layer, &spec.canvas, &font.used.font.family, &font.db)?;
    let color = compose::parse_color(&layer.color)?;
    let stroke = layer
        .stroke
        .as_ref()
        .map(|s| compose::parse_color(&s.color))
        .transpose()?;
    // 覆う画素は**縁を含む**——縁は見えるインクで、重なりも覆いもそこまで数える
    let pixels = measure::covered_pixels(&layout.pixels);

    // **重ねる前に測る。** いまのキャンバスがこの文字の背後そのものである。
    // 縁取りのある層では縁が背後になる（`measure::stroked_contrast`）。ただし
    // **縁が塗りと同じ色なら、縁は字を何からも分けていない**——黒地に白の字へ
    // 白の縁を付けて 1.0 と言えば、読める字に「読めない」と言う。そのとき字を
    // 分けているのは縁の下の地なので、塗り + 縁が覆う画素の背後とも比べ、
    // 良いほうを採る（13-phase-32.md §13.7）
    let contrast = layer
        .role
        .wants_contrast()
        .then(|| match stroke {
            None => measure::contrast(&layout.pixels, canvas, color, flatten_to),
            Some(stroke) => (!pixels.is_empty()).then(|| {
                let halo = measure::stroked_contrast(color, stroke);
                measure::contrast(&layout.pixels, canvas, color, flatten_to)
                    .map_or(halo, |ground| halo.max(ground))
            }),
        })
        .flatten();
    let layer_overlap = has_subject.then(|| measure::overlap(&pixels, subject));

    if pixels.is_empty() {
        warnings.push(
            Warning::new(
                WarningCode::TextNotRendered,
                format!("'{}' が 1 画素も描かれていません", layer.id),
            )
            .with_hint(
                "rect がキャンバスの外にあるか、字体にその文字のグリフが無い\
                 可能性があります",
            )
            .with_data("rect", layer.rect.to_vec())
            .with_data("placed", layout.bbox.to_vec()),
        );
    }

    for &p in &pixels {
        if let Some(slot) = painter.get_mut(p as usize) {
            *slot = index;
        }
    }
    canvas_mod::composite_clipped(canvas, &layout.pixels, (0, 0));

    // `text_obscured` は重ねた後でなければ決まらない。`build` が埋める
    let report = LayerReport {
        line_widths: Some(layout.line_widths),
        text_overflow: Some(measure::overflow(layer.rect, layout.bbox)),
        text_contrast: contrast,
        layer_overlap,
        outside_safe_area: spec.safe_area.map(|a| measure::outside(a, layout.bbox)),
        align: Some(layer.align),
        font: Some(font.used.clone()),
        ..LayerReport::bare(
            layer.id.clone(),
            "text",
            layer.role,
            layer.rect,
            layout.bbox,
        )
    };
    Ok((report, pixels, layout.pixels))
}

/// `subject` の覆いを記録する。**1 画素でも書けたら真を返す。**
///
/// 最大値で重ねるのは、商品が 2 枚あるときに後の 1 枚が前の 1 枚の覆いを
/// 消さないようにするためである。
fn stamp_alpha(mask: &mut [u8], w: u32, h: u32, src: &RgbaImage, at: (i64, i64)) -> bool {
    let mut touched = false;
    for_each_pixel(w, h, src, at, |index, pixel| {
        if pixel[3] > 0 {
            touched = true;
        }
        mask[index] = mask[index].max(pixel[3]);
    });
    touched
}

/// 「この画素を最後に塗ったのは誰か」を記録する。
fn paint(painter: &mut [u16], w: u32, h: u32, src: &RgbaImage, at: (i64, i64), index: u16) {
    for_each_pixel(w, h, src, at, |i, pixel| {
        if pixel[3] >= COVERAGE {
            painter[i] = index;
        }
    });
}

/// キャンバスに収まる画素だけを歩く。**`u32` で足さない**（`composite_clipped`
/// と同じ理由）。
fn for_each_pixel(
    w: u32,
    h: u32,
    src: &RgbaImage,
    at: (i64, i64),
    mut f: impl FnMut(usize, [u8; 4]),
) {
    let (cw, ch) = (i64::from(w), i64::from(h));
    for y in 0..i64::from(src.height()) {
        let cy = at.1 + y;
        if cy < 0 || cy >= ch {
            continue;
        }
        for x in 0..i64::from(src.width()) {
            let cx = at.0 + x;
            if cx < 0 || cx >= cw {
                continue;
            }
            let index = (cy as usize) * (w as usize) + (cx as usize);
            f(index, src.get_pixel(x as u32, y as u32).0);
        }
    }
}

/// 出力をエンコードする。**書くのは `write_all` である**——プレビューと一緒に、
/// 全部をエンコードし終えてから書く（モジュールの doc の「書く約束」）。
fn encode_output(
    canvas: &RgbaImage,
    path: &Path,
    format: save::OutputFormat,
    args: &ComposeArgs,
    warnings: &mut Vec<Warning>,
) -> Result<(ComposeOutputReport, Vec<u8>)> {
    let options = save::SaveOptions {
        format,
        quality: args.quality,
        effort: args.effort,
        background: args.background,
        flatten: args.flatten,
        icc: save::IccPolicy::Embed,
    };

    // `--dry-run` でも**エンコードまでは通す。** バイト数を返さずに「書ける」と
    // 言うと、本番で初めて形式の制約に当たることになる
    let (bytes, encode_warnings) = save::encode(canvas, &options)?;
    warnings.extend(encode_warnings);

    let report = ComposeOutputReport {
        path: path.display().to_string(),
        format: format.as_str().to_string(),
        width: canvas.width(),
        height: canvas.height(),
        bytes: bytes.len() as u64,
    };
    Ok((report, bytes))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `cover` で切り落とされた部分は、不透明な矩形からも落ちる。
    #[test]
    fn the_opaque_rect_is_clipped_by_the_cover_crop() {
        // 200x100 の素材を 2 倍の 400x200 へ広げ、中央の 200x200 を残す
        let plan = crate::transform::ResizePlan {
            scaled: (400, 200),
            crop: Some((100, 0, 200, 200)),
            output: (200, 200),
        };
        // 左端の 60x100 だけが不透明 → 拡大後 [0,0,120,200] → 切り取り後 [0,0,20,200]
        assert_eq!(
            opaque_in_fitted([0, 0, 60, 100], (200, 100), &plan),
            Some([0.0, 0.0, 20.0, 200.0])
        );
        // 左端の 40x100 は切り取りで全部落ちる
        assert_eq!(opaque_in_fitted([0, 0, 40, 100], (200, 100), &plan), None);
    }

    /// 割り切れない倍率でも、返す矩形は画素の格子に乗り、不透明な範囲を覆う。
    #[test]
    fn the_opaque_rect_is_rounded_outward_to_whole_pixels() {
        // 300 → 100 の 1/3 倍。[10, 10, 50, 50] は x 3.33〜20 に写り、[3, 3, 17, 17] へ丸まる
        let plan = crate::transform::ResizePlan {
            scaled: (100, 100),
            crop: None,
            output: (100, 100),
        };
        assert_eq!(
            opaque_in_fitted([10, 10, 50, 50], (300, 300), &plan),
            Some([3.0, 3.0, 17.0, 17.0])
        );
    }

    /// 書く途中で失敗したら、新しく作ったファイルを消す。上書きしたものは
    /// 戻せないので、そう言う（13-phase-32.md §13.7）。
    #[test]
    fn a_failed_write_removes_the_files_it_created() {
        let dir = tempfile::TempDir::new().unwrap();
        let fresh = dir.path().join("out.png");
        let kept = dir.path().join("kept.png");
        std::fs::write(&kept, b"old").unwrap();
        // 親がファイルなので、ディレクトリを作れず書けない
        let blocker = dir.path().join("blocker");
        std::fs::write(&blocker, b"").unwrap();
        let blocked = blocker.join("out.pc_list.png");

        let err = write_all(&[
            (fresh.as_path(), b"new"),
            (kept.as_path(), b"newer"),
            (blocked.as_path(), b"x"),
        ])
        .unwrap_err();
        assert!(!fresh.exists(), "新しく作った出力が残った");
        assert_eq!(std::fs::read(&kept).unwrap(), b"newer", "上書きは戻せない");
        let hint = err.hint.clone().unwrap_or_default();
        assert!(hint.contains("kept.png"), "{hint}");

        // 上書きが無ければ hint を足さない
        let err = write_all(&[(fresh.as_path(), b"new"), (blocked.as_path(), b"x")]).unwrap_err();
        assert!(!fresh.exists());
        assert!(err.hint.is_none(), "{:?}", err.hint);
    }

    /// プレビューの名前は stem を `OsString` のまま組む。
    #[test]
    fn the_preview_name_keeps_the_stem() {
        assert_eq!(
            preview_path(Path::new("dir/header.png"), "pc_list"),
            Path::new("dir/header.pc_list.png")
        );
        assert_eq!(
            preview_path(Path::new("a.b.jpg"), "mobile_list"),
            Path::new("a.b.mobile_list.png")
        );
    }

    /// 容量の hint は**いまの形式で効く手**を言う（§12.9 の R5）。JPEG / AVIF に
    /// 「JPEG / AVIF で書け」と言っても次の手にならない。PNG と kiri の WebP は
    /// 可逆なので、`--quality` は効かない。
    #[test]
    fn the_max_bytes_hint_depends_on_the_format() {
        use save::OutputFormat;
        for lossy in [OutputFormat::Jpeg, OutputFormat::Avif] {
            let hint = max_bytes_hint(lossy);
            assert!(hint.contains("--quality"), "{hint}");
            assert!(!hint.contains(".jpg"), "{hint}");
        }
        for lossless in [OutputFormat::Png, OutputFormat::WebP] {
            let hint = max_bytes_hint(lossless);
            assert!(hint.contains(".jpg") && hint.contains(".avif"), "{hint}");
            assert!(!hint.contains("--quality"), "{hint}");
        }
    }
}
