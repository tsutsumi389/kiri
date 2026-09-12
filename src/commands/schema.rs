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

use clap::{ArgAction, CommandFactory};

use crate::cli::Cli;
use crate::cutout::{MAX_FOREGROUND_RATIO, MIN_FOREGROUND_RATIO, background, diagnostics, subject};
use crate::error::{ErrorCode, ErrorKind};
use crate::report::{
    ArgEntry, CommandEntry, ErrorCodeEntry, ExitCodeEntry, FieldEntry, FieldGate, FieldThreshold,
    SCHEMA_VERSION, SchemaReport, WarningCodeEntry,
};
use crate::warning::WarningCode;

pub fn run() -> SchemaReport {
    SchemaReport {
        schema_version: SCHEMA_VERSION,
        kiri_version: env!("CARGO_PKG_VERSION"),
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

fn commands() -> Vec<CommandEntry> {
    Cli::command()
        .get_subcommands()
        .map(|sub| {
            let (arguments, options): (Vec<_>, Vec<_>) = sub
                .get_arguments()
                .filter(|arg| !is_clap_builtin(arg))
                .map(arg_entry)
                .partition(|entry| !entry.name.starts_with("--"));

            CommandEntry {
                name: sub.get_name().to_string(),
                about: sub.get_about().map(|a| a.to_string()),
                arguments,
                options,
            }
        })
        .collect()
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
                "背景自身のばらつき。mask.separability がこれを下回る画像は、背景を飲み込める \
                 tolerance が商品も飲み込むので救えない（NOT_SEPARABLE）",
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
            summary: "切り抜き境界の内側で測った商品と背景の色差の中位値",
            notes: Some(
                "background.perimeter_delta_e.p50 を下回ると NOT_SEPARABLE が出る。固定の\
                 しきい値ではなく画像ごとの値と比べるので、ここには threshold を載せられない",
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
            notes: Some("白い下地では見えず、黒や色付きの下地に載せて初めて輪郭の光として現れる"),
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
                 1px は縮めると 0.18px になって見えない。きれいに解けた合成シーンは 0.01 前後、\
                 実写背景でフィルが届かなかった結果は 0.32 以上で、20MP の実写（最良設定）は \
                 0.39 だった。0.06 未満なら滑らかと読んでよい。幅 3px 級の細部（ストラップ、\
                 ひも）は平滑化参照から消えるので粗さとして数える——合成の 3px ストラップで \
                 0.08 になる",
            ),
        },
        FieldEntry {
            path: "mask.rim_contamination",
            appears_in: vec!["cutout"],
            unit: "ratio",
            nullable: true,
            null_means: Some("判定できる帯の画素が無かった。0（汚染が無い）ではない"),
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
                 漏れる。近さは局所前景・局所背景それぞれの散らばり（σ）で正規化してから\
                 比べるので、背景のばらつきが大きくても「その色は背景テクスチャの範囲内か」\
                 を問える。2 つの分布が散らばりの中で重なる画素（淡色商品 × 白背景）は\
                 判定しない。きれいに解けたシーンは 0.007 以下、20MP の実写（最良設定）は \
                 0.102 だった。マスクが背景を大きく飲み込んでいるときは局所前景そのものが\
                 背景になるので、この値は当てにならない（BBOX_RECOMMENDED や HALO_REMAINS \
                 が同時に出ていたらそちらを先に読む）",
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
