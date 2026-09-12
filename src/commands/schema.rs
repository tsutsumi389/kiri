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
            summary: "画素を 1 つ以上塗った入口の名前（trimap / fg_mask / bg_mask / fg_polygon / \
                      bg_polygon / fg_seed）",
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
                "空間的な指示（--trimap / --fg-mask / --bg-mask / --fg-polygon / --bg-polygon / \
                 --fg-seed）を渡したときだけ現れる。渡していなければ constraints ごと無い。\
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
