//! `kiri lint` — **既にあるファイルが規格を満たすかを検査する。書かない。**
//!
//! `cutout --profile` が「規格へ収めて書く」側で、ここは「収まっているかを
//! 見る」側である。**表は 1 つ**（`profile::Rules`）で、両側がそれを読む
//! ——条件を lint の側へ書き写すと、片方を直したときにもう片方が古い規格の
//! まま残る（`profile.rs` の冒頭がそう宣言している）。
//!
//! # 測り方を新設しない
//!
//! 背景も主体も `info` とまったく同じ経路（`load_with` → `see_background` →
//! `analyse_background_seen`）で測る。lint だけが別の測り方を持つと、
//! `kiri info` が返した背景色と lint が照らした背景色が食い違い、
//! **どちらが本当かを利用者が確かめる手段が無くなる。**
//!
//! # 何を合格と呼ぶか
//!
//! `checks[]` がすべて `pass` であることだけが合格である。`fail` も
//! `unmeasurable` も `skipped` も合格ではない（`compliance::FailOn::evaluate`
//! と同じ定義）。**exit 5 の意味は「成果物はある。人が見る対象」**で、
//! 「検査できなかったので人が見てほしい」はまさにそれである。

use serde_json::{Value, json};

use crate::cli::LintArgs;
use crate::color::lab::delta_e_rgb;
use crate::commands::output::{SUBJECT_FROM_COLOUR, round4};
use crate::compliance::{FAIL, PASS, UNMEASURABLE};
use crate::cutout::{BackgroundModel, analyse_background_seen, see_background};
use crate::error::{Error, ErrorCode, Result};
use crate::image_io::avif_meta::{self, ColorNaming};
use crate::image_io::load::{COLOR_SPACE_SRGB, COLOR_SPACE_UNCALIBRATED};
use crate::image_io::{OutputFormat, load};
use crate::profile::{BACKGROUND_DELTA_E_TOLERANCE, Rules};
use crate::report::{LintCheck, LintReport, ProfileRef, SCHEMA_VERSION};
use crate::warning::{Warning, WarningCode};

/// **この形式では構造的に測れない**（AVIF の画素）。
///
/// `UNMEASURABLE`（測ろうとしたが、この画像からは出なかった）と名前を分けるのは、
/// 次の一手が違うためである。`skipped` は形式を変えれば必ず測れるようになり、
/// `unmeasurable` は形式を変えても同じ結果になる。`compliance.rs` が持つ
/// 3 つの綴りはそのまま使い、ここが足すのはこの 1 つだけ——同じ語彙を
/// 2 箇所で定義すると、片方に綴り違いが入っても型は何も言わない。
pub const SKIPPED: &str = "skipped";

/// `fill_ratio` を**アルファの外接矩形**から測ったことを表す綴り。
///
/// 色で見立てた主体から測ったときは `SUBJECT_FROM_COLOUR`（`info` /
/// `cutout` の `subject.source` と同じ語）を名乗る。**どちらで測ったかを
/// 名乗らせる**のは、同じ 0.80 でも意味が違うためである——切り抜き済みの
/// 成果物ではアルファが唯一の正解で、色の見立ては背景の均一度に左右される。
pub const FILL_FROM_ALPHA: &str = "alpha";

/// 検査する条件。**この並びがそのまま `checks[]` の並びになる。**
///
/// 指定の順や `HashMap` の順に依存させない。同じ入力を 2 回走らせて
/// `checks[]` の並びが変わると、差分で結果を見張れなくなる
/// （`compliance::Metric::ALL` とまったく同じ理由）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Check {
    Format,
    LongestSide,
    MaxPixels,
    Square,
    FileSize,
    Alpha,
    ColorSpace,
    Background,
    FillRatio,
}

impl Check {
    /// 見る条件のすべて。**並びはコンテナだけで分かるものが先**で、
    /// 画素を読まないと測れない 2 つを最後に置く。AVIF で `skipped` が
    /// 並ぶのが末尾にまとまり、人が読む出力でも切れ目が見える。
    pub const ALL: [Check; 9] = [
        Check::Format,
        Check::LongestSide,
        Check::MaxPixels,
        Check::Square,
        Check::FileSize,
        Check::Alpha,
        Check::ColorSpace,
        Check::Background,
        Check::FillRatio,
    ];

    pub const fn as_str(self) -> &'static str {
        match self {
            Check::Format => "format",
            Check::LongestSide => "longest_side",
            Check::MaxPixels => "max_pixels",
            Check::Square => "square",
            Check::FileSize => "file_size",
            Check::Alpha => "alpha",
            Check::ColorSpace => "color_space",
            Check::Background => "background",
            Check::FillRatio => "fill_ratio",
        }
    }

    /// 画素を読まないと測れない条件か。**AVIF ではここが `skipped` になる。**
    const fn needs_pixels(self) -> bool {
        matches!(self, Check::Background | Check::FillRatio)
    }

    /// この規格がこの条件を**規定しているか**。
    ///
    /// 規定していない条件は `checks[]` に 1 行も出さない。`Rules` の `Option` が
    /// `None` なのは「規定なし」であって「どんな値でも合格」ではない——
    /// `pass` として並べると、shopify が構図を見ていないことが結果から読めなくなる
    /// （`Rules` の doc が `Option` を置いた理由そのものである）。
    ///
    /// 真偽で持つ 3 つ（`square` / `alpha_allowed` / `srgb_required`）も同じ扱いで、
    /// **偽の側が「規定なし」である**。`square: false` は「正方形不可」ではないと
    /// `Profile::canvas` の doc が述べており、`alpha_allowed: true` は
    /// 「透過を残してよい」なので検査することが無い。
    fn regulated(self, rules: &Rules) -> bool {
        match self {
            Check::Format => !rules.formats.is_empty(),
            Check::LongestSide => {
                rules.longest_side_min.is_some() || rules.longest_side_max.is_some()
            }
            Check::MaxPixels => rules.max_pixels.is_some(),
            Check::Square => rules.square,
            Check::FileSize => rules.max_bytes.is_some(),
            Check::Alpha => !rules.alpha_allowed,
            Check::ColorSpace => rules.srgb_required,
            Check::Background => rules.background.is_some(),
            Check::FillRatio => rules.fill_ratio_min.is_some(),
        }
    }

    /// 規格が求めた値。**`skipped` でも返す**——検査できなかったことと、
    /// 何を求められていたかは別の事実で、後者は形式に関係なく分かっている。
    fn expected(self, rules: &Rules) -> Option<Value> {
        match self {
            Check::Format => Some(json!(
                rules
                    .formats
                    .iter()
                    .map(|f| f.as_str())
                    .collect::<Vec<&str>>()
            )),
            // **上下限の組で返す。** 片方しか無い規格でも両方のキーを出し、
            // 無いほうは null にする——省くと「上限が無い」と「上限を報告して
            // いない」が同じ形になる（`ComplianceCheck` と同じ作法）
            Check::LongestSide => Some(json!({
                "min": rules.longest_side_min,
                "max": rules.longest_side_max,
            })),
            Check::MaxPixels => rules.max_pixels.map(Value::from),
            // 寸法そのものは結果の `width` / `height` が言う。ここが求めて
            // いるのは「2 つが等しいこと」という 1 つの事実である
            Check::Square => Some(Value::Bool(true)),
            Check::FileSize => rules.max_bytes.map(Value::from),
            // 「透過が無いこと」。`Rules::alpha_allowed` の綴りを裏返した形に
            // しないのは、`actual` と同じ問い（透過はあるか）で並べるためである
            Check::Alpha => Some(Value::Bool(false)),
            Check::ColorSpace => Some(json!(COLOR_SPACE_SRGB)),
            // **許容する色差も一緒に配る。** RGB だけを出すと、完全一致を
            // 求められていると読まれる（読んだ側が自分で `!=` を書く）
            Check::Background => rules
                .background
                .map(|rgb| json!({ "rgb": rgb, "delta_e_max": BACKGROUND_DELTA_E_TOLERANCE })),
            Check::FillRatio => rules.fill_ratio_min.map(Value::from),
        }
    }
}

/// 検査に使える事実。**形式によって `pixels` が無い。**
///
/// 測定と判定を分けてあるのは、AVIF と JPEG/PNG で**測り方だけが違い、
/// 判定は 1 通りしかない**ためである。判定の側を形式で分岐させると、
/// 「AVIF のときだけ緩い条件で通る」という枝をいつでも作れてしまう。
struct Facts {
    width: u32,
    height: u32,
    file_size: u64,
    /// 実ファイルの形式。**拡張子ではない**
    format: OutputFormat,
    has_alpha: bool,
    /// sRGB を名乗っているかの判定と、その根拠
    naming: Naming,
    /// 画素から測れたもの。AVIF では `None`
    pixels: Option<Pixels>,
}

/// 画素を読めたときにだけ分かる事実。
struct Pixels {
    /// 外周から推定した背景色
    background: [u8; 3],
    /// 占有率。**主体を 1 つも検出できなければ `None`**（`unmeasurable`）
    fill: Option<Fill>,
}

struct Fill {
    ratio: f64,
    /// `FILL_FROM_ALPHA` か `SUBJECT_FROM_COLOUR`
    source: &'static str,
    /// 測った外接矩形 [x1, y1, x2, y2]（両端を含む）
    bbox: [u32; 4],
}

/// 色の名乗りの判定。**`status` と根拠を組で持つ。**
///
/// 形式ごとに根拠の形が違う（JPEG/PNG は色空間名と ICC の有無、AVIF は
/// CICP の 4 つ）ので、判定した場所でそのまま `actual` を組む。上位で
/// 組み直すと、名乗りの出所ごとに分岐が増えるだけで答えは変わらない。
struct Naming {
    status: &'static str,
    actual: Option<Value>,
}

/// sRGB の CICP。ITU-T H.273 の値で、AVIF はこの 4 つで sRGB を名乗る。
const SRGB_PRIMARIES: u16 = 1;
const SRGB_TRANSFER: u16 = 13;
const SRGB_MATRIX: u16 = 6;

/// CICP の「未指定」。**「別の色空間」ではなく「名乗っていない」である。**
const CICP_UNSPECIFIED: u16 = 2;

pub fn run(args: &LintArgs) -> Result<LintReport> {
    let rules = &args.profile.rules;
    let (facts, mut warnings) = measure(args)?;

    let checks: Vec<LintCheck> = Check::ALL
        .into_iter()
        .filter(|c| c.regulated(rules))
        .map(|c| judge(c, rules, &facts))
        .collect();

    // **飛ばした項目は検査結果から数える。** 別に数え上げると、条件を 1 つ
    // 足したときに `checks[]` と警告が食い違いうる
    let skipped: Vec<&str> = checks
        .iter()
        .filter(|c| c.status == SKIPPED)
        .map(|c| c.name)
        .collect();
    warnings.extend(uncheckable(&skipped, facts.format));

    let passed = checks.iter().all(|c| c.status == PASS);
    Ok(LintReport {
        schema_version: SCHEMA_VERSION,
        input: args.input.display().to_string(),
        profile: ProfileRef {
            name: args.profile.name,
            revision: args.profile.revision,
        },
        file_size: facts.file_size,
        width: facts.width,
        height: facts.height,
        format: facts.format.as_str().to_string(),
        passed,
        // **不合格のときだけ code を名乗る。** exit 5 は `ErrorReport` を
        // 返さない（検査は成功していて、対象のファイルもそのままある）ので、
        // `kiri schema` の `errors[]` が配る語彙と結果を突き合わせられる場所が
        // ここ以外に無い（`ComplianceReport::code` とまったく同じ事情）
        code: (!passed).then_some(ErrorCode::ProfileViolation),
        checks,
        warnings,
    })
}

/// ファイルを測る。**形式の判別は AVIF を先に見る。**
///
/// `load::load_with` は AVIF に対して必ず `UNSUPPORTED_FORMAT` を返すので、
/// そちらを先に呼ぶと「失敗したので次を試す」という形になる。**エラーを
/// 握り潰して次へ進む経路を作ると、本当に壊れた JPEG まで AVIF の判定へ
/// 流れる**——そして AVIF としても読めないので、利用者が受け取るのは
/// 「AVIF として辿れません」という的外れな文面になる。
///
/// 一方 `avif_meta::probe` は「AVIF ではない」（`UNSUPPORTED_FORMAT`）と
/// 「AVIF だが辿れない」（`INPUT_DECODE_FAILED`）を**別の code で返す**と
/// doc で約束している。前者だけを「次を試してよい」と読めばよく、
/// 後者はそのまま返す——AVIF を名乗るファイルが壊れていることは事実で、
/// JPEG として読み直す意味が無い。
///
/// **どちらでも読めなかったときは検査結果を返さない**（`Err` がそのまま
/// 上がり、exit 3 になる）。`checks[]` は「ファイルがあり、形式が分かった」
/// ことを前提に組むもので、1 バイトも解釈できないファイルに `format: fail` と
/// 答えると、`width` / `height` / `format` という必ず出る項目へ嘘の数を
/// 入れることになる。
fn measure(args: &LintArgs) -> Result<(Facts, Vec<Warning>)> {
    let bytes = std::fs::read(&args.input).map_err(|e| {
        Error::new(
            ErrorCode::InputUnreadable,
            format!("{} を読めません: {e}", args.input.display()),
        )
        .with_hint("パスと読み取り権限を確認してください")
    })?;
    let file_size = bytes.len() as u64;

    match avif_meta::probe(&bytes) {
        Ok(meta) => Ok((
            Facts {
                width: meta.width,
                height: meta.height,
                file_size,
                format: OutputFormat::Avif,
                has_alpha: meta.has_alpha,
                naming: avif_naming(meta.color),
                // **画素は 1 つも読めない。** kiri は AVIF をデコードできない
                pixels: None,
            },
            Vec::new(),
        )),
        Err(e) if e.code == ErrorCode::UnsupportedFormat => measure_pixels(args, file_size),
        Err(e) => Err(e),
    }
}

/// JPEG / PNG を画素まで読んで測る。
///
/// **バイト列を 2 度読むことになる**（`measure` が AVIF の判別のために
/// 1 度、`load_with` がもう 1 度）。それでもパスを渡す形を崩さないのは、
/// 読み込みの経路——EXIF Orientation の適用、ICC の解釈、`has_alpha` の
/// 判定——が `info` / `cutout` と 1 バイトも違わないことのほうが、
/// 1 回分の読み出しより重いためである。25MP の背景推定に比べれば、
/// 数 MB の再読み込みは測れるほどの差にならない。
fn measure_pixels(args: &LintArgs, file_size: u64) -> Result<(Facts, Vec<Warning>)> {
    let loaded = load::load_with(&args.input, &args.color.to_load_options())?;

    // **`info` とまったく同じ経路で見立てる。** 1 度だけ測って両方に使う
    // （`info::run` が `seen` を持ち回しているのと同じ理由で、24.5MP の
    // 測定を 2 度払わない）。
    //
    // **モデルは `Auto`（`cutout` の既定）を渡す。** lint が読むのは
    // `estimate`（外周の中央値）と `subject` の 2 つで、どちらも照明場の
    // 当てはめとは無関係なので、どのモデルでも同じ数になる。それでも
    // `analyse_background_seen` を通すのは、**lint が自前の測り方を
    // 持たない**ことを構造で示すためである
    let seen = see_background(&loaded.image, args.border);
    let analysis = analyse_background_seen(
        &loaded.image,
        Some(&seen),
        args.border,
        BackgroundModel::Auto,
        None,
        None,
    );

    let width = loaded.width();
    let height = loaded.height();
    // **アルファがあるならアルファが正解である。** 切り抜き済みの成果物で
    // 色の見立てを使うと、透明な余白を「背景」として測り直すことになり、
    // 書いた側（`canvas::plan` はアルファ付きの内容をそのまま置く）と
    // 違う矩形が出る
    let fill = if loaded.has_alpha {
        alpha_bbox(&loaded.image).map(|bbox| Fill {
            ratio: fill_ratio(bbox, width, height),
            source: FILL_FROM_ALPHA,
            bbox,
        })
    } else {
        analysis.subject.as_ref().map(|s| Fill {
            ratio: fill_ratio(s.bbox, width, height),
            source: SUBJECT_FROM_COLOUR,
            bbox: s.bbox,
        })
    };

    let naming = Naming {
        status: match loaded.color_space.as_str() {
            COLOR_SPACE_SRGB => PASS,
            // **名乗っていない。** ICC も EXIF の申告も無い状態で、
            // 「sRGB ではない」と断じる材料が 1 つも無い。`fail` にすると
            // 名乗りの無い素材を規格違反として落とすことになり、`pass` に
            // すると見ていないものを見たことにする——`unmeasurable` だけが
            // 事実に合う（合格ではないので `passed` は落ちる）
            COLOR_SPACE_UNCALIBRATED => UNMEASURABLE,
            // 別の色空間をはっきり名乗っている（Display P3 など）。
            // **LUT 型の ICC（`COLOR_PROFILE_UNSUPPORTED`）もここへ来る**
            // ——kiri はその中身を読めないので名前だけが残る。なぜ落ちたかは
            // 同時に出るその警告が言う
            _ => FAIL,
        },
        actual: Some(json!({
            "color_space": loaded.color_space,
            "icc_profile": loaded.icc_profile,
            // **検査したのはファイルの名乗りであって、読み込んだ画素では
            // ない。** kiri が sRGB へ変換して測ったことは、ファイルが
            // sRGB を名乗っているかという問いに何の影響も与えない
            "color_converted": loaded.color_converted,
        })),
    };

    Ok((
        Facts {
            width,
            height,
            file_size,
            format: match loaded.format {
                image::ImageFormat::Png => OutputFormat::Png,
                // `load_with` が通すのは JPEG と PNG だけである
                // （それ以外は `UNSUPPORTED_FORMAT` で断られてここへ来ない）
                _ => OutputFormat::Jpeg,
            },
            has_alpha: loaded.has_alpha,
            naming,
            pixels: Some(Pixels {
                background: analysis.estimate.rgb,
                fill,
            }),
        },
        loaded.warnings(),
    ))
}

/// 1 つの条件を判定する。
fn judge(check: Check, rules: &Rules, facts: &Facts) -> LintCheck {
    let expected = check.expected(rules);
    // **画素が要る条件で画素が無ければ、判定に入らず飛ばす。**
    // ここを通さずに下の分岐へ落とすと、`pixels` が無いことを
    // 「背景が黒だった」のような既定値で埋める枝がいつでも書ける
    if check.needs_pixels() && facts.pixels.is_none() {
        return LintCheck {
            name: check.as_str(),
            status: SKIPPED,
            expected,
            actual: None,
        };
    }

    let (status, actual) = match check {
        Check::Format => (
            flag(rules.formats.contains(&facts.format)),
            Some(json!(facts.format.as_str())),
        ),
        Check::LongestSide => {
            let long = facts.width.max(facts.height);
            let ok = rules.longest_side_min.is_none_or(|min| long >= min)
                && rules.longest_side_max.is_none_or(|max| long <= max);
            (flag(ok), Some(Value::from(long)))
        }
        Check::MaxPixels => {
            let pixels = u64::from(facts.width) * u64::from(facts.height);
            let ok = rules.max_pixels.is_none_or(|max| pixels <= max);
            (flag(ok), Some(Value::from(pixels)))
        }
        Check::Square => (
            flag(facts.width == facts.height),
            Some(Value::Bool(facts.width == facts.height)),
        ),
        Check::FileSize => {
            let ok = rules.max_bytes.is_none_or(|max| facts.file_size <= max);
            (flag(ok), Some(Value::from(facts.file_size)))
        }
        Check::Alpha => (flag(!facts.has_alpha), Some(Value::Bool(facts.has_alpha))),
        Check::ColorSpace => (facts.naming.status, facts.naming.actual.clone()),
        Check::Background => {
            // `regulated` が `Some` を確かめてからここへ来る
            let (want, seen) = match (rules.background, facts.pixels.as_ref()) {
                (Some(want), Some(pixels)) => (want, pixels.background),
                _ => return unregulated(check, expected),
            };
            let delta_e = delta_e_rgb(seen, want);
            (
                flag(delta_e <= BACKGROUND_DELTA_E_TOLERANCE),
                // **測った色と色差の両方を返す。** 色だけでは規格からどれだけ
                // 外れたのかが測れず、色差だけではどちらへ外れたのかが分からない
                Some(json!({ "rgb": seen, "delta_e": round4(delta_e) })),
            )
        }
        Check::FillRatio => {
            let min = match rules.fill_ratio_min {
                Some(min) => min,
                None => return unregulated(check, expected),
            };
            match facts.pixels.as_ref().and_then(|p| p.fill.as_ref()) {
                Some(fill) => (
                    flag(fill.ratio >= min),
                    Some(json!({
                        "value": round4(fill.ratio),
                        "source": fill.source,
                        "bbox": fill.bbox,
                    })),
                ),
                // **主体を 1 つも検出できない。** 背景しか写っていないか、
                // 背景と見分けが付かないかのどちらかで、`skipped`（この形式では
                // 構造的に測れない）とは別の事実である
                None => (UNMEASURABLE, None),
            }
        }
    };

    LintCheck {
        name: check.as_str(),
        status,
        expected,
        actual,
    }
}

/// `regulated` が真と言った条件の値が取れなかったときの保険。
///
/// **到達しない。** `regulated` と `judge` は同じ `Option` を見ているので、
/// 片方が `Some` と言ってもう片方が `None` を見ることはない。`unwrap` に
/// しないのは、条件を足したときに**対応を書き忘れた側が panic ではなく
/// `unmeasurable` として表に出る**ほうが、検査という用途に合うためである。
fn unregulated(check: Check, expected: Option<Value>) -> LintCheck {
    LintCheck {
        name: check.as_str(),
        status: UNMEASURABLE,
        expected,
        actual: None,
    }
}

const fn flag(ok: bool) -> &'static str {
    if ok { PASS } else { FAIL }
}

/// AVIF の CICP から sRGB の名乗りを判定する。
///
/// **「未指定」と「別の色空間」を分ける。** CICP の 2 は unspecified で、
/// 「読めたが名乗っていない」という事実である（`ColorNaming` の doc が
/// `Unknown` と潰さない理由を述べている）。
///
/// # 未指定を `fail` ではなく `unmeasurable` にした理由
///
/// `AvifMeta` は**埋め込み ICC の有無を持たない**（`color_naming` の doc が
/// そう宣言している）。CICP が未指定でも ICC で sRGB を名乗っている AVIF は
/// ありうるので、`fail` と断じると **実際には sRGB を名乗っているファイルを
/// 規格違反として落とす**ことになる。落とすほうへ外すのは、合否に納品を
/// 止める重みがある以上いちばん高くつく誤りである。
/// `unmeasurable` は合格ではない（`passed` は落ちる）ので黙って通すことには
/// ならず、しかも「kiri に見えていない」ことを名前で正しく言う。
///
/// ICC の名乗りまで見たくなったときに足すのは `AvifMeta` のフィールドで、
/// その判断が変わればここは `fail` を返せるようになる。
fn avif_naming(color: ColorNaming) -> Naming {
    let (source, primaries, transfer, matrix, full_range) = match color {
        ColorNaming::Colr {
            primaries,
            transfer,
            matrix,
            full_range,
        } => ("colr", primaries, transfer, matrix, full_range),
        ColorNaming::SequenceHeader {
            primaries,
            transfer,
            matrix,
            full_range,
        } => (
            "sequence_header",
            u16::from(primaries),
            u16::from(transfer),
            u16::from(matrix),
            full_range,
        ),
        // `colr` も AV1 のシーケンスヘッダも読めなかった。名乗りが無いのか
        // 読み損ねたのかすら分からないので、値を 1 つも作らない
        ColorNaming::Unknown => {
            return Naming {
                status: UNMEASURABLE,
                actual: None,
            };
        }
    };

    let named = json!({
        "source": source,
        "primaries": primaries,
        "transfer": transfer,
        "matrix": matrix,
        "full_range": full_range,
    });
    // **完全一致だけを合格にする。** 原色と伝達関数が sRGB でもレンジが
    // studio swing なら、復号に使われる値が違うので sRGB を名乗っては
    // いないことになる
    let srgb = primaries == SRGB_PRIMARIES
        && transfer == SRGB_TRANSFER
        && matrix == SRGB_MATRIX
        && full_range;
    let status = if srgb {
        PASS
    } else if [primaries, transfer, matrix].contains(&CICP_UNSPECIFIED) {
        UNMEASURABLE
    } else {
        FAIL
    };
    Naming {
        status,
        actual: Some(named),
    }
}

/// アルファが 0 でない画素の外接矩形 [x1, y1, x2, y2]（両端を含む）。
///
/// **完全に透明な画像では `None`。** 0 を返すと「1 画素だけの商品」と
/// 見分けが付かない。
fn alpha_bbox(image: &image::RgbaImage) -> Option<[u32; 4]> {
    let (mut x1, mut y1) = (u32::MAX, u32::MAX);
    let (mut x2, mut y2) = (0u32, 0u32);
    let mut found = false;
    for (x, y, pixel) in image.enumerate_pixels() {
        if pixel[3] == 0 {
            continue;
        }
        found = true;
        x1 = x1.min(x);
        y1 = y1.min(y);
        x2 = x2.max(x);
        y2 = y2.max(y);
    }
    found.then_some([x1, y1, x2, y2])
}

/// 占有率を**書く側とまったく同じ測り方で**出す。
///
/// `canvas::plan` は「内容が (W×r, H×r) の枠に収まる」ところまで縮めるので、
/// 書き出された画像で枠に触れているのは縦横のうち大きいほうである。
/// したがって測る側は `max(bbox_w / W, bbox_h / H)` になる。
///
/// **面積比（bbox の画素数 ÷ 画像の画素数）にしてはいけない。** 正方形の
/// キャンバスに正方形の商品を占有率 0.86 で置いた画像は、面積比では 0.74 と
/// 出る。書いた側と測る側で定義が違うと、**profile で書いたものが同じ
/// profile の lint で落ちる**——`FILL_RATIO_MARGIN` が防いでいるのと同じ
/// 失敗を、余裕をいくら積んでも防げない形で作ることになる。
fn fill_ratio(bbox: [u32; 4], width: u32, height: u32) -> f64 {
    // 両端を含む矩形なので、幅は差に 1 を足したもの
    let w = f64::from(bbox[2].saturating_sub(bbox[0]) + 1);
    let h = f64::from(bbox[3].saturating_sub(bbox[1]) + 1);
    (w / f64::from(width.max(1))).max(h / f64::from(height.max(1)))
}

/// 検査できなかった項目を報せる。**黙って合格にはしていない**ことを言う。
///
/// 1 件にまとめるのは、飛ばした理由が項目ごとに違わないためである
/// （`PROFILE_OVERRIDDEN` を項目ごとに出すのとはそこが違う——あちらは
/// どの指定が押しのけたかが項目ごとに違う）。**どれを飛ばしたかは
/// `data` に配列で必ず入れる**——文面から項目名を抜き直させない。
fn uncheckable(skipped: &[&str], format: OutputFormat) -> Option<Warning> {
    if skipped.is_empty() {
        return None;
    }
    Some(
        Warning::new(
            WarningCode::ProfileUncheckable,
            format!(
                "{} は画素を読まないと測れないため検査していません（kiri は {} を\
                 デコードできないので、コンテナから読める事実だけで判定しました）",
                skipped.join(" / "),
                format.as_str()
            ),
        )
        .with_hint(
            "飛ばした項目も合格ではないので passed は false です。構図まで見るなら \
             JPEG か PNG を渡してください",
        )
        .with_data("checks", json!(skipped))
        .with_data("format", format.as_str()),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::profile;

    fn facts() -> Facts {
        Facts {
            width: 1600,
            height: 1600,
            file_size: 500_000,
            format: OutputFormat::Jpeg,
            has_alpha: false,
            naming: Naming {
                status: PASS,
                actual: Some(json!({ "color_space": COLOR_SPACE_SRGB })),
            },
            pixels: Some(Pixels {
                background: [255, 255, 255],
                fill: Some(Fill {
                    ratio: 0.86,
                    source: SUBJECT_FROM_COLOUR,
                    bbox: [112, 112, 1487, 1487],
                }),
            }),
        }
    }

    fn run_checks(facts: &Facts, name: &str) -> Vec<LintCheck> {
        let rules = &profile::named(name).unwrap().rules;
        Check::ALL
            .into_iter()
            .filter(|c| c.regulated(rules))
            .map(|c| judge(c, rules, facts))
            .collect()
    }

    fn status_of(checks: &[LintCheck], name: &str) -> &'static str {
        checks
            .iter()
            .find(|c| c.name == name)
            .unwrap_or_else(|| panic!("{name} が checks に無い"))
            .status
    }

    /// 並びは `Check::ALL` の順で決まる。
    ///
    /// 同じ入力を 2 回走らせて並びが変わると、差分で結果を見張れない。
    #[test]
    fn the_checks_are_ordered_by_the_table() {
        let names: Vec<&str> = run_checks(&facts(), "amazon")
            .iter()
            .map(|c| c.name)
            .collect();
        let want: Vec<&str> = Check::ALL
            .into_iter()
            .filter(|c| c.regulated(&profile::named("amazon").unwrap().rules))
            .map(Check::as_str)
            .collect();
        assert_eq!(names, want);
        assert!(names.windows(2).all(|w| w[0] != w[1]), "name は一意である");
    }

    /// 規定の無い条件は `checks[]` に 1 行も出さない。
    ///
    /// `pass` として並べると、shopify が構図を見ていないことが結果から読めなくなる。
    #[test]
    fn a_rule_that_says_nothing_produces_no_check() {
        let names: Vec<&str> = run_checks(&facts(), "shopify")
            .iter()
            .map(|c| c.name)
            .collect();
        assert!(!names.contains(&"background"), "{names:?}");
        assert!(!names.contains(&"fill_ratio"), "{names:?}");
        assert!(!names.contains(&"alpha"), "透過を許す規格で検査している");
        assert!(!names.contains(&"square"), "正方形の規定が無い");
        assert!(names.contains(&"max_pixels"));
    }

    /// 素直に規格を満たした画像は全項目 `pass`。
    #[test]
    fn a_conforming_image_passes_every_check() {
        let checks = run_checks(&facts(), "amazon");
        assert!(
            checks.iter().all(|c| c.status == PASS),
            "{:?}",
            checks
                .iter()
                .map(|c| (c.name, c.status))
                .collect::<Vec<_>>()
        );
    }

    /// **背景に完全一致を求めない。** JPEG の量子化で純白は 255 のまま揃わない。
    #[test]
    fn a_near_white_background_is_still_white() {
        let mut f = facts();
        f.pixels.as_mut().unwrap().background = [254, 254, 253];
        assert_eq!(status_of(&run_checks(&f, "amazon"), "background"), PASS);

        // 目に見えて灰色なら落ちる
        f.pixels.as_mut().unwrap().background = [245, 245, 245];
        let checks = run_checks(&f, "amazon");
        assert_eq!(status_of(&checks, "background"), FAIL);
        // 測った色と色差の両方を返す
        let actual = checks
            .iter()
            .find(|c| c.name == "background")
            .unwrap()
            .actual
            .clone()
            .unwrap();
        assert_eq!(actual["rgb"], json!([245, 245, 245]));
        assert!(actual["delta_e"].as_f64().unwrap() > BACKGROUND_DELTA_E_TOLERANCE);
    }

    /// 占有率は**どう測ったかを必ず名乗る。**
    #[test]
    fn the_fill_ratio_names_where_the_box_came_from() {
        let checks = run_checks(&facts(), "amazon");
        let actual = checks
            .iter()
            .find(|c| c.name == "fill_ratio")
            .unwrap()
            .actual
            .clone()
            .unwrap();
        assert_eq!(actual["source"], json!(SUBJECT_FROM_COLOUR));
        assert_eq!(actual["bbox"], json!([112, 112, 1487, 1487]));
    }

    /// 主体を検出できなければ `unmeasurable`。**`skipped` とは別の名前である。**
    #[test]
    fn a_subject_that_cannot_be_found_is_unmeasurable_not_skipped() {
        let mut f = facts();
        f.pixels.as_mut().unwrap().fill = None;
        let checks = run_checks(&f, "amazon");
        assert_eq!(status_of(&checks, "fill_ratio"), UNMEASURABLE);
        assert!(!checks.iter().all(|c| c.status == PASS), "合格にしない");
    }

    /// AVIF では画素の検査が `skipped` になり、**黙って合格にしない。**
    #[test]
    fn an_avif_skips_the_pixel_checks_without_passing_them() {
        let mut f = facts();
        f.format = OutputFormat::Avif;
        f.pixels = None;
        let checks = run_checks(&f, "amazon");
        assert_eq!(status_of(&checks, "background"), SKIPPED);
        assert_eq!(status_of(&checks, "fill_ratio"), SKIPPED);
        // 形式そのものは検査できる。amazon は AVIF を許していない
        assert_eq!(status_of(&checks, "format"), FAIL);
        // **求められていた値は飛ばしても返す**——検査できなかったことと、
        // 何を求められていたかは別の事実である
        assert!(
            checks
                .iter()
                .find(|c| c.name == "background")
                .unwrap()
                .expected
                .is_some()
        );

        let skipped: Vec<&str> = checks
            .iter()
            .filter(|c| c.status == SKIPPED)
            .map(|c| c.name)
            .collect();
        let warning = uncheckable(&skipped, OutputFormat::Avif).expect("警告が出る");
        assert_eq!(warning.code, WarningCode::ProfileUncheckable);
        // どれを飛ばしたかは必ず配列で入る
        assert_eq!(warning.data["checks"], json!(["background", "fill_ratio"]));
    }

    /// 飛ばした項目が 1 つも無ければ警告も出ない。
    #[test]
    fn nothing_skipped_means_no_warning() {
        assert!(uncheckable(&[], OutputFormat::Jpeg).is_none());
    }

    /// 占有率は**書く側と同じ測り方**である（面積比ではない）。
    #[test]
    fn the_fill_ratio_matches_what_the_canvas_planner_targets() {
        let spec = crate::transform::canvas::CanvasSpec {
            width: 1000,
            height: 1000,
            fill_ratio: 0.85,
            background: Some([255, 255, 255]),
        };
        let plan = crate::transform::canvas::plan((400, 300), &spec).unwrap();
        // 書いた内容の外接矩形（両端を含む座標へ直す）
        let bbox = [0, 0, plan.content.0 - 1, plan.content.1 - 1];
        let measured = fill_ratio(bbox, spec.width, spec.height);
        assert!(
            (measured - spec.fill_ratio).abs() < 0.01,
            "書いた占有率 {} と測った占有率 {measured} が食い違う",
            spec.fill_ratio
        );
    }

    /// sRGB の CICP を名乗る AVIF は通る。
    #[test]
    fn an_avif_that_names_srgb_passes() {
        let naming = avif_naming(ColorNaming::Colr {
            primaries: SRGB_PRIMARIES,
            transfer: SRGB_TRANSFER,
            matrix: SRGB_MATRIX,
            full_range: true,
        });
        assert_eq!(naming.status, PASS);
        assert_eq!(naming.actual.unwrap()["source"], json!("colr"));
    }

    /// **未指定（2）は `fail` ではない。**
    ///
    /// `AvifMeta` は埋め込み ICC の有無を持たないので、「名乗っていない」と
    /// 断じると ICC で sRGB を名乗っているファイルまで落とす。
    #[test]
    fn an_unspecified_cicp_is_unmeasurable_not_a_violation() {
        let naming = avif_naming(ColorNaming::SequenceHeader {
            primaries: 2,
            transfer: 2,
            matrix: 2,
            full_range: true,
        });
        assert_eq!(naming.status, UNMEASURABLE);
        // 読めた事実は返す。「読めなかった」とは別である
        assert_eq!(naming.actual.unwrap()["primaries"], json!(2));

        // 読めなかったときは値を 1 つも作らない
        let unknown = avif_naming(ColorNaming::Unknown);
        assert_eq!(unknown.status, UNMEASURABLE);
        assert!(unknown.actual.is_none());
    }

    /// 別の色域をはっきり名乗っていれば `fail`。
    #[test]
    fn a_cicp_that_names_another_gamut_fails() {
        // BT.2020
        let naming = avif_naming(ColorNaming::Colr {
            primaries: 9,
            transfer: 16,
            matrix: 9,
            full_range: true,
        });
        assert_eq!(naming.status, FAIL);

        // 原色と伝達関数が sRGB でも、レンジが studio swing なら名乗りが違う
        let limited = avif_naming(ColorNaming::Colr {
            primaries: SRGB_PRIMARIES,
            transfer: SRGB_TRANSFER,
            matrix: SRGB_MATRIX,
            full_range: false,
        });
        assert_eq!(limited.status, FAIL);
    }

    /// 完全に透明な画像では外接矩形を作らない。
    #[test]
    fn a_fully_transparent_image_has_no_alpha_box() {
        let clear = image::RgbaImage::new(4, 4);
        assert!(alpha_bbox(&clear).is_none());

        let mut one = image::RgbaImage::new(4, 4);
        one.put_pixel(2, 3, image::Rgba([0, 0, 0, 1]));
        assert_eq!(alpha_bbox(&one), Some([2, 3, 2, 3]));
    }

    /// **`write_defaults` で書いたものが同じ profile の lint で落ちない。**
    ///
    /// これが Phase 22 でいちばん高くつく失敗である。`profile.rs` の
    /// `the_write_defaults_never_contradict_the_rules` は表と設定の整合を
    /// 見るが、**lint の判定を通していない**——占有率の測り方や背景の許容が
    /// ずれていれば、そちらは通ったままここが落ちる。
    #[test]
    fn what_a_profile_writes_passes_the_same_profiles_lint() {
        for p in profile::ALL {
            let w = p.write_defaults();
            let (cw, ch) = w.canvas.unwrap();
            let ratio = w.fill_ratio.unwrap_or(0.9);
            // 占有率どおりに置かれた外接矩形
            let side = (f64::from(cw) * ratio).round() as u32;
            let margin = (cw - side) / 2;
            let f = Facts {
                width: cw,
                height: ch,
                file_size: 1_000_000,
                format: w.format.unwrap_or(OutputFormat::Png),
                // 潰す規格では不透明になり、許す規格では検査されない
                has_alpha: false,
                naming: Naming {
                    status: PASS,
                    actual: None,
                },
                pixels: Some(Pixels {
                    // 書いた背景色そのまま。色差 0 で通る
                    background: w.background.unwrap_or([255, 255, 255]),
                    fill: Some(Fill {
                        ratio: fill_ratio(
                            [margin, margin, margin + side - 1, margin + side - 1],
                            cw,
                            ch,
                        ),
                        source: FILL_FROM_ALPHA,
                        bbox: [margin, margin, margin + side - 1, margin + side - 1],
                    }),
                }),
            };
            let checks = run_checks(&f, p.name);
            let bad: Vec<(&str, &str)> = checks
                .iter()
                .filter(|c| c.status != PASS)
                .map(|c| (c.name, c.status))
                .collect();
            assert!(bad.is_empty(), "{}: {bad:?}", p.name);
        }
    }
}
