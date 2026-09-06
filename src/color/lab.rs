//! sRGB と CIE Lab の相互変換、および知覚的色距離。
//!
//! 背景判定のしきい値を RGB のユークリッド距離で切ると、人間の見た目と一致しない。
//! たとえば暗部の小さな差は目立たないのに RGB 距離では大きく出る。Lab 空間の ΔE を
//! 使うことで `--tolerance` の値が直感と一致するようにする。

/// D65 白色点
const XN: f64 = 0.950_47;
const YN: f64 = 1.0;
const ZN: f64 = 1.088_83;

/// sRGB 8bit → 線形値の変換表。
///
/// `powf` は 1 回でも数十 ns かかる。境界帯の推定は境界画素ごとに何度も Lab へ
/// 変換するため、12MP のメッシュ状の素材では変換だけで数百 ms を占めていた。
/// 入力が 8bit しか取り得ない以上、表を引けば結果は完全に同じで済む。
static LINEAR: std::sync::LazyLock<[f64; 256]> = std::sync::LazyLock::new(|| {
    std::array::from_fn(|i| {
        let c = i as f64 / 255.0;
        if c <= 0.040_45 {
            c / 12.92
        } else {
            ((c + 0.055) / 1.055).powf(2.4)
        }
    })
});

/// 上の表を f32 に落としたもの。
///
/// フラッドフィルは画素ごとに Lab を持つので、f64 では表そのものより
/// 保持する側の代償が大きい（12MP で 288MB と 144MB の差）。値は `LINEAR`
/// から作るため、f64 経路との食い違いは f32 の丸め誤差に限られる。
static LINEAR_F32: std::sync::LazyLock<[f32; 256]> =
    std::sync::LazyLock::new(|| std::array::from_fn(|i| LINEAR[i] as f32));

fn srgb_to_linear(c: u8) -> f64 {
    LINEAR[c as usize]
}

fn pivot(t: f64) -> f64 {
    if t > 0.008_856 {
        t.cbrt()
    } else {
        7.787 * t + 16.0 / 116.0
    }
}

/// sRGB 8bit → 線形 RGB (f32) の変換表。`linear_to_lab` と組で使う。
///
/// 参照を返すのは、呼ぶたびに 256 回の変換を走らせないため。表は共有しても
/// 中身が変わらないので、複製する理由が無い。
pub fn srgb_linear_lut() -> &'static [f32; 256] {
    &LINEAR_F32
}

/// 線形 RGB (0.0-1.0) を CIE Lab に変換する。`srgb_linear_lut` と組で使う。
pub fn linear_to_lab(rgb: [f32; 3]) -> [f32; 3] {
    let pivot32 = |t: f32| -> f32 {
        if t > 0.008_856 {
            t.cbrt()
        } else {
            7.787 * t + 16.0 / 116.0
        }
    };
    let (r, g, b) = (rgb[0], rgb[1], rgb[2]);

    let x = 0.412_456_4 * r + 0.357_576_1 * g + 0.180_437_5 * b;
    let y = 0.212_672_9 * r + 0.715_152_2 * g + 0.072_175_0 * b;
    // 0.119_192_0 と書くと f32 では表現できない桁だと clippy に叱られる
    let z = 0.019_333_9 * r + 0.119_192 * g + 0.950_304_1 * b;

    let fx = pivot32(x / XN as f32);
    let fy = pivot32(y / YN as f32);
    let fz = pivot32(z / ZN as f32);

    [116.0 * fy - 16.0, 500.0 * (fx - fy), 200.0 * (fy - fz)]
}

/// CIE76 の色差（f32 版）。
pub fn delta_e76_f32(a: [f32; 3], b: [f32; 3]) -> f32 {
    let dl = a[0] - b[0];
    let da = a[1] - b[1];
    let db = a[2] - b[2];
    (dl * dl + da * da + db * db).sqrt()
}

/// sRGB (0-255) を CIE Lab に変換する。
pub fn srgb_to_lab(rgb: [u8; 3]) -> [f64; 3] {
    let r = srgb_to_linear(rgb[0]);
    let g = srgb_to_linear(rgb[1]);
    let b = srgb_to_linear(rgb[2]);

    let x = 0.412_456_4 * r + 0.357_576_1 * g + 0.180_437_5 * b;
    let y = 0.212_672_9 * r + 0.715_152_2 * g + 0.072_175_0 * b;
    let z = 0.019_333_9 * r + 0.119_192_0 * g + 0.950_304_1 * b;

    let fx = pivot(x / XN);
    let fy = pivot(y / YN);
    let fz = pivot(z / ZN);

    [116.0 * fy - 16.0, 500.0 * (fx - fy), 200.0 * (fy - fz)]
}

/// CIE76 の色差。おおむね ΔE<=2.3 が「見分けがつかない」水準。
pub fn delta_e76(a: [f64; 3], b: [f64; 3]) -> f64 {
    let dl = a[0] - b[0];
    let da = a[1] - b[1];
    let db = a[2] - b[2];
    (dl * dl + da * da + db * db).sqrt()
}

/// sRGB 同士の色差。
pub fn delta_e_rgb(a: [u8; 3], b: [u8; 3]) -> f64 {
    delta_e76(srgb_to_lab(a), srgb_to_lab(b))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn approx(a: f64, b: f64, eps: f64) -> bool {
        (a - b).abs() < eps
    }

    #[test]
    fn white_maps_to_l100_neutral_ab() {
        let lab = srgb_to_lab([255, 255, 255]);
        assert!(approx(lab[0], 100.0, 0.01), "L={}", lab[0]);
        assert!(approx(lab[1], 0.0, 0.01), "a={}", lab[1]);
        assert!(approx(lab[2], 0.0, 0.01), "b={}", lab[2]);
    }

    #[test]
    fn black_maps_to_l0() {
        let lab = srgb_to_lab([0, 0, 0]);
        assert!(approx(lab[0], 0.0, 0.01), "L={}", lab[0]);
    }

    #[test]
    fn mid_gray_is_neutral() {
        let lab = srgb_to_lab([128, 128, 128]);
        assert!(approx(lab[1], 0.0, 0.01) && approx(lab[2], 0.0, 0.01));
        // 知覚的な中間グレーは L=50 付近（線形の 50% ではない）
        assert!(lab[0] > 50.0 && lab[0] < 56.0, "L={}", lab[0]);
    }

    #[test]
    fn identical_colors_have_zero_distance() {
        assert_eq!(delta_e_rgb([200, 100, 50], [200, 100, 50]), 0.0);
    }

    #[test]
    fn near_white_shades_are_perceptually_close() {
        // スタジオ背景でよくある「ほぼ白」同士は小さな ΔE に収まる
        assert!(delta_e_rgb([255, 255, 255], [248, 248, 247]) < 5.0);
    }

    #[test]
    fn white_and_black_are_far_apart() {
        assert!(delta_e_rgb([255, 255, 255], [0, 0, 0]) > 99.0);
    }

    /// f32 経路は f64 経路と実質同じ値を返さなければならない。
    /// 段差の判定は ΔE 1 前後の差で結論が変わるため、ここがずれると
    /// フラッドフィルの停止位置が変わってしまう。
    #[test]
    fn the_f32_path_agrees_with_the_f64_path() {
        let lut = srgb_linear_lut();
        for rgb in [
            [0u8, 0, 0],
            [255, 255, 255],
            [248, 248, 247],
            [232, 232, 230],
            [190, 70, 55],
            [40, 40, 45],
            [7, 3, 1],
        ] {
            let a = srgb_to_lab(rgb);
            let b = linear_to_lab([
                lut[rgb[0] as usize],
                lut[rgb[1] as usize],
                lut[rgb[2] as usize],
            ]);
            for k in 0..3 {
                assert!(
                    (a[k] - f64::from(b[k])).abs() < 0.01,
                    "{rgb:?} の成分 {k} がずれている: {} vs {}",
                    a[k],
                    b[k]
                );
            }
        }
    }
}
