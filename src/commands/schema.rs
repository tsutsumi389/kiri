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
use crate::error::{ErrorCode, ErrorKind};
use crate::report::{
    ArgEntry, CommandEntry, ErrorCodeEntry, ExitCodeEntry, SCHEMA_VERSION, SchemaReport,
    WarningCodeEntry,
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
        repeatable: matches!(arg.get_action(), ArgAction::Append),
        global: arg.is_global_set(),
        summary: arg.get_help().map(|h| h.to_string()).unwrap_or_default(),
        // 長いヘルプは「指定の前に知っていないと選びようがないこと」が
        // 置かれている場所なので、短い方で上書きせず両方返す
        detail: arg.get_long_help().map(|h| h.to_string()),
    }
}
