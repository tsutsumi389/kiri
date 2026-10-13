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

use std::path::Path;
use std::time::Instant;

use image::RgbaImage;

use crate::cli::ComposeArgs;
use crate::compose::font::LayerFont;
use crate::compose::measure::{self, COVERAGE, LayerReport};
use crate::compose::profile::ComposeProfile;
use crate::compose::{self, ComposeSpec, ImageLayer, Layer, Role, TextLayer, display};
use crate::error::{Error, ErrorCode, Result};
use crate::image_io::{load, save};
use crate::report::{
    ComposeCanvasReport, ComposeOutputReport, ComposeProfileRef, ComposeReport, SCHEMA_VERSION,
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
    let spec = compose::load_with(&args.spec, profile)?;
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
        profile,
        &mut warnings,
    )?;
    let canvas = built.canvas;
    let mut layers = built.layers;

    if let Some(profile) = profile {
        // **透過は白として縮める**——note の一覧の地は白で、透過の PNG を上げた
        // ときに見えるのはその白である（12-phase-31.md §12.6 の 2）。透過を
        // 保てない出力なら、実際に潰される色のほうが見える
        let base = flatten_to.unwrap_or([255, 255, 255]);
        measure_displays(profile, &canvas, base, &built.glyphs, &spec, &mut layers);
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
    let output = write(&canvas, &args.output, format, args, &mut warnings)?;

    // **`--dry-run` でも判定する。** dry-run はエンコードまで通し、書いたはずの
    // バイト数を `output.bytes` に返している（`write` の doc）——数が決まって
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
        dry_run: args.dry_run,
        layers,
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

/// 全層を重ねたキャンバスを profile の表示ごとに縮め、文字の層ごとに測る。
///
/// **表示ごとに 1 回だけ縮める。** 縮めた絵は文字の層をまたいで同じなので、
/// 層ごとに縮め直すと同じ計算を層の数だけ払う。
fn measure_displays(
    profile: &ComposeProfile,
    canvas: &RgbaImage,
    base: [u8; 3],
    glyphs: &[Glyphs],
    spec: &ComposeSpec,
    layers: &mut [LayerReport],
) {
    let (w, h) = (spec.canvas.width, spec.canvas.height);
    for entry in glyphs {
        layers[entry.index].display = Some(Vec::with_capacity(profile.displays.len()));
    }
    for (d, target) in profile.displays.iter().enumerate() {
        // 表示幅がキャンバスより広い表示は測らず、`display[]` に行ごと出さない
        // （`display::measurable`）
        if !display::measurable(w, target) {
            continue;
        }
        let height = display::display_height(w, h, target.width);
        let shrunk = display::shrink_canvas(canvas, base, target.width, height);
        for entry in glyphs {
            let Some(coverage) = entry.coverage[d].as_deref() else {
                continue;
            };
            let layer = &mut layers[entry.index];
            let report =
                display::measure(&shrunk, coverage, layer.placed, entry.size, (w, h), target);
            if let Some(list) = layer.display.as_mut() {
                list.push(report);
            }
        }
    }
}

/// 組み上がったもの。
struct Built {
    canvas: RgbaImage,
    layers: Vec<LayerReport>,
    /// 縮小表示で測る文字の層のグリフ。`--profile` が無ければ空
    glyphs: Vec<Glyphs>,
}

/// 縮小表示で測るために取っておく、1 つの文字の層。
struct Glyphs {
    /// `layers` の中での位置
    index: usize,
    /// spec の `size`。表示上の大きさはここから計算で出す
    size: f64,
    /// この文字の被覆を表示ごとに縮めて区分へ畳んだもの。並びは `displays` の
    /// 並びで、測らない表示（`display::measurable`）は `None`
    coverage: Vec<Option<Vec<u8>>>,
}

/// 文字の層について、後から覆われた割合を数えるために取っておくもの。
struct Covered {
    /// `layers` の中での位置。これより後に塗られていたら覆われている
    index: u16,
    /// その文字が覆うキャンバスの画素（通し番号）
    pixels: Vec<u32>,
}

/// 下地を作り、層を順に重ね、各層を測る。
fn build(
    spec: &ComposeSpec,
    base: &Path,
    fonts: &[Option<LayerFont>],
    flatten_to: Option<[u8; 3]>,
    profile: Option<&ComposeProfile>,
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

    let mut reports = Vec::with_capacity(spec.layers.len());
    let mut covered: Vec<Covered> = Vec::new();
    let mut glyphs: Vec<Glyphs> = Vec::new();

    for (i, layer) in spec.layers.iter().enumerate() {
        let index = i as u16;
        let report = match layer {
            Layer::Image(image) => {
                let placed = draw_image(
                    &mut canvas,
                    image,
                    base,
                    index,
                    &mut subject,
                    &mut has_subject,
                    &mut painter,
                )?;
                LayerReport {
                    id: image.id.clone(),
                    kind: layer.kind(),
                    role: image.role,
                    rect: image.rect,
                    placed,
                    line_widths: None,
                    text_overflow: None,
                    text_contrast: None,
                    text_obscured: None,
                    layer_overlap: None,
                    outside_safe_area: spec.safe_area.map(|a| measure::outside(a, placed)),
                    align: None,
                    font: None,
                    display: None,
                }
            }
            Layer::Text(text) => {
                let (report, pixels, drawn) = draw_text(
                    &mut canvas,
                    text,
                    spec,
                    fonts.get(i).and_then(Option::as_ref),
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
                if let Some(profile) =
                    profile.filter(|_| text.role.wants_contrast() && drawn_at_full_size)
                {
                    let coverage = profile
                        .displays
                        .iter()
                        .map(|d| {
                            display::measurable(w, d).then(|| {
                                let height = display::display_height(w, h, d.width);
                                display::shrink_coverage(&drawn, d.width, height)
                            })
                        })
                        .collect();
                    glyphs.push(Glyphs {
                        index: i,
                        size: text.size,
                        coverage,
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
    })
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

/// 画像を枠へ当てはめて重ね、**実際に置かれた矩形**を返す。
///
/// **原点を 0 で止めない。** 止めると「枠の外へ置いた」という事実が結果から
/// 消え、`placed` が spec と違う位置を名乗る。はみ出した分は
/// 切り落とし、返す矩形は要求どおりの位置を言う。
fn draw_image(
    canvas: &mut RgbaImage,
    layer: &ImageLayer,
    base: &Path,
    index: u16,
    subject: &mut [u8],
    has_subject: &mut bool,
    painter: &mut [u16],
) -> Result<[f64; 4]> {
    let path = base.join(&layer.source);
    let loaded = load::load_with(&path, &Default::default())?;

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
    let plan = crate::transform::plan((loaded.image.width(), loaded.image.height()), &spec)?;
    let fitted = crate::transform::apply(&loaded.image, &plan)?;

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
    paint(painter, canvas.width(), canvas.height(), &fitted, at, index);
    canvas_mod::composite_clipped(canvas, &fitted, at);

    Ok([at.0 as f64, at.1 as f64, f64::from(fw), f64::from(fh)])
}

/// 文字を組んで重ね、測りと「覆った画素」と、この文字だけを描いた画像を返す。
#[allow(clippy::too_many_arguments)]
fn draw_text(
    canvas: &mut RgbaImage,
    layer: &TextLayer,
    spec: &ComposeSpec,
    font: Option<&LayerFont>,
    flatten_to: Option<[u8; 3]>,
    index: u16,
    subject: &[u8],
    has_subject: bool,
    painter: &mut [u16],
    warnings: &mut Vec<Warning>,
) -> Result<(LayerReport, Vec<u32>, RgbaImage)> {
    // `compose::validate` が「文字があるのに font が無い」を既に断っているので、
    // ここへ `None` で来ることは無い。**それでも unwrap しない**——届かない
    // はずの経路が届いたときに、panic ではなく code で言えるほうがよい
    let Some(font) = font else {
        return Err(Error::new(
            ErrorCode::FontNotFound,
            format!("'{}' を組むための字体が決まっていません", layer.id),
        ));
    };

    let layout = compose::text::layout(layer, &spec.canvas, &font.family, &font.db)?;
    let color = compose::parse_color(&layer.color)?;

    // **重ねる前に測る。** いまのキャンバスがこの文字の背後そのものである
    let contrast = layer
        .role
        .wants_contrast()
        .then(|| measure::contrast(&layout.pixels, canvas, color, flatten_to))
        .flatten();
    let pixels = measure::covered_pixels(&layout.pixels);
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

    let report = LayerReport {
        id: layer.id.clone(),
        kind: "text",
        role: layer.role,
        rect: layer.rect,
        placed: layout.bbox,
        line_widths: Some(layout.line_widths),
        text_overflow: Some(measure::overflow(layer.rect, layout.bbox)),
        text_contrast: contrast,
        // 重ねた後でなければ決まらない。`build` が埋める
        text_obscured: None,
        layer_overlap,
        outside_safe_area: spec.safe_area.map(|a| measure::outside(a, layout.bbox)),
        align: Some(layer.align),
        font: Some(font.used.clone()),
        display: None,
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

fn write(
    canvas: &RgbaImage,
    path: &Path,
    format: save::OutputFormat,
    args: &ComposeArgs,
    warnings: &mut Vec<Warning>,
) -> Result<ComposeOutputReport> {
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
    if !args.dry_run {
        save::write_encoded(path, &bytes)?;
    }

    Ok(ComposeOutputReport {
        path: path.display().to_string(),
        format: format.as_str().to_string(),
        width: canvas.width(),
        height: canvas.height(),
        bytes: bytes.len() as u64,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

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
