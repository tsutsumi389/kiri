//! マスク境界のフェザリング。
//!
//! フラッドフィルの出力は 0 か 255 の二値なので、そのまま透過させると輪郭が
//! ギザギザになる。境界に幅を持たせて中間のアルファ値を作る。

use crate::cutout::mask::Mask;

/// マスクを平滑化して境界に階調を持たせる。
///
/// 内部は 255、外部は 0 のまま変わらず、境界付近だけが中間値になる。
pub fn feather(mask: &Mask, radius: u32) -> Mask {
    if radius == 0 {
        return mask.clone();
    }
    let horizontal = blur(mask, radius, true);
    blur(&horizontal, radius, false)
}

fn blur(mask: &Mask, radius: u32, horizontal: bool) -> Mask {
    let (w, h) = (mask.width(), mask.height());
    let mut out = Mask::new(w, h, 0);
    let r = radius as i64;

    for y in 0..h {
        for x in 0..w {
            let (limit, pos) = if horizontal { (w, x) } else { (h, y) };
            let from = (pos as i64 - r).max(0) as u32;
            let to = ((pos as i64 + r) as u32).min(limit - 1);

            let mut sum = 0u32;
            for k in from..=to {
                sum += u32::from(if horizontal {
                    mask.get(k, y)
                } else {
                    mask.get(x, k)
                });
            }
            let count = to - from + 1;
            // 四捨五入する。切り捨てると内部の 255 がわずかに下がってしまう
            out.set(x, y, ((sum + count / 2) / count) as u8);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn half_and_half(w: u32, h: u32) -> Mask {
        let mut mask = Mask::new(w, h, 0);
        for y in 0..h {
            for x in 0..w / 2 {
                mask.set(x, y, 255);
            }
        }
        mask
    }

    #[test]
    fn radius_zero_is_the_identity() {
        let mask = half_and_half(10, 4);
        assert_eq!(feather(&mask, 0), mask);
    }

    #[test]
    fn the_interior_stays_fully_opaque() {
        let mask = half_and_half(20, 6);
        let out = feather(&mask, 1);
        assert_eq!(out.get(2, 3), 255, "内部が薄くなっている");
    }

    #[test]
    fn the_exterior_stays_fully_transparent() {
        let mask = half_and_half(20, 6);
        let out = feather(&mask, 1);
        assert_eq!(out.get(17, 3), 0, "外部に色が漏れている");
    }

    #[test]
    fn the_boundary_becomes_a_ramp() {
        let mask = half_and_half(20, 6);
        let out = feather(&mask, 2);
        // 境界(x=9|10)をまたいで単調に減っていること
        let values: Vec<u8> = (6..14).map(|x| out.get(x, 3)).collect();
        assert!(
            values.windows(2).all(|w| w[0] >= w[1]),
            "境界が単調でない: {values:?}"
        );
        assert!(
            values.iter().any(|&v| v > 0 && v < 255),
            "中間値が生まれていない: {values:?}"
        );
    }

    #[test]
    fn a_larger_radius_produces_a_wider_transition() {
        let mask = half_and_half(40, 6);
        let narrow = feather(&mask, 1);
        let wide = feather(&mask, 4);
        let count = |m: &Mask| {
            (0..40)
                .filter(|&x| {
                    let v = m.get(x, 3);
                    v > 0 && v < 255
                })
                .count()
        };
        assert!(
            count(&wide) > count(&narrow),
            "半径を広げても遷移幅が変わらない"
        );
    }

    #[test]
    fn an_all_foreground_mask_is_unchanged() {
        let mask = Mask::new(8, 8, 255);
        assert_eq!(feather(&mask, 2), mask, "全面前景が縁で削られている");
    }
}
