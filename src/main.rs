//! kiri のエントリポイント。
//!
//! 出力の規約:
//! - `--json` 指定時は stdout に JSON のみを出す。成功でも失敗でも JSON を返す
//! - 非 JSON 時は人間向けの要約を stdout に、警告とエラーを stderr に出す

use std::process::ExitCode;

use clap::parser::ValueSource;
use clap::{ArgMatches, CommandFactory, FromArgMatches};
use serde::Serialize;

use kiri::cli::{Cli, Command};
use kiri::commands;
use kiri::compliance;
use kiri::cutout::{Confidence, OptimizeFixed, bbox_argument};
use kiri::error::{Error, ErrorCode, ErrorKind, Result};
use kiri::report::{
    BackgroundReport, BatchReport, ComplianceReport, CutoutReport, ErrorReport, InfoReport,
    ModelReport, ProcessReport, SchemaReport, SegmentReport, SettingsReport, SubjectReport,
};
use kiri::warning::Warning;

fn main() -> ExitCode {
    let cli = parse();
    match dispatch(&cli) {
        Ok(code) => ExitCode::from(code as u8),
        Err(e) => {
            emit_error(&e, cli.json);
            ExitCode::from(e.exit_code() as u8)
        }
    }
}

/// 引数を解いたうえで、**利用者が明示した項目**を記録する。
///
/// `Cli::parse()` では足りない。`--tolerance` は `default_value_t` を持つので、
/// 解いた後の値からは「12 を明示した」と「既定のまま」を区別できない。
/// `--optimize` はそこを区別しなければならない——明示した値は探索の軸から外す
/// という規約があり、区別できなければ「30 に固定して探せ」と書いた指定が
/// 12〜60 を舐めてしまう。
///
/// 既定値を `Option` にして区別する手もあるが、それをやると `kiri schema` の
/// `commands[].options[].default` から 12 が消える。**指定しなくても何が効くのかを
/// 読めることは契約そのもの**なので、表示は 1 文字も変えずに、明示したかどうかだけを
/// `ValueSource` から拾って `CutoutArgs::fixed` へ畳む。
fn parse() -> Cli {
    let matches = Cli::command().get_matches();
    // **`from_arg_matches_mut` は使えない。** あちらは組み立てながら
    // `ArgMatches` から値を抜き取るので、終わったあとの `value_source` は
    // どの id についても `None` を返す。clone を 1 つ節約する代わりに、
    // 「明示したかどうか」を知る手段がそこで消える
    let mut cli = match Cli::from_arg_matches(&matches) {
        Ok(cli) => cli,
        // clap 自身の作法で出して終わる。ここに来るのは derive とパーサの
        // 食い違いだけなので、kiri の ErrorCode に混ぜる意味が無い。
        // **`format` を通すのは usage を添えるため**——`Parser::parse()` は
        // 内部で同じことをしており、ここだけ素っ気ない出力にする理由が無い
        Err(e) => e.format(&mut Cli::command()).exit(),
    };
    if let (Command::Cutout(args), Some(sub)) =
        (&mut cli.command, matches.subcommand_matches("cutout"))
    {
        args.fixed = OptimizeFixed {
            tolerance: from_command_line(sub, "tolerance"),
            bbox: from_command_line(sub, "bbox"),
            background_model: from_command_line(sub, "background_model"),
        };
    }
    cli
}

fn from_command_line(matches: &ArgMatches, id: &str) -> bool {
    matches.value_source(id) == Some(ValueSource::CommandLine)
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
            // **`Err` 経路を通さない。** 処理は成功していて成果物も存在する。
            // `ErrorReport` へ差し替えると `outputs[]` も `mask` も消え、
            // 「何が不合格だったか」も「何が書かれたか」も追えなくなる。
            // batch が `failed > 0` で 4 を返しつつ `BatchReport` を出している
            // 既存の形をそのまま踏襲する
            if report.compliance.as_ref().is_some_and(|c| !c.passed) {
                return Ok(ErrorKind::Compliance.exit_code());
            }
        }
        Command::Model(args) => {
            let report = commands::model::run(&args.command)?;
            if cli.json {
                print_json(&report)?;
            } else {
                print_models(&report);
            }
        }
        Command::Schema => {
            let report = commands::schema::run();
            if cli.json {
                print_json(&report)?;
            } else {
                print_schema(&report);
            }
        }
        Command::Batch(args) => {
            let report = commands::batch::run(args)?;
            if cli.json {
                print_json(&report)?;
            } else {
                print_batch(&report);
            }
            // 失敗した項目があれば処理失敗として知らせる。詳細は results[] にある。
            // **4 が 5 に優先する。** 両方あるときに 5 を返すと、成果物が
            // 1 つも無い項目があることが番号から消え、「見れば分かる結果」として
            // 扱われてしまう
            if report.failed > 0 {
                return Ok(ErrorKind::Processing.exit_code());
            }
            if report.rejected > 0 {
                return Ok(ErrorKind::Compliance.exit_code());
            }
        }
    }
    Ok(0)
}

/// 契約の人間向け要約。
///
/// **オプションの一覧はここに出さない。** 7 コマンド分を並べると 100 行を超えて
/// 読めなくなるし、人間には `--help` という専用の入口がある。テキスト出力で
/// 価値があるのは「どんな code が返りうるか」の見通しで、これは `--help` の
/// どこにも無い。完全な契約は `--json` が返す。
fn print_schema(report: &SchemaReport) {
    println!(
        "kiri {}  (schema {})",
        report.kiri_version, report.schema_version
    );

    println!("\nexit code");
    for e in &report.exit_codes {
        println!("  {}  {}", e.code, e.meaning);
    }

    let width = |codes: Vec<&str>| codes.iter().map(|c| c.chars().count()).max().unwrap_or(0);

    println!("\n警告 ({})", report.warnings.len());
    let w = width(report.warnings.iter().map(|e| e.code.as_str()).collect());
    for e in &report.warnings {
        println!("  {:<w$}  {}", e.code.as_str(), e.summary, w = w);
    }

    println!("\nエラー ({})", report.errors.len());
    let w = width(report.errors.iter().map(|e| e.code.as_str()).collect());
    for e in &report.errors {
        println!(
            "  {:<w$}  [{}]  {}",
            e.code.as_str(),
            e.exit_code,
            e.summary,
            w = w
        );
    }

    if !report.global_options.is_empty() {
        let names: Vec<&str> = report
            .global_options
            .iter()
            .map(|o| o.name.as_str())
            .collect();
        println!("\n全コマンドで受ける  {}", names.join("  "));
    }

    println!("\nコマンド");
    for c in &report.commands {
        println!("  {:<9}{}", c.name, c.about.as_deref().unwrap_or(""));
    }

    println!("\nオプションの既定値と綴りは --json が返す（kiri schema --json）");
}

/// モデルの一覧を人間向けに出す。
///
/// **置いていないときこそ役に立つ出力である。** そのまま貼れる `curl` の
/// 1 行を出すのが主目的で、置いてある場合は突き合わせた結果を 1 行で言う。
fn print_models(report: &ModelReport) {
    if !report.segment_available {
        println!(
            "この build には segment 機能がありません（--features segment で入れ直してください）"
        );
    }
    for m in &report.models {
        println!("{}  {}  {}", m.name, human_bytes(m.bytes), m.license);
        println!("  URL       {}", m.url);
        println!("  入力寸法  {}x{}", m.input_size, m.input_size);
        println!("  MD5       {}  (配布元の名乗り)", m.md5);
        println!("  SHA-256   {}  (kiri が検証に使う)", m.sha256);
        println!(
            "  想定パス  {}",
            m.path.as_deref().unwrap_or("(決められません)")
        );
        match (m.verified, m.actual_bytes) {
            (Some(true), _) => println!("  状態      あり・検証済み"),
            // 大きさで弾いた側はダイジェストを持たない。**「合いません」と
            // だけ言うと、利用者は 176MB を舐めた結果だと読む**
            (Some(false), Some(actual)) => println!(
                "  状態      あり・**大きさが違います**（実際 {} / 想定 {}）",
                human_bytes(actual),
                human_bytes(m.bytes)
            ),
            (Some(false), None) => println!(
                "  状態      あり・**ダイジェストが合いません**（実際 {}）",
                m.actual_sha256.as_deref().unwrap_or("?")
            ),
            (None, _) => println!("  状態      なし"),
        }
        if m.verified != Some(true) {
            println!("  取得      {}", m.hint);
        }
    }
}

/// 推論そのものの報告。**走ったときしか出ない。**
fn print_segment(segment: Option<&SegmentReport>) {
    let Some(s) = segment else {
        return;
    };
    println!(
        "  モデル    {} {}x{}  確定 前景 {:.1}% / 背景 {:.1}%  不明 {:.1}%  ({} ms)",
        s.model,
        s.input_size,
        s.input_size,
        s.fg_ratio * 100.0,
        s.bg_ratio * 100.0,
        s.uncertain_ratio * 100.0,
        s.elapsed_ms
    );
}

/// 探索の要約。**走ったときしか出ない。**
///
/// 人間向けには「何通り試して、原寸で何回回して、何が選ばれたか」の 1 行で足りる。
/// 候補の一覧は 20 行になるのでテキストには出さない——比べたい人は `--json` を読む。
fn print_optimize(optimize: Option<&kiri::report::OptimizeReport>, settings: &SettingsReport) {
    let Some(o) = optimize else {
        return;
    };
    let finals = o.candidates.iter().filter(|c| c.stage == "final").count();
    let c = &o.chosen;
    println!(
        "  探索      {} 候補を {}px で試し、原寸で {} 回  ({} ms)",
        o.candidates.len(),
        o.searched_at,
        finals,
        o.elapsed_ms
    );
    // **モデルは効いた値と要求値の両方を出す。** 候補表が持つのは要求値
    // （`auto`）だが、すぐ上の「背景」ブロックは効いた値を語っているので、
    // 効いた値だけを出すと `--background-model auto` を写せる候補表の側と
    // 食い違い、要求値だけを出すと同じ画面で同じ語が 2 つの意味を持つ
    let model = if settings.background_model == c.background_model {
        settings.background_model.to_string()
    } else {
        format!(
            "{}（要求 {}）",
            settings.background_model, c.background_model
        )
    };
    println!(
        "  採用      tolerance {}  bbox {}  background-model {model}  (致命 {} / 品質 {:.2}{})",
        c.tolerance,
        match c.bbox {
            Some([x1, y1, x2, y2]) => format!("{x1},{y1} - {x2},{y2}"),
            None => "なし".to_string(),
        },
        c.score.fatal,
        c.score.quality,
        if c.collapsed {
            " / 商品を飲んだ"
        } else {
            ""
        }
    );
}

fn print_json<T: Serialize>(value: &T) -> Result<()> {
    let text = serde_json::to_string_pretty(value)
        .map_err(|e| Error::new(ErrorCode::JsonEncodeFailed, e.to_string()))?;
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
    print_segment(report.segment.as_ref());
    print_subject(report.subject.as_ref());
    print_warnings(&report.warnings);
}

/// dry-run の行頭に付ける印。
///
/// 人間向けの出力でも「書いていない」を最初に言う。パスとバイト数だけが
/// 並んでいると、ファイルが出来ている前提で次の作業に移ってしまう。
fn dry_run_prefix(dry_run: bool) -> &'static str {
    if dry_run { "[dry-run] " } else { "" }
}

fn print_process(report: &ProcessReport) {
    for out in &report.outputs {
        println!(
            "{}{}  {}x{}  {}  {}  ({} ms)",
            dry_run_prefix(report.dry_run),
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
            "{}{}  {}x{}  {}  {}  ({} ms)",
            dry_run_prefix(report.dry_run),
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
    print_segment(report.segment.as_ref());
    print_optimize(report.optimize.as_ref(), &report.settings);
    // 指示を渡したときだけ 1 行増やす。**どの入口が効いたかまで出す**のは、
    // 渡したはずの入口が並びに無いことが「その指示は空だった」を意味するため
    // （そのときは CONSTRAINT_EMPTY も出る）。
    //
    // 画素数を添えるのは、比率が桁落ちするためである。20MP の 400 画素は
    // 0.0% と表示されるが、指示としては確かに置かれている
    if let Some(c) = &report.constraints {
        println!(
            "  制約      前景 {:.1}% ({} px)  背景 {:.1}% ({} px)  ({})",
            c.fg_ratio * 100.0,
            c.fg_pixels,
            c.bg_ratio * 100.0,
            c.bg_pixels,
            c.sources.join(", ")
        );
    }
    println!("  前景比率  {:.1}%", report.mask.foreground_ratio * 100.0);
    if let Some(sep) = report.mask.separability {
        // tolerance と並べて出す。両者の大小そのものが判断材料であるため
        println!(
            "  境界色差  ΔE {sep:.1}  (tolerance {})",
            report.settings.tolerance
        );
    }
    // 「境界色差」の並びに合わせて、境界の質を語る値をここへ続ける。
    // どちらも長辺 1000px 換算／割合で、しきい値と並べて初めて読める
    if let Some(roughness) = report.mask.contour_roughness {
        println!(
            "  輪郭粗さ  {roughness:.2} px  (1000px 換算, 警告 {:.2} 超)",
            kiri::cutout::diagnostics::CONTOUR_ROUGH_WARN
        );
    }
    if let Some(rim) = report.mask.rim_contamination {
        println!(
            "  縁の汚染  {:.1}%  (警告 {:.1}% 超)",
            rim * 100.0,
            kiri::cutout::diagnostics::RIM_CONTAMINATION_WARN * 100.0
        );
    }
    match report.mask.bbox {
        Some([x1, y1, x2, y2]) => println!("  前景範囲  {x1},{y1} - {x2},{y2}"),
        None => println!("  前景範囲  なし"),
    }
    if report.mask.touches_edge {
        println!("  外周接触  あり");
    }
    // 切り抜き → 回転 → キャンバス の順に並べる。**`前景範囲` は回す前の
    // 座標**なので、そのすぐ後に「この後で回した」と言う位置がここである
    if let Some(r) = &report.rotate {
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
    print_compliance(report.compliance.as_ref());
    print_warnings(&report.warnings);
}

/// 合否を人間向けに出す。**`--fail-on` を渡したときだけ 1 行以上増える。**
///
/// 落ちた条件は 1 行に 1 つ、実測値としきい値を並べて出す。まとめて
/// 「不合格です」とだけ言うと、次に何を直せばよいかが JSON を読むまで分からない。
/// 通った条件は数だけを言う——**見た上で通ったこと**は伝わるが、20 行を
/// 埋める価値は無い（内訳は `compliance.checks[]` が全部持っている）。
fn print_compliance(compliance: Option<&ComplianceReport>) {
    let Some(c) = compliance else {
        return;
    };
    println!(
        "  規格      {}  (--fail-on {})",
        if c.passed { "合格" } else { "**不合格**" },
        c.fail_on
    );
    for check in &c.checks {
        if check.status == compliance::PASS {
            continue;
        }
        let detail = match (
            check.status,
            &check.actual,
            check.operator,
            &check.threshold,
        ) {
            (compliance::UNMEASURABLE, _, _, _) => "測れず".to_string(),
            (_, Some(actual), Some(op), Some(threshold)) => {
                format!("{actual} {} {threshold}", compliance::symbol_of(op))
            }
            // 固定のしきい値を持たない条件（NOT_SEPARABLE、外周接触）は
            // 実測値だけを出す。比べた相手は警告の message が言う
            (_, Some(actual), _, _) => actual.to_string(),
            _ => String::new(),
        };
        println!(
            "    x {} {}{}",
            check.name,
            detail,
            match check.code {
                Some(code) => format!("  {}", code.as_str()),
                None => String::new(),
            }
        );
    }
}

fn print_batch(report: &BatchReport) {
    for item in &report.results {
        match (&item.result, &item.error) {
            (Some(r), _) => {
                // **不合格は警告より強い印にする。** `!` は「目視で確かめて
                // ほしい」で、`R` は「条件に照らして落ちた」である。同じ印に
                // すると、数百行の中で仕分ける手がかりが 1 つ減る
                let mark = match (item.status, r.warnings.is_empty()) {
                    (kiri::commands::batch::REJECTED, _) => "R",
                    (_, true) => " ",
                    (_, false) => "!",
                };
                // **派生を全部並べる。** 1 本目だけを出していた頃は、
                // `--sizes` を渡した実行で書かれたファイルの大半が画面から
                // 消えていた。行そのものにも印を付けるのは、サマリが数百行の
                // 後ろにあり、途中の 1 行だけを見た目には成果物が出来ている
                // ように読めるためである。**行頭の印は項目ごとの警告を指す**
                // ので、同じ項目の派生には同じ印が並ぶ
                for out in &r.outputs {
                    println!(
                        "{mark} {}{}  {}x{}  {}",
                        dry_run_prefix(r.dry_run),
                        out.path,
                        out.width,
                        out.height,
                        human_bytes(out.bytes)
                    );
                }
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
                eprintln!(
                    "  エラー [{}]: {}: {}",
                    item.input,
                    e.code.as_str(),
                    e.message
                );
            }
            _ => {}
        }
    }
    // 実行全体に掛かる警告はサマリの直前に出す。項目の行に混ぜると
    // 「どの項目の話か」に読めてしまう（`MANIFEST_PARTIAL` はどの項目の話でもない）
    print_warnings(&report.warnings);
    println!(
        "\n{}{} 件中 {} 件成功、{} 件失敗、{} 件不合格、{} 件に警告  ({} ms)",
        dry_run_prefix(report.dry_run),
        report.total,
        report.succeeded,
        report.failed,
        report.rejected,
        report.with_warnings,
        report.elapsed_ms
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
        "  主体候補  {}  (面積 {:.1}%, 信頼度 {}, {} 由来)",
        bbox_argument(s.normalized_bbox),
        s.area_ratio * 100.0,
        match s.confidence {
            Confidence::High => "high",
            Confidence::Low => "low",
        },
        s.source
    );
    // **傾きは測れたときだけ 1 行増やす。** 測れなかった（丸いものなど）を
    // 「0 度」と書くと、傾いていないと測り切ったように読める
    if let Some(deg) = s.level_rotation {
        println!("  傾き      --rotate {deg} で水平になる");
    }
}

fn print_perimeter(bg: &BackgroundReport) {
    let d = &bg.perimeter_delta_e;
    println!(
        "  外周ΔE    p50 {:.1}  p90 {:.1}  max {:.1}",
        d.p50, d.p90, d.max
    );
    // 照明場を使ったときだけ 1 行増やす。1 色なら残差は上の行と同じ値になり、
    // 同じ数を二通りで見せることにしかならない
    if bg.model == "field" {
        let r = &bg.residual;
        println!(
            "  場の残差  p50 {:.1}  p90 {:.1}  max {:.1}  (場の振れ幅 ΔE {:.1}〜{:.1})",
            r.p50, r.p90, r.max, bg.field_range[0], bg.field_range[1]
        );
    }
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
