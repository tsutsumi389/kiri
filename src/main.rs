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
use kiri::error::{Error, ErrorKind, Result};
use kiri::report::{
    BackgroundReport, BatchReport, CutoutReport, ErrorReport, InfoReport, ProcessReport,
};

fn main() -> ExitCode {
    let cli = Cli::parse();
    match dispatch(&cli) {
        Ok(code) => ExitCode::from(code as u8),
        Err(e) => {
            emit_error(&e, cli.json);
            ExitCode::from(e.exit_code() as u8)
        }
    }
}

/// 成功時も終了コードを返す。バッチは一部の項目が失敗しても処理を続けるため、
/// 「全体としては動いたが失敗がある」を表現する必要がある。
fn dispatch(cli: &Cli) -> Result<i32> {
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
                print_process(&report);
            }
        }
        Command::Resize(args) => {
            let report = commands::resize::run(args)?;
            if cli.json {
                print_json(&report)?;
            } else {
                print_process(&report);
            }
        }
        Command::Cutout(args) => {
            let report = commands::cutout::run(args)?;
            if cli.json {
                print_json(&report)?;
            } else {
                print_cutout(&report);
            }
        }
        Command::Batch(args) => {
            let report = commands::batch::run(args)?;
            if cli.json {
                print_json(&report)?;
            } else {
                print_batch(&report);
            }
            // 失敗した項目があれば処理失敗として知らせる。詳細は results[] にある
            if report.failed > 0 {
                return Ok(ErrorKind::Processing.exit_code());
            }
        }
    }
    Ok(0)
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
        "  色空間    {}{}{}",
        report.color_space,
        if report.icc_profile {
            " (ICCあり)"
        } else {
            ""
        },
        if report.color_converted {
            " → sRGB に変換"
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
    print_perimeter(&report.background);
    print_warnings(&report.warnings);
}

fn print_process(report: &ProcessReport) {
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
    print_color(&report.color_space, report.color_converted);
    print_warnings(&report.warnings);
}

/// 色を触ったときだけ知らせる。sRGB の素材で毎回 1 行増えても意味がない。
fn print_color(space: &str, converted: bool) {
    if converted {
        println!("  色空間    {space} → sRGB に変換");
    }
}

fn print_cutout(report: &CutoutReport) {
    let [r, g, b] = report.background.rgb;
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
    print_color(&report.color_space, report.color_converted);
    println!(
        "  背景色    #{r:02X}{g:02X}{b:02X}  (均一度 {:.2}, tolerance {})",
        report.background.uniformity, report.settings.tolerance
    );
    print_perimeter(&report.background);
    println!("  前景比率  {:.1}%", report.mask.foreground_ratio * 100.0);
    if let Some(sep) = report.mask.separability {
        // tolerance と並べて出す。両者の大小そのものが判断材料であるため
        println!(
            "  境界色差  ΔE {sep:.1}  (tolerance {})",
            report.settings.tolerance
        );
    }
    match report.mask.bbox {
        Some([x1, y1, x2, y2]) => println!("  前景範囲  {x1},{y1} - {x2},{y2}"),
        None => println!("  前景範囲  なし"),
    }
    if report.mask.touches_edge {
        println!("  外周接触  あり");
    }
    if let Some(c) = &report.canvas {
        println!(
            "  キャンバス {}x{}  占有率 {:.0}%  配置 {}x{} @ {},{}  (倍率 {:.2})",
            c.width,
            c.height,
            c.fill_ratio * 100.0,
            c.content[0],
            c.content[1],
            c.offset[0],
            c.offset[1],
            c.scale
        );
    }
    if let Some(path) = &report.mask.debug_mask {
        println!("  マスク    {path}");
    }
    if let Some(path) = &report.preview {
        println!("  プレビュー  {path}");
    }
    print_warnings(&report.warnings);
}

fn print_batch(report: &BatchReport) {
    for item in &report.results {
        match (&item.result, &item.error) {
            (Some(r), _) => {
                let out = &r.outputs[0];
                let mark = if r.warnings.is_empty() { " " } else { "!" };
                println!(
                    "{mark} {}  {}x{}  {}",
                    item.output,
                    out.width,
                    out.height,
                    human_bytes(out.bytes)
                );
                for w in &r.warnings {
                    eprintln!("  警告 [{}]: {w}", item.input);
                }
            }
            (_, Some(e)) => {
                println!("x {}  失敗", item.input);
                eprintln!("  エラー [{}]: {}: {}", item.input, e.code, e.message);
            }
            _ => {}
        }
    }
    println!(
        "\n{} 件中 {} 件成功、{} 件失敗、{} 件に警告  ({} ms)",
        report.total, report.succeeded, report.failed, report.with_warnings, report.elapsed_ms
    );
}

fn print_perimeter(bg: &BackgroundReport) {
    let d = &bg.perimeter_delta_e;
    println!(
        "  外周ΔE    p50 {:.1}  p90 {:.1}  max {:.1}",
        d.p50, d.p90, d.max
    );
    // 色のばらつき（外周ΔE）と別に出す。両者は別の失敗を予告するためで、
    // ざらつきは tolerance ではなく堤防のほうを狂わせる
    let t = &bg.texture;
    println!("  外周勾配  p50 {:.1}  p90 {:.1}", t.p50, t.p90);
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
