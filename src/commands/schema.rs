//! `kiri schema` — 契約そのものを返す。
//!
//! README は 1000 行ある。**エージェントの文脈に丸ごと載せられる長さではない**し、
//! 載せたところでオプションの綴りと既定値は散文の中に埋まっている。必要なのは
//! 「どう呼ぶか」と「返ってきた code が何を意味するか」だけなので、それを
//! 1 コマンドで配る。
//!
//! **中身は実装から組み立てる。** オプションは clap のパーサそのものから、
//! code は `warning.rs` / `error.rs` のカタログから取る。手で書いた一覧は必ず
//! 実装から離れ、離れた一覧は「指定したのに効かない」という最も追いにくい
//! 失敗をそのまま招く。ここに書き写す余地を残さないことが要点である。

use std::sync::LazyLock;

use clap::{ArgAction, CommandFactory};

use crate::cli::Cli;
use crate::commands::lint::{CHECK_STATUSES, Check};
use crate::cutout::{MAX_FOREGROUND_RATIO, MIN_FOREGROUND_RATIO, background, diagnostics, subject};
use crate::error::{ErrorCode, ErrorKind};
use crate::profile;
use crate::report::{
    ArgEntry, CommandEntry, ErrorCodeEntry, ExitCodeEntry, FieldEntry, FieldGate, FieldThreshold,
    LintCheckEntry, ProfileEntry, ProfileRules, SCHEMA_VERSION, SchemaReport, WarningCodeEntry,
};
use crate::warning::WarningCode;

pub fn run() -> SchemaReport {
    SchemaReport {
        schema_version: SCHEMA_VERSION,
        kiri_version: env!("CARGO_PKG_VERSION"),
        // `kiri model list` と同じ 1 つの定数を見る。2 箇所で別々に
        // `cfg!` を書くと、片方だけが feature の綴りを取りこぼしても
        // コンパイルは通る
        segment_available: crate::commands::model::AVAILABLE,
        exit_codes: exit_codes(),
        errors: ErrorCode::ALL
            .iter()
            .map(|&code| ErrorCodeEntry {
                code,
                exit_code: code.kind().exit_code(),
                summary: code.summary(),
            })
            .collect(),
        warnings: WarningCode::ALL
            .iter()
            .map(|&code| WarningCodeEntry {
                code,
                summary: code.summary(),
            })
            .collect(),
        fields: fields(),
        lint_checks: lint_checks(),
        profiles: profiles(),
        global_options: global_options(),
        commands: commands(),
    }
}

/// トップレベルの引数。`--json` がここにいる。
fn global_options() -> Vec<ArgEntry> {
    Cli::command()
        .get_arguments()
        .filter(|arg| !is_clap_builtin(arg))
        .map(arg_entry)
        .collect()
}

/// `--help` / `--version` は clap が全コマンドへ足すもので、契約として語る
/// 価値が無いうえに毎コマンド 2 行を占める。
fn is_clap_builtin(arg: &clap::Arg) -> bool {
    matches!(arg.get_id().as_str(), "help" | "version")
}

/// 0 は `ErrorKind` に無い。成功はエラーの分類ではないためである。
fn exit_codes() -> Vec<ExitCodeEntry> {
    let mut codes = vec![ExitCodeEntry {
        code: 0,
        meaning: "成功",
    }];
    codes.extend(ErrorKind::ALL.iter().map(|&kind| ExitCodeEntry {
        code: kind.exit_code(),
        meaning: kind.meaning(),
    }));
    codes
}

/// `kiri lint` が見る条件の一覧を配る。
///
/// **`Check::ALL` から組む。書き写さない**（`profiles` / `exit_codes` と同じ
/// 作法）。ここへ手で一覧を書くと、`Check` を 1 つ足したときに **lint が実際に
/// 並べる項目と、schema が「これが出る」と配った一覧が食い違う**——受け手は
/// 知らない `name` を受け取るか、来ない `name` を待つことになる。
///
/// `regulated`（どの規格がこれを規定するか）は載せない。それは条件の側では
/// なく規格の側の事実で、`profiles[].rules` が既に機械可読で配っている。
fn lint_checks() -> Vec<LintCheckEntry> {
    Check::ALL
        .into_iter()
        .map(|c| LintCheckEntry {
            name: c.as_str(),
            needs_pixels: c.needs_pixels(),
        })
        .collect()
}

/// `checks[].name` の綴りを定数から並べたもの。
///
/// **散文の中に一覧を打ち直さない。** `LazyLock` を通すのは `FieldEntry` の
/// 散文が `&'static str` だからで、静的に生きる `String` を借りれば同じ
/// 寿命になる——語を足せば散文のほうが自動で追随する。
static LINT_CHECK_NAMES: LazyLock<String> = LazyLock::new(|| Check::NAMES.join(" / "));

/// `checks[]` の読み方。名前の一覧だけを定数から差し込む。
static LINT_CHECKS_NOTE: LazyLock<String> = LazyLock::new(|| {
    format!(
        "**規格が規定している条件だけが並ぶ。** 規定の無い条件は 1 行も出ない\
                 （shopify は構図を規定しないので background も fill_ratio も現れない）\
                 ——「規定なし」を pass として並べると、見ていないものを見たことにする。\
                 **見たものは全部載せる**（pass も）。name は {} で、**checks[] の中で\
                 一意である**（compliance.checks[] とはそこが違う）。綴りと、画素を要するか\
                 どうかの別は lint_checks[] が機械可読で配る。**並びは決定的**で、\
                 指定にも入力にも依らない。\
                 expected / actual は条件ごとに形が違う（数値・真偽・許容する形式の配列・\
                 上下限の組・測定値と出所の組）——**文字列へ畳まないので、数値は数値の\
                 まま読める**。expected は status が skipped でも出る（検査できなかった\
                 ことと、何を求められていたかは別の事実である）。actual は skipped では\
                 必ず null になるが、**unmeasurable でも値が入ることがある**——AVIF の \
                 color_space は「CICP は読めたが unspecified と書いてあった」を取り、\
                 background は「色も色差も測れたが、外周が揃っていないので\
                 『この画像の背景色』とは呼べない」を取るので、合否は actual の\
                 有無ではなく status で読むこと。\
                 background の actual は測った rgb と規格の色からの色差 delta_e に加えて、\
                 **測定に使った外周の帯幅 border_px** を返す——同じ delta_e 0.0 でも、\
                 2px を見た 0.0 と 48px を見た 0.0 は別のことを言っている。\
                 **完全一致は求めない**（JPEG の量子化で 255 は揃わない）。\
                 color_space の actual は color_named を返す——**偽なら ICC も EXIF の\
                 申告も無く、ファイルは何も名乗っていない**（このとき status は\
                 unmeasurable で、AVIF の CICP unspecified と同じ扱いである）。\
                 fill_ratio の actual は value と source を返す——source は \
                 alpha（透過の外接矩形。切り抜き済みの成果物ではこれが正しい）か \
                 colour（色で見立てた主体）で、**同じ 0.80 でも意味が違う**",
        *LINT_CHECK_NAMES
    )
});

/// `checks[].status` の語彙。4 語は `CHECK_STATUSES` が唯一の定義である。
static LINT_STATUS_SUMMARY: LazyLock<String> =
    LazyLock::new(|| format!("{} のいずれか", CHECK_STATUSES.join(" / ")));

/// 規格の表を配る。
///
/// **`profile::ALL` から組む。書き写さない**（`exit_codes` / `fields` と同じ作法）。
/// ここへ手で表を書くと、`--profile` が使う表と `kiri lint` が使う表と `kiri schema`
/// が配る表の 3 つが並ぶことになり、**不合格の根拠として配った条件が、実際に
/// 効いた条件と違う**という一番高くつく食い違いを作れてしまう。
///
/// `write_defaults`（profile が書く側で何を決めるか）は載せない。あれは規格
/// そのものではなく kiri の判断で、同じ表に並べると規定と好みが見分けられなく
/// なる——`Rules` に推奨を入れないのとまったく同じ線引きである。
fn profiles() -> Vec<ProfileEntry> {
    profile::ALL
        .iter()
        .map(|p| ProfileEntry {
            name: p.name,
            revision: p.revision,
            summary: p.summary,
            source: p.source,
            rules: ProfileRules {
                longest_side_min: p.rules.longest_side_min,
                longest_side_max: p.rules.longest_side_max,
                max_pixels: p.rules.max_pixels,
                square: p.rules.square,
                background: p.rules.background,
                fill_ratio_min: p.rules.fill_ratio_min,
                max_bytes: p.rules.max_bytes,
                formats: p.rules.formats.iter().map(|f| f.as_str()).collect(),
                alpha_allowed: p.rules.alpha_allowed,
                srgb_required: p.rules.srgb_required,
            },
        })
        .collect()
}

/// 実際に呼べるコマンドだけを並べる。
///
/// **入れ子のサブコマンドを持つものは、それ自身では呼べない。** `kiri model`
/// は `list` を要求するので、ここに `model` という行を出すと「引数を取らない
/// コマンドがある」と読めてしまう。葉だけを `"model list"` の形で並べる——
/// 綴りをそのまま繋げば実行できる並びである。
fn commands() -> Vec<CommandEntry> {
    let mut out = Vec::new();
    for sub in Cli::command().get_subcommands() {
        collect_command(sub, "", &mut out);
    }
    out
}

fn collect_command(command: &clap::Command, prefix: &str, out: &mut Vec<CommandEntry>) {
    let name = if prefix.is_empty() {
        command.get_name().to_string()
    } else {
        format!("{prefix} {}", command.get_name())
    };
    let mut children = command.get_subcommands().peekable();
    if children.peek().is_some() {
        for child in command.get_subcommands() {
            collect_command(child, &name, out);
        }
        return;
    }

    let (arguments, options): (Vec<_>, Vec<_>) = command
        .get_arguments()
        .filter(|arg| !is_clap_builtin(arg))
        .map(arg_entry)
        .partition(|entry| !entry.name.starts_with("--"));

    out.push(CommandEntry {
        name,
        about: command.get_about().map(|a| a.to_string()),
        arguments,
        options,
    });
}

fn arg_entry(arg: &clap::Arg) -> ArgEntry {
    // 値を取るのは Set / Append だけ。**フラグでも clap は value_name を
    // 自動で用意する**ので、num_args や value_name から判定すると
    // `--dry-run` のような真偽のフラグが「値を取る」と名乗ってしまう
    let takes_value = matches!(arg.get_action(), ArgAction::Set | ArgAction::Append);
    let defaults = arg.get_default_values();

    ArgEntry {
        name: match arg.get_long() {
            Some(long) => format!("--{long}"),
            None => arg.get_id().to_string(),
        },
        short: arg.get_short().map(|c| format!("-{c}")),
        required: arg.is_required_set(),
        takes_value,
        value_name: takes_value
            .then(|| arg.get_value_names())
            .flatten()
            .and_then(|names| names.first())
            .map(|name| name.to_string()),
        // 既定値を名乗らない項目がある。`--edge-threshold` は「未指定」と
        // 「8 を明示」を区別するため、clap の既定値を持たない。ここに 8 が
        // 現れると「指定しなくても 8 が効く」という誤った前提をそのまま渡す
        default: (!defaults.is_empty()).then(|| {
            defaults
                .iter()
                .map(|v| v.to_string_lossy().into_owned())
                .collect::<Vec<_>>()
                .join(",")
        }),
        // 値を取らない項目に選択肢は無い。clap は bool のフラグにも
        // true / false を possible_values として持つが、`--flatten true` と
        // 書けるわけではないので、そのまま配ると受け付けない書き方を勧めてしまう
        accepts: takes_value
            .then(|| {
                let values: Vec<String> = arg
                    .get_possible_values()
                    .iter()
                    .map(|v| v.get_name().to_string())
                    .collect();
                (!values.is_empty()).then_some(values)
            })
            .flatten(),
        repeatable: matches!(arg.get_action(), ArgAction::Append),
        global: arg.is_global_set(),
        summary: arg.get_help().map(|h| h.to_string()).unwrap_or_default(),
        // 長いヘルプは「指定の前に知っていないと選びようがないこと」が
        // 置かれている場所なので、短い方で上書きせず両方返す
        detail: arg.get_long_help().map(|h| h.to_string()),
    }
}

/// 結果の値の読み方。
///
/// **警告が出ていない値をどう読むか**を配るための表である。しきい値は実装の
/// 定数をそのまま置く——`0.10` と書き写した瞬間に、較正で定数を動かしたときに
/// 嘘になる。
fn fields() -> Vec<FieldEntry> {
    let both = || vec!["info", "cutout"];
    vec![
        FieldEntry {
            path: "background.uniformity",
            appears_in: both(),
            unit: "ratio",
            nullable: false,
            null_means: None,
            warns: vec![FieldThreshold {
                code: WarningCode::LowUniformity,
                operator: "lt",
                threshold: background::MIN_UNIFORMITY,
            }],
            gates: None,
            summary: "外周サンプルのうち、推定背景色から ΔE≤5 に収まる割合",
            notes: Some(
                "下回れば単色背景ではない。bbox で救えるかどうかは subject.confidence が言う",
            ),
        },
        FieldEntry {
            path: "background.perimeter_delta_e.p50",
            appears_in: both(),
            unit: "delta_e",
            nullable: false,
            null_means: None,
            warns: vec![],
            gates: None,
            summary: "外周が推定背景色からどれだけ離れているかの中位値",
            notes: Some(
                "背景を 1 色で表したときのばらつき。**主体検出のしきい値はここから導く**——\
                 info の NOT_SEPARABLE は subject.delta_e をこれと比べる。切り抜き後の \
                 mask.separability が比べる相手は background.residual.p50 のほうである\
                 （model が flat なら両者は同じ値になる）",
            ),
        },
        FieldEntry {
            path: "background.model",
            appears_in: both(),
            unit: "enum",
            nullable: false,
            null_means: None,
            warns: vec![],
            gates: None,
            summary: "実際に効いた背景のモデル（flat / field）",
            notes: Some(
                "--background-model の既定は auto で、background.uniformity が下限を切ったときだけ \
                 field になる。auto の cutout が field を選んだときだけ BACKGROUND_FIELD_USED が \
                 1 行出る（--background-model で明示したときと info では出ない）——**直すものが\
                 あるという意味ではない**（既定で正しく動いた報告で、hint も付かない）。\
                 field を求めても、外周の帯のうち背景として使えた割合が下限を切れば flat へ落ち、\
                 そのときは BACKGROUND_FIELD_SKIPPED が出る（info でも出る）。\
                 info が返す値は「この画像なら cutout がどちらを使うか」の予告である",
            ),
        },
        FieldEntry {
            path: "background.field_range",
            appears_in: both(),
            unit: "delta_e",
            nullable: false,
            null_means: None,
            warns: vec![],
            gates: None,
            summary: "照明場が大域の 1 色からどれだけ離れているかの [最小, 最大] ΔE",
            notes: Some(
                "**場が何を吸ったか**を 1 行で言う。model が flat なら [0, 0]。大きいほど\
                 「単色では表せない照明が乗っていた」ことを意味するが、それ自体は欠陥ではない\
                 ——吸えていれば residual が小さくなる",
            ),
        },
        FieldEntry {
            path: "background.residual.p50",
            appears_in: both(),
            unit: "delta_e",
            nullable: false,
            null_means: None,
            warns: vec![],
            gates: None,
            summary: "外周が**場**からどれだけ離れているかの中位値",
            notes: Some(
                "perimeter_delta_e.p50（1 色に対する分布）との差がそのまま「場が吸った量」である。\
                 residual が小さいのに perimeter_delta_e が大きい画像は、単色でないのではなく\
                 **単色に照明が乗っている**ので、照明場モデルで救える。芯の許容量と \
                 NOT_SEPARABLE の判定はこちらから導く（model が flat なら定義から \
                 perimeter_delta_e と同じ値になる）。**主体検出だけは 1 色に対する分布から導く**\
                 ——--background-model を変えても subject の判定は動かない",
            ),
        },
        FieldEntry {
            path: "background.texture.p50",
            appears_in: both(),
            unit: "gradient",
            nullable: false,
            null_means: None,
            warns: vec![],
            gates: None,
            summary: "外周の帯で測った 1px あたりの輝度変化量の中位値",
            notes: Some(
                "settings.edge_threshold に達していれば、その背景は輪郭の堤防を張れない素材で、\
                 cutout はしきい値を自動で引き上げる（EDGE_THRESHOLD_RAISED）",
            ),
        },
        FieldEntry {
            path: "subject.area_ratio",
            appears_in: both(),
            unit: "ratio",
            nullable: false,
            null_means: None,
            warns: vec![],
            gates: Some(FieldGate {
                confidence: "high",
                operator: "gte",
                threshold: subject::MIN_AREA_RATIO,
            }),
            summary: "背景色から遠い画素の最大の塊が、画像に占める割合",
            notes: None,
        },
        FieldEntry {
            path: "subject.capture_ratio",
            appears_in: both(),
            unit: "ratio",
            nullable: false,
            null_means: None,
            warns: vec![],
            gates: Some(FieldGate {
                confidence: "high",
                operator: "gte",
                threshold: subject::MIN_CAPTURE_RATIO,
            }),
            summary: "しきい値を超えた画素のうち、最大の塊が占める割合",
            notes: Some("まとまっていれば商品、散っていれば背景の粗さである"),
        },
        FieldEntry {
            path: "subject.leftover_ratio",
            appears_in: both(),
            unit: "ratio",
            nullable: false,
            null_means: None,
            warns: vec![],
            gates: Some(FieldGate {
                confidence: "high",
                operator: "lt",
                threshold: subject::MAX_LEFTOVER_RATIO,
            }),
            summary: "提案した矩形の外に残った、背景とは言えない画素の最大の塊の割合",
            notes: Some("大きければ、その矩形は主体を取りこぼしている"),
        },
        FieldEntry {
            path: "subject.confidence",
            appears_in: both(),
            unit: "enum",
            nullable: false,
            null_means: None,
            warns: vec![],
            gates: None,
            summary: "主体の推定をどれだけ信用できるか（high / low）",
            notes: Some(
                "high のときだけ subject.normalized_bbox を根拠に動いてよい。low で bbox を\
                 渡すと、商品ですらない領域へ切り抜きを誘導する",
            ),
        },
        FieldEntry {
            path: "subject.normalized_bbox",
            appears_in: both(),
            unit: "normalized_bbox",
            nullable: false,
            null_means: None,
            warns: vec![],
            gates: None,
            summary: "主体の外接矩形を 0.0-1.0 で表したもの [x1, y1, x2, y2]",
            notes: Some("--bbox <値> --normalized へそのまま渡せる並びになっている"),
        },
        FieldEntry {
            path: "subject.level_rotation",
            appears_in: both(),
            unit: "deg",
            nullable: true,
            null_means: Some(
                "どの角度でも最小外接矩形の面積が変わらない形（円など）で、傾きを測れなかった。\
                 0（傾いていない）ではない",
            ),
            warns: vec![],
            gates: None,
            summary: "主体の最小外接矩形が軸に揃う回転角(度、時計回りが正、範囲は (-45, 45])",
            notes: Some(
                "cutout --rotate <値> や kiri rotate --angle <値> へそのまま渡せる。傾きそのもの\
                 （符号を反転して渡す値）ではないので、符号を考え直さないこと。**自動では\
                 適用しない**——傾きを直すかどうかは bbox と同じく構図の判断である。\
                 subject.confidence が high のときだけ根拠にしてよいのも bbox と同じ。\
                 **分解能は 0.1〜0.5 度**（主体は縮小版で測る）。小数第 4 位まで出るが、\
                 0.5 度を下回る差を有意と読まないこと——0 になるまで回し直すループは組めない",
            ),
        },
        FieldEntry {
            path: "subject.border",
            appears_in: both(),
            unit: "px",
            nullable: false,
            null_means: None,
            warns: vec![],
            gates: None,
            summary: "主体の統計を測った外周の帯の幅",
            notes: Some(
                "既定では --border と同じ値。--border が短辺の 3% を超えたときだけ、ここが\
                 頭打ちになって食い違う。**--border は背景色の推定範囲を決める値であって、\
                 主体の較正のための値ではない**——帯を画像の半分まで広げると外周 ΔE の分布\
                 （主体を拾うしきい値そのもの）が商品自身に汚染され、救えない画像が high と\
                 名乗り始める。area_ratio / capture_ratio / leftover_ratio / delta_e と\
                 level_rotation はこの帯で測った値で、background.rgb（--border の帯で測る）\
                 とは別の背景色を基準にすることがある",
            ),
        },
        FieldEntry {
            path: "subject.source",
            appears_in: both(),
            unit: "enum",
            nullable: false,
            null_means: None,
            warns: vec![],
            gates: None,
            summary: "この矩形が何から出たか（colour / segment）",
            notes: Some(
                "既定は colour（背景色から遠い画素の最大の塊）。info --segment でモデルが走った\
                 ときだけ segment になる。**判定（area_ratio / capture_ratio / leftover_ratio と \
                 confidence）はどちらでも同じもの**を通るので、2 つの high は同じ意味を持つ。\
                 cutout の subject は --segment を渡しても colour のまま——あちらは\
                 「切り抜きとは別に、色で見たらどこが商品か」を言う参考値である",
            ),
        },
        FieldEntry {
            path: "segment.model",
            appears_in: both(),
            unit: "enum",
            nullable: false,
            null_means: None,
            warns: vec![],
            gates: None,
            summary: "実際に走ったモデルの名前（isnet）",
            notes: Some(
                "**モデルが走ったときだけ segment ブロックごと現れる。** 走ったかどうかは \
                 settings.segment_ran が真偽で言う（--segment auto は色で解けると判断すれば\
                 走らない）。この build に segment 機能が無ければ --segment off 以外は \
                 SEGMENT_UNAVAILABLE で断られる",
            ),
        },
        FieldEntry {
            path: "segment.input_size",
            appears_in: both(),
            unit: "px",
            nullable: false,
            null_means: None,
            warns: vec![],
            gates: None,
            summary: "モデルが受け取った正方形の一辺",
            notes: Some(
                "**利用者は選べない。** ISNet の ONNX は 1024 を graph に焼き込んでいるので、\
                 別の寸法を渡すとグラフの解析そのものが通らない。kiri model list の \
                 input_size が同じ値を返す",
            ),
        },
        FieldEntry {
            path: "segment.elapsed_ms",
            appears_in: both(),
            unit: "ms",
            nullable: false,
            null_means: None,
            warns: vec![],
            gates: None,
            summary: "モデルの読み込みから確率マップまでの時間",
            notes: Some(
                "結果全体の elapsed_ms の内数である。1024x1024 は M4 Pro で 1.3 秒前後\
                 （読み込み 0.1 秒 + 推論 1.2 秒）で、既定の切り抜きとは桁が違う",
            ),
        },
        FieldEntry {
            path: "segment.fg_ratio",
            appears_in: both(),
            unit: "ratio",
            nullable: false,
            null_means: None,
            warns: vec![],
            gates: None,
            summary: "モデルが確定前景として置いた画素の割合",
            notes: Some(
                "確率が 0.9 以上の領域を、長辺 1000px 換算で 8px 収縮したもの。**輪郭の\
                 位置はモデルが決めない**——確定領域のあいだの帯は今までどおり色と\
                 連結性とマッティングが決める。利用者の空間的な指示と重なれば\
                 指示のほうが勝つので、constraints.fg_ratio とは一致しないことがある",
            ),
        },
        FieldEntry {
            path: "segment.bg_ratio",
            appears_in: both(),
            unit: "ratio",
            nullable: false,
            null_means: None,
            warns: vec![],
            gates: None,
            summary: "モデルが確定背景として置いた画素の割合",
            notes: Some("確率が 0.1 以下の領域を同じだけ収縮したもの"),
        },
        FieldEntry {
            path: "segment.uncertain_ratio",
            appears_in: both(),
            unit: "ratio",
            nullable: false,
            null_means: None,
            warns: vec![FieldThreshold {
                code: WarningCode::SegmentUncertain,
                operator: "gt",
                threshold: crate::segment::SEG_UNCERTAIN_WARN,
            }],
            gates: None,
            summary: "モデルがどちらとも言わなかった帯の割合",
            notes: Some(
                "3 つの比率の和は 1 になる。大きいほどモデルが対象を掴めておらず、結果は \
                 --segment off に近づく。しきい値を超えたら、同じモデルを回し直すより \
                 --trimap で直接教えるほうが早い",
            ),
        },
        FieldEntry {
            path: "segment.model_path",
            appears_in: both(),
            unit: "path",
            nullable: false,
            null_means: None,
            warns: vec![],
            gates: None,
            summary: "実際に読んだ ONNX のパス",
            notes: Some(
                "--model-path を渡していなければ $KIRI_MODEL_DIR > $XDG_CACHE_HOME/kiri/models > \
                 OS 既定のキャッシュの順に探した結果である。置き場所と取得手順は \
                 kiri model list が返す",
            ),
        },
        FieldEntry {
            path: "settings.segment",
            appears_in: vec!["cutout"],
            unit: "enum",
            nullable: false,
            null_means: None,
            warns: vec![],
            gates: None,
            summary: "--segment の指定値（off / auto / isnet）",
            notes: Some("**実際に走ったかは settings.segment_ran のほう**を読むこと"),
        },
        FieldEntry {
            path: "settings.segment_ran",
            appears_in: vec!["cutout"],
            unit: "bool",
            nullable: false,
            null_means: None,
            warns: vec![],
            gates: None,
            summary: "実際にモデルが走ったか",
            notes: Some(
                "auto では指定値から読めない——background.uniformity が下限を切っていて、かつ \
                 NOT_SEPARABLE になるか subject.confidence が low のときだけ走る。off なら\
                 必ず false で、そのときの成果物の画素は --segment を足す前と 1 バイトも\
                 変わらない（報告 JSON には subject.source / settings.segment / \
                 settings.segment_ran の 3 つが増える。schema_version は据え置き）",
            ),
        },
        FieldEntry {
            path: "settings.profile",
            appears_in: vec!["cutout"],
            // 値は {name, revision} の組だが、**この項目が答えているのは
            // 「どの profile か」の 1 つである**（revision はその行を一意に
            // する版であって、別の測定値ではない）。list にすると受け手は
            // 要素の並びを数えにきて、text にすると照合してよいものを
            // 照合できないと読む
            unit: "enum",
            nullable: false,
            null_means: None,
            warns: vec![],
            gates: None,
            summary: "効いた profile の名前と版 {name, revision}",
            notes: Some(
                "--profile を渡したときだけキーごと現れる。渡さない実行の結果 JSON は \
                 --profile を足す前と 1 バイトも変わらない（schema_version は据え置き）。\
                 **条件そのものはここに無い**——長辺・背景・占有率・出典の URL は \
                 kiri schema の profiles[] が name で引ける形で 1 度だけ配る。\
                 **これは指定値である。** profile が実際に決めた値は settings の他の項目と \
                 canvas ブロックが効いた値として語り、明示指定が上書きした項目は \
                 PROFILE_OVERRIDDEN が 1 件ずつ「profile が求めた値」と「実際に効いた値」を\
                 並べて言う。revision は kiri がその規格を写し取った時点で、\
                 **規格が変わっても古い kiri は古い条件で合格を出す**——鮮度はここで判断する",
            ),
        },
        FieldEntry {
            path: "settings.optimize",
            appears_in: vec!["cutout"],
            unit: "bool",
            nullable: false,
            null_means: None,
            warns: vec![],
            gates: None,
            summary: "--optimize を渡したか",
            notes: Some(
                "true なら settings.tolerance / settings.background_model / applied_bbox は\
                 kiri が探索して選んだ値である（渡した値ではない）。何を試したかは \
                 optimize.candidates[]、選ばれた理由は optimize.chosen.score が言う。\
                 **明示した軸は探索しない**——--tolerance 30 --optimize なら候補の \
                 tolerance は全部 30 になる。false のときの成果物の画素は --optimize を\
                 足す前と 1 バイトも変わらない（報告 JSON にはこの 1 キーだけが増える。\
                 schema_version は据え置き）",
            ),
        },
        FieldEntry {
            path: "optimize.candidates",
            appears_in: vec!["cutout"],
            unit: "list",
            nullable: false,
            null_means: None,
            warns: vec![],
            gates: None,
            summary: "試した全候補。並びは探索段（縮小版）の順位のまま",
            notes: Some(
                "**探索が走ったときだけ optimize ブロックごと現れる。** 各候補の \
                 tolerance / bbox / background_model はそのまま --tolerance / --bbox / \
                 --background-model へ写せる（bbox は原寸の画素座標）。2 位のほうが\
                 目的に合うなら、その値を明示指定して回し直せばよい",
            ),
        },
        FieldEntry {
            path: "optimize.candidates[].stage",
            appears_in: vec!["cutout"],
            unit: "enum",
            nullable: false,
            null_means: None,
            warns: vec![],
            gates: None,
            summary: "その候補をどの寸法で回したか（search / final）",
            notes: Some(
                "search は縮小版（optimize.searched_at px）を境界処理（refine）抜きで\
                 回した候補で、final は原寸・利用者の設定そのままで回し直した候補である\
                 （指標は原寸の値に上書きされる）。\
                 **search の halo_ratio / contour_roughness / rim_contamination / \
                 touches_edge は参考値で、順位には使っていない**——refine を抜くと縁に\
                 背景が残るため、実測で rim が 4〜6 倍、粗さが 3〜5 倍に膨らみ、しかも\
                 倍率が候補ごとに違う（寸法の換算では戻せない）。外周接触も同じ理由で\
                 search でだけ出る。search の順位を決めるのは NOT_SEPARABLE / \
                 FOREGROUND_TOO_SMALL / FOREGROUND_TOO_LARGE の数 + collapsed と、\
                 refine にも寸法にもほとんど依らない separability だけである。\
                 **2 つの stage の数値を直接比べないこと。** 原寸どうしの比較は final の\
                 候補どうしでだけ成り立つ",
            ),
        },
        FieldEntry {
            path: "optimize.chosen.score.fatal",
            appears_in: vec!["cutout"],
            unit: "count",
            nullable: false,
            null_means: None,
            warns: vec![],
            gates: None,
            summary: "選ばれた候補に残った致命的な警告の数",
            notes: Some(
                "数えるのは NOT_SEPARABLE / FOREGROUND_TOO_SMALL / FOREGROUND_TOO_LARGE / \
                 SUBJECT_TOUCHES_EDGE / BBOX_RECOMMENDED の 5 つ。最後の 2 つは\
                 「前景が外周に接している」という同じ事実の別の読み方なので、同じ重さで数える。\
                 **最終段（stage=final）の順位を決める第 1 項はこの数 + \
                 optimize.candidates[].collapsed** で、崩れは警告 code を持たないぶん\
                 ここには現れない（少ないほど良い）。探索段はこのうち refine に依らない\
                 3 つだけを数える（optimize.candidates[].stage を参照）。\
                 **0 でなくても OPTIMIZE_NO_CLEAN_CANDIDATE が出るとは限らない。** \
                 残ったのが BBOX_RECOMMENDED だけなら出ない——あちらが矩形つきで\
                 次の一手を言っているので、重ねて「撮り直せ」とは言わない。警告が\
                 数えるのは残り 4 つで、その code は warnings[].data.remaining に出る",
            ),
        },
        FieldEntry {
            path: "optimize.candidates[].collapsed",
            appears_in: vec!["cutout"],
            unit: "bool",
            nullable: false,
            null_means: None,
            warns: vec![],
            gates: None,
            summary: "同じ列で許容量を 1 段上げたときに、前景比率が 3 割を超えて落ちた候補か",
            notes: Some(
                "列は「同じ bbox・同じ background_model」で、その中を tolerance の昇順に見る。\
                 **淡い色の商品が背景ごと飲まれた候補を捕まえるためにある**——飲まれた結果は \
                 halo_ratio と rim_contamination が減った良い数値として現れ、警告は 1 つも出ない。\
                 順位の上では致命的な警告 1 つと同じ重さで扱うが、score.fatal には足さない",
            ),
        },
        FieldEntry {
            path: "optimize.chosen.score.quality",
            appears_in: vec!["cutout"],
            unit: "ratio",
            nullable: false,
            null_means: None,
            warns: vec![],
            gates: None,
            summary: "品質の重み和。小さいほど良い（最終段で fatal と unmeasured が同数のときに比べる）",
            notes: Some(
                "rim_contamination / contour_roughness / halo_ratio を、それぞれの警告\
                 しきい値で割って足したもの。**3.0 が「3 つとも警告ちょうど」**にあたり、\
                 0 に近いほど良い。測れなかった項（null）はしきい値ちょうど（1.0）として\
                 数える——0 と扱うと、測れなかった候補が最良として勝ってしまう。\
                 **ただし 1.0 でも足りないので、重み和より先に score.unmeasured\
                 （測れなかった項の数、0-3）を見る。** 診断が 3 つとも null になるのは\
                 たいてい測る対象の境界が無いからで、そういう候補が重み和 2〜3 で\
                 中位に紛れ込む（前景比率 0.0001 のほぼ空のマスクが実際に上位へ来ていた）。\
                 境界を測れる候補は測れない候補に勝つ。同点なら separability（大きいほど\
                 良い）、さらに同点なら小さい tolerance、bbox 無し、auto の順で選ぶ。\
                 **この順序は最終段のもので、探索段は separability だけを見る**\
                 （optimize.candidates[].stage を参照）",
            ),
        },
        FieldEntry {
            path: "optimize.searched_at",
            appears_in: vec!["cutout"],
            unit: "px",
            nullable: false,
            null_means: None,
            warns: vec![],
            gates: None,
            summary: "探索段で使った長辺(px)",
            notes: Some(
                "全候補はこの寸法で、境界処理（refine）抜きに回す。元の長辺がこれより\
                 小さければ元の寸法がそのまま入る。上位の候補だけを原寸で回し直すので、\
                 stage が final の候補の指標は原寸のものである",
            ),
        },
        FieldEntry {
            path: "settings.shadow",
            appears_in: vec!["cutout"],
            unit: "enum",
            nullable: false,
            null_means: None,
            warns: vec![],
            gates: None,
            summary: "落ち影を合成したか（off / synth）",
            notes: Some(
                "**--shadow-tolerance とは向きが逆である。** あちらは実写に写っている影を背景として\
                 消す側で、こちらは消した後のアルファから影を作り直す側になる。off（既定）なら\
                 成果物の画素は --shadow を足す前と 1 バイトも変わらず、shadow ブロックも現れない\
                 （schema_version は据え置き）。実際に効いたずらし量とぼかしは shadow.offset / \
                 shadow.blur のほう",
            ),
        },
        FieldEntry {
            path: "shadow.offset",
            appears_in: vec!["cutout"],
            unit: "px",
            nullable: false,
            null_means: None,
            warns: vec![],
            gates: None,
            summary: "影を実際にずらした量 [dx, dy]",
            notes: Some(
                "**指定値ではない。** --shadow-offset は長辺 1000px 換算で、基準は最終画像の長辺\
                 （--canvas があればキャンバスの長辺、無ければ --rotate まで済ませた画像の長辺。\
                 --rotate を渡さなければ元画像の長辺と同じ）である。24.5MP の素材に既定の \
                 0,12 を渡すと 0,69 になる。負値は上・左へ出したことを意味する",
            ),
        },
        FieldEntry {
            path: "outputs[].path",
            appears_in: vec!["convert", "resize", "rotate", "cutout"],
            unit: "path",
            nullable: false,
            null_means: None,
            warns: vec![],
            gates: None,
            summary: "この派生を書き出した先（--dry-run ならファイルは無い）",
            notes: Some(
                "**outputs[] は派生ごとに 1 要素である**（schema_version 2）。--derive / \
                 --sizes / --formats / --naming のどれも渡さない実行では 1 要素で、値は \
                 --output そのものになる。順序は指定した順で、--sizes と --formats を\
                 併せたときは size が外・format が内の直積になる（--naming の {index} と\
                 同じ番号）。\
                 **派生ごとに出うる警告は、どれも data.output で「どの出力の話か」を名乗る**\
                 ——ALPHA_FLATTENED / QUALITY_REDUCED / MAX_BYTES_UNREACHABLE / \
                 ICC_NOT_EMBEDDED / UPSCALED / DRY_RUN_OUTPUT_EXISTS の 6 つで、\
                 1 実行に複数回出うるので、どの出力の話かはその値で引く。\
                 値は outputs[].path か、--manifest のパスである——目録も\
                 上書きの規約に従うので、--dry-run で既にあれば \
                 DRY_RUN_OUTPUT_EXISTS がそのパスを名乗る。\
                 UPSCALED だけは data.output を持たないことがある。\
                 **持たないものは出どころが違う**——kiri resize --allow-upscale \
                 そのものの拡大は最終画像に起きたことで、派生ごとの事象ではない。\
                 派生のリサイズで出たものは持つので、data.output の有無が\
                 そのまま出どころの区別になる",
            ),
        },
        FieldEntry {
            path: "outputs[].role",
            appears_in: vec!["convert", "resize", "rotate", "cutout"],
            unit: "enum",
            nullable: true,
            null_means: Some("この派生に --derive の role を書かなかった（役目を付けていない）"),
            warns: vec![],
            gates: None,
            summary: "この派生の役目（--derive の role にそのまま書いた文字列）",
            notes: Some(
                "kiri は中身を解釈しない。配信側が outputs[] のどれを使うかを選ぶための札で、\
                 thumb / hero のような綴りを自由に置ける。--naming の {role} で\
                 ファイル名にも使える（役目を持たない派生があるのに書くと \
                 INVALID_NAMING_TEMPLATE で断る）。**キーは常に出す**ので、null は\
                 「役目を付けなかった」を意味し、キーが無ければ schema_version 1 の\
                 古い kiri で走ったことを意味する",
            ),
        },
        FieldEntry {
            path: "outputs[].icc",
            appears_in: vec!["convert", "resize", "rotate", "cutout"],
            unit: "enum",
            nullable: false,
            null_means: None,
            warns: vec![],
            gates: None,
            summary: "出力が色空間をどう名乗っているか（embedded / nclx / none）",
            notes: Some(
                "**outputs[] は派生ごとに 1 要素である**（schema_version 2）。名乗りは形式で\
                 変わるので、AVIF と PNG を同時に書く実行ではここが要素ごとに違う。\
                 embedded は PNG の iCCP か JPEG の APP2 に sRGB の ICC を埋めた。nclx は AVIF で、\
                 ICC ではなく AV1 の色情報（BT.709 の原色と sRGB の転送特性）で sRGB を名乗る——\
                 colr ボックスは既定値と同じなので省かれ、コンテナには現れない。none は何も名乗って\
                 いない。--no-color-convert で画素を sRGB へ変換しなかったときだけこうなり、\
                 ICC_NOT_EMBEDDED が理由を言う（AVIF はそのときも nclx のまま）。--dry-run でも\
                 実際にエンコードした結果を言う",
            ),
        },
        FieldEntry {
            path: "outputs[].quality_used",
            appears_in: vec!["convert", "resize", "rotate", "cutout"],
            // 0-100 の品質は既存のどの綴りでもない。`ratio` と名乗ると 0.0-1.0 に
            // 読めてしまうので、`delta_e` / `px_at_1000` と同じく専用の綴りを置く
            unit: "quality",
            nullable: true,
            null_means: Some("PNG は無損失で品質を持たない（--quality も --max-bytes も効かない）"),
            // **しきい値では出ない警告なので warns には載せない。**
            // ここは「この値がいくつを超えたら鳴るか」を配る欄で、
            // QUALITY_REDUCED / MAX_BYTES_UNREACHABLE はどちらも
            // 「--max-bytes に収まったか」で決まる。notes で名指しする
            warns: vec![],
            gates: None,
            summary: "実際に使った品質（--quality の指定値、--max-bytes で落としたならその段）",
            notes: Some(
                "**outputs[] は派生ごとに 1 要素である**（schema_version 2）。--derive で\
                 派生ごとに quality / max_bytes を変えれば、ここも要素ごとに違う値になる。\
                 --max-bytes が無ければ --quality の指定値がそのまま出る。収まらなかったときは\
                 固定の梯子 85 / 75 / 65 / 55 / 45 / 35 / 25 のうち --quality より小さい段を\
                 上から試し、最初に収まった段がここに出て QUALITY_REDUCED が落とした事実を言う。\
                 **下限まで降りても届かなければ要求品質のものを書く**ので、ここは --quality の\
                 指定値に戻り、MAX_BYTES_UNREACHABLE が未達を言う（成果物は残り、終了コードも\
                 変わらない）。段は時刻にもタイムアウトにも依存しないので、同じ入力からは\
                 毎回同じ値が出る。--dry-run でも実際にエンコードした結果である",
            ),
        },
        FieldEntry {
            path: "outputs[].attempts",
            appears_in: vec!["convert", "resize", "rotate", "cutout"],
            unit: "count",
            nullable: false,
            null_means: None,
            warns: vec![],
            gates: None,
            summary: "この出力をエンコードした回数",
            notes: Some(
                "**outputs[] は派生ごとに 1 要素で、ここはその 1 本ぶんの回数である**\
                 （schema_version 2）。実行全体の費用を見るなら全要素の和を取ること。\
                 --max-bytes が無ければ必ず 1 である。探索が走ると、要求品質の 1 回に\
                 降りた段の数が積む（既定の --quality 75 なら降りる段は 5 つで最大 6 回、\
                 --quality 100 でも最大 8 回）。PNG は品質を持たず\
                 段を降りないので --max-bytes を渡しても 1 のまま。**時間の見積もりに使える\
                 唯一の値**で、24.5MP の AVIF では 1 回あたり数秒かかる。\
                 --optimize と併せても掛け算にはならない——候補はマスクの指標で選ばれ、\
                 エンコードするのは決まった 1 枚だけなので、この回数は探索の後ろに 1 度だけ積む",
            ),
        },
        FieldEntry {
            path: "rotate.angle",
            // `kiri rotate` と `cutout --rotate` が同じブロックを返す。
            // 順序を固定したいなら後者を使う（README「切り抜いた後に回す」）
            appears_in: vec!["cutout", "rotate"],
            unit: "deg",
            nullable: false,
            null_means: None,
            warns: vec![],
            gates: None,
            summary: "実際に適用した時計回りの角度",
            notes: Some(
                "**指定値ではない。** -90 と 270 と 630 は同じ操作なので [0, 360) へ正規化した\
                 値が入る。回らない指定（0 と 360）では rotate ブロックごと現れない——\
                 渡した値は settings.rotate のほうが常に持つ。cutout では mask / background / \
                 subject の座標は**回す前**のものである",
            ),
        },
        FieldEntry {
            path: "rotate.resampled",
            appears_in: vec!["cutout", "rotate"],
            unit: "bool",
            nullable: false,
            null_means: None,
            warns: vec![],
            gates: None,
            summary: "画素を補間し直したか",
            notes: Some(
                "90 度単位なら false で、色は 1 バイトも変わらない（入れ替えだけで回る）。\
                 true なら Catmull-Rom で引き直しており、四隅に透過の余白が出る",
            ),
        },
        FieldEntry {
            path: "shadow.blur",
            appears_in: vec!["cutout"],
            unit: "px",
            nullable: false,
            null_means: None,
            warns: vec![],
            gates: None,
            summary: "影に実際に掛けたぼかしの σ",
            notes: Some(
                "shadow.offset と同じく長辺 1000px 換算からの掛け戻し。0 ならぼかしていない。\
                 箱型フィルタ 3 回の近似なので、値をいくつにしても所要時間は変わらない",
            ),
        },
        FieldEntry {
            path: "shadow.bounds",
            appears_in: vec!["cutout"],
            unit: "px",
            nullable: true,
            null_means: Some("影が 1 画素も残らなかった。矩形が空（面積 0）ではない"),
            warns: vec![],
            gates: None,
            summary: "影のアルファが 0 より大きい画素の外接矩形 [x1, y1, x2, y2]",
            notes: Some(
                "商品ではなく影の占める範囲である。--shadow-opacity 0 や、ずらし量が画像より\
                 大きいときに null になる。どちらだったかは shadow.clipped が分ける\
                 （前者は false、後者は true）",
            ),
        },
        FieldEntry {
            path: "shadow.clipped",
            appears_in: vec!["cutout"],
            unit: "bool",
            nullable: false,
            null_means: None,
            warns: vec![],
            gates: None,
            summary: "影の一部が画像（またはキャンバス）の外にあるか",
            notes: Some(
                "**最終の影のアルファで決める。** ずらしただけで画像の外へ落ちた画素があるか、\
                 外周の 1 列・1 行に影が残っている（= その先へ続いていた）ときに true になる。\
                 ぼかしの台が縁を跨いだかどうかでは決めない——箱型の台は約 3σ あるので、\
                 裾が丸めで消えている場合まで true になってしまう。\
                 **shadow.bounds が null でも true になりうる**——ずらし量が画像より大きければ\
                 影は 1 画素も残らないが、それは「影を置かなかった」のではなく「全部はみ出した」\
                 である。--shadow-opacity 0 は影を置かない指定なので必ず false。\
                 true のときに影を収めたければ --canvas を広げるか、--shadow-offset / \
                 --shadow-blur を小さくする——**商品は影のために動かさない**ので、kiri が\
                 勝手に縮めることはない",
            ),
        },
        FieldEntry {
            path: "mask.foreground_ratio",
            appears_in: vec!["cutout"],
            unit: "ratio",
            nullable: false,
            null_means: None,
            warns: vec![
                FieldThreshold {
                    code: WarningCode::ForegroundTooSmall,
                    operator: "lt",
                    threshold: MIN_FOREGROUND_RATIO,
                },
                FieldThreshold {
                    code: WarningCode::ForegroundTooLarge,
                    operator: "gt",
                    threshold: MAX_FOREGROUND_RATIO,
                },
            ],
            gates: None,
            summary: "前景が画像全体に占める割合",
            notes: Some(
                "どれだけ残ったかしか言わず、その輪郭が妥当かは何も語らない。妥当さは \
                 mask.separability が言う",
            ),
        },
        FieldEntry {
            path: "mask.separability",
            appears_in: vec!["cutout"],
            unit: "delta_e",
            nullable: true,
            null_means: Some("測れる境界が無かった。0（色差が無い）ではない"),
            warns: vec![],
            gates: None,
            summary: "切り抜き境界の内側で測った、商品と**効いたモデルの背景**との色差の中位値",
            notes: Some(
                "background.residual.p50 を下回ると NOT_SEPARABLE が出る。固定のしきい値ではなく\
                 画像ごとの値と比べるので、ここには threshold を載せられない。**分子と分母は\
                 同じモデルで測る**——model が field ならここも場に対する色差で、比べる相手も\
                 場に対する分布である（model が flat ならどちらも 1 色に対する値になる）",
            ),
        },
        FieldEntry {
            path: "mask.halo_ratio",
            appears_in: vec!["cutout"],
            unit: "ratio",
            nullable: true,
            null_means: Some("測る境界が無かった。0（縁が残っていない）ではない"),
            warns: vec![FieldThreshold {
                code: WarningCode::HaloRemains,
                operator: "gt",
                threshold: diagnostics::HALO_WARN,
            }],
            gates: None,
            summary: "境界近傍で不透明なのに、元の色が局所背景と見分けがつかない画素の割合",
            notes: Some(
                "白い下地では見えず、黒や色付きの下地に載せて初めて輪郭の光として現れる。\
                 参照にする局所背景は背景モデルと矛盾しないものだけから採る——\
                 輪郭の色差が tolerance を下回る素材ではフィルが商品の外縁を食い、\
                 その跡を参照にすると残った商品が「背景色のまま」と数えられる",
            ),
        },
        FieldEntry {
            path: "mask.edge_width",
            appears_in: vec!["cutout"],
            unit: "px",
            nullable: true,
            null_means: Some("遷移を 1 本も追えなかった。0（幅が無い）ではない"),
            warns: vec![],
            gates: None,
            summary: "境界法線方向にアルファが 0.9 から 0.1 へ落ちるまでの幅の中位値",
            notes: Some(
                "1〜3 なら鮮鋭。大きい値がそのまま欠陥ではなく、8px かけて溶ける素材では 6.5 が\
                 正解である。鮮鋭なはずの輪郭で 6 を超えたらぼやけていると読む",
            ),
        },
        FieldEntry {
            path: "mask.contour_roughness",
            appears_in: vec!["cutout"],
            unit: "px_at_1000",
            nullable: true,
            null_means: Some("測れる輪郭が無かった。0（完全に滑らか）ではない"),
            warns: vec![FieldThreshold {
                code: WarningCode::ContourRough,
                operator: "gt",
                threshold: diagnostics::CONTOUR_ROUGH_WARN,
            }],
            gates: None,
            summary: "二値輪郭が、それを滑らかにした参照輪郭からどれだけ離れているかの平均",
            notes: Some(
                "長辺 1000px へ縮めたときの px で報告する。納品先がその寸法だからで、20MP の \
                 1px は縮めると 0.18px になって見えない。参照は自分自身を σ = 2px（換算）で\
                 ぼかして 0.5 で再二値化したもので、距離は 1px 刻みのチャンファー。画素ごとの\
                 距離は平滑化が届く距離（箱ぼかし 3 回ぶんの 3r、換算 6px 相当）で頭打ちに\
                 してから平均するので、値の上限もそこで決まる。幅 3px 級の細部（ストラップ、\
                 ひも）は平滑化参照から消えるため、粗さとして数える側に入る。\
                 **この値は蛇行だけを見る。** 輪郭が滑らかなまま違う場所にある切り抜き\
                 （実写の照明勾配 + 既定値で、輪郭が真の位置から 59.7px ずれているのに \
                 0.19）はここには出ない。それを捕まえるのは BBOX_RECOMMENDED / \
                 RIM_CONTAMINATED / HALO_REMAINS で、両方を見ること",
            ),
        },
        FieldEntry {
            path: "mask.rim_contamination",
            appears_in: vec!["cutout"],
            unit: "ratio",
            nullable: true,
            null_means: Some(
                "帯が無かったか、帯の半分以上で判定できなかった。0（汚染が無い）ではない",
            ),
            warns: vec![FieldThreshold {
                code: WarningCode::RimContaminated,
                operator: "gt",
                threshold: diagnostics::RIM_CONTAMINATION_WARN,
            }],
            gates: None,
            summary: "境界の内側の帯で、元の色が局所前景より局所背景に近い画素の割合",
            notes: Some(
                "halo_ratio が見落とすものを見る。あちらは「局所背景と ΔE≤3」という絶対的な\
                 基準なので、繊維のばらつきが ΔE 5〜10 ある不織布では張り付いた繊維が数から\
                 漏れる。こちらは線形 RGB で、局所前景・局所背景それぞれの散らばり（σ）で\
                 正規化した距離を比べるので、背景のばらつきが大きくても「その色は背景\
                 テクスチャの範囲内か」を問える。分母は判定できた画素である——2 つの分布が\
                 散らばりの中で重なる画素（淡色商品 × 白背景）は判定しないので、帯の\
                 半分以上が判定できなければ値ではなく null を返す。マスクが背景を大きく\
                 飲み込んでいるときは局所前景そのものが背景になるので、値は正解より小さく\
                 出る（BBOX_RECOMMENDED や HALO_REMAINS が同時に出ていたらそちらを先に読む）",
            ),
        },
        FieldEntry {
            path: "constraints.sources",
            appears_in: vec!["cutout"],
            unit: "list",
            nullable: false,
            null_means: None,
            warns: vec![],
            gates: None,
            summary: "画素を 1 つ以上塗った入口の名前（trimap / alpha_trimap / fg_mask / bg_mask / \
                      fg_polygon / bg_polygon / fg_seed）",
            notes: Some(
                "渡した入口のうち 1 画素以上塗ったものだけが並ぶ。渡したのにここへ無ければ、\
                 その指示は空だった（空のマスク、画像の外だけを指す多角形）——そのときは \
                 CONSTRAINT_EMPTY が data.source にその名前を載せて出る。--bbox はここに\
                 入らない（settings と applied_bbox が言う）",
            ),
        },
        FieldEntry {
            path: "constraints.fg_pixels",
            appears_in: vec!["cutout"],
            unit: "px",
            nullable: false,
            null_means: None,
            warns: vec![],
            gates: None,
            summary: "確定前景として指示された画素数",
            notes: Some(
                "比率（constraints.fg_ratio）は小数第 4 位までなので、20MP の数百画素は 0.0 に\
                 落ちる。sources に名前があるのに比率が 0.0 のときは、こちらを読む",
            ),
        },
        FieldEntry {
            path: "constraints.bg_pixels",
            appears_in: vec!["cutout"],
            unit: "px",
            nullable: false,
            null_means: None,
            warns: vec![],
            gates: None,
            summary: "確定背景として指示された画素数",
            notes: Some("constraints.fg_pixels と同じ理由で持つ"),
        },
        FieldEntry {
            path: "constraints.fg_ratio",
            appears_in: vec!["cutout"],
            unit: "ratio",
            nullable: false,
            null_means: None,
            warns: vec![],
            gates: None,
            summary: "確定前景として指示された画素が、画像に占める割合",
            notes: Some(
                "空間的な指示（--trimap / --alpha-trimap / --fg-mask / --bg-mask / --fg-polygon / \
                 --bg-polygon / --fg-seed）を渡したときだけ現れる。渡していなければ constraints ごと無い。\
                 --bbox はここに入らない（settings と applied_bbox が言う）",
            ),
        },
        FieldEntry {
            path: "constraints.bg_ratio",
            appears_in: vec!["cutout"],
            unit: "ratio",
            nullable: false,
            null_means: None,
            warns: vec![],
            gates: None,
            summary: "確定背景として指示された画素が、画像に占める割合",
            notes: Some(
                "指示があったときだけ現れる。確定背景はフィルの種にもなるので、商品に囲まれて\
                 外周から届かない背景もここで消える",
            ),
        },
        FieldEntry {
            path: "constraints.unknown_ratio",
            appears_in: vec!["cutout"],
            unit: "ratio",
            nullable: false,
            null_means: None,
            warns: vec![],
            gates: None,
            summary: "どちらも指示されていない画素の割合。色と連結性で決まる領域",
            notes: Some(
                "指示があったときだけ現れる。トライマップの不明帯（輝度 64-191）はここに入る。\
                 3 つの比率の和は 1 になる",
            ),
        },
        FieldEntry {
            path: "compliance.fail_on",
            appears_in: vec!["cutout"],
            unit: "text",
            nullable: false,
            null_means: None,
            warns: vec![],
            gates: None,
            summary: "利用者が書いた --fail-on の文字列そのまま",
            notes: Some(
                "**何を頼んだかが結果だけで分かる。** 書式は default / <指標><演算子><値> / \
                 真偽の指標（裸のトークン）をカンマで並べたもので、綴りは \
                 kiri cutout --help の --fail-on が配る。解かずにそのまま返すので、\
                 空白の入れ方も書いたとおりである",
            ),
        },
        FieldEntry {
            path: "compliance.passed",
            appears_in: vec!["cutout"],
            unit: "bool",
            nullable: false,
            null_means: None,
            warns: vec![],
            gates: None,
            summary: "--fail-on の条件をすべて満たしたか",
            notes: Some(
                "--fail-on を渡したときだけブロックごと現れる。false なら終了コードは 5 で、\
                 **結果 JSON は通常どおり全部返る**（処理は成功していて成果物もある）。\
                 checks[] に fail も unmeasurable も 1 つも無いことが true の条件で、\
                 **測れなかった指標は合格にしない**——separability の null は「前景が無い」、\
                 halo_ratio の null は「測る境界が無い」で、どちらも黙って通してよい状態ではない",
            ),
        },
        FieldEntry {
            path: "compliance.code",
            appears_in: vec!["cutout"],
            unit: "enum",
            nullable: true,
            null_means: Some("合格した（passed が true）"),
            warns: vec![],
            gates: None,
            summary: "不合格のときだけ名乗る code。QUALITY_GATE_FAILED になる",
            notes: Some(
                "**exit 5 は errors[] を返さない**（成果物があるので結果 JSON を通常どおり\
                 返す）ので、errors[] が配る語彙と結果を突き合わせられる場所がここ以外に無い。\
                 exit 5 を見てここを引けば、他の失敗とまったく同じ形で分岐できる。\
                 どの条件で落ちたかは checks[].code のほうが言う——**別の値である**",
            ),
        },
        FieldEntry {
            path: "compliance.checks",
            appears_in: vec!["cutout"],
            unit: "list",
            nullable: false,
            null_means: None,
            warns: vec![],
            gates: None,
            summary: "評価した条件の内訳。{name, status, operator, threshold, actual, code}",
            notes: Some(
                "**見たものを全部載せる**（pass も）。落ちたものだけだと「見た上で通った」と\
                 「そもそも見ていない」が区別できない。name は mask.* のキーそのままで、\
                 actual はその値と同じ数である。operator の綴りは fields[].warns[].operator と\
                 同じ（lt / lte / gt / gte）。真偽の指標は裸のトークンで書くので（touches_edge）、\
                 operator も threshold も null になる。固定のしきい値を持たない条件\
                 （NOT_SEPARABLE は画像ごとの background.residual.p50 と比べ、外周接触の 2 つは\
                 複合条件）でも同じく null。**並びは決定的**で、指定の順には依らない。\
                 **name は一意ではない。** default は指標ではなく code ごとに 1 行出すので、\
                 foreground_ratio が 2 行、touches_edge が 2 行並ぶ——name で畳むと片方が黙って\
                 消える。**一意なのは code のほう**である。\
                 **default の status は actual から出ていない。** 合否は実際に出た警告\
                 （生値で判定）から取り、actual は報告値（小数第 4 位で丸めた mask.* の値）\
                 なので、4 桁目で両者がずれうる（生の halo_ratio が 0.10003 なら \
                 status=fail / threshold=0.1 / actual=0.1 という行が出る）。明示のしきい値には\
                 このずれが無い。加えて外周接触の 2 つは片方しか警告が出ないので、\
                 actual=true なのに status=pass の行（もう片方の code）が並びうる",
            ),
        },
        FieldEntry {
            path: "compliance.checks[].status",
            appears_in: vec!["cutout"],
            unit: "enum",
            nullable: false,
            null_means: None,
            warns: vec![],
            gates: None,
            summary: "pass / fail / unmeasurable のいずれか",
            notes: Some(
                "unmeasurable は「測れなかった」で、**合格ではない**。fail（しきい値に触れた）\
                 とは別の事実なので名前を分けてある——次の一手が違う（前者は素材か指示を、\
                 後者はしきい値か設定を見る）",
            ),
        },
        // kiri lint の結果。**`LintReport` が結果 JSON の根そのもの**なので、
        // path にコマンド名の接頭辞は付かない（`compliance.*` は `CutoutReport`
        // の中のブロックだった、という違いである）。どのコマンドの話かは
        // `appears_in` が言う
        FieldEntry {
            path: "profile",
            appears_in: vec!["lint"],
            // 値は {name, revision} の組だが、この項目が答えているのは
            // 「どの規格に照らしたか」の 1 つである（`settings.profile` と
            // まったく同じ理由で enum を採る）
            unit: "enum",
            nullable: false,
            null_means: None,
            warns: vec![],
            gates: None,
            summary: "照らした規格の名前と版 {name, revision}",
            notes: Some(
                "lint では --profile が必須なので必ず出る。**条件そのものはここに無い**\
                 ——長辺・背景・占有率・出典の URL は kiri schema の profiles[] が name で\
                 引ける形で 1 度だけ配る。revision は kiri がその規格を写し取った時点で、\
                 **規格が変わっても古い kiri は古い条件で合格を出す**——鮮度はここで判断する。\
                 書く側（cutout --profile）が返す settings.profile と同じ形である",
            ),
        },
        FieldEntry {
            path: "passed",
            appears_in: vec!["lint"],
            unit: "bool",
            nullable: false,
            null_means: None,
            warns: vec![],
            gates: None,
            summary: "--profile の条件をすべて満たしたか",
            notes: Some(
                "**checks[] が全部 pass のときだけ true** である。fail も unmeasurable も \
                 skipped も合格ではない（compliance.passed とまったく同じ定義）。false なら\
                 終了コードは 5 で、**結果 JSON は通常どおり全部返る**（検査は成功していて、\
                 対象のファイルもそのままある）。**測れなかった項目を合格に数えない**のは、\
                 exit 5 の意味が「成果物はある。人が見る対象」だからで、\
                 「検査できなかったので人が見てほしい」はまさにそれである",
            ),
        },
        FieldEntry {
            path: "code",
            appears_in: vec!["lint"],
            unit: "enum",
            nullable: true,
            null_means: Some("合格した（passed が true）"),
            warns: vec![],
            gates: None,
            summary: "不合格のときだけ名乗る code。PROFILE_VIOLATION になる",
            notes: Some(
                "**exit 5 は errors[] を返さない**ので、errors[] が配る語彙と結果を\
                 突き合わせられる場所がここ以外に無い。exit 5 を見てここを引けば、\
                 他の失敗とまったく同じ形で分岐できる（compliance.code と同じ役目）。\
                 どの条件で落ちたかは checks[] のほうが言う",
            ),
        },
        FieldEntry {
            path: "checks",
            appears_in: vec!["lint"],
            unit: "list",
            nullable: false,
            null_means: None,
            warns: vec![],
            gates: None,
            summary: "検査した条件の内訳。{name, status, expected, actual}",
            notes: Some(&LINT_CHECKS_NOTE),
        },
        FieldEntry {
            path: "checks[].status",
            appears_in: vec!["lint"],
            unit: "enum",
            nullable: false,
            null_means: None,
            warns: vec![],
            gates: None,
            summary: &LINT_STATUS_SUMMARY,
            notes: Some(
                "pass 以外は**すべて合格ではない**。3 つを分けてあるのは次の一手が違う\
                 ためで、fail は条件に触れた（素材か規格の選び直し）、unmeasurable は\
                 この画像では測れなかった（主体を 1 つも検出できない、外周に不透明な\
                 画素が 1 つも無い、背景が単色ではない、色空間を何も名乗っていない）、\
                 skipped はこの形式では構造的に測れない（AVIF の画素）。\
                 **skipped が 1 つでもあれば \
                 PROFILE_UNCHECKABLE が出て、飛ばした項目を data.checks に配列で並べる**\
                 ——黙って合格にはしていない。skipped を消したければ JPEG か PNG を渡す",
            ),
        },
        FieldEntry {
            path: "mask.touches_edge",
            appears_in: vec!["cutout"],
            unit: "bool",
            nullable: false,
            null_means: None,
            warns: vec![],
            gates: None,
            summary: "前景が画像の外周に接しているか",
            notes: Some(
                "true でも商品の見切れとは限らない。背景側が前景として残ったまま端に達した\
                 場合も true になる。bbox 未指定で背景が不均一、主体が high なら \
                 BBOX_RECOMMENDED（bbox 一つで解ける）、それ以外は SUBJECT_TOUCHES_EDGE\
                 （撮り直すしかない）に分かれる",
            ),
        },
    ]
}
