//! 悪い区間だけ境界を解き直す（`--edge-repair auto`）のベンチ。
//!
//! `tests/real_backgrounds.rs` と同じ 27 点（合成背景の S1〜S12 を既定値で、
//! 実写背景の R1〜R7 を defaults / assisted（指示つきは trimap / polygon も））を、
//! off と auto で回して正解由来の誤差を並べる。

mod common;

use std::time::Instant;

use common::{EdgeMetrics, edge_scene, edge_scenes, measure_edges_with, real_scene, real_scenes};
use kiri::cutout::edge_repair::{self, Bands};
use kiri::cutout::{CutoutOptions, Diagnostics, cutout};
use kiri::warning::WarningCode;

struct Run {
    warnings: Vec<String>,
    metrics: EdgeMetrics,
    diagnostics: Diagnostics,
    windows: usize,
    ms: f64,
}

/// 1 回目を切り、`bands` を渡せば `edge_repair::repair` を通す（本体と同じ経路）。
fn run(
    image: &image::RgbaImage,
    opts: &CutoutOptions,
    bands: Option<&Bands>,
    truth: &common::EdgeTruth,
) -> Run {
    let started = Instant::now();
    let first = cutout(image, opts);
    let warnings: Vec<String> = first
        .warnings
        .iter()
        .map(|w| w.code.as_str().to_string())
        .collect();
    let (result, windows) = match bands {
        None => (first, 0),
        Some(bands) => {
            let (result, outcome) = edge_repair::repair(image, opts, first, None, bands);
            (result, outcome.windows.len())
        }
    };
    let ms = started.elapsed().as_secs_f64() * 1000.0;
    Run {
        warnings,
        metrics: measure_edges_with(
            truth,
            &result.image,
            &result.mask,
            opts.bbox,
            opts.constraints.as_ref(),
        ),
        diagnostics: result.diagnostics,
        windows,
        ms,
    }
}

fn each_point(mut visit: impl FnMut(&str, &CutoutOptions, &common::EdgeTruth)) {
    for scene in edge_scenes() {
        let truth = edge_scene(&scene);
        visit(
            &format!("{} / 既定", scene.name),
            &CutoutOptions::default(),
            &truth,
        );
    }
    for scene in real_scenes() {
        let truth = real_scene(&scene);
        let bbox = common::assisted_bbox(&truth);
        let mut settings = vec![
            ("defaults", CutoutOptions::default()),
            (
                "assisted",
                CutoutOptions {
                    bbox: Some(bbox),
                    tolerance: scene.assisted_tolerance,
                    ..Default::default()
                },
            ),
        ];
        if scene.spatial {
            settings.push((
                "trimap",
                CutoutOptions {
                    constraints: Some(common::truth_trimap(&truth)),
                    tolerance: scene.assisted_tolerance,
                    ..Default::default()
                },
            ));
            settings.push((
                "polygon",
                CutoutOptions {
                    constraints: Some(common::truth_polygons(&truth)),
                    tolerance: scene.assisted_tolerance,
                    ..Default::default()
                },
            ));
        }
        for (setting, opts) in settings {
            visit(&format!("{} / {}", scene.name, setting), &opts, &truth);
        }
    }
}

fn show(v: Option<f64>) -> String {
    v.map_or("null".into(), |v| format!("{v:.3}"))
}

/// ```text
/// KIRI_BANDS=bg,fg,pad,blend cargo test --release --test edge_repair -- --ignored --nocapture
/// ```
#[test]
#[ignore = "計測用。判定はせず表を出すだけ"]
fn print_the_edge_repair_bench() {
    let bands = std::env::var("KIRI_BANDS")
        .ok()
        .map(|s| {
            let v: Vec<f64> = s.split(',').map(|x| x.parse().unwrap()).collect();
            Bands {
                background_inset: v[0],
                foreground_inset: v[1],
                pad: v[2],
                blend: v[3],
            }
        })
        .unwrap_or_default();
    println!("\n帯: {bands:?}");
    println!(
        "{:<36} {:>4} | {:>6} {:>6} | {:>6} {:>6} | {:>7} {:>7} | {:>6} {:>6} | {:>6} {:>6} | {:>6} {:>6}",
        "シーン / 設定",
        "窓",
        "誤差",
        "→",
        "rim真",
        "→",
        "eaten",
        "→",
        "粗さ",
        "→",
        "汚染",
        "→",
        "ms",
        "→"
    );
    let mut sum = [0.0f64; 4];
    // 壊れていない点（R の defaults と S9 を除く）だけの集計。壊れた切り抜きの
    // 縁を直しても切り抜きは直らないので、そこで動く数を混ぜると傾向が読めない
    let mut kept = [0.0f64; 6];
    let mut worst_eaten = (0.0f32, String::new());
    let mut worse = Vec::new();
    let quiet = std::env::var("KIRI_SUMMARY").is_ok();
    each_point(|label, opts, truth| {
        let image = &truth.image;
        let off = run(image, opts, None, truth);
        let auto = run(image, opts, Some(&bands), truth);
        sum[0] += f64::from(off.metrics.contour_error);
        sum[1] += f64::from(auto.metrics.contour_error);
        sum[2] += off.ms;
        sum[3] += auto.ms;
        let broken = label.ends_with("defaults") || label.starts_with("S9");
        if !broken {
            kept[0] += f64::from(off.metrics.contour_error);
            kept[1] += f64::from(auto.metrics.contour_error);
            kept[2] += f64::from(off.metrics.rim_truth);
            kept[3] += f64::from(auto.metrics.rim_truth);
            kept[4] += f64::from(off.metrics.eaten);
            kept[5] += f64::from(auto.metrics.eaten);
            let grew = auto.metrics.eaten - off.metrics.eaten;
            if grew > worst_eaten.0 {
                worst_eaten = (grew, label.to_string());
            }
            if auto.metrics.contour_error > off.metrics.contour_error + 0.05 {
                worse.push(label.to_string());
            }
        }
        if quiet {
            return;
        }
        println!(
            "{:<36} {:>4} | {:>6.2} {:>6.2} | {:>6.3} {:>6.3} | {:>7.4} {:>7.4} | {:>6} {:>6} | {:>6} {:>6} | {:>6.0} {:>6.0}",
            label,
            auto.windows,
            off.metrics.contour_error,
            auto.metrics.contour_error,
            off.metrics.rim_truth,
            auto.metrics.rim_truth,
            off.metrics.eaten,
            auto.metrics.eaten,
            show(off.diagnostics.contour_roughness),
            show(auto.diagnostics.contour_roughness),
            show(off.diagnostics.rim_contamination),
            show(auto.diagnostics.rim_contamination),
            off.ms,
            auto.ms,
        );
        println!("    {}", off.warnings.join(","));
    });
    println!(
        "合計: 誤差 {:.2} → {:.2} / 時間 {:.0}ms → {:.0}ms",
        sum[0], sum[1], sum[2], sum[3]
    );
    println!(
        "壊れていない点: 誤差 {:.2} → {:.2} / rim真 {:.3} → {:.3} / eaten {:.4} → {:.4} \
         / eaten の最大の増え {:.4}（{}）/ 誤差が悪化 {:?}",
        kept[0], kept[1], kept[2], kept[3], kept[4], kept[5], worst_eaten.0, worst_eaten.1, worse
    );
}

fn scene(name: &str) -> common::RealScene {
    real_scenes()
        .into_iter()
        .find(|s| s.name.starts_with(name))
        .unwrap_or_else(|| panic!("{name} がベンチに無い"))
}

/// 実写の不織布 + 黒商品（R1 assisted）で、解き直しは正解との誤差を減らし、
/// 商品を食わない。
///
/// **欠けを見るのが要点である。** 不明帯を輪郭の内側に置くので、2 回目が
/// 商品の縁を背景と取り違えれば欠けとして出る。誤差だけを見ていると、
/// 輪郭が内へ寄りすぎたことを「縁の汚れが減った」と読んでしまう。
#[test]
fn the_repair_moves_the_contour_onto_the_truth_without_eating_the_product() {
    let s = scene("R1");
    let truth = real_scene(&s);
    let opts = CutoutOptions {
        bbox: Some(common::assisted_bbox(&truth)),
        tolerance: s.assisted_tolerance,
        ..Default::default()
    };
    let off = run(&truth.image, &opts, None, &truth);
    let auto = run(&truth.image, &opts, Some(&Bands::default()), &truth);
    assert!(auto.windows > 0, "前提: 区間がある");
    assert!(
        auto.metrics.contour_error < off.metrics.contour_error * 0.8,
        "誤差が減っていない: {} → {}",
        off.metrics.contour_error,
        auto.metrics.contour_error
    );
    assert!(
        auto.metrics.rim_truth < off.metrics.rim_truth * 0.8,
        "縁の汚れが減っていない: {} → {}",
        off.metrics.rim_truth,
        auto.metrics.rim_truth
    );
    assert!(
        auto.metrics.eaten <= off.metrics.eaten + 0.001,
        "商品を食った: {} → {}",
        off.metrics.eaten,
        auto.metrics.eaten
    );
    let (before, after) = (
        off.diagnostics.contour_roughness.unwrap(),
        auto.diagnostics.contour_roughness.unwrap(),
    );
    assert!(after < before, "指標が動いていない: {before} → {after}");
}

/// 切り抜きそのものが外れているときは解き直さない。結果は 1 回目そのもの。
///
/// S9（明度が背景を横切る淡色商品）は `HALO_REMAINS` を出す。門が無かった
/// ときは解き直して輪郭の誤差が 20.3 → 27.6 へ悪化した。
#[test]
fn a_failed_cut_comes_back_untouched() {
    let s9 = edge_scenes()
        .into_iter()
        .find(|s| s.name.starts_with("S9"))
        .unwrap();
    let truth = edge_scene(&s9);
    let opts = CutoutOptions::default();
    let first = cutout(&truth.image, &opts);
    let (mask, image) = (first.mask.clone(), first.image.clone());
    let (result, outcome) =
        edge_repair::repair(&truth.image, &opts, first, None, &Bands::default());
    assert_eq!(outcome.gated_by, Some(WarningCode::HaloRemains));
    assert!(outcome.windows.is_empty());
    assert_eq!(result.mask.as_slice(), mask.as_slice());
    assert_eq!(result.image.as_raw(), image.as_raw());
}

/// 解き直した結果が門の警告を新たに出したら、1 回目へ戻す。結果は 1 回目そのもの。
///
/// 実写のキーボード（`--segment isnet`）で起きる。手持ちの 27 点には起きる点が
/// 無いので、判定だけを差し替えて戻す経路を通す。
#[test]
fn a_repair_that_raises_a_gate_is_taken_back() {
    let s = scene("R1");
    let truth = real_scene(&s);
    let opts = CutoutOptions {
        bbox: Some(common::assisted_bbox(&truth)),
        tolerance: s.assisted_tolerance,
        ..Default::default()
    };
    let first = cutout(&truth.image, &opts);
    let (mask, image) = (first.mask.clone(), first.image.clone());
    let codes: Vec<WarningCode> = first.warnings.iter().map(|w| w.code).collect();
    let before = first.diagnostics.clone();
    let (result, outcome) = edge_repair::repair_with(
        &truth.image,
        &opts,
        first,
        None,
        &Bands::default(),
        |_, _| Some(WarningCode::HaloRemains),
    );
    assert_eq!(outcome.reverted_by, Some(WarningCode::HaloRemains));
    assert_eq!(outcome.gated_by, None);
    assert!(outcome.windows.is_empty());
    assert_eq!(outcome.before, before);
    assert_eq!(outcome.after, before);
    assert_eq!(result.mask.as_slice(), mask.as_slice());
    assert_eq!(result.image.as_raw(), image.as_raw());
    assert_eq!(
        result.warnings.iter().map(|w| w.code).collect::<Vec<_>>(),
        codes
    );
}

/// 効いている解き直しは戻さない（R1 assisted は門の警告を新たに出さない）。
#[test]
fn a_repair_that_helps_is_kept() {
    let s = scene("R1");
    let truth = real_scene(&s);
    let opts = CutoutOptions {
        bbox: Some(common::assisted_bbox(&truth)),
        tolerance: s.assisted_tolerance,
        ..Default::default()
    };
    let first = cutout(&truth.image, &opts);
    let (_, outcome) = edge_repair::repair(&truth.image, &opts, first, None, &Bands::default());
    assert_eq!(outcome.reverted_by, None);
    assert!(!outcome.windows.is_empty());
}

/// 解き直しは決定的である。
#[test]
fn the_repair_is_deterministic() {
    let s = scene("R2");
    let truth = real_scene(&s);
    let opts = CutoutOptions {
        bbox: Some(common::assisted_bbox(&truth)),
        tolerance: s.assisted_tolerance,
        ..Default::default()
    };
    let once = edge_repair::repair(
        &truth.image,
        &opts,
        cutout(&truth.image, &opts),
        None,
        &Bands::default(),
    );
    let twice = edge_repair::repair(
        &truth.image,
        &opts,
        cutout(&truth.image, &opts),
        None,
        &Bands::default(),
    );
    assert!(!once.1.windows.is_empty(), "前提: 区間がある");
    assert_eq!(once.1, twice.1);
    assert_eq!(once.0.mask.as_slice(), twice.0.mask.as_slice());
    assert_eq!(once.0.image.as_raw(), twice.0.image.as_raw());
}

fn cli(input: &std::path::Path, out: &std::path::Path, extra: &[&str]) -> serde_json::Value {
    let mut args = vec![
        "cutout",
        input.to_str().unwrap(),
        "-o",
        out.to_str().unwrap(),
        "--force",
        "--json",
    ];
    args.extend_from_slice(extra);
    // **モデルの無い機械と同じ条件で回す。** `--auto` は探索しても致命の警告が
    // 残るとモデルへ昇格するので、置いてある機械でだけ結果が変わる検査にしない
    // （昇格そのものは `tests/segment.rs` が見る）
    let run = common::kiri()
        .args(&args)
        .env(
            kiri::segment::model::KIRI_MODEL_DIR,
            input.parent().unwrap(),
        )
        .output()
        .unwrap();
    assert!(
        run.status.success(),
        "{}",
        String::from_utf8_lossy(&run.stderr)
    );
    common::json_stdout(&run)
}

fn without_elapsed(mut v: serde_json::Value) -> serde_json::Value {
    v.as_object_mut().unwrap().remove("elapsed_ms");
    for o in v["outputs"].as_array_mut().unwrap() {
        o.as_object_mut().unwrap().remove("path");
    }
    v
}

/// `--edge-repair off` は渡さないのと 1 バイトも違わない。auto でも直す区間が
/// 無ければ成果物は同じで、増えるのは `edge_repair` ブロックだけである。
#[test]
fn off_changes_nothing_and_auto_without_regions_only_adds_the_block() {
    let dir = tempfile::TempDir::new().unwrap();
    let img = common::product_image(&common::ProductSpec {
        noise: false,
        ..Default::default()
    });
    let input = common::write_png(dir.path(), "clean.png", &img);
    let (a, b, c) = (
        dir.path().join("a.png"),
        dir.path().join("b.png"),
        dir.path().join("c.png"),
    );
    let plain = cli(&input, &a, &[]);
    let off = cli(&input, &b, &["--edge-repair", "off"]);
    let auto = cli(&input, &c, &["--edge-repair", "auto"]);
    assert!(plain.get("edge_repair").is_none());
    assert_eq!(without_elapsed(plain.clone()), without_elapsed(off));
    let block = &auto["edge_repair"];
    assert_eq!(block["repaired"], 0, "{block}");
    assert_eq!(block["windows"].as_array().unwrap().len(), 0);
    assert!(block["gated_by"].is_null());
    assert!(block["reverted_by"].is_null());
    assert_eq!(block["before"], block["after"]);
    let mut auto = without_elapsed(auto);
    auto.as_object_mut().unwrap().remove("edge_repair");
    assert_eq!(without_elapsed(plain), auto);
    assert_eq!(std::fs::read(&a).unwrap(), std::fs::read(&c).unwrap());
}

/// 直したときは、`mask.*` が差し替えた後の値で、`edge_repair.after` と一致する。
#[test]
fn the_block_reports_what_the_mask_now_says() {
    let dir = tempfile::TempDir::new().unwrap();
    let s = scene("R1");
    let truth = real_scene(&s);
    let input = common::write_png(dir.path(), "r1.png", &truth.image);
    let (x1, y1, x2, y2) = common::assisted_bbox(&truth);
    let bbox = format!("{x1},{y1},{x2},{y2}");
    let tolerance = s.assisted_tolerance.to_string();
    let v = cli(
        &input,
        &dir.path().join("out.png"),
        &[
            "--bbox",
            &bbox,
            "--tolerance",
            &tolerance,
            "--edge-repair",
            "auto",
        ],
    );
    let block = &v["edge_repair"];
    let repaired = block["repaired"].as_u64().unwrap();
    assert!(repaired > 0, "{block}");
    assert_eq!(block["windows"].as_array().unwrap().len() as u64, repaired);
    for key in ["halo_ratio", "contour_roughness", "rim_contamination"] {
        assert_eq!(block["after"][key], v["mask"][key], "{key}");
    }
    assert!(
        block["after"]["contour_roughness"].as_f64().unwrap()
            < block["before"]["contour_roughness"].as_f64().unwrap()
    );
}

/// **`--optimize --edge-repair auto` と同じ成果物を、解き直しが働く場面でも返す。**
///
/// 致命の警告で昇格する場面は門で解き直しが見送られるので、「同じ」のうち
/// 解き直しの側はここでしか確かめられない。矩形と tolerance は明示するので、
/// 探索に残る軸は背景の持ち方だけである。
#[test]
fn auto_reaches_the_repair_and_matches_the_explicit_path() {
    let dir = tempfile::TempDir::new().unwrap();
    let s = scene("R1");
    let truth = real_scene(&s);
    let input = common::write_png(dir.path(), "r1.png", &truth.image);
    let (x1, y1, x2, y2) = common::assisted_bbox(&truth);
    let bbox = format!("{x1},{y1},{x2},{y2}");
    let tolerance = s.assisted_tolerance.to_string();
    let given = ["--bbox", &bbox, "--tolerance", &tolerance];

    let auto = dir.path().join("auto.png");
    let explicit = dir.path().join("explicit.png");
    let v = cli(&input, &auto, &[&given[..], &["--auto"]].concat());
    cli(
        &input,
        &explicit,
        &[&given[..], &["--optimize", "--edge-repair", "auto"]].concat(),
    );

    // 品質の警告だけで昇格している（致命の警告が無いので門は開いている）
    assert!(v["optimize"]["candidates"].is_array(), "{v}");
    assert!(v["edge_repair"]["repaired"].as_u64().unwrap() > 0, "{v}");
    assert_eq!(
        std::fs::read(&auto).unwrap(),
        std::fs::read(&explicit).unwrap()
    );
    // 明示した軸は昇格しても探索されない
    for candidate in v["optimize"]["candidates"].as_array().unwrap() {
        assert_eq!(candidate["tolerance"], s.assisted_tolerance, "{candidate}");
    }
}

/// **探索しても致命の警告が残ったら、探索の結果ではなく既定の 1 回を返す。**
///
/// きれいな候補が 1 つも無いとき、致命の警告どうしの順位は当てにならない。
/// R3（不織布 + 淡色商品）では探索が選んだ候補は商品を丸ごと消す（正解に対する
/// 欠け 0.04 → 0.9995）。**`--auto` は付けないより悪くなってはならない**——
/// それが崩れると Skill が「最初から付ける」と言えなくなる。
#[test]
fn auto_returns_the_plain_cut_when_the_search_finds_nothing_clean() {
    let dir = tempfile::TempDir::new().unwrap();
    let truth = real_scene(&scene("R3"));
    let input = common::write_png(dir.path(), "r3.png", &truth.image);

    let plain = dir.path().join("plain.png");
    let searched = dir.path().join("searched.png");
    let auto = dir.path().join("auto.png");
    let p = cli(&input, &plain, &["--edge-repair", "auto"]);
    let s = cli(&input, &searched, &["--optimize", "--edge-repair", "auto"]);
    let v = cli(&input, &auto, &["--auto"]);

    // 前提: 探索は既定と違う結果を選び、それでも致命の警告が残る
    assert_ne!(
        std::fs::read(&plain).unwrap(),
        std::fs::read(&searched).unwrap(),
        "素材が前提を満たさない"
    );
    let codes = |v: &serde_json::Value| -> Vec<String> {
        v["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .map(|w| w["code"].as_str().unwrap().to_string())
            .collect()
    };
    assert!(
        codes(&s).iter().any(|c| c == "OPTIMIZE_NO_CLEAN_CANDIDATE"),
        "{:?}",
        codes(&s)
    );
    // 自分で --optimize を渡した実行は、選ばれた候補をそのまま返す
    assert_eq!(s["optimize"]["adopted"], true);

    // --auto は既定の 1 回（に解き直しを掛けたもの）へ戻る
    assert_eq!(
        std::fs::read(&auto).unwrap(),
        std::fs::read(&plain).unwrap()
    );
    // 探索が走った事実は残し、採らなかったことを言う
    assert!(v["optimize"]["candidates"].is_array(), "{v}");
    assert_eq!(v["optimize"]["adopted"], false);
    // settings / mask は返した結果のもの（探索が選んだ候補のものではない）
    for key in ["tolerance", "background_model"] {
        assert_eq!(v["settings"][key], p["settings"][key], "{key}");
    }
    assert_eq!(v["applied_bbox"], p["applied_bbox"]);
    assert_eq!(v["mask"], p["mask"]);
    let warning = v["warnings"]
        .as_array()
        .unwrap()
        .iter()
        .find(|w| w["code"] == "OPTIMIZE_NO_CLEAN_CANDIDATE")
        .unwrap_or_else(|| panic!("{:?}", codes(&v)));
    assert_eq!(warning["data"]["adopted"], false, "{warning}");
}

/// 3 軸とも明示した実行は昇格しない。候補はいま切った 1 つだけなので、
/// 探索は同じ切り抜きをもう 1 度回すだけになる。解き直しは掛かる。
#[test]
fn auto_does_not_search_when_every_axis_is_given() {
    let dir = tempfile::TempDir::new().unwrap();
    let s = scene("R1");
    let truth = real_scene(&s);
    let input = common::write_png(dir.path(), "r1.png", &truth.image);
    let (x1, y1, x2, y2) = common::assisted_bbox(&truth);
    let bbox = format!("{x1},{y1},{x2},{y2}");
    let tolerance = s.assisted_tolerance.to_string();
    let given = [
        "--bbox",
        &bbox,
        "--tolerance",
        &tolerance,
        "--background-model",
        "auto",
    ];

    let auto = dir.path().join("auto.png");
    let repaired = dir.path().join("repaired.png");
    let v = cli(&input, &auto, &[&given[..], &["--auto"]].concat());
    cli(
        &input,
        &repaired,
        &[&given[..], &["--edge-repair", "auto"]].concat(),
    );

    assert!(v.get("optimize").is_none(), "{v}");
    assert!(v["edge_repair"]["repaired"].as_u64().unwrap() > 0, "{v}");
    assert_eq!(
        std::fs::read(&auto).unwrap(),
        std::fs::read(&repaired).unwrap()
    );
}

/// batch の spec でも書ける。綴り違いの値は断る。
#[test]
fn the_batch_spec_takes_edge_repair() {
    let dir = tempfile::TempDir::new().unwrap();
    let img = common::product_image(&common::ProductSpec::default());
    common::write_png(dir.path(), "p.png", &img);
    let spec = |value: &str| {
        let path = dir.path().join("spec.json");
        std::fs::write(
            &path,
            format!(
                r#"{{"defaults":{{"edge_repair":"{value}"}},
                     "items":[{{"input":"p.png","output":"out/p.png"}}]}}"#
            ),
        )
        .unwrap();
        common::kiri()
            .args(["batch", path.to_str().unwrap(), "--json", "--dry-run"])
            .output()
            .unwrap()
    };
    let ok = spec("auto");
    assert!(
        ok.status.success(),
        "{}",
        String::from_utf8_lossy(&ok.stderr)
    );
    assert!(String::from_utf8_lossy(&ok.stdout).contains("\"edge_repair\""));
    assert!(!spec("sometimes").status.success());
}
