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
        // 白背景に置いた淡色商品。上端のハイライトで商品は 221 まで明るくなり、
        // 背景 248 との輪郭のコントラストは ΔE 9.5 まで落ちる。既定の
        // tolerance 12 より小さいので、**色だけを見れば商品はまるごと背景**
        // である。連結性と段差の検査だけがこれを商品として残している
        EdgeScene {
            name: "S3 淡色商品(輪郭 ΔE 9.5)",
            product: [232, 232, 230],
            shading: (0.98, 0.80),
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
        EdgeScene {
            name: "S7 中間グレー商品+影",
            product: [150, 150, 150],
            shadow: true,
            ..Default::default()
        },
        EdgeScene {
            name: "S8 黒商品+影",
            product: [20, 20, 20],
            shadow: true,
            ..Default::default()
        },
        // 高解像度での影の暴走を捕まえるシーン。無彩色の商品・落ち影・柔らかい
        // 輪郭という、影の判定にとって最悪の 3 つを重ねてある。影の段だけは
        // 堤防を無視するので、柔らかい輪郭は通り抜けられてしまう。進める距離が
        // 解像度に比例して伸びると、そこから商品の内部まで届く。
        // 他のシーンは 600px なので、解像度に依存する崩れはここでしか出ない
        EdgeScene {
            name: "S10 高解像度/無彩色商品+影+柔輪郭",
            width: 1600,
            height: 1600,
            product: [150, 150, 150],
            softness: 4.0,
            shadow: true,
            ..Default::default()
        },
        // 解けないケース。商品の明度が上から下へ変化する途中で背景色を
        // **横切る**ため、輪郭のコントラストが 0 になる行が存在する。そこでは
        // 色による分離が原理的に不可能で、いったん入られると商品の内部は
        // 一様なのでフィルが広がる。判定はせず、表に出して限界を可視化する
        EdgeScene {
            name: "S9 淡色商品(明度が背景を横切る)",
            product: [232, 232, 230],
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
    // 実測 0.139。8px かけて溶ける輪郭では、帯の上限(10px)まで広げても
    // 遷移の外側で背景と区別が付かなくなる分が残る。0.15 はその実測に
    // 1 割弱の余裕を足した値
    assert!(
        m.alpha_mae < 0.15,
        "柔らかい輪郭のアルファ誤差が大きい: {:.3}\n{m:?}",
        m.alpha_mae
    );
    // ずれは 0 ではなく +1.0px が基準値。エッジ堤防は Sobel の勾配が輪郭の
    // 両側に立つため、フィルの停止位置が真の輪郭より 1px 外側になる
    // （docs/design.md「堤防の副作用」）。アルファの再推定はその縁を透明へ
    // 戻すが、停止位置そのものは動かさないので、この 1px は残る。
    // 0 を要求すると堤防の頑健化まで巻き込むので、基準値からの幅で見る
    let drift = m.offset - 1.0;
    assert!(
        drift.abs() <= 0.5,
        "境界位置が基準の +1.00px から {drift:+.2}px 動いている (実測 {:+.2}px)\n{m:?}",
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
fn a_light_product_is_not_flooded_through_its_faint_outline() {
    // 商品の上端は 221、背景は 248。輪郭のコントラストは ΔE 9.5 しかなく、
    // 既定の tolerance 12 より小さい。つまり **色だけを見れば商品はまるごと
    // 背景**である。勾配の堤防（1px あたり輝度 8）も、JPEG で滲んだ角では
    // 反応しない。段差の検査が入る前は、境界近傍の商品の 6 割が削れていた
    let truth = find("S3");
    let m = run(&truth, &CutoutOptions::default());
    assert!(
        m.eaten < 0.05,
        "淡色商品が削られている: {:.1}%\n{m:?}",
        m.eaten * 100.0
    );
    assert!(
        m.rim < 0.01,
        "背景色のままの画素が不透明で残っている: {:.1}%\n{m:?}",
        m.rim * 100.0
    );
}

#[test]
fn a_cast_shadow_is_removed() {
    // tolerance 12 では影の濃い部分（ΔE 20-30）に届かない。影の専用判定が
    // 入る前は 56% が商品の直下に残っていた
    let truth = find("S6");
    let m = run(&truth, &CutoutOptions::default());
    assert!(
        m.shadow_kept < 0.05,
        "落ち影が残っている: {:.1}%\n{m:?}",
        m.shadow_kept * 100.0
    );
    assert!(
        m.eaten < 0.01,
        "影を消すために商品まで削っている: {:.1}%\n{m:?}",
        m.eaten * 100.0
    );
}

/// 影の判定が商品を巻き込まないこと。
///
/// 「背景より暗い無彩色」という条件だけを見れば、中間グレーや黒の商品も
/// 影候補になる。それでも消えないのは、輪郭の段差でフィルが止まるからである。
/// 影の判定を入れたときに最も壊れやすいのがここなので、別立てで固定する。
#[test]
fn the_shadow_rule_does_not_eat_a_neutral_product() {
    for (name, limit) in [("S7", 0.01f32), ("S8", 0.0)] {
        let truth = find(name);
        let m = run(&truth, &CutoutOptions::default());
        assert!(
            m.eaten <= limit,
            "{name}: 無彩色の商品が影として消されている: {:.2}%\n{m:?}",
            m.eaten * 100.0
        );
    }
}

/// 高解像度でも影の判定が商品を食わないこと。
///
/// 影が進める距離は画像の短辺に比例させてある（同じ被写体を 2 倍で撮れば
/// 影の裾も 2 倍の画素数になるため）。比例させたままだと 3000px の素材で
/// 125px まで伸び、柔らかい輪郭を通り抜けた影の判定が商品の内部へ届く。
/// 受け入れテストが 600px までしか回っていなかったので、この崩れは
/// 数値に一切現れていなかった。
#[test]
fn the_shadow_pass_does_not_eat_into_the_product_at_high_resolution() {
    let truth = find("S10");
    let m = run(&truth, &CutoutOptions::default());
    // 実測 0.00%。0.002 は測り方の揺らぎを吸収するだけの幅で、
    // 「600px では見えない崩れが入ったら落ちる」ことを狙っている
    assert!(
        m.eaten < 0.002,
        "高解像度で商品が影として削られている: {:.2}%\n{m:?}",
        m.eaten * 100.0
    );
    // 影そのものは消えていること。距離を切りすぎれば eaten は下がるが、
    // それは影を消さなくなっただけで改善ではない
    assert!(
        m.shadow_kept < 0.05,
        "落ち影が残っている: {:.1}%\n{m:?}",
        m.shadow_kept * 100.0
    );
}

/// 影の判定を切れば影が残ることを確かめる対照実験。
/// 「もともと残っていなかっただけ」で上のテストが通るのを防ぐ。
#[test]
fn turning_off_the_shadow_rule_leaves_the_shadow_behind() {
    let truth = find("S6");
    let m = run(
        &truth,
        &CutoutOptions {
            shadow_tolerance: 0.0,
            ..Default::default()
        },
    );
    assert!(
        m.shadow_kept > 0.20,
        "対照が成立していない（影判定なしでも影が消えている）: {:.1}%",
        m.shadow_kept * 100.0
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

/// 櫛状の商品画像。周期 `period` px（歯と隙間が半分ずつ）の細かい構造を作る。
///
/// メッシュ・レース・ニット・ワイヤーラック・文字のように、境界帯が構造そのもの
/// より太くなる素材を模す。滑らかなシルエットでは帯は周長ぶんしか立たないが、
/// この形では帯が面ごと埋まり、窓の走査量が帯の面積に比例して効いてくる。
fn comb_image(width: u32, height: u32, period: u32) -> RgbaImage {
    let mut img = RgbaImage::from_pixel(width, height, Rgba([248, 248, 247, 255]));
    let product = Rgba([190u8, 70, 55, 255]);
    let (x0, x1) = (width / 5, width * 4 / 5);
    let (y0, y1) = (height / 5, height * 4 / 5);
    // 歯を1つの連結成分にまとめる背骨。面積フィルタで歯だけが消えるのを防ぐ
    let spine = height * 3 / 4;
    let tooth = (period / 2).max(1);
    for y in y0..y1 {
        for x in x0..x1 {
            if y >= spine || (x - x0) % period < tooth {
                img.put_pixel(x, y, product);
            }
        }
    }
    img
}

/// 同じ画像を切り抜くのにかかる最短時間(ms)。
///
/// 最短を採るのは、他プロセスに邪魔された回を混ぜないため。
fn fastest(image: &RgbaImage, refine: bool) -> f64 {
    let opts = CutoutOptions {
        refine,
        ..Default::default()
    };
    let mut best = f64::MAX;
    for _ in 0..2 {
        let started = std::time::Instant::now();
        let result = cutout(image, &opts);
        std::hint::black_box(result.stats.foreground_ratio);
        best = best.min(started.elapsed().as_secs_f64() * 1000.0);
    }
    best
}

/// 境界帯の推定が窓の面積ぶんだけ膨らんでいないこと。
///
/// 帯画素ごとに窓を全走査していた頃は、構造が帯より細い素材で計算量が
/// 「帯の面積 × 窓²」になった。12MP・周期 12px の櫛で refine の追加コストが
/// +9.4 秒に達していたが、これは「帯は周長ぶんしか立たない」という前提が
/// 崩れた形でしか現れず、滑らかなシルエットの計測では一切見えない。
///
/// 絶対時間は機械によって何倍も違うので、同じ画像を refine 抜きで回した時間との
/// 比で見る。実測はこの実装で release 0.5 / debug 0.9、崩れていた頃は同じ形の
/// 320x320 で release 5.9 / debug 14.9 だった。3.0 はその間に置いた緩い上限である。
///
/// 画像を 640x640 にしてあるのは、320x320 だと release の計測対象が 10ms しか
/// 残らず、暖機を含む最短 2 回の比が 0.5 と 0.8 の間で振れたためである。
/// 1 回あたりの時間を稼いだほうが、試行回数を増やすより安い。
#[test]
fn refine_does_not_scale_with_the_area_of_the_window() {
    let image = comb_image(640, 640, 12);
    let with = fastest(&image, true);
    let without = fastest(&image, false);
    let ratio = (with - without) / without;
    assert!(
        ratio < 3.0,
        "refine の追加コストが膨らんでいる: {with:.1} ms vs {without:.1} ms (比 {ratio:.2})"
    );
}

/// 12MP での refine の追加コスト。`--ignored` を付けたときだけ走る。
///
/// 所要時間は帯の周長と帯幅で決まる。角丸矩形のような滑らかなシルエットでは
/// ほぼ無視できるが、構造が帯より細い素材では帯が面ごと埋まって桁が変わるので、
/// 両方を測る。判定値は「桁が変わったら落ちる」ための緩いもので、
/// 実際の値は出力を読むこと。
///
/// ```text
/// cargo test --release --test edge_quality -- --ignored --nocapture
/// ```
#[test]
#[ignore = "計測用。12MP を数枚回すので十数秒かかる"]
fn print_the_refine_cost_on_large_inputs() {
    let (w, h) = (3000u32, 4000u32);
    let mut cases: Vec<(String, RgbaImage)> = vec![(
        "角丸矩形".to_string(),
        edge_scene(&EdgeScene {
            name: "計測",
            width: w,
            height: h,
            ..Default::default()
        })
        .image,
    )];
    for period in [24u32, 12, 6] {
        cases.push((format!("櫛 {period}px 周期"), comb_image(w, h, period)));
    }

    for (name, image) in cases {
        let with = fastest(&image, true);
        let without = fastest(&image, false);
        let overhead = with - without;
        println!(
            "{name:<14} refine {with:>7.0} ms / refine なし {without:>7.0} ms / 追加 {overhead:>+7.0} ms"
        );
        assert!(
            overhead < 2000.0,
            "{name} で refine の追加コストが膨らんでいる: {overhead:+.0} ms"
        );
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
