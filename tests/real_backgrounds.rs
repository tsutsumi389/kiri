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
#[derive(Clone)]
struct Point {
    label: String,
    metrics: EdgeMetrics,
    diagnostics: Diagnostics,
    warnings: Vec<String>,
    /// 空間的な指示が占めた割合 (確定前景, 確定背景)。指示が無ければ None
    constraints: Option<(f64, f64)>,
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
            constraints: None,
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
                constraints: run.constraints,
            });
        }
    }
    points
}

/// 較正の母集団をどちら側に置くか。**正解だけで決める。**
///
/// クリーン側を「受け入れ基準が名指しするシーン」に限ると、較正がテストを見て
/// テストが較正を見ることになる。指標そのものの値は一切見ずに、正解由来の
/// `contour_error` / `rim_truth` だけで分ける。間に挟まる点はどちらにも
/// 使わない——正解が「欠陥がある」とも「無い」とも言っていないのだから、
/// しきい値を縛る資格が無い。
#[derive(PartialEq, Debug, Clone, Copy)]
enum Side {
    Clean,
    Defective,
    /// 正解がどちらとも言っていない
    Between,
}

/// 輪郭の粗さの母集団。境目は「長辺 1000px 換算で何 px ずれているか」。
///
/// 1.0 未満は納品寸法で 1px を切るので見えない。3.0 以上は design.md が
/// 「1000px の素材で 3px なら見える」と書いた、その 3px である。
fn roughness_side(m: &EdgeMetrics) -> Side {
    match m.contour_error {
        e if e < 1.0 => Side::Clean,
        e if e >= 3.0 => Side::Defective,
        _ => Side::Between,
    }
}

/// 縁の汚染の母集団。境目は「帯の何割が真の背景か」。
///
/// 1% は点々としか残らない。10% は、帯（換算 3px）の 1 割が純粋な背景と
/// いうことで、輪郭ぐるりに 0.3px の縁が乗っているのと同じになる。
fn contamination_side(m: &EdgeMetrics) -> Side {
    match m.rim_truth {
        r if r < 0.01 => Side::Clean,
        r if r >= 0.10 => Side::Defective,
        _ => Side::Between,
    }
}

/// 母集団の両端。(クリーン側の最大, 欠陥側の最小)。
fn window(
    points: &[Point],
    side: fn(&EdgeMetrics) -> Side,
    value: fn(&Point) -> Option<f64>,
) -> (f64, f64) {
    let pick = |want: Side| -> Vec<f64> {
        points
            .iter()
            .filter(|p| side(&p.metrics) == want)
            .filter_map(value)
            .collect()
    };
    let clean = pick(Side::Clean).into_iter().fold(f64::MIN, f64::max);
    let defective = pick(Side::Defective).into_iter().fold(f64::MAX, f64::min);
    (clean, defective)
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

/// 空間的な指示が**守られ**、bbox と tolerance で救った状態（assisted）の
/// 輪郭を悪くしないこと。
///
/// **判定の中心は `forced_kept` である。** 「assisted より悪くなっていない」
/// だけを見ていた頃は、`eaten` が全設定で 0 だったので `0 <= 0` しか検査して
/// おらず、指示が届いているかどうかを何も言っていなかった。確定前景が残った
/// 割合を 1.0 で固定すれば、芯の判定が指示を握りつぶす経路（C1）も、面積
/// フィルタが小さな指示を消す経路（H1）も、ここで必ず赤になる。
///
/// R1 に渡すのは正解から作った粗いトライマップ（長辺の 2% で収縮したものが
/// 確定前景、膨張したものの外が確定背景）で、輪郭そのものは教えていない——
/// 不明の帯は長辺の 4% ある。それでも輪郭の位置を大きく絞り込むので、
/// `contour_error` は assisted 以下になる。
///
/// **ポリゴンには輪郭の改善を求めない。** 商品の中央半分と、余白 5% だけ
/// 離した外側の帯 4 枚しか教えていないので、指示は輪郭について何も言って
/// いない。実測でも assisted より僅かに悪い（19.76 vs 18.42）。求めるのは
/// 「指示のせいで輪郭が崩れていないこと」までで、1 割の余裕を窓に取る。
#[test]
fn spatial_instructions_are_kept_and_do_not_worsen_the_contour() {
    let points = bench();
    let assisted = point(&points, "R1 不織布 + 黒商品 / assisted");
    let trimap = point(&points, "R1 不織布 + 黒商品 / trimap");
    let polygon = point(&points, "R1 不織布 + 黒商品 / polygon");

    // 指示が空のまま回っていないことを先に確かめる。**空のトライマップでも
    // 「悪くなっていない」は成立してしまう**ので、ここが抜けると以降の
    // 判定は何も守らない
    for p in [trimap, polygon] {
        let (fg, bg) = p
            .constraints
            .unwrap_or_else(|| panic!("{}: 指示が無い", p.label));
        assert!(
            fg > 0.0 && bg > 0.0,
            "{}: 指示が画素を 1 つも占めていない (fg={fg:.3} bg={bg:.3})",
            p.label
        );
    }

    // **確定前景は 1 画素も落ちない。** 指示は色より強いという約束そのもの
    for p in [trimap, polygon] {
        assert_eq!(
            p.metrics.forced_kept, 1.0,
            "{}: 確定前景が前景として残っていない: {:.4}",
            p.label, p.metrics.forced_kept
        );
    }

    assert!(
        trimap.metrics.contour_error <= assisted.metrics.contour_error,
        "トライマップで輪郭誤差が悪化した: {:.2} vs assisted {:.2}",
        trimap.metrics.contour_error,
        assisted.metrics.contour_error
    );
    assert!(
        polygon.metrics.contour_error <= assisted.metrics.contour_error * 1.1,
        "粗いポリゴンで輪郭誤差が 1 割を超えて悪化した: {:.2} vs assisted {:.2}",
        polygon.metrics.contour_error,
        assisted.metrics.contour_error
    );
    for p in [trimap, polygon] {
        assert!(
            p.metrics.eaten <= assisted.metrics.eaten,
            "{}: 商品を assisted より削っている: {:.4} vs {:.4}",
            p.label,
            p.metrics.eaten,
            assisted.metrics.eaten
        );
    }
}

/// 輪郭の粗さが実写背景でだけ跳ねること。
///
/// **既定値の側で見る。** bbox と tolerance で救った後（assisted）でも実写背景の
/// 輪郭は蛇行しているが、蛇行が桁で出るのは既定値のまま布が前景として残って
/// いる状態のほう（R1 defaults 0.98、R2 defaults 0.69）で、「合成のきれいな
/// シーンとの比」で語るならそちらが素直である。
///
/// **R4 既定はこの一覧から外した。** Phase 3 の縁の再分類が、暗い机の上の
/// 白い商品を既定値のまま解けるようにしたためである（輪郭誤差 36.23 → 0.41、
/// 粗さ 2.44 → 0.002、警告なし）。**壊れていないシーンに「壊れている」ことを
/// 求め続けるのは、改善を退行として報告するのと同じである。** 代わりに、
/// 同じく既定値では解けない R2 既定を置いた——背景が別の不織布なので、
/// 「1 枚の素材でだけ成り立つ話」にならない。
#[test]
fn the_contour_roughness_rises_only_on_a_real_background() {
    let points = bench();
    let clean = point(&points, "S1")
        .diagnostics
        .contour_roughness
        .expect("輪郭がある");
    for label in [
        "R1 不織布 + 黒商品 / defaults",
        "R2 照明勾配の不織布 + 黒商品 / defaults",
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

/// Phase 3 の 3 段が、実写背景の**正解由来の**指標を実際に下げること。
///
/// **これが Phase 3 の目的そのものである。** 診断値（`contour_roughness` /
/// `rim_contamination`）は処理が自分で均せる量なので、良くなったことの証拠に
/// ならない。正解の輪郭からの距離（`contour_error`）と、帯のうち真に背景だった
/// 割合（`rim_truth`）だけが、外から見て嘘をつけない。
///
/// 基準は「Phase 2 までの経路（3 つのスイッチを明示）の半分以下」。設計書の
/// 合格条件（R1 assisted で 18.4 → 9.2、rim 正解 0.512 → 0.26）と同じ −50% で、
/// **絶対値ではなく比で書く**のは、シーンの生成や較正が動いても意味が変わらない
/// ようにするためである。
#[test]
fn the_matting_stages_halve_the_truth_side_error_on_a_real_background() {
    use kiri::cutout::Matting;

    let scene = real_scenes()
        .into_iter()
        .find(|s| s.name.starts_with("R1"))
        .unwrap();
    let truth = common::real_scene(&scene);
    let bbox = common::assisted_bbox(&truth);
    let measure = |opts: &CutoutOptions| {
        let result = cutout(&truth.image, opts);
        common::measure_edges_with(&truth, &result.image, &result.mask, opts.bbox, None)
    };
    let assisted = CutoutOptions {
        bbox: Some(bbox),
        tolerance: scene.assisted_tolerance,
        ..Default::default()
    };
    let before = measure(&CutoutOptions {
        matting: Matting::Projection,
        smooth_contour: 0.0,
        reclassify: false,
        ..assisted.clone()
    });
    let after = measure(&assisted);

    assert!(
        after.contour_error <= before.contour_error * 0.5,
        "輪郭誤差が半分になっていない: {:.2} → {:.2}",
        before.contour_error,
        after.contour_error
    );
    assert!(
        after.rim_truth <= before.rim_truth * 0.5,
        "帯の正解側の汚染が半分になっていない: {:.3} → {:.3}",
        before.rim_truth,
        after.rim_truth
    );
    // **代わりに何を払ったかも固定する。** 帯を均すぶんアルファ誤差は増える。
    // 増分に上限が無ければ、輪郭の位置を稼ぐために matte を潰し放題になる
    assert!(
        after.alpha_mae <= before.alpha_mae + 0.01,
        "輪郭を稼ぐためにアルファ誤差を払いすぎている: {:.3} → {:.3}",
        before.alpha_mae,
        after.alpha_mae
    );
}

/// 診断値が正解由来の誤差と同じ向きに動くこと。
///
/// **相関しなければ、指標は欠陥ではない何かを測っている。** 値そのものではなく
/// 順位で見るのは、両者の単位も分布も違うためである（輪郭誤差は px、粗さも px
/// だが基準が違い、実写背景の既定値では 100px を超える）。
///
/// **`defaults` を除いた部分集合でも見る。** R シーンの既定値は切り抜きその
/// ものが失敗している壊滅ケースで、そこだけで順位が付いてしまうと「実用域で
/// 正解と同じ向きに動くか」を確かめたことにならない。全点は 0.7、壊滅ケースを
/// 抜いた 20 点は 0.5 を下限にする——標本が 3 分の 2 に減り、値の幅も桁で
/// 狭まるので、同着が増えて ρ は原理的に下がる。
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
            "{name}: 正解と相関していない: ρ={rho:.3} ({} 点)\n{}",
            samples.len(),
            listing(&points, name)
        );
    }
    for (name, samples) in correlated(&without_broken_cuts(&points)) {
        let rho = spearman(&samples);
        assert!(
            rho >= 0.5,
            "{name}: 壊滅ケースを除くと正解と相関していない: ρ={rho:.3} ({} 点)\n{}",
            samples.len(),
            listing(&points, name)
        );
    }
}

/// R シーンの `defaults`——切り抜きそのものが失敗している点——を除いた部分集合。
fn without_broken_cuts(points: &[Point]) -> Vec<Point> {
    points
        .iter()
        .filter(|p| !p.label.ends_with("/ defaults"))
        .cloned()
        .collect()
}

/// 失敗したときに、ρ だけでなく各点の値を出す。
///
/// **ρ が下がったことより、どの点が順位を崩したかが知りたい。** 27 点を
/// 目で追えるだけの量しかないのだから、出さない理由が無い。
fn listing(points: &[Point], metric: &str) -> String {
    let mut out = String::new();
    for p in points {
        let (value, truth) = if metric == "contour_roughness" {
            (
                p.diagnostics.contour_roughness,
                f64::from(p.metrics.contour_error),
            )
        } else {
            (
                p.diagnostics.rim_contamination,
                f64::from(p.metrics.rim_truth),
            )
        };
        let shown = value.map_or("null".to_string(), |v| format!("{v:.3}"));
        out.push_str(&format!(
            "  {:<44} {:>8} / 正解 {:.3}\n",
            p.label, shown, truth
        ));
    }
    out
}

/// しきい値が、正解で分けた 2 つの群のあいだに入っていること。
///
/// **これは較正そのものの回帰テストである。** 母集団は正解だけで決まる
/// （`roughness_side` / `contamination_side`）ので、しきい値を動かしても
/// 母集団は動かない。
///
/// 問うことは 2 つある。
///
/// 1. **誤分類が 1 点も無いこと。** クリーン側の最大がしきい値を下回り、
///    欠陥側の最小がしきい値を上回る。これが契約そのもので、余裕の話ではない
/// 2. **その余裕が記録どおりであること。** 余裕が縮むこと自体は退行ではない
///    （処理が良くなれば群は近づく）が、**黙って縮むのは退行である**
///
/// # 粗さの欠陥側の余裕は Phase 3 で 1.97 倍から 1.12 倍へ縮んだ
///
/// Phase 3 は輪郭を**実際に均す**（色の門つきメディアンと guided
/// feathering）。すると壊れた切り抜きの輪郭まで滑らかになり、
/// `contour_roughness` は「輪郭が真の位置から遠い」ことを見なくなる——
/// 欠陥側の最小は R7 既定（輪郭誤差 59.7px、正解の帯の 57% が背景）で、
/// 粗さは 0.316 から 0.180 へ落ちた。しきい値 0.16 は今も全 29 点を
/// 正しく分けるが、**この指標は「輪郭が汚い」ことしか見ておらず、
/// 「輪郭が違う場所にある」ことは `halo_ratio` と `rim_contamination` と
/// `BBOX_RECOMMENDED` が見る**、という分担がはっきりした。
///
/// クリーン側は 1.96 倍のまま動かない（S5、幅 3px のストラップ）。
#[test]
fn the_thresholds_sit_between_the_clean_and_the_defective() {
    let points = bench();
    for (name, threshold, clean_margin, defective_margin, side, value) in [
        (
            "contour_roughness",
            CONTOUR_ROUGH_WARN,
            1.9,
            1.1,
            roughness_side as fn(&EdgeMetrics) -> Side,
            (|p: &Point| p.diagnostics.contour_roughness) as fn(&Point) -> Option<f64>,
        ),
        (
            "rim_contamination",
            RIM_CONTAMINATION_WARN,
            2.0,
            2.0,
            contamination_side as fn(&EdgeMetrics) -> Side,
            (|p: &Point| p.diagnostics.rim_contamination) as fn(&Point) -> Option<f64>,
        ),
    ] {
        let (clean, defective) = window(&points, side, value);
        // まず誤分類。**ここは余裕ではなく契約である**
        assert!(
            clean < threshold && defective > threshold,
            "{name}: しきい値 {threshold} が 2 つの群を分けていない \
             (クリーン最大 {clean:.3} / 欠陥最小 {defective:.3})\n{}",
            listing(&points, name)
        );
        assert!(
            clean * clean_margin <= threshold,
            "{name}: クリーン側の最大 {clean:.3} がしきい値 {threshold} に近すぎる\n{}",
            listing(&points, name)
        );
        assert!(
            defective >= threshold * defective_margin,
            "{name}: 欠陥側の最小 {defective:.3} がしきい値 {threshold} に近すぎる\n{}",
            listing(&points, name)
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
/// 絶対時間は機械によって何倍も違うので、同じ画像の `cutout()` との比で見る
/// （`refine_does_not_scale_with_the_area_of_the_window` と同じ流儀）。
///
/// **分母から診断を引く。** `cutout()` は中で `diagnose()` を呼ぶので、
/// そのまま割ると「全体のうち何割か」になり、診断が重くなるほど分母も
/// 太って比が鈍る（診断が全体の 100% を占めても比は 1.0 にしかならない）。
/// 知りたいのは「診断を足したことで何割増えたか」なので、分母は診断を
/// 除いた切り抜きの時間にする。
///
/// 診断は距離変換 2 本と箱ぼかし 6 パス、格子の箱和 2 面を回すので、面積に
/// 比例はするが係数が小さい。0.35 は実測（0.29）の上に置いた緩い上限である。
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
    let share = diagnose / (whole - diagnose);
    assert!(
        share <= 0.35,
        "診断の追加コストが膨らんでいる: {diagnose:.1} ms / 切り抜き {:.1} ms (比 {share:.2})",
        whole - diagnose
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
        "\n{:<44} {:>8} {:>8} {:>3} {:>8} {:>8} {:>3} {:>7} {:>7} {:>7}  警告",
        "シーン / 設定",
        "粗さ",
        "正解",
        "群",
        "縁の汚染",
        "正解",
        "群",
        "strap",
        "eaten",
        "指示保持"
    );
    let points = bench();
    for p in &points {
        let show = |v: Option<f64>| match v {
            Some(v) => format!("{v:.3}"),
            None => "null".to_string(),
        };
        // 母集団のどちら側かを表に出す。**しきい値を動かす人が、窓の両端が
        // どの点から来ているかを目で確かめられなければ較正できない**
        let mark = |side: Side| match side {
            Side::Clean => "C",
            Side::Defective => "D",
            Side::Between => "-",
        };
        // 幅 3px のストラップがどれだけ残ったか。粗さの较正で S5 / R5 が
        // 効いてくるので、細部が生きているかどうかを同じ表で見る
        let strap = if p.metrics.strap_kept.is_finite() {
            format!("{:.0}%", p.metrics.strap_kept * 100.0)
        } else {
            "-".to_string()
        };
        // 指示の占有率も出す。値が動いたときに「指示が変わった」のか
        // 「切り抜きが変わった」のかを、同じ表の上で切り分けられる
        let constraints = match p.constraints {
            Some((fg, bg)) => format!("  制約 fg={fg:.3}/bg={bg:.3}"),
            None => String::new(),
        };
        // 渡した確定前景がどれだけ残ったか。**「悪くなっていない」だけを見る
        // 判定は空振りする**ので、指示が届いたかどうかを同じ表に並べて出す
        let kept = if p.metrics.forced_kept.is_finite() {
            format!("{:.1}%", p.metrics.forced_kept * 100.0)
        } else {
            "-".to_string()
        };
        println!(
            "{:<44} {:>8} {:>8.2} {:>3} {:>8} {:>8.3} {:>3} {:>7} {:>7} {:>7}  {}{constraints}",
            p.label,
            show(p.diagnostics.contour_roughness),
            p.metrics.contour_error,
            mark(roughness_side(&p.metrics)),
            show(p.diagnostics.rim_contamination),
            p.metrics.rim_truth,
            mark(contamination_side(&p.metrics)),
            strap,
            format!("{:.4}", p.metrics.eaten),
            kept,
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
    // **窓（群間の分離）も表の一部である。** 群 C の最大と群 D の最小の比は、
    // 同着が多くて ρ が伸びない指標でも「2 つの群が離れているか」を語る
    for (name, threshold, side, value) in [
        (
            "contour_roughness",
            CONTOUR_ROUGH_WARN,
            roughness_side as fn(&EdgeMetrics) -> Side,
            (|p: &Point| p.diagnostics.contour_roughness) as fn(&Point) -> Option<f64>,
        ),
        (
            "rim_contamination",
            RIM_CONTAMINATION_WARN,
            contamination_side as fn(&EdgeMetrics) -> Side,
            (|p: &Point| p.diagnostics.rim_contamination) as fn(&Point) -> Option<f64>,
        ),
    ] {
        let (clean, defective) = window(&points, side, value);
        println!(
            "{name}: クリーン最大 {clean:.3} / 欠陥最小 {defective:.3} (群間比 {:.1} 倍) \
             → 窓 [{:.3}, {:.3}]、採用 {threshold}（余裕 {:.2} 倍 / {:.2} 倍）",
            defective / clean,
            clean * 2.0,
            defective / 2.0,
            threshold / clean,
            defective / threshold,
        );
    }
    // **相関も表の一部である。** しきい値を動かすときに窓だけを見て、
    // 「そもそも正解と同じ向きに動いているか」を見落とさないために並べて出す
    for (label, set) in [
        ("全点", points.clone()),
        ("defaults 除外", without_broken_cuts(&points)),
    ] {
        for (name, samples) in correlated(&set) {
            println!(
                "Spearman {name} vs 正解（{label}）: ρ={:.3} ({} 点)",
                spearman(&samples),
                samples.len()
            );
        }
    }
}

/// 段ごとの対照表。`--ignored` を付けたときだけ走る。
///
/// **全部入りだけを固定すると、1 段が死んでも気づけない。** 再分類 (b) /
/// 色の門つき平滑化 (c) / guided feathering (e) を個別に切って、どの段が
/// どの指標を動かしたかを並べる。
///
/// ```text
/// cargo test --release --test real_backgrounds -- --ignored --nocapture print_the_stage_table
/// ```
#[test]
#[ignore = "計測用。判定はせず表を出すだけ"]
fn print_the_stage_table() {
    use kiri::cutout::Matting;

    let base = CutoutOptions::default();
    let stages: Vec<(&str, CutoutOptions)> = vec![
        (
            "phase2",
            CutoutOptions {
                matting: Matting::Projection,
                smooth_contour: 0.0,
                reclassify: false,
                ..base.clone()
            },
        ),
        (
            "+b 再分類",
            CutoutOptions {
                matting: Matting::Projection,
                smooth_contour: 0.0,
                reclassify: true,
                ..base.clone()
            },
        ),
        (
            "+c 平滑化",
            CutoutOptions {
                matting: Matting::Projection,
                smooth_contour: base.smooth_contour,
                reclassify: false,
                ..base.clone()
            },
        ),
        (
            "+e guided",
            CutoutOptions {
                matting: Matting::Guided,
                smooth_contour: 0.0,
                reclassify: false,
                ..base.clone()
            },
        ),
        ("既定(b+c+e)", base.clone()),
        (
            "-b",
            CutoutOptions {
                reclassify: false,
                ..base.clone()
            },
        ),
        (
            "-c",
            CutoutOptions {
                smooth_contour: 0.0,
                ..base.clone()
            },
        ),
        (
            "-e",
            CutoutOptions {
                matting: Matting::Projection,
                ..base.clone()
            },
        ),
    ];

    println!(
        "\n{:<34} {:<12} {:>9} {:>8} {:>9} {:>8} {:>7} {:>8} {:>8} {:>9}",
        "シーン",
        "段",
        "輪郭誤差",
        "rim正解",
        "alphaMAE",
        "eaten",
        "strap",
        "shadow残",
        "粗さ",
        "縁の汚染"
    );
    // 帯幅の下限も並べる。値が動いたときに「段が効いた」のか「帯が変わった」
    // のかを、同じ表の上で切り分けられる
    let show = |v: Option<f64>| match v {
        Some(v) => format!("{v:.3}"),
        None => "null".to_string(),
    };
    let percent = |v: f32| {
        if v.is_finite() {
            format!("{:.1}%", v * 100.0)
        } else {
            "-".to_string()
        }
    };

    let row = |label: &str, truth: &common::EdgeTruth, opts: &CutoutOptions, stage: &str| {
        let result = cutout(&truth.image, opts);
        let m = common::measure_edges_with(truth, &result.image, &result.mask, opts.bbox, None);
        println!(
            "{label:<34} {stage:<12} {:>9.2} {:>8.3} {:>9.3} {:>8.4} {:>7} {:>8} {:>8} {:>9} r={:?}",
            m.contour_error,
            m.rim_truth,
            m.alpha_mae,
            m.eaten,
            percent(m.strap_kept),
            percent(m.shadow_kept),
            show(result.diagnostics.contour_roughness),
            show(result.diagnostics.rim_contamination),
            result.band_min_radius,
        );
    };

    for scene in edge_scenes() {
        let truth = edge_scene(&scene);
        for (stage, opts) in &stages {
            row(scene.name, &truth, opts, stage);
        }
        println!();
    }
    for scene in real_scenes() {
        let truth = common::real_scene(&scene);
        let bbox = common::assisted_bbox(&truth);
        for (stage, opts) in &stages {
            let opts = CutoutOptions {
                bbox: Some(bbox),
                tolerance: scene.assisted_tolerance,
                ..opts.clone()
            };
            row(scene.name, &truth, &opts, stage);
        }
        println!();
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
/// として設定に使う（キーは `cutout` のオプション名と同じ）。読めない JSON や
/// 解釈できない bbox は**素材ごと飛ばす**——既定の設定で回した数字を指定した
/// 設定の数字として表に出すのが、いちばん質の悪い嘘になる。
///
/// 空間的な指示も同じ JSON から受ける（キーは `batch` の spec と同じ）。
///
/// ```json
/// { "trimap": "remote.trimap.png", "tolerance": 60,
///   "fg_polygons": [[0.2,0.4,0.8,0.4,0.8,0.6,0.2,0.6]], "normalized": true }
/// ```
///
/// 画像のパスは JSON の隣を基準に解決する。`normalized` は bbox と多角形の
/// 両方に効く。
///
/// `輪郭誤差` の床は合成シーンの表と違う。あちらは解析的な距離場なので完璧に
/// 解けても 0.4〜0.5 から下がらないが、こちらは正解アルファの二値輪郭からの
/// 距離変換なので 0 まで下がる。**2 つの表の数字を直接比べないこと。**
#[test]
#[ignore = "計測用。KIRI_BENCH_DIR が無ければ何もしない"]
fn print_the_external_bench() {
    let Ok(dir) = std::env::var(common::KIRI_BENCH_DIR) else {
        return;
    };
    let path = std::path::Path::new(&dir);
    if !path.is_dir() {
        // **「ディレクトリが無い」と「対が無い」を同じ文面で報せない。**
        // 前者は綴り間違いか置き場所の取り違えで、後者は素材の並べ方の問題。
        // 打つ手がまったく違う
        println!("{dir}: ディレクトリが無い（KIRI_BENCH_DIR の綴りを確認する）");
        return;
    }
    let pairs = common::external_bench_pairs(path);
    if pairs.is_empty() {
        println!("{dir}: <name>.jpg|png と <name>.alpha.png の対が 1 つも無い");
        return;
    }
    println!(
        "\n{:<24} {:>9} {:>9} {:>9} {:>9} {:>9} {:>9} {:>8} {:>5} {:>22} {:>17}",
        "素材",
        "帯MAE",
        "輪郭誤差",
        "rim正解",
        "粗さ",
        "縁の汚染",
        "halo",
        "ms",
        "tol",
        "bbox",
        "制約"
    );
    for pair in pairs {
        let started = std::time::Instant::now();
        let result = cutout(&pair.truth.image, &pair.options);
        let elapsed = started.elapsed().as_millis();
        let m = common::measure_edges_with(
            &pair.truth,
            &result.image,
            &result.mask,
            pair.options.bbox,
            pair.options.constraints.as_ref(),
        );
        let show = |v: Option<f64>| match v {
            Some(v) => format!("{v:.3}"),
            None => "null".to_string(),
        };
        // **どの設定で回した数字かを表に出す。** `<name>.json` を置いたのに
        // 読めていない、という状態が数字の上では見分けられない
        let bbox = match pair.options.bbox {
            Some((x1, y1, x2, y2)) => format!("{x1},{y1},{x2},{y2}"),
            None => "なし".to_string(),
        };
        // 指示も同じ理由で出す。`<name>.json` に書いたのに読めていない、
        // という状態が数字の上では見分けられない
        let constraints = match pair.options.constraints.as_ref() {
            Some(c) => {
                let (fg, bg, _) = c.ratios();
                format!("fg={fg:.3}/bg={bg:.3}")
            }
            None => "なし".to_string(),
        };
        println!(
            "{:<24} {:>9.3} {:>9.2} {:>9.3} {:>9} {:>9} {:>9} {:>8} {:>5.0} {:>22} {:>17}",
            pair.name,
            m.alpha_mae,
            m.contour_error,
            m.rim_truth,
            show(result.diagnostics.contour_roughness),
            show(result.diagnostics.rim_contamination),
            show(result.diagnostics.halo_ratio),
            elapsed,
            pair.options.tolerance,
            bbox,
            constraints,
        );
    }
}
