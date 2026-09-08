//! 回転。
//!
//! `resize` と同じく、出力寸法の決定（`plan`）と画素処理（`apply`）を分ける。
//! 寸法計算のほうが取り違えやすく、かつ画像なしで網羅的に検証できるためである。
//!
//! **角度は時計回りを正とする。** 「写真が右に傾いているので左へ戻す」を
//! `--angle -3` と書けることを優先した。負値も 360 を超える値も受け付け、
//! `plan` が `[0, 360)` へ正規化する。
//!
//! **EXIF の向きは読み込み時に適用済み**なので、ここで扱うのは常に正立した
//! 画像である。`--angle 90` は「EXIF の値に 90 を足す」ではなく「見えている
//! 絵を 90 度回す」を意味する。

use image::RgbaImage;

use crate::error::{Error, Result};

#[derive(Debug, Clone, Copy)]
pub struct RotateSpec {
    /// 時計回りの角度(度)。負値は反時計回り
    pub angle: f64,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RotatePlan {
    /// `[0, 360)` へ正規化した時計回りの角度
    pub angle: f64,
    /// 90 度単位なら Some(0..=3)。この場合は再サンプリングしない
    pub quarter_turns: Option<u8>,
    /// 出力寸法
    pub output: (u32, u32),
}

impl RotatePlan {
    /// 画素を補間し直すか。90 度単位なら false で、出力は入力と 1 バイトも変わらない。
    pub fn resampled(&self) -> bool {
        self.quarter_turns.is_none()
    }
}

/// 元寸法と角度から出力寸法を決める。
pub fn plan(source: (u32, u32), spec: &RotateSpec) -> Result<RotatePlan> {
    let (sw, sh) = source;
    if sw == 0 || sh == 0 {
        return Err(Error::processing("EMPTY_IMAGE", "画像の寸法が 0 です"));
    }
    if !spec.angle.is_finite() {
        return Err(Error::argument(
            "INVALID_ANGLE",
            format!("'{}' は角度として扱えません", spec.angle),
        )
        .with_hint("--angle には有限の数値を指定してください（例: --angle 90、--angle -3.5）"));
    }

    // 時計回りの [0, 360) へ寄せる。-90 と 270 と 630 は同じ操作であり、
    // 以降の分岐をこの 1 箇所に集約する
    let mut angle = spec.angle.rem_euclid(360.0);
    // `rem_euclid` は極小の負値に対して 360.0 ちょうどを返す。360 の ulp が
    // 約 5.7e-14 なので、`-1e-15 + 360.0` は 360.0 へ丸め上がるためである。
    // **上端を閉じるのはここしかない。** 素通しすると 90 で割った商が 4 になり、
    // 「90 度単位だが 4 分の 1 回転が 4 回」という存在しない状態が下流へ流れる
    if angle >= 360.0 {
        angle = 0.0;
    }

    // 90 の倍数かどうかは厳密一致で見る。「ほぼ 90 度」を無劣化の枝へ
    // 流すと、指定した角度と実際に回った角度が黙って食い違う
    let quarter = angle / 90.0;
    let quarter_turns = if quarter == quarter.trunc() {
        Some(quarter as u8)
    } else {
        None
    };

    let output = match quarter_turns {
        Some(1 | 3) => (sh, sw),
        Some(_) => (sw, sh),
        None => {
            let rad = angle.to_radians();
            let (s, c) = (rad.sin().abs(), rad.cos().abs());
            // 切り上げる。外接矩形は sw*c + sh*s ちょうどなので、丸めると
            // 四隅が小数画素ぶん欠ける。**回転は情報を捨てる操作であっては
            // ならない**ので、余るほうへ倒す
            (
                bounding_dim(sw as f64 * c + sh as f64 * s),
                bounding_dim(sw as f64 * s + sh as f64 * c),
            )
        }
    };

    Ok(RotatePlan {
        angle,
        quarter_turns,
        output,
    })
}

/// 外接矩形の 1 辺を画素数へ落とす。0 は後段の全処理を壊すため 1 を下回らせない。
fn bounding_dim(v: f64) -> u32 {
    (v.ceil() as u32).max(1)
}

/// 計画に従って実際に回す。
pub fn apply(image: &RgbaImage, plan: &RotatePlan) -> Result<RgbaImage> {
    match plan.quarter_turns {
        Some(0) => Ok(image.clone()),
        Some(1) => Ok(image::imageops::rotate90(image)),
        Some(2) => Ok(image::imageops::rotate180(image)),
        Some(3) => Ok(image::imageops::rotate270(image)),
        // `plan` が角度を [0, 360) に閉じるので 4 以上は現れない。仮に現れても
        // 任意角の経路が同じ絵を出すため、内部不変条件の破れを利用者への
        // エラーに変換しない（無劣化ではなくなるだけで、結果は正しい）
        _ => Ok(resample(image, plan)),
    }
}

/// 任意角の回転。出力側の画素中心を入力へ逆写像して補間する。
///
/// **事前乗算アルファで補間する。** `resize` が `use_alpha` に頼っているのと
/// 同じ理由で、素の RGB を混ぜると透明な画素の色が境界に滲み出る。切り抜き済みの
/// 画像を回すのは kiri の主用途そのものなので、ここを外すと輪郭が色づく。
///
/// 入力の外側は「透明」として扱う。四隅の余白がアルファ 0 で埋まるのも、
/// 縁が滑らかに落ちるのも、この 1 つの決めから出てくる。
fn resample(image: &RgbaImage, plan: &RotatePlan) -> RgbaImage {
    use rayon::prelude::*;

    let (sw, sh) = (image.width() as i64, image.height() as i64);
    let (ow, oh) = plan.output;
    let rad = plan.angle.to_radians();
    let (sin, cos) = rad.sin_cos();

    // 出力の中心から入力の中心への逆写像。前進が時計回り [[c,-s],[s,c]] なので、
    // その逆は [[c,s],[-s,c]] になる（画面座標は y が下向き）
    let (ocx, ocy) = (ow as f64 / 2.0, oh as f64 / 2.0);
    let (scx, scy) = (sw as f64 / 2.0, sh as f64 / 2.0);

    let mut out = RgbaImage::new(ow, oh);
    let row_bytes = ow as usize * 4;

    out.as_mut()
        .par_chunks_mut(row_bytes)
        .enumerate()
        .for_each(|(y, row)| {
            let dy = y as f64 + 0.5 - ocy;
            for (x, px) in row.chunks_exact_mut(4).enumerate() {
                let dx = x as f64 + 0.5 - ocx;
                // 画素中心が (i+0.5, j+0.5) なので、標本座標へは 0.5 を引いて渡す
                let fx = (cos * dx + sin * dy + scx) - 0.5;
                let fy = (-sin * dx + cos * dy + scy) - 0.5;
                px.copy_from_slice(&sample(image, sw, sh, fx, fy));
            }
        });

    out
}

/// Catmull-Rom（4x4 タップ）で 1 点を補間する。
///
/// 縮小を伴わないので、`resize` の Lanczos3 ほど広いカーネルは要らない。
/// 双一次では回すたびに目に見えて甘くなるため、間を取って三次で拾う。
fn sample(image: &RgbaImage, sw: i64, sh: i64, fx: f64, fy: f64) -> [u8; 4] {
    let (ix, iy) = (fx.floor(), fy.floor());
    let wx = catmull_rom_weights(fx - ix);
    let wy = catmull_rom_weights(fy - iy);
    let (ix, iy) = (ix as i64, iy as i64);

    let mut acc = [0.0f64; 4];
    for (j, wyj) in wy.iter().enumerate() {
        let sy = iy - 1 + j as i64;
        if *wyj == 0.0 || sy < 0 || sy >= sh {
            continue;
        }
        for (i, wxi) in wx.iter().enumerate() {
            let sx = ix - 1 + i as i64;
            if *wxi == 0.0 || sx < 0 || sx >= sw {
                continue;
            }
            let p = image.get_pixel(sx as u32, sy as u32).0;
            let w = wxi * wyj;
            // 事前乗算した値を混ぜる
            let a = p[3] as f64;
            acc[0] += w * p[0] as f64 * a;
            acc[1] += w * p[1] as f64 * a;
            acc[2] += w * p[2] as f64 * a;
            acc[3] += w * a;
        }
    }

    // 三次補間は硬い輪郭で必ず行き過ぎる（オーバーシュート）ので、アルファは
    // 255 を超えうる。**戻すときは丸める前の acc[3] で割ること。**
    // 先に 255 で頭打ちしてから割ると、超過ぶんがそのまま色の水増しになる
    // ——単色の商品を回しただけで、その商品に無い明るい色が輪郭に生まれる。
    // 収めるのは事前乗算を解いた後の、色として意味のある値のほうである
    if acc[3] <= 0.0 {
        return [0, 0, 0, 0];
    }
    let alpha = acc[3].clamp(0.0, 255.0).round() as u8;
    // 丸めて透明になったなら色も落とす。見えない画素に色を残すと、
    // 「入力の外側は透明」という決めと食い違ううえ、PNG も縮まない
    if alpha == 0 {
        return [0, 0, 0, 0];
    }
    let unpremultiply = |v: f64| -> u8 { (v / acc[3]).clamp(0.0, 255.0).round() as u8 };
    [
        unpremultiply(acc[0]),
        unpremultiply(acc[1]),
        unpremultiply(acc[2]),
        alpha,
    ]
}

/// Catmull-Rom の重み（a = -0.5）。タップは -1, 0, 1, 2 の順。
fn catmull_rom_weights(t: f64) -> [f64; 4] {
    let (t2, t3) = (t * t, t * t * t);
    [
        -0.5 * t3 + t2 - 0.5 * t,
        1.5 * t3 - 2.5 * t2 + 1.0,
        -1.5 * t3 + 2.0 * t2 + 0.5 * t,
        0.5 * t3 - 0.5 * t2,
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::Rgba;

    fn spec(angle: f64) -> RotateSpec {
        RotateSpec { angle }
    }

    // --- plan: 90 度単位 ---

    #[test]
    fn quarter_turns_swap_the_dimensions_only_when_upright_changes() {
        for (angle, expected) in [
            (0.0, (400, 500)),
            (90.0, (500, 400)),
            (180.0, (400, 500)),
            (270.0, (500, 400)),
        ] {
            let p = plan((400, 500), &spec(angle)).unwrap();
            assert_eq!(p.output, expected, "angle={angle}");
        }
    }

    #[test]
    fn quarter_turns_are_never_resampled() {
        for angle in [0.0, 90.0, 180.0, 270.0, 360.0, 450.0, -90.0, -720.0] {
            let p = plan((400, 500), &spec(angle)).unwrap();
            assert!(!p.resampled(), "angle={angle} は無劣化で回せるはず");
        }
    }

    #[test]
    fn a_negative_angle_is_normalized_clockwise() {
        // -90 度（反時計回りに 90）は 270 度（時計回り）と同じ
        let p = plan((400, 500), &spec(-90.0)).unwrap();
        assert_eq!(p.angle, 270.0);
        assert_eq!(p.quarter_turns, Some(3));
        assert_eq!(p.output, (500, 400));
    }

    #[test]
    fn a_full_turn_is_normalized_to_zero() {
        let p = plan((400, 500), &spec(360.0)).unwrap();
        assert_eq!(p.angle, 0.0);
        assert_eq!(p.quarter_turns, Some(0));
        assert_eq!(p.output, (400, 500));
    }

    // --- plan: 任意角 ---

    #[test]
    fn an_arbitrary_angle_expands_to_hold_every_corner() {
        // 100x100 を 45 度回すと外接矩形は一辺 100*sqrt(2) = 141.42。
        // 切り上げるのは、四隅を 1px たりとも欠かさないため
        let p = plan((100, 100), &spec(45.0)).unwrap();
        assert_eq!(p.output, (142, 142));
        assert!(p.resampled());
    }

    #[test]
    fn an_arbitrary_angle_keeps_the_bounding_box_symmetric() {
        // 30 度と 150 度、-30 度は同じ外接矩形になる（cos/sin の絶対値だけで決まる）
        let base = plan((400, 500), &spec(30.0)).unwrap().output;
        for angle in [150.0, -30.0, 210.0, 330.0] {
            assert_eq!(plan((400, 500), &spec(angle)).unwrap().output, base);
        }
    }

    #[test]
    fn a_tiny_angle_still_counts_as_a_rotation() {
        // 90 の倍数から少しでも外れていれば再サンプリングする。
        // 「ほぼ 90 度だから無劣化で回した」と黙って丸めると、指定と結果がずれる
        let p = plan((400, 500), &spec(90.001)).unwrap();
        assert!(p.resampled());
        assert_eq!(p.quarter_turns, None);
    }

    // --- plan: 入力の検証 ---

    #[test]
    fn rejects_a_non_finite_angle() {
        for angle in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            let err = plan((400, 500), &spec(angle)).unwrap_err();
            assert_eq!(err.code, "INVALID_ANGLE");
            assert_eq!(err.exit_code(), 2);
        }
    }

    #[test]
    fn a_negligible_negative_angle_does_not_become_a_fourth_quarter_turn() {
        // `rem_euclid` は極小の負値に対して 360.0 ちょうどを返す（360 の ulp は
        // 約 5.7e-14）。正規化で上端を閉じないと 90 で割った商が 4 になり、
        // 「4 分の 1 回転が 4 回」という存在しない状態で下流が落ちる
        for angle in [-1e-15, -1e-300, -2.8e-14, -f64::MIN_POSITIVE] {
            let p = plan((400, 500), &spec(angle)).unwrap();
            assert!(
                (0.0..360.0).contains(&p.angle),
                "angle={angle} -> {} が [0, 360) の外",
                p.angle
            );
            assert!(
                matches!(p.quarter_turns, None | Some(0..=3)),
                "angle={angle} -> {:?}",
                p.quarter_turns
            );
            assert!(apply(&marked_image(4, 6), &p).is_ok(), "angle={angle}");
        }
    }

    #[test]
    fn rejects_an_empty_image() {
        let err = plan((0, 500), &spec(90.0)).unwrap_err();
        assert_eq!(err.code, "EMPTY_IMAGE");
    }

    // --- apply: 90 度単位は無劣化 ---

    /// 左上に 1 つだけ白い画素を置いた黒画像。回転で「どこへ行ったか」を追う。
    fn marked_image(w: u32, h: u32) -> RgbaImage {
        let mut img = RgbaImage::from_pixel(w, h, Rgba([0, 0, 0, 255]));
        img.put_pixel(0, 0, Rgba([255, 255, 255, 255]));
        img
    }

    #[test]
    fn a_positive_angle_turns_clockwise() {
        // 左上の印は、時計回りに 90 度回せば右上へ移る。
        // ここを取り違えると、以降のすべての角度が鏡像になる
        let img = marked_image(4, 6);
        let p = plan((4, 6), &spec(90.0)).unwrap();
        let out = apply(&img, &p).unwrap();

        assert_eq!((out.width(), out.height()), (6, 4));
        assert_eq!(out.get_pixel(5, 0), &Rgba([255, 255, 255, 255]), "右上");
        assert_eq!(out.get_pixel(0, 0), &Rgba([0, 0, 0, 255]), "左上ではない");
    }

    #[test]
    fn a_negative_angle_turns_counter_clockwise() {
        let img = marked_image(4, 6);
        let p = plan((4, 6), &spec(-90.0)).unwrap();
        let out = apply(&img, &p).unwrap();

        assert_eq!((out.width(), out.height()), (6, 4));
        assert_eq!(out.get_pixel(0, 3), &Rgba([255, 255, 255, 255]), "左下");
    }

    #[test]
    fn an_arbitrary_angle_turns_the_same_way_as_a_quarter_turn() {
        // 90.001 度は厳密一致から外れるので `resample` を通る。90 度との差は
        // 0.001 度しかないので、印は `imageops::rotate90` と同じ右上に来るはず。
        // **このテストが無いと、逆写像の符号を反転しても全テストが通る**——
        // 任意角のテストがどれも点対称な題材を使っており、鏡像に気づけない
        let img = marked_image(4, 6);
        let p = plan((4, 6), &spec(90.001)).unwrap();
        assert!(p.resampled(), "90.001 度は補間の経路を通ること");
        let out = apply(&img, &p).unwrap();

        let brightness = |x: u32, y: u32| out.get_pixel(x, y).0[0];
        assert!(
            brightness(out.width() - 1, 0) > 200,
            "右上に印が無い＝回転方向が逆になっている"
        );
        assert!(
            brightness(0, out.height() - 1) < 60,
            "左下に印がある＝反時計回りに回っている"
        );
    }

    #[test]
    fn an_arbitrary_angle_keeps_the_half_pixel_offset() {
        // 中心の取り方を半画素間違えても、点対称な題材では気づけない。
        // 180.001 度なら、ほぼ点対称の位置（左下）に印が来ることで固定できる
        let img = marked_image(4, 6);
        let p = plan((4, 6), &spec(180.001)).unwrap();
        let out = apply(&img, &p).unwrap();

        assert!(
            out.get_pixel(out.width() - 1, out.height() - 1).0[0] > 200,
            "右下に印が無い＝中心がずれている"
        );
    }

    #[test]
    fn four_quarter_turns_restore_the_original_bit_for_bit() {
        let img = marked_image(4, 6);
        let mut out = img.clone();
        for _ in 0..4 {
            let p = plan((out.width(), out.height()), &spec(90.0)).unwrap();
            out = apply(&out, &p).unwrap();
        }
        assert_eq!(out, img, "90 度 4 回で元に戻らないなら画素を失っている");
    }

    #[test]
    fn a_zero_angle_returns_the_image_untouched() {
        let img = marked_image(4, 6);
        let p = plan((4, 6), &spec(0.0)).unwrap();
        assert_eq!(apply(&img, &p).unwrap(), img);
    }

    // --- apply: 任意角 ---

    #[test]
    fn the_corners_left_over_by_an_arbitrary_angle_are_transparent() {
        let img = RgbaImage::from_pixel(40, 40, Rgba([200, 60, 50, 255]));
        let p = plan((40, 40), &spec(45.0)).unwrap();
        let out = apply(&img, &p).unwrap();

        assert_eq!(out.get_pixel(0, 0)[3], 0, "左上の余白");
        assert_eq!(out.get_pixel(out.width() - 1, 0)[3], 0, "右上の余白");
        assert_eq!(out.get_pixel(0, out.height() - 1)[3], 0, "左下の余白");
        assert_eq!(
            out.get_pixel(out.width() - 1, out.height() - 1)[3],
            0,
            "右下の余白"
        );
    }

    #[test]
    fn the_interior_keeps_its_color_and_stays_opaque() {
        let img = RgbaImage::from_pixel(40, 40, Rgba([200, 60, 50, 255]));
        let p = plan((40, 40), &spec(30.0)).unwrap();
        let out = apply(&img, &p).unwrap();

        let center = out.get_pixel(out.width() / 2, out.height() / 2);
        assert_eq!(center[3], 255, "中心は不透明のまま");
        assert_eq!([center[0], center[1], center[2]], [200, 60, 50]);
    }

    #[test]
    fn transparent_pixels_do_not_bleed_their_color_into_the_subject() {
        // 透明部に「見えない緑」を仕込む。事前乗算せずに補間すると、
        // 境界の画素がこの緑を吸って商品の輪郭が色づく
        let mut img = RgbaImage::from_pixel(40, 40, Rgba([0, 255, 0, 0]));
        for y in 10..30 {
            for x in 10..30 {
                img.put_pixel(x, y, Rgba([200, 40, 30, 255]));
            }
        }
        let p = plan((40, 40), &spec(20.0)).unwrap();
        let out = apply(&img, &p).unwrap();

        // しきい値は商品の緑成分 40 のすぐ上に置く。透明部の緑は 255 なので
        // 90 でも「事前乗算を完全に忘れた」実装は捕まるが、**数値の取り扱いを
        // 少し間違えただけの滲み（実測で 40 → 45）は素通しする**
        for px in out.pixels().filter(|p| p[3] > 128) {
            assert!(
                px[1] <= 41,
                "不透明側に透明画素の緑が滲んでいる: {:?}",
                px.0
            );
        }
    }

    #[test]
    fn an_arbitrary_angle_never_invents_a_colour_the_product_does_not_have() {
        // 不透明な色は (200,40,30) の 1 色しかない。事前乗算の戻しが正しければ、
        // アルファが何であれ RGB はこの色を超えない。**オーバーシュートした
        // アルファを 255 で頭打ちしてから割ると、ここが 226 まで持ち上がる**
        // ——回しただけで商品に無い明るい縁が生まれる
        let mut img = RgbaImage::from_pixel(40, 40, Rgba([0, 255, 0, 0]));
        for y in 10..30 {
            for x in 10..30 {
                img.put_pixel(x, y, Rgba([200, 40, 30, 255]));
            }
        }
        let p = plan((40, 40), &spec(20.0)).unwrap();
        let out = apply(&img, &p).unwrap();

        for px in out.pixels().filter(|p| p.0[3] > 0) {
            assert!(
                px.0[0] <= 200 && px.0[1] <= 40 && px.0[2] <= 30,
                "商品に無い色が出た: {:?}",
                px.0
            );
        }
    }

    #[test]
    fn a_pixel_that_rounds_to_transparent_carries_no_colour() {
        // アルファが 0 に丸まったのに RGB が残ると、「入力の外側は透明」という
        // 決めと食い違う。合成しても見えないので、気づけるのはここだけである
        let mut img = RgbaImage::from_pixel(40, 40, Rgba([0, 255, 0, 0]));
        for y in 10..30 {
            for x in 10..30 {
                img.put_pixel(x, y, Rgba([200, 40, 30, 255]));
            }
        }
        let p = plan((40, 40), &spec(20.0)).unwrap();
        let out = apply(&img, &p).unwrap();

        for px in out.pixels().filter(|p| p.0[3] == 0) {
            assert_eq!(px.0, [0, 0, 0, 0], "透明な画素に色が残っている");
        }
    }

    #[test]
    fn an_arbitrary_angle_preserves_the_subject_area() {
        // 回しても商品の面積（アルファの総和）はほぼ変わらない。
        // 大きく減れば取りこぼし、増えれば背景を巻き込んでいる
        let mut img = RgbaImage::from_pixel(60, 60, Rgba([0, 0, 0, 0]));
        for y in 15..45 {
            for x in 15..45 {
                img.put_pixel(x, y, Rgba([200, 40, 30, 255]));
            }
        }
        let before: f64 = img.pixels().map(|p| p[3] as f64).sum();

        let p = plan((60, 60), &spec(37.0)).unwrap();
        let out = apply(&img, &p).unwrap();
        let after: f64 = out.pixels().map(|p| p[3] as f64).sum();

        let ratio = after / before;
        assert!(
            (0.98..=1.02).contains(&ratio),
            "面積比が {ratio:.4}（1.0 から離れすぎ）"
        );
    }
}
