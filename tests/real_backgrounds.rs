//! 実写背景のベンチと、新しい 2 つの診断値の較正。
//!
//! **合成背景では実写の不織布を再現できない。** `EdgeScene` の織り目は周期も
//! 振幅も一定の正弦の積で、照明ムラ・しわ・繊維の向きを持たない。design.md も
//! 「周期 8px では再現しない」と認めているとおり、崩れが出るかどうかが周期の
//! 選び方に依存してしまう。
//!
//! そこで**背景だけを実写にする**。テクスチャは本物のまま、商品の輪郭には
//! 解析的な正解（被覆率と符号付き距離）が残るので、
//!
//! - `contour_roughness` を正解版の `contour_error` と
//! - `rim_contamination` を正解版の `rim_truth` と
//!
//! 突き合わせられる。**指標が正解由来の誤差と相関しなければ、その指標は欠陥では
//! ない何かを測っている。** 以降の Phase（トライマップ、matting、背景モデル）は
//! この数値が改善したかで合否を決めるので、ここが狂っていれば全部が狂う。

mod common;

use common::{
    EdgeMetrics, EdgeScene, edge_scene, edge_scenes, measure_edges, real_scenes, run_real,
};
use kiri::cutout::diagnostics::{CONTOUR_ROUGH_WARN, RIM_CONTAMINATION_WARN};
use kiri::cutout::{CutoutOptions, Diagnostics, cutout};

/// ベンチの 1 点。シーンと設定の組。
struct Point {
    label: String,
    metrics: EdgeMetrics,
    diagnostics: Diagnostics,
    warnings: Vec<String>,
}

/// S シーンを既定値で、R シーンを defaults / assisted で回した全点。
///
/// **1 つの関数にまとめてある。** 相関も較正もこの一覧の上で決まるので、
/// 測る対象がテストごとに違うと「どのテストが何を根拠にしているか」が
/// 追えなくなる。
fn bench() -> Vec<Point> {
    let mut points = Vec::new();
    for scene in edge_scenes() {
        let truth = edge_scene(&scene);
        let result = cutout(&truth.image, &CutoutOptions::default());
        points.push(Point {
            label: format!("{} / 既定", scene.name),
            metrics: measure_edges(&truth, &result.image, &result.mask),
            diagnostics: result.diagnostics.clone(),
            warnings: result
                .warnings
                .iter()
                .map(|w| w.code.as_str().to_string())
                .collect(),
        });
    }
    for scene in real_scenes() {
        let (_, runs) = run_real(&scene);
        for run in runs {
            points.push(Point {
                label: format!("{} / {}", scene.name, run.setting),
                metrics: run.metrics,
                diagnostics: run.diagnostics,
                warnings: run.warnings,
            });
        }
    }
    points
}

fn point<'a>(points: &'a [Point], label: &str) -> &'a Point {
    points
        .iter()
        .find(|p| p.label.starts_with(label))
        .unwrap_or_else(|| panic!("{label} がベンチに無い"))
}

/// 単一のシーンを回して診断値だけを取る。
fn diagnose_scene(scene: &EdgeScene) -> Diagnostics {
    let truth = edge_scene(scene);
    cutout(&truth.image, &CutoutOptions::default()).diagnostics
}

/// 指標が欠陥を見ていること。
///
/// **これが Phase 1 の目的そのものである。** 実写（不織布の上の黒いリモコン）は
/// `halo_ratio` 0.001 / `separability` 54.7 という合格の数値を返しながら、
/// 拡大すると輪郭がギザギザで繊維が張り付いていた。R1 はその構図を、正解を
/// 持った形で縮めたものである。
///
/// 「S1 既定の 3 倍以上」で比べるのは、絶対値の基準を先に決めてしまうと
/// 較正をテストに書き写すことになるためである。**きれいなシーンとの比**なら、
/// 較正が動いても意味が変わらない。
#[test]
fn the_new_diagnostics_see_the_defect_that_the_old_ones_missed() {
    let points = bench();
    let clean = point(&points, "S1");
    let defect = point(&points, "R1 不織布 + 黒商品 / assisted");

    let ratio = |value: f64, base: f64| -> bool { value >= base * 3.0 };

    // 正解側。輪郭が真の位置からどれだけ離れているかは、実写背景では桁が違う
    assert!(
        ratio(
            f64::from(defect.metrics.contour_error),
            f64::from(clean.metrics.contour_error)
        ),
        "正解側の輪郭誤差が離れていない: R1 {:.2} vs S1 {:.2}",
        defect.metrics.contour_error,
        clean.metrics.contour_error
    );

    // 診断側。**S1 が 0 のときは比では語れない**ので絶対値で見る
    let (a, b) = (
        defect.diagnostics.rim_contamination.expect("帯がある"),
        clean.diagnostics.rim_contamination.expect("帯がある"),
    );
    assert!(
        ratio(a, b) || a >= 0.05,
        "縁の汚染が S1 と変わらない: R1 {a:.4} vs S1 {b:.4}"
    );
    assert!(
        a > RIM_CONTAMINATION_WARN,
        "実写背景の縁の汚染が警告に届かない: {a:.4}"
    );
    assert!(
        defect.warnings.contains(&"RIM_CONTAMINATED".to_string()),
        "警告が出ていない: {:?}",
        defect.warnings
    );
}

/// 輪郭の粗さが実写背景でだけ跳ねること。
///
/// **既定値の側で見る。** bbox と tolerance で救った後（assisted）でも実写背景の
/// 輪郭は蛇行しているが（R1 assisted 1.00）、蛇行が桁で出るのは既定値のまま
/// 布が前景として残っている状態のほう（R1 defaults 1.80、R4 defaults 14.00）で、
/// 「合成のきれいなシーンとの比」で語るならそちらが素直である。
#[test]
fn the_contour_roughness_rises_only_on_a_real_background() {
    let points = bench();
    let clean = point(&points, "S1")
        .diagnostics
        .contour_roughness
        .expect("輪郭がある");
    for label in [
        "R1 不織布 + 黒商品 / defaults",
        "R4 暗い机 + 白商品 / defaults",
    ] {
        let p = point(&points, label);
        let rough = p.diagnostics.contour_roughness.expect("輪郭がある");
        assert!(
            rough >= (clean * 3.0).max(0.05),
            "{label}: 粗さが S1 既定（{clean:.2}）と変わらない: {rough:.2}"
        );
        assert!(
            p.warnings.contains(&"CONTOUR_ROUGH".to_string()),
            "{label}: 警告が出ていない: {:?}",
            p.warnings
        );
        // 正解側も同じ向きに動いていること。片方だけなら測り方の問題である
        assert!(
            p.metrics.contour_error >= point(&points, "S1").metrics.contour_error * 3.0,
            "{label}: 正解側の輪郭誤差が動いていない: {:.2}",
            p.metrics.contour_error
        );
    }
}

/// きれいに解けているシーンで誤警報しないこと。
///
/// **しきい値の半分にも届かないこと**を求める。ぎりぎり下回っているだけなら、
/// JPEG の量子化や 1 画素の揺らぎで警告が出たり出なかったりする。
#[test]
fn a_clean_scene_never_raises_either_warning() {
    for name in ["S1", "S2", "S6", "S8"] {
        let scene = edge_scenes()
            .into_iter()
            .find(|s| s.name.starts_with(name))
            .unwrap();
        let d = diagnose_scene(&scene);
        let rough = d.contour_roughness.expect("輪郭がある");
        let rim = d.rim_contamination.expect("帯がある");
        assert!(
            rough <= CONTOUR_ROUGH_WARN / 2.0,
            "{name}: 粗さがしきい値に近い: {rough:.3} (警告 {CONTOUR_ROUGH_WARN})"
        );
        assert!(
            rim <= RIM_CONTAMINATION_WARN / 2.0,
            "{name}: 縁の汚染がしきい値に近い: {rim:.4} (警告 {RIM_CONTAMINATION_WARN})"
        );
    }
}

/// 実写の照明とノイズを持ちながら欠陥の無いシーンで、どちらの警告も出ないこと。
///
/// **誤警報側の較正が合成の S シーンだけに依っていると、しきい値が
/// 「実写であること」に反応していても気づけない。** R7 は手持ちの不織布を
/// ぼかして繊維だけを消したもので、照明勾配と実写のノイズ床はそのまま残る。
/// bbox と tolerance を与えた `assisted` では正解側も `contour_error` 0.41 /
/// `rim_truth` 0.000 と完全に解けており、ここで警告が出れば**それは
/// 誤警報以外の何物でもない**。
///
/// **`defaults` は対象にしない。** 手持ちの実写背景はどれも照明の起伏が
/// 大きく（R7 の外周 ΔE は p90 16.7 / max 33.2）、既定の tolerance 12 は
/// 推定背景色からの絶対的な色差の上限なので、背景の隅まで届かない。実測でも
/// 前景比率 0.469（正解 0.375）、`contour_error` 65.1、`rim_truth` 0.655 で、
/// **切り抜きそのものが失敗している**。そこで出る警告は誤警報ではなく正しい。
/// ぼかしを σ 24 まで強めても、低周波の照明むらは残るので結論は変わらなかった。
#[test]
fn a_clean_real_lit_scene_raises_neither_warning() {
    let points = bench();
    let p = point(&points, "R7 照明勾配のある紙 + 黒商品 / assisted");
    let rough = p.diagnostics.contour_roughness.expect("輪郭がある");
    let rim = p.diagnostics.rim_contamination.expect("帯がある");
    // 正解側が「欠陥が無い」と言っていることを先に確かめる。ここが崩れたら
    // 警告が出ないことを固定しても意味が無い
    assert!(
        p.metrics.contour_error < 1.0 && p.metrics.rim_truth < 0.01,
        "R7 assisted が clean な点ではなくなっている: 輪郭誤差 {:.2} / rim 正解 {:.3}",
        p.metrics.contour_error,
        p.metrics.rim_truth
    );
    assert!(
        rough <= CONTOUR_ROUGH_WARN / 2.0,
        "実写の照明で粗さがしきい値に近い: {rough:.3} (警告 {CONTOUR_ROUGH_WARN})"
    );
    assert!(
        rim <= RIM_CONTAMINATION_WARN / 2.0,
        "実写の照明で縁の汚染がしきい値に近い: {rim:.4} (警告 {RIM_CONTAMINATION_WARN})"
    );
    assert!(
        !p.warnings.contains(&"CONTOUR_ROUGH".to_string())
            && !p.warnings.contains(&"RIM_CONTAMINATED".to_string()),
        "clean な実写照明のシーンで警告が出た: {:?}",
        p.warnings
    );
}

/// 幅 3px のストラップを「粗い輪郭」と言わないこと。
///
/// 平滑化参照は σ = 2 × scale px でぼかすので、**3px の細部は参照から消える**。
/// 原理的にこの指標の苦手な側にあり、較正がずれれば真っ先にここが誤警報する。
/// 合成 S5（一様背景の 3px ストラップ）で警告が出ないことを固定する。
#[test]
fn a_three_pixel_strap_is_not_called_a_rough_contour() {
    let scene = edge_scenes()
        .into_iter()
        .find(|s| s.name.starts_with("S5 "))
        .unwrap();
    let truth = edge_scene(&scene);
    let result = cutout(&truth.image, &CutoutOptions::default());
    let rough = result.diagnostics.contour_roughness.expect("輪郭がある");
    let codes: Vec<&str> = result.warnings.iter().map(|w| w.code.as_str()).collect();
    assert!(
        !codes.contains(&"CONTOUR_ROUGH"),
        "3px のストラップが粗い輪郭と判定された（roughness={rough:.3}）: {codes:?}"
    );
}

/// 診断値が正解由来の誤差と同じ向きに動くこと。
///
/// **相関しなければ、指標は欠陥ではない何かを測っている。** 値そのものではなく
/// 順位で見るのは、両者の単位も分布も違うためである（輪郭誤差は px、粗さも px
/// だが基準が違い、実写背景の既定値では 100px を超える）。
///
/// Spearman は自前で書く。依存を足すほどの計算ではない。
#[test]
fn the_diagnostics_track_the_truth() {
    let points = bench();
    for (name, samples) in correlated(&points) {
        assert!(
            samples.len() >= 20,
            "{name}: 標本が足りない: {}",
            samples.len()
        );
        let rho = spearman(&samples);
        assert!(
            rho >= 0.7,
            "{name}: 正解と相関していない: ρ={rho:.3} ({} 点)",
            samples.len()
        );
    }
}

/// 同じ入力から同じ結果が出ること。
///
/// 新しい 2 つの診断値は距離変換と箱ぼかしを通るので、実装によっては
/// 走査順に依存しうる。**決定性は契約である**（`edge_quality.rs` の
/// `the_same_input_produces_the_same_bytes` と同じ理由）。
#[test]
fn a_real_background_scene_is_deterministic() {
    let scene = real_scenes()
        .into_iter()
        .find(|s| s.name.starts_with("R1"))
        .unwrap();
    let truth = common::real_scene(&scene);
    let opts = CutoutOptions::default();
    let a = cutout(&truth.image, &opts);
    let b = cutout(&truth.image, &opts);
    assert_eq!(a.image.as_raw(), b.image.as_raw(), "出力画像が一致しない");
    assert_eq!(a.mask, b.mask, "マスクが一致しない");
    assert_eq!(a.diagnostics, b.diagnostics, "診断値が一致しない");
}

/// 診断の追加コストが切り抜き全体を圧迫しないこと。
///
/// 絶対時間は機械によって何倍も違うので、同じ画像の `cutout()` 全体との比で見る
/// （`refine_does_not_scale_with_the_area_of_the_window` と同じ流儀）。
/// 診断は距離変換 2 本と箱ぼかし 6 パス、格子の箱和 2 面を回すので、
/// 面積に比例はするが係数が小さい。
#[test]
fn the_diagnostics_are_a_small_part_of_the_cutout() {
    let scene = edge_scenes()
        .into_iter()
        .find(|s| s.name.starts_with("S10"))
        .unwrap();
    let truth = edge_scene(&scene);
    let opts = CutoutOptions::default();

    // 最短を採る。他プロセスに邪魔された回を混ぜない
    let (mut whole, mut diagnose) = (f64::MAX, f64::MAX);
    for _ in 0..3 {
        let started = std::time::Instant::now();
        let result = cutout(&truth.image, &opts);
        whole = whole.min(started.elapsed().as_secs_f64() * 1000.0);

        let started = std::time::Instant::now();
        let d = kiri::cutout::diagnostics::diagnose(
            &truth.image,
            &result.mask,
            result.background.rgb,
            None,
        );
        std::hint::black_box(d.contour_roughness);
        diagnose = diagnose.min(started.elapsed().as_secs_f64() * 1000.0);
    }
    let share = diagnose / whole;
    assert!(
        share <= 0.25,
        "診断が切り抜き全体を圧迫している: {diagnose:.1} ms / {whole:.1} ms (比 {share:.2})"
    );
}

/// 診断値と、それに対応する正解側の指標の対。None と NaN は落とす。
///
/// **判定するテストと表が同じ関数を呼ぶ。** 別々に組み立てると、表に出ている
/// ρ と `the_diagnostics_track_the_truth` が見ている ρ が静かに離れる。
fn correlated(points: &[Point]) -> Vec<(&'static str, Vec<(f64, f64)>)> {
    vec![
        (
            "contour_roughness",
            points
                .iter()
                .filter_map(|p| {
                    let d = p.diagnostics.contour_roughness?;
                    p.metrics
                        .contour_error
                        .is_finite()
                        .then_some((d, f64::from(p.metrics.contour_error)))
                })
                .collect(),
        ),
        (
            "rim_contamination",
            points
                .iter()
                .filter_map(|p| {
                    let d = p.diagnostics.rim_contamination?;
                    p.metrics
                        .rim_truth
                        .is_finite()
                        .then_some((d, f64::from(p.metrics.rim_truth)))
                })
                .collect(),
        ),
    ]
}

/// 順位相関（Spearman）。同順位は平均順位で扱う。
fn spearman(samples: &[(f64, f64)]) -> f64 {
    let xs = ranks(&samples.iter().map(|s| s.0).collect::<Vec<_>>());
    let ys = ranks(&samples.iter().map(|s| s.1).collect::<Vec<_>>());
    let n = samples.len() as f64;
    let (mx, my) = (xs.iter().sum::<f64>() / n, ys.iter().sum::<f64>() / n);
    let (mut num, mut dx, mut dy) = (0.0, 0.0, 0.0);
    for (x, y) in xs.iter().zip(&ys) {
        num += (x - mx) * (y - my);
        dx += (x - mx) * (x - mx);
        dy += (y - my) * (y - my);
    }
    if dx == 0.0 || dy == 0.0 {
        return 0.0;
    }
    num / (dx * dy).sqrt()
}

/// 昇順の順位。同じ値には平均順位を与える。
fn ranks(values: &[f64]) -> Vec<f64> {
    let mut order: Vec<usize> = (0..values.len()).collect();
    order.sort_by(|&a, &b| values[a].total_cmp(&values[b]));
    let mut out = vec![0.0; values.len()];
    let mut i = 0;
    while i < order.len() {
        let mut j = i;
        while j + 1 < order.len() && values[order[j + 1]] == values[order[i]] {
            j += 1;
        }
        let average = ((i + j) as f64) / 2.0;
        for &k in &order[i..=j] {
            out[k] = average;
        }
        i = j + 1;
    }
    out
}

/// 較正の表。`--ignored` を付けたときだけ走る。
///
/// しきい値を動かすときは必ずこれを取ること。**窓（クリーン側の最大 × 2 と
/// 欠陥側の最小 ÷ 2）が空いているかは、この表を見なければ判断できない。**
///
/// ```text
/// cargo test --release --test real_backgrounds -- --ignored --nocapture
/// ```
#[test]
#[ignore = "計測用。判定はせず表を出すだけ"]
fn print_the_calibration_table() {
    println!(
        "\n{:<44} {:>10} {:>10} {:>10} {:>10}  警告",
        "シーン / 設定", "粗さ", "正解", "縁の汚染", "正解"
    );
    for p in bench() {
        let show = |v: Option<f64>| match v {
            Some(v) => format!("{v:.3}"),
            None => "null".to_string(),
        };
        println!(
            "{:<44} {:>10} {:>10.2} {:>10} {:>10.3}  {}",
            p.label,
            show(p.diagnostics.contour_roughness),
            p.metrics.contour_error,
            show(p.diagnostics.rim_contamination),
            p.metrics.rim_truth,
            p.warnings
                .iter()
                .filter(|c| c.as_str() == "CONTOUR_ROUGH" || c.as_str() == "RIM_CONTAMINATED")
                .cloned()
                .collect::<Vec<_>>()
                .join(","),
        );
    }
    println!(
        "\nしきい値: CONTOUR_ROUGH_WARN={CONTOUR_ROUGH_WARN} RIM_CONTAMINATION_WARN={RIM_CONTAMINATION_WARN}"
    );
    // **相関も表の一部である。** しきい値を動かすときに窓だけを見て、
    // 「そもそも正解と同じ向きに動いているか」を見落とさないために並べて出す
    let points = bench();
    for (name, samples) in correlated(&points) {
        println!(
            "Spearman {name} vs 正解: ρ={:.3} ({} 点)",
            spearman(&samples),
            samples.len()
        );
    }
}

/// 手持ちの正解つき実写を回す入口。`KIRI_BENCH_DIR` が指す場所を読む。
///
/// **合成の正解はどこまで行っても合成である。** 実写に正解アルファを付ける
/// （Photoshop / GIMP / Pixelmator で切り抜いてグレー PNG に書き出す）作業は
/// 人にしかできないので、リポジトリには置かず、環境変数で差し込めるようにする。
///
/// ```text
/// KIRI_BENCH_DIR=~/bench cargo test --release --test real_backgrounds -- --ignored --nocapture
/// ```
///
/// `<name>.jpg|png` と `<name>.alpha.png`（8bit グレー、255 = 商品）の対を置く。
/// `<name>.json` があれば `{"bbox":[x1,y1,x2,y2],"normalized":true,"tolerance":60}`
/// として設定に使う（キーは `cutout` のオプション名と同じ）。
#[test]
#[ignore = "計測用。KIRI_BENCH_DIR が無ければ何もしない"]
fn print_the_external_bench() {
    let Ok(dir) = std::env::var(common::KIRI_BENCH_DIR) else {
        return;
    };
    let pairs = common::external_bench_pairs(std::path::Path::new(&dir));
    if pairs.is_empty() {
        println!("{dir} に <name>.jpg|png と <name>.alpha.png の対が無い");
        return;
    }
    println!(
        "\n{:<24} {:>9} {:>9} {:>9} {:>9} {:>9} {:>9} {:>8}",
        "素材", "帯MAE", "輪郭誤差", "rim正解", "粗さ", "縁の汚染", "halo", "ms"
    );
    for pair in pairs {
        let started = std::time::Instant::now();
        let result = cutout(&pair.truth.image, &pair.options);
        let elapsed = started.elapsed().as_millis();
        let m =
            common::measure_edges_with(&pair.truth, &result.image, &result.mask, pair.options.bbox);
        let show = |v: Option<f64>| match v {
            Some(v) => format!("{v:.3}"),
            None => "null".to_string(),
        };
        println!(
            "{:<24} {:>9.3} {:>9.2} {:>9.3} {:>9} {:>9} {:>9} {:>8}",
            pair.name,
            m.alpha_mae,
            m.contour_error,
            m.rim_truth,
            show(result.diagnostics.contour_roughness),
            show(result.diagnostics.rim_contamination),
            show(result.diagnostics.halo_ratio),
            elapsed,
        );
    }
}
