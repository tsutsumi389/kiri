//! kiri のエントリポイント。
//!
//! 出力の規約:
//! - `--json` 指定時は stdout に JSON のみを出す。成功でも失敗でも JSON を返す
//! - 非 JSON 時は人間向けの要約を stdout に、警告とエラーを stderr に出す

use std::process::ExitCode;

use clap::Parser;
use serde::Serialize;

use kiri::cli::{Cli, Command};
use kiri::commands;
use kiri::error::{Error, Result};
use kiri::report::{ConvertReport, ErrorReport, InfoReport};

fn main() -> ExitCode {
    let cli = Cli::parse();
    match dispatch(&cli) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            emit_error(&e, cli.json);
            ExitCode::from(e.exit_code() as u8)
        }
    }
}

fn dispatch(cli: &Cli) -> Result<()> {
    match &cli.command {
        Command::Info(args) => {
            let report = commands::info::run(args)?;
            if cli.json {
                print_json(&report)?;
            } else {
                print_info(&report);
            }
        }
        Command::Convert(args) => {
            let report = commands::convert::run(args)?;
            if cli.json {
                print_json(&report)?;
            } else {
                print_convert(&report);
            }
        }
    }
    Ok(())
}

fn print_json<T: Serialize>(value: &T) -> Result<()> {
    let text = serde_json::to_string_pretty(value)
        .map_err(|e| Error::general("JSON_ENCODE_FAILED", e.to_string()))?;
    println!("{text}");
    Ok(())
}

fn emit_error(error: &Error, json: bool) {
    if json {
        let report = ErrorReport::from(error);
        match serde_json::to_string_pretty(&report) {
            Ok(text) => println!("{text}"),
            Err(e) => eprintln!("エラー: {error}（JSON化にも失敗: {e}）"),
        }
    } else {
        eprintln!("エラー: {error}");
    }
}

fn print_info(report: &InfoReport) {
    let [r, g, b] = report.background.rgb;
    println!("{}", report.input);
    println!("  寸法      {} x {}", report.width, report.height);
    println!("  形式      {}", report.format);
    println!(
        "  EXIF回転  {}{}",
        report.exif_orientation,
        if report.orientation_applied {
            " (適用済み)"
        } else {
            ""
        }
    );
    println!(
        "  色空間    {}{}",
        report.color_space,
        if report.icc_profile {
            " (ICCあり)"
        } else {
            ""
        }
    );
    println!(
        "  透過      {}",
        if report.has_alpha { "あり" } else { "なし" }
    );
    println!(
        "  背景色    #{r:02X}{g:02X}{b:02X}  (均一度 {:.2})",
        report.background.uniformity
    );
    print_warnings(&report.warnings);
}

fn print_convert(report: &ConvertReport) {
    for out in &report.outputs {
        println!(
            "{}  {}x{}  {}  {}  ({} ms)",
            out.path,
            out.width,
            out.height,
            out.format,
            human_bytes(out.bytes),
            report.elapsed_ms
        );
    }
    print_warnings(&report.warnings);
}

fn print_warnings(warnings: &[String]) {
    for w in warnings {
        eprintln!("警告: {w}");
    }
}

fn human_bytes(bytes: u64) -> String {
    const KB: f64 = 1024.0;
    const MB: f64 = KB * 1024.0;
    let b = bytes as f64;
    if b >= MB {
        format!("{:.1} MB", b / MB)
    } else if b >= KB {
        format!("{:.1} KB", b / KB)
    } else {
        format!("{bytes} B")
    }
}
