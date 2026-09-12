//! 境界品質の回帰テスト。
//!
//! 真の被覆率が解析的に分かる合成画像で切り抜きを回し、境界のずれ・背景色の縁・
//! アルファ誤差・ハロー・細部の消失を数値で固定する。境界の良し悪しは目で見ないと
//! 分からないと思われがちだが、正解を持った合成シーンを使えば数値で追える。
//! 追えなければ「直したつもりで悪化させた」ことに気づけない。

mod common;

use common::{EdgeScene, EdgeTruth, edge_scene, edge_scenes, measure_edges};
use image::{Rgba, RgbaImage};
use kiri::cutout::{CutoutOptions, cutout};

fn find(name: &str) -> EdgeTruth {
    let scene = edge_scenes()
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
        edge_threshold: Some(0.0),
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
    // S8 の実測は 0.00% だが、余裕ゼロで固定すると 1 画素の揺らぎでも落ちる。
    // ここで見たいのは「無彩色の商品がまるごと影と見なされる」退行であって、
    // 端の 1 画素ではない
    for (name, limit) in [("S7", 0.01f32), ("S8", 0.002)] {
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

/// 織り目のある背景でも、既定値のままで切り抜けること。
///
/// 実写（不織布の上の黒いリモコン）で見つかった崩れを縮めて固定する。布の
/// 織り目は 1px あたり 8 を超える勾配を持つので、堤防が**背景の中で**壁に
/// なり、フィルが商品まで届かない。
///
/// 利用者が `--edge-threshold 0` を知っていれば救えるが、既定値で通らないなら
/// AI エージェントには救えない。kiri が既定値で解くべき問題である。
#[test]
fn a_woven_background_is_cut_out_with_the_defaults() {
    let truth = find("S11");
    let m = run(&truth, &CutoutOptions::default());
    assert!(
        m.eaten < 0.02,
        "商品が削られている: {:.1}%\n{m:?}",
        m.eaten * 100.0
    );
    assert!(
        m.rim < 0.02,
        "輪郭の外に布が残っている: {:.1}%\n{m:?}",
        m.rim * 100.0
    );
    assert!(
        m.speckles < 0.005,
        "織り目のゴミが背景に残っている: {:.2}%\n{m:?}",
        m.speckles * 100.0
    );
}

/// 境界で復元した色に、商品には無い彩度が乗らないこと。
///
/// 復元式 F=(C-(1-a)B)/a はアルファが小さいほど誤差を増幅する。増幅されるのは
/// 「観測色と局所背景色の差」で、織り目のある背景ではこの差そのものが布の
/// ざらつきと同じ大きさしかない。増幅した結果をチャンネルごとに切り詰めると、
/// 切られたチャンネルだけ動きが止まって色相がねじれる。実写（不織布の上の
/// 黒いリモコン）では輪郭が緑と紫の点線になって出た。
///
/// 輝度で測る `halo` と `白地誤差` はこれを捉えない。合成後の明るさは合って
/// いるのに色だけが外れている状態なので、彩度のずれで固定する。
///
/// 上限は素材ごとに置く。復元が最も効くのは背景と商品の色差が大きい素材で、
/// 崩れたときの振れ幅もそこが一番大きい（修正前の実測で、白背景の濃色商品
/// 102・非圧縮 142・柔輪郭 73・織り目 54 に対し、淡色商品は 0）。
/// 淡色商品のシーン（S3/S9/S12）は帯の大半が幾何的フェザーへ落ちて観測色の
/// まま残るため、この指標では最初から 0 で、網としては働かない
#[test]
fn the_boundary_colour_does_not_pick_up_a_tint() {
    // 実測（修正後）は S2 が 1、他は 2 以下。JPEG とノイズの揺らぎぶんを載せる
    for (name, limit) in [
        ("S1", 8.0f32),
        ("S2", 8.0),
        ("S4", 8.0),
        ("S5", 8.0),
        ("S6", 8.0),
        ("S8", 8.0),
        ("S11", 8.0),
    ] {
        let truth = find(name);
        let m = run(&truth, &CutoutOptions::default());
        assert!(
            m.cast < limit,
            "{name}: 境界の色が商品から彩度で {:.0} ずれている\n{m:?}",
            m.cast
        );
    }
}

/// 画面の端で見切れた柄物の商品が、テクスチャ検知を誤発火させないこと。
///
/// EC では「商品が画面の下端で切れている」構図が頻出する。このとき外周の帯の
/// 1 辺はまるごと商品の内部になり、その商品が無地でなければ帯の勾配が跳ねる。
/// **背景はきれいなのに堤防が引き上がる**という誤検知で、堤防が守るはずだった
/// 淡色商品（S3）が背景ごと消える。
///
/// 発火条件を p90 だけに置いていた頃の実測（このシーン）では、外周の勾配は
/// p50 0.4 / p90 19.2 で、堤防は 8 から 28.9 へ上がっていた。淡色商品が
/// 耐えられるのは 25 までなので、28.9 は堤防を無効化したのと同じで、
/// 境界近傍の欠けは 8.7% から 38.7% へ跳ねる。
///
/// 判定を「既定と同じ」ではなく「堤防 8 を明示したときと同じ」に置くのは、
/// 誤発火していないことを直接言うためである。
#[test]
fn a_patterned_product_cropped_at_the_bottom_does_not_raise_the_dam() {
    let scene = EdgeScene {
        name: "H1",
        width: 1200,
        height: 1200,
        product: [232, 232, 230],
        shading: (0.98, 0.80),
        // 下端 60px が、周期 10px の柄を持つ別の商品で埋まっている
        cropped_band: Some((60, 24.0, 10.0)),
        ..Default::default()
    };
    let truth = edge_scene(&scene);
    let clean = edge_scene(&EdgeScene {
        cropped_band: None,
        ..scene
    });

    let auto = cutout(&truth.image, &CutoutOptions::default());
    let texture = auto.background.texture;
    assert!(
        texture.p90 > 15.0,
        "前提が崩れている: 見切れた柄が p90 を押し上げていない: {texture:?}"
    );
    assert!(
        texture.p50 < 2.0,
        "前提が崩れている: 背景そのものはきれいなはず: {texture:?}"
    );
    assert_eq!(
        auto.edge_threshold,
        kiri::cutout::DEFAULT_EDGE_THRESHOLD,
        "帯の 1 辺が商品でも堤防が引き上がっている: {texture:?}"
    );

    // 堤防 8 を明示した場合と、見切れの無い同じシーンと、3 つが揃うこと
    let pinned = run(
        &truth,
        &CutoutOptions {
            edge_threshold: Some(kiri::cutout::DEFAULT_EDGE_THRESHOLD),
            ..Default::default()
        },
    );
    let m = measure_edges(&truth, &auto.image, &auto.mask);
    let without = run(&clean, &CutoutOptions::default());
    assert!(
        (m.eaten - pinned.eaten).abs() < 0.005,
        "既定と「堤防 8 を明示」で結果が違う: {:.1}% と {:.1}%",
        m.eaten * 100.0,
        pinned.eaten * 100.0
    );
    assert!(
        (m.eaten - without.eaten).abs() < 0.005,
        "見切れの有無で淡色商品の削れ方が変わっている: {:.1}% と {:.1}%",
        m.eaten * 100.0,
        without.eaten * 100.0
    );
}

/// 上の対照実験。堤防を既定値のまま**明示**すれば布の縁が残ること。
///
/// 「もともと堤防が邪魔をしていなかっただけ」で上のテストが通るのを防ぐ。
/// 明示指定に自動調整が割り込まないことも、ここで同時に固定している。
#[test]
fn pinning_the_dam_by_hand_leaves_the_woven_background_behind() {
    let truth = find("S11");
    let m = run(
        &truth,
        &CutoutOptions {
            edge_threshold: Some(kiri::cutout::DEFAULT_EDGE_THRESHOLD),
            ..Default::default()
        },
    );
    assert!(
        m.rim > 0.20,
        "対照が成立していない（堤防を明示しても布が残らない）: {:.1}%\n{m:?}",
        m.rim * 100.0
    );
}

/// テクスチャ検知は堤防を**無効化するのではなく引き上げる**こと。
///
/// S11 は濃色商品なので、堤防が 21 でも 0 でも同じ結果になる。つまり
/// `TEXTURE_DAM_HEADROOM` を 10 倍にしても S11 は通ってしまい、
/// 「引き上げ」という設計の核心は何にも固定されていなかった。
///
/// S12 は織り目（p90 14.0）の上に、輪郭の段差が 1px あたり 28 の淡色商品を
/// 置く。堤防を切ると商品はまるごと背景として飲まれ、既定の 8 に置くと
/// 織り目が壁になって背景が残る。**その間にしか正解が無い。** 実測では
/// 12〜24 の窓で両立し、既定の 1.5 倍（21）はその中にある。
#[test]
fn the_raised_dam_still_protects_a_light_product_on_a_woven_background() {
    let truth = find("S12");
    let auto = cutout(&truth.image, &CutoutOptions::default());
    assert!(
        auto.edge_threshold > kiri::cutout::DEFAULT_EDGE_THRESHOLD,
        "テクスチャ検知が発火していない: {:.1}",
        auto.edge_threshold
    );
    let m = measure_edges(&truth, &auto.image, &auto.mask);
    assert!(
        m.eaten < 0.02,
        "引き上げた堤防が淡色商品を守れていない: {:.1}%\n{m:?}",
        m.eaten * 100.0
    );
    assert!(
        m.rim < 0.02,
        "引き上げた堤防が織り目を残している: {:.1}%\n{m:?}",
        m.rim * 100.0
    );

    // 両端の対照。どちらか一方でも成立していなければ、上の合格は
    // 「もともと両立していただけ」で引き上げ幅を何も語らない
    let off = run(
        &truth,
        &CutoutOptions {
            edge_threshold: Some(0.0),
            ..Default::default()
        },
    );
    assert!(
        off.eaten > 0.50,
        "対照が成立していない（堤防を切っても淡色商品が残る）: {:.1}%\n{off:?}",
        off.eaten * 100.0
    );
    let pinned = run(
        &truth,
        &CutoutOptions {
            edge_threshold: Some(kiri::cutout::DEFAULT_EDGE_THRESHOLD),
            ..Default::default()
        },
    );
    assert!(
        pinned.rim > 0.50,
        "対照が成立していない（既定の堤防でも織り目が残らない）: {:.1}%\n{pinned:?}",
        pinned.rim * 100.0
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

/// ゴミの大きさは解像度に比例するので、面積の下限も比例させること。
///
/// 同じ被写体を 4 倍で撮れば、同じ大きさに見えるゴミは 16 倍の画素を占める。
/// 面積を 25px² に固定していた頃、20MP の不織布では織り目の 1 粒が 150px² あり、
/// 既定の `--cleanup 2` では一つも消えなかった。`--cleanup 8` を渡せば消えたが、
/// それは「解像度を見て利用者が換算する」ことを求めており、既定値の意味を失う。
///
/// 小さい画像では従来どおりであることも同時に見る。単に下限を上げただけなら
/// サムネイルの細部まで巻き添えになるが、それは改善ではない。
#[test]
fn a_speck_scales_with_the_resolution_but_small_images_are_untouched() {
    let speck_survives = |size: u32| -> bool {
        let bg = [248u8, 248, 247];
        let mut img = RgbaImage::from_pixel(size, size, Rgba([bg[0], bg[1], bg[2], 255]));
        let product = Rgba([40u8, 40, 45, 255]);
        let (a, b) = (size / 3, size * 2 / 3);
        for y in a..b {
            for x in a..b {
                img.put_pixel(x, y, product);
            }
        }
        // 7x7 = 49px²。長辺 1000px までは下限 25px² を超えるので残り、
        // 長辺 2000px では下限が 100px² になるので消える。
        //
        // 崖に寄せない。長辺 1500px の下限は 56px² で 49px² との差が 12% しか
        // なく、丸めや境界帯の 1px の増減で符号が変わる。「消えるか残るか」を
        // 見たいのであって、丸めの向きを見たいのではない
        let (sx, sy) = (size / 10, size / 10);
        for y in sy..sy + 7 {
            for x in sx..sx + 7 {
                img.put_pixel(x, y, product);
            }
        }
        let result = cutout(&img, &CutoutOptions::default());
        result.mask.is_foreground(sx + 3, sy + 3)
    };

    assert!(speck_survives(400), "小さい画像で 7x7 の細部まで消えている");
    assert!(speck_survives(1000), "長辺ちょうど 1000px で消えている");
    assert!(
        !speck_survives(2000),
        "高解像度で 7x7 相当のゴミが残っている"
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

/// guided feathering が、きれいな合成シーンのアルファを動かさないこと。
///
/// **ここが動いたら ε の決め方が壊れている。** ε は窓の中の確定背景の分散から
/// 決まるので、スタジオ背景では `EPS_FLOOR`（σ0² = 0.015²）に落ちて guided
/// filter はほぼ恒等になる。均すのは背景がざらついている場所だけである。
///
/// S1（濃色 × 白 × JPEG）/ S2（非圧縮）/ S4（8px で溶ける輪郭）で、Phase 2
/// までの経路（`--matting projection --smooth-contour 0 --no-reclassify`）との
/// アルファ誤差の差を ±0.005 に収める。
#[test]
fn the_matting_stages_leave_a_clean_synthetic_edge_alone() {
    let plain = CutoutOptions {
        matting: kiri::cutout::Matting::Projection,
        smooth_contour: 0.0,
        reclassify: false,
        ..Default::default()
    };
    for name in ["S1", "S2", "S4", "S5", "S5b"] {
        let truth = find(name);
        let before = run(&truth, &plain);
        let after = run(&truth, &CutoutOptions::default());
        assert!(
            (after.alpha_mae - before.alpha_mae).abs() <= 0.005,
            "{name}: アルファ誤差が動いた {:.4} → {:.4}",
            before.alpha_mae,
            after.alpha_mae
        );
        assert!(
            (after.offset - before.offset).abs() <= 0.25,
            "{name}: 境界位置が動いた {:+.3} → {:+.3}",
            before.offset,
            after.offset
        );
        if before.strap_kept.is_finite() {
            assert!(
                after.strap_kept >= before.strap_kept,
                "{name}: ストラップの残存が落ちた {:.3} → {:.3}",
                before.strap_kept,
                after.strap_kept
            );
        }
    }
}

/// 3 つのスイッチを明示した経路と既定の経路が、**織り目の上では本当に違う**こと。
///
/// 上のテストは「動かないこと」しか見ない。対照が無いと、3 段がまるごと死んで
/// いても両方通ってしまう。
#[test]
fn the_three_switches_actually_change_something() {
    let truth = find("S11");
    let plain = cutout(
        &truth.image,
        &CutoutOptions {
            matting: kiri::cutout::Matting::Projection,
            smooth_contour: 0.0,
            reclassify: false,
            ..Default::default()
        },
    );
    let staged = cutout(&truth.image, &CutoutOptions::default());
    assert_ne!(
        plain.mask, staged.mask,
        "3 段を切っても切らなくても同じマスクが出ている＝段が効いていない"
    );
    assert_eq!(
        plain.band_min_radius,
        Some(kiri::cutout::refine::DEFAULT_MIN_RADIUS),
        "3 つ切った経路で帯幅の下限が動いている"
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

    // **診断値の計測を先に置く。** 後ろへ回すと、refine の計測で確保された
    // 空きをアロケータが使い回してしまい、RSS の増分が 0 としか出ない。
    // **判定はしない。** 絶対時間は機械によって何倍も違い、書き写した数値は
    // 必ず嘘になる。その場で回して読むためのものである
    let result = cutout(&cases[0].1, &CutoutOptions::default());

    // **RSS を先に、1 回だけ測る。** サンプラーでピークを追っていた頃は 0MB と
    // しか出なかった——アロケータは解放したページを OS へ返さないので、同じ
    // 関数を 2 回目に回したときは「確保済みの空き」を使い回し、RSS がまったく
    // 動かない。知りたいのは診断が要求する常駐量なので、**まだ一度も回して
    // いない状態から 1 回だけ回して前後の差を見る**。時間の計測（最短を採るには
    // 何回か回す必要がある）と同じパスではできない
    let before = resident_kb();
    let d =
        kiri::cutout::diagnostics::diagnose(&cases[0].1, &result.mask, result.background.rgb, None);
    let after = resident_kb();
    std::hint::black_box((d.contour_roughness, d.rim_contamination));

    // 時間は最短を採る。他プロセスに邪魔された回を混ぜない。
    // **判定はしない。** 絶対時間は機械によって何倍も違い、書き写した数値は
    // 必ず嘘になる。その場で回して読むためのものである
    let mut best = f64::MAX;
    for _ in 0..5 {
        let started = std::time::Instant::now();
        let d = kiri::cutout::diagnostics::diagnose(
            &cases[0].1,
            &result.mask,
            result.background.rgb,
            None,
        );
        std::hint::black_box((d.contour_roughness, d.rim_contamination));
        best = best.min(started.elapsed().as_secs_f64() * 1000.0);
    }
    // 内訳。**新しい 2 つがいくら足したかは、古い 2 つと分けないと分からない**
    let mut halo = f64::MAX;
    let mut width = f64::MAX;
    for _ in 0..5 {
        let started = std::time::Instant::now();
        std::hint::black_box(kiri::cutout::diagnostics::halo_ratio(
            &cases[0].1,
            &result.mask,
            result.background.rgb,
        ));
        halo = halo.min(started.elapsed().as_secs_f64() * 1000.0);
        let started = std::time::Instant::now();
        std::hint::black_box(kiri::cutout::diagnostics::edge_width(&result.mask));
        width = width.min(started.elapsed().as_secs_f64() * 1000.0);
    }
    println!(
        "診断           diagnose() {best:>7.1} ms（halo_ratio {halo:.1} / edge_width {width:.1} / \
         新しい 2 つ {:.1}）/ RSS {:+} MB",
        best - halo - width,
        (after - before) / 1024,
    );
    // **0 は「増えなかった」ではなく「切り抜きが確保して解放したページを
    // 使い回した」の意味である。** アロケータは解放したページを OS へ
    // 返さないので、`cutout()` の後に測るかぎりこれ以上のことは分からない。
    // 診断が確保する量そのものは design.md 4.8 の表にある（12MP で
    // チェビシェフ距離 12MB + 二値マスク 12MB + 枠ぶんの距離場と格子）
    if (after - before) / 1024 == 0 {
        println!("               （RSS 0 は cutout() の解放済みページを使い回したという意味）");
    }
    drop(result);

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

/// 自プロセスの常駐メモリ(KB)。依存を足さずに済ませるため `ps` を呼ぶ。
fn resident_kb() -> i64 {
    std::process::Command::new("ps")
        .args(["-o", "rss=", "-p", &std::process::id().to_string()])
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .and_then(|s| s.trim().parse().ok())
        .unwrap_or(0)
}

/// 実装前後の比較に使う一覧表。`--ignored` を付けたときだけ走る。
///
/// **合成背景（S）と実写背景（R）を同じ表に並べる。** 診断値のしきい値は
/// 「クリーンなシーンの最大値」と「欠陥シーンの最小値」の窓で決まるので、
/// 両方を同じ物差しで見られなければ較正できない。
///
/// ```text
/// cargo test --release --test edge_quality -- --ignored --nocapture
/// ```
#[test]
#[ignore = "計測用。判定はせず表を出すだけ"]
fn print_the_metrics_table() {
    println!(
        "\n{:<28} {:<10} {:>8} {:>7} {:>7} {:>9} {:>7} {:>7} {:>8} {:>8} {:>8} {:>7}",
        "シーン",
        "設定",
        "境界ずれ",
        "rim",
        "eaten",
        "alphaMAE",
        "halo",
        "白地誤差",
        "strap",
        "shadow残",
        "織り目残",
        "色ずれ"
    );

    let round1 = |v: f64| (v * 10.0).round() / 10.0;
    let round3 = |v: f64| (v * 1000.0).round() / 1000.0;
    // 新しい 4 指標は 2 行目にまとめて出す。粗さは「正解版」と並べないと、
    // 値が大きいことが欠陥なのか測り方なのか読み手には分からない
    let second_line = |m: &common::EdgeMetrics, d: &kiri::cutout::Diagnostics| {
        println!(
            "{:<39} roughness={:?} / 正解 {:.2}   rim_contam={:?} / 正解 {:.3}   halo_ratio={:?} edge_width={:?}",
            "",
            d.contour_roughness.map(round3),
            m.contour_error,
            d.rim_contamination.map(round3),
            m.rim_truth,
            d.halo_ratio.map(round3),
            d.edge_width.map(round1),
        );
    };

    for scene in edge_scenes() {
        let truth = edge_scene(&scene);
        for (label, opts) in [
            ("既定", CutoutOptions::default()),
            (
                "堤防なし",
                CutoutOptions {
                    edge_threshold: Some(0.0),
                    ..Default::default()
                },
            ),
        ] {
            let result = cutout(&truth.image, &opts);
            let m = measure_edges(&truth, &result.image, &result.mask);
            print_row(scene.name, label, &m);
            second_line(&m, &result.diagnostics);
            println!(
                "{:<39} separability={:?} 外周勾配 p50={:.1}/p90={:.1} 効いた堤防={:.1}",
                "",
                result.separability.map(round1),
                result.background.texture.p50,
                result.background.texture.p90,
                result.edge_threshold,
            );
        }
        println!();
    }

    for scene in common::real_scenes() {
        let (_, runs) = common::run_real(&scene);
        for run in &runs {
            print_row(scene.name, run.setting, &run.metrics);
            second_line(&run.metrics, &run.diagnostics);
            // **指示の占有率を並べる。** 指標が良くなったのが「指示が良かった」
            // からなのか「画像の 7 割を確定背景だと言い切った」からなのかは、
            // 占有率を見なければ分けられない
            let constraints = match run.constraints {
                Some((fg, bg)) => format!(" 制約 fg={:.3}/bg={:.3}", fg, bg),
                None => String::new(),
            };
            println!(
                "{:<39} tolerance={:.0} fg={:.3} separability={:?}{constraints} warnings={:?}",
                "",
                run.tolerance,
                run.foreground_ratio,
                run.separability.map(round1),
                run.warnings,
            );
        }
        println!();
    }
}

fn print_row(scene: &str, setting: &str, m: &common::EdgeMetrics) {
    println!(
        "{scene:<28} {setting:<10} {:>+8.3} {:>6.1}% {:>6.1}% {:>9.3} {:>7.1} {:>8.1} {:>7.1}% {:>7.1}% {:>7.2}% {:>7.1}",
        m.offset,
        m.rim * 100.0,
        m.eaten * 100.0,
        m.alpha_mae,
        m.halo,
        m.white_error,
        m.strap_kept * 100.0,
        m.shadow_kept * 100.0,
        m.speckles * 100.0,
        m.cast,
    );
}

/// 櫛状の素材で、正当な隙間が最終アルファまで抜けること。
///
/// `--seal` は「幅 2N px 以下の隙間だけを塞ぐ」と約束している。堤防が隙間の
/// 両側を背景候補から外すぶん、素直に組み合わせると 3px の通路まで塞がって
/// いた（実測 7.9% しか抜けない）。メッシュ・レース・ワイヤーラックのように
/// 隙間が意味を持つ素材では、これは目に見える欠陥になる。
///
/// 単体テストは背景マスクを見るが、こちらは最終的なアルファで測る。
/// 帯の再推定が縁を透明へ戻すので、利用者が受け取る結果はここに出る。
#[test]
fn the_gaps_of_a_comb_are_transparent_in_the_result() {
    for (gap, open_ratio) in [(1u32, false), (2, false), (3, true), (5, true)] {
        let (w, h) = (240u32, 240u32);
        let mut img = RgbaImage::from_pixel(w, h, Rgba([248, 248, 247, 255]));
        let period = 5 + gap;
        let (x0, x1) = (w / 5, w * 4 / 5);
        let (y0, y1) = (h / 5, h * 4 / 5);
        // 歯を 1 つの連結成分にまとめる背骨。面積フィルタで歯だけが消えるのを防ぐ
        let spine = h * 3 / 4;
        for y in y0..y1 {
            for x in x0..x1 {
                if y >= spine || (x - x0) % period < 5 {
                    img.put_pixel(x, y, Rgba([190, 70, 55, 255]));
                }
            }
        }

        let out = cutout(&img, &CutoutOptions::default());
        let (mut total, mut clear) = (0u32, 0u32);
        for y in (y0 + 2)..(spine - 2) {
            for x in x0..x1 {
                if (x - x0) % period >= 5 {
                    total += 1;
                    if out.image.get_pixel(x, y)[3] < 128 {
                        clear += 1;
                    }
                }
            }
        }
        let ratio = f64::from(clear) / f64::from(total.max(1));
        if open_ratio {
            assert!(
                ratio > 0.90,
                "幅 {gap}px の隙間が抜けていない: {:.1}%",
                ratio * 100.0
            );
        } else {
            assert!(
                ratio < 0.10,
                "幅 {gap}px の破れが塞がっていない: {:.1}%",
                ratio * 100.0
            );
        }
    }
}

/// ベンチのシーンを PNG として書き出す。`--ignored` を付けたときだけ走る。
///
/// **「前の Phase と 1 バイトも変わらない」を確かめるための足場である。**
/// 過去の実装はリポジトリの中に無いので、同じ入力を両方のバイナリへ通して
/// md5 を突き合わせるしかない。シーンの生成器（`tests/common`）は共有なので、
/// どちらの worktree から書き出しても画素は同じになる。
///
/// ```text
/// git worktree add /tmp/prev <前の Phase のブランチ>
/// (cd /tmp/prev && cargo build --release)
/// KIRI_DUMP_DIR=/tmp/scenes cargo test --release --test edge_quality -- --ignored dump_scenes
/// for f in /tmp/scenes/*.png; do
///   /tmp/prev/target/release/kiri cutout "$f" -o /tmp/a.png --force
///   ./target/release/kiri cutout "$f" -o /tmp/b.png --force \
///     --matting projection --smooth-contour 0 --no-reclassify
///   md5 -q /tmp/a.png /tmp/b.png
/// done
/// ```
#[test]
#[ignore = "計測用。KIRI_DUMP_DIR が無ければ何もしない"]
fn dump_scenes() {
    let Ok(dir) = std::env::var("KIRI_DUMP_DIR") else {
        return;
    };
    let dir = std::path::Path::new(&dir);
    std::fs::create_dir_all(dir).unwrap();
    for scene in edge_scenes() {
        let truth = edge_scene(&scene);
        let name = scene.name.split(' ').next().unwrap();
        truth.image.save(dir.join(format!("{name}.png"))).unwrap();
    }
    for scene in common::real_scenes() {
        let truth = common::real_scene(&scene);
        let name = scene.name.split(' ').next().unwrap();
        truth.image.save(dir.join(format!("{name}.png"))).unwrap();
    }
}
