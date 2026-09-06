//! 境界品質の回帰テスト。
//!
//! 真の被覆率が解析的に分かる合成画像で切り抜きを回し、境界のずれ・背景色の縁・
//! アルファ誤差・ハロー・細部の消失を数値で固定する。境界の良し悪しは目で見ないと
//! 分からないと思われがちだが、正解を持った合成シーンを使えば数値で追える。
//! 追えなければ「直したつもりで悪化させた」ことに気づけない。

mod common;

use common::{EdgeScene, EdgeTruth, edge_scene, measure_edges};
use image::{Rgba, RgbaImage};
use kiri::cutout::{CutoutOptions, cutout};

fn scenes() -> Vec<EdgeScene> {
    vec![
        EdgeScene {
            name: "S1 濃色商品/白背景/JPEG q90",
            ..Default::default()
        },
        EdgeScene {
            name: "S2 同上 PNG(非圧縮)",
            jpeg: None,
            ..Default::default()
        },
        EdgeScene {
            name: "S4 柔らかい輪郭(8px)",
            softness: 8.0,
            ..Default::default()
        },
        EdgeScene {
            name: "S5 3px のストラップ",
            strap: Some(3),
            ..Default::default()
        },
        EdgeScene {
            name: "S5b 5px のストラップ",
            strap: Some(5),
            ..Default::default()
        },
        EdgeScene {
            name: "S6 落ち影あり",
            shadow: true,
            ..Default::default()
        },
    ]
}

fn find(name: &str) -> EdgeTruth {
    let scene = scenes()
        .into_iter()
        .find(|s| s.name.starts_with(name))
        .unwrap_or_else(|| panic!("シーン {name} が無い"));
    edge_scene(&scene)
}

/// 既定値で切り抜き、指標を返す。
fn run(truth: &EdgeTruth, opts: &CutoutOptions) -> common::EdgeMetrics {
    let result = cutout(&truth.image, opts);
    measure_edges(truth, &result.image, &result.mask)
}

#[test]
fn a_hard_edge_recovers_the_true_alpha() {
    // 濃い商品を白背景から切る、最も基本的なケース。二値マスクの形から
    // アルファを作っていた頃は、エッジ堤防が残す 1px の背景色の縁がそのまま
    // 不透明で残り、黒い下地に載せると白い光輪になっていた
    let truth = find("S1");
    let m = run(&truth, &CutoutOptions::default());
    assert!(
        m.alpha_mae < 0.03,
        "アルファ誤差が大きい: {:.3}\n{m:?}",
        m.alpha_mae
    );
    assert!(
        m.rim < 0.01,
        "背景色のままの画素が不透明で残っている: {:.1}%\n{m:?}",
        m.rim * 100.0
    );
    assert!(
        m.halo < 5.0,
        "黒地に載せたときのハローが明るい: {:.1}\n{m:?}",
        m.halo
    );
    assert!(
        m.eaten < 0.01,
        "商品が削られている: {:.1}%\n{m:?}",
        m.eaten * 100.0
    );
}

#[test]
fn a_soft_edge_is_followed_instead_of_being_cut_at_the_dam() {
    // 8px かけて背景へ溶ける輪郭。エッジ堤防は混色のかなり背景寄りで止まるので、
    // マスクの形からアルファを作ると遷移全体がほぼ不透明のまま残ってしまう
    let truth = find("S4");
    let m = run(&truth, &CutoutOptions::default());
    assert!(
        m.alpha_mae < 0.15,
        "柔らかい輪郭のアルファ誤差が大きい: {:.3}\n{m:?}",
        m.alpha_mae
    );
    assert!(
        m.offset.abs() <= 1.0,
        "境界位置が {:+.2}px ずれている\n{m:?}",
        m.offset
    );
}

#[test]
fn a_three_pixel_strap_survives_without_the_edge_dam() {
    // 幅 3px のストラップ。オープニング(半径2)は幅 5px 未満の構造を消すため、
    // 堤防が縁を太らせて偶然 5px に見せかけている間しか生き残れなかった。
    // 堤防を切っても残ることで、面積フィルタが効いていることを確かめる
    let truth = find("S5");
    let opts = CutoutOptions {
        edge_threshold: 0.0,
        ..Default::default()
    };
    let m = run(&truth, &opts);
    assert!(
        m.strap_kept > 0.90,
        "細いストラップが消えている: 残存 {:.1}%\n{m:?}",
        m.strap_kept * 100.0
    );
}

#[test]
fn a_three_pixel_strap_does_not_leave_a_halo() {
    let truth = find("S5");
    let m = run(&truth, &CutoutOptions::default());
    assert!(
        m.strap_kept > 0.90,
        "細いストラップが消えている: 残存 {:.1}%",
        m.strap_kept * 100.0
    );
    assert!(
        m.halo < 5.0,
        "細部の周りにハローが残っている: {:.1}\n{m:?}",
        m.halo
    );
}

#[test]
fn an_isolated_speck_is_removed_but_a_thin_line_is_not() {
    // 面積フィルタの二面性を1つのシーンで見る。孤立した 3x3 のゴミは消え、
    // 本体につながった幅 3px の線は残る。オープニングでは両方消えていた
    let bg = [248u8, 248, 247];
    let mut img = RgbaImage::from_pixel(120, 120, Rgba([bg[0], bg[1], bg[2], 255]));
    let product = Rgba([40u8, 40, 45, 255]);
    for y in 40..80 {
        for x in 40..80 {
            img.put_pixel(x, y, product);
        }
    }
    // 本体から上へ伸びる幅 3px の線
    for y in 12..40 {
        for x in 58..61 {
            img.put_pixel(x, y, product);
        }
    }
    // 孤立した 3x3 のゴミ
    for y in 100..103 {
        for x in 20..23 {
            img.put_pixel(x, y, product);
        }
    }

    let result = cutout(&img, &CutoutOptions::default());
    assert!(
        !result.mask.is_foreground(21, 101),
        "孤立した 3x3 のゴミが残っている"
    );
    let line_kept = (14..38)
        .filter(|&y| result.mask.is_foreground(59, y))
        .count();
    assert!(
        line_kept >= 22,
        "幅 3px の線が消えている: 24 行中 {line_kept} 行しか残っていない"
    );
}

#[test]
fn the_same_input_produces_the_same_bytes() {
    // 並列化しても順序に依存しないことを固定する
    let truth = find("S1");
    let opts = CutoutOptions::default();
    let a = cutout(&truth.image, &opts);
    let b = cutout(&truth.image, &opts);
    assert_eq!(a.image.as_raw(), b.image.as_raw(), "出力画像が一致しない");
    assert_eq!(a.mask, b.mask, "マスクが一致しない");
}

#[test]
fn the_halo_diagnostic_notices_a_background_coloured_rim() {
    // 診断値そのものの検証。境界にわざと背景色の縁を残したマスクを与え、
    // halo_ratio が跳ね上がることを見る
    let truth = find("S1");
    let clean = cutout(&truth.image, &CutoutOptions::default());
    let rimmed = cutout(
        &truth.image,
        &CutoutOptions {
            refine: false,
            ..Default::default()
        },
    );
    let (rimmed, clean) = (
        rimmed.diagnostics.halo_ratio.expect("境界があるので測れる"),
        clean.diagnostics.halo_ratio.expect("境界があるので測れる"),
    );
    assert!(
        rimmed > clean + 0.05,
        "縁を残した結果のほうが halo_ratio が高くあるべき: {rimmed:.3} vs {clean:.3}"
    );
}

#[test]
fn the_edge_width_diagnostic_tracks_the_softness_of_the_contour() {
    let hard = cutout(&find("S1").image, &CutoutOptions::default());
    let soft = cutout(&find("S4").image, &CutoutOptions::default());
    let (soft, hard) = (
        soft.diagnostics.edge_width.expect("輪郭があるので測れる"),
        hard.diagnostics.edge_width.expect("輪郭があるので測れる"),
    );
    assert!(
        soft > hard,
        "柔らかい輪郭のほうが遷移幅が広くあるべき: {soft:.2} vs {hard:.2}"
    );
}

/// 大きな素材での所要時間。`--ignored` を付けたときだけ走る。
///
/// 境界帯の推定は帯の画素だけを走査するので、面積ではなく周長にほぼ比例する。
/// それでも実素材の 12MP で数百 ms に収まっているかは確かめておく必要がある。
#[test]
#[ignore = "計測用。判定はせず所要時間を出すだけ"]
fn print_the_timing() {
    for (w, h) in [(1000u32, 1000u32), (3000, 4000)] {
        let truth = edge_scene(&EdgeScene {
            name: "計測",
            width: w,
            height: h,
            ..Default::default()
        });
        for (label, opts) in [
            ("refine", CutoutOptions::default()),
            (
                "no-refine",
                CutoutOptions {
                    refine: false,
                    ..Default::default()
                },
            ),
        ] {
            let started = std::time::Instant::now();
            let result = cutout(&truth.image, &opts);
            let elapsed = started.elapsed();
            println!(
                "{w}x{h} ({:.1} MP) {label:<10} {:>7.1} ms  前景比率 {:.3}",
                (w as f64) * (h as f64) / 1e6,
                elapsed.as_secs_f64() * 1000.0,
                result.stats.foreground_ratio,
            );
        }
    }
}

/// 実装前後の比較に使う一覧表。`--ignored` を付けたときだけ走る。
///
/// ```text
/// cargo test --release --test edge_quality -- --ignored --nocapture
/// ```
#[test]
#[ignore = "計測用。判定はせず表を出すだけ"]
fn print_the_metrics_table() {
    println!(
        "\n{:<28} {:<20} {:>8} {:>7} {:>7} {:>9} {:>7} {:>7} {:>8} {:>8}",
        "シーン",
        "設定",
        "境界ずれ",
        "rim",
        "eaten",
        "alphaMAE",
        "halo",
        "白地誤差",
        "strap",
        "shadow残"
    );
    for scene in scenes() {
        let truth = edge_scene(&scene);
        for (label, opts) in [
            ("既定", CutoutOptions::default()),
            (
                "堤防なし",
                CutoutOptions {
                    edge_threshold: 0.0,
                    ..Default::default()
                },
            ),
        ] {
            let result = cutout(&truth.image, &opts);
            let m = measure_edges(&truth, &result.image, &result.mask);
            println!(
                "{:<28} {:<20} {:>+8.3} {:>6.1}% {:>6.1}% {:>9.3} {:>7.1} {:>8.1} {:>7.1}% {:>7.1}%",
                scene.name,
                label,
                m.offset,
                m.rim * 100.0,
                m.eaten * 100.0,
                m.alpha_mae,
                m.halo,
                m.white_error,
                m.strap_kept * 100.0,
                m.shadow_kept * 100.0,
            );
            let round1 = |v: f64| (v * 10.0).round() / 10.0;
            println!(
                "{:<49} halo_ratio={:?} edge_width={:?} separability={:?}",
                "",
                result
                    .diagnostics
                    .halo_ratio
                    .map(|v| (v * 1000.0).round() / 1000.0),
                result.diagnostics.edge_width.map(round1),
                result.separability.map(round1),
            );
        }
        println!();
    }
}
