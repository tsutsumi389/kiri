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
use kiri::cutout::{Confidence, bbox_argument};
use kiri::error::{Error, ErrorKind, Result};
use kiri::report::{
    BackgroundReport, BatchReport, CutoutReport, ErrorReport, InfoReport, ProcessReport,
    SubjectReport,
};
use kiri::warning::Warning;

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
        Command::Rotate(args) => {
            let report = commands::rotate::run(args)?;
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
    // 名乗りが色空間名と食い違うときは名乗りのほうを出す。sRGB 相当と判定して
    // 素通ししたとき、どのプロファイルが付いていたのかがここでしか分からない
    let icc = match &report.color_profile {
        Some(name) if *name != report.color_space => format!(" (ICC '{name}')"),
        _ if report.icc_profile => " (ICCあり)".to_string(),
        _ => String::new(),
    };
    println!(
        "  色空間    {}{}{}",
        report.color_space,
        icc,
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
    print_subject(report.subject.as_ref());
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
    if let Some(r) = &report.rotate {
        // 無劣化かどうかを添える。90 度単位とそれ以外では、同じ「回した」でも
        // 出力の意味が違う（前者は色が 1 バイトも変わらない）
        println!(
            "  回転      {}°  ({})",
            r.angle,
            if r.resampled {
                "再サンプリング"
            } else {
                "無劣化"
            }
        );
    }
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
    print_subject(report.subject.as_ref());
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
                // hint も出す。単体実行では出るのに batch でだけ消えると、
                // **同じ画像の同じ失敗が、呼び方によって回復できたりできなかったり
                // する。** 行頭に入力名を置く体裁だけを揃えて、中身は落とさない
                for w in &r.warnings {
                    eprintln!("  警告 [{}]: {}", item.input, w.message);
                    if let Some(hint) = &w.hint {
                        eprintln!("         {hint}");
                    }
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

/// 主体候補の位置を 1 行で出す。
///
/// そのまま `--bbox <値> --normalized` へ貼れる並びにしてある。人間が読む側でも
/// 「どこを商品と見たか」が数値で分かることが、結果を疑うための取っ掛かりになる。
/// 検出できなかったときは黙る。テキスト出力は人間向けなので、無いものを
/// 「なし」と 1 行使って言う価値が薄い（JSON 側は null を必ず返す）。
///
/// **丸めは `bbox_argument` に任せる。** ここで見栄えのために小数第 2 位へ
/// 落とすと、同じ矩形が「テキストの行」と「警告の hint」で二通りに出る。
/// しかも貼り付け可能と謳っている側が狭いほうで、`bbox_argument` 自身の
/// コメントどおり 20MP では 28px 内側に入る。bbox の外は色によらず背景と
/// 確定されるため、その差はそのまま商品の欠けになる。
fn print_subject(subject: Option<&SubjectReport>) {
    let Some(s) = subject else {
        return;
    };
    println!(
        "  主体候補  {}  (面積 {:.1}%, 信頼度 {})",
        bbox_argument(s.normalized_bbox),
        s.area_ratio * 100.0,
        match s.confidence {
            Confidence::High => "high",
            Confidence::Low => "low",
        }
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

/// 人間向けの警告表示。
///
/// `code` はテキストには出さない。JSON が持っている以上ここでは冗長で、
/// 読み手（人間）には文言のほうが速い。`hint` は次の行へインデントして出す。
/// 「何が起きたか」と「次に何をするか」を 1 行に詰めると、後者が読み飛ばされる。
fn print_warnings(warnings: &[Warning]) {
    for w in warnings {
        eprintln!("警告: {}", w.message);
        if let Some(hint) = &w.hint {
            eprintln!("      {hint}");
        }
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
