//! マスク境界のフェザリング。
//!
//! フラッドフィルの出力は 0 か 255 の二値なので、そのまま透過させると輪郭が
//! ギザギザになる。境界に幅を持たせて中間のアルファ値を作る。

use rayon::prelude::*;

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

/// `feather(mask, radius)` の、矩形 `rect`（両端を含む）の中だけの値を返す。
///
/// 並びは矩形の行順。**値は `feather` の全面の結果を切り出したものと 1 ビットも
/// 違わない**——横のぼかしを矩形の上下に半径ぶん広げた行で取り、縦はそこから
/// 取るので、足す画素の集合も丸め方も全面版と同じである。
///
/// `refine` の受け皿がタイルごとに要る分だけ作るための窓口である。全面を
/// 作ると 24.5MP で 24.5MB を抱え、しかも並列のタイル走査の途中で 1 度だけ
/// 作ることになって、作っている間は他のタイルが待たされる。
pub fn feather_window(mask: &Mask, radius: u32, rect: (u32, u32, u32, u32)) -> Vec<u8> {
    let (x0, y0, x1, y1) = rect;
    let (w, h) = (mask.width(), mask.height());
    let rw = (x1 - x0 + 1) as usize;
    if radius == 0 {
        return (y0..=y1)
            .flat_map(|y| (x0..=x1).map(move |x| (x, y)))
            .map(|(x, y)| mask.get(x, y))
            .collect();
    }
    let span = |pos: u32, limit: u32| -> (u32, u32) {
        (
            pos.saturating_sub(radius),
            pos.saturating_add(radius).min(limit - 1),
        )
    };
    let (hy0, hy1) = (span(y0, h).0, span(y1, h).1);
    let mut across = vec![0u8; rw * ((hy1 - hy0 + 1) as usize)];
    for k in hy0..=hy1 {
        for x in x0..=x1 {
            let (from, to) = span(x, w);
            let sum: u32 = (from..=to).map(|j| u32::from(mask.get(j, k))).sum();
            let count = to - from + 1;
            across[((k - hy0) as usize) * rw + (x - x0) as usize] =
                ((sum + count / 2) / count) as u8;
        }
    }
    let mut out = Vec::with_capacity(rw * ((y1 - y0 + 1) as usize));
    for y in y0..=y1 {
        let (from, to) = span(y, h);
        let count = to - from + 1;
        for x in 0..rw {
            let sum: u32 = (from..=to)
                .map(|k| u32::from(across[((k - hy0) as usize) * rw + x]))
                .sum();
            out.push(((sum + count / 2) / count) as u8);
        }
    }
    out
}

/// 片方向の箱ぼかし。窓は端ではみ出した分を数えない。
///
/// **出力の行ごとに並列に回す。** どの行も入力を読むだけで、和は整数なので、
/// どの順に足しても値は同じである。縦方向は列を 1 画素ずつ縦に拾わず、
/// 窓に入る行を語ごとにまとめて足す（足す集合は同じ）。
fn blur(mask: &Mask, radius: u32, horizontal: bool) -> Mask {
    let (w, h) = (mask.width(), mask.height());
    let mut out = Mask::new(w, h, 0);
    let stride = w as usize;
    if stride == 0 || h == 0 {
        return out;
    }
    let r = radius as i64;
    let src = mask.as_slice();
    let window = |pos: u32, limit: u32| -> (u32, u32) {
        let from = (pos as i64 - r).max(0) as u32;
        let to = ((pos as i64 + r) as u32).min(limit - 1);
        (from, to)
    };

    out.as_mut_slice()
        .par_chunks_mut(stride)
        .enumerate()
        .for_each_init(
            || vec![0u32; stride],
            |sums, (y, row)| {
                let y = y as u32;
                if horizontal {
                    let line = &src[(y as usize) * stride..(y as usize + 1) * stride];
                    for x in 0..w {
                        let (from, to) = window(x, w);
                        let sum: u32 = line[from as usize..=to as usize]
                            .iter()
                            .map(|&v| u32::from(v))
                            .sum();
                        let count = to - from + 1;
                        row[x as usize] = ((sum + count / 2) / count) as u8;
                    }
                } else {
                    let (from, to) = window(y, h);
                    sums.fill(0);
                    for k in from..=to {
                        let line = &src[(k as usize) * stride..(k as usize + 1) * stride];
                        for (acc, &v) in sums.iter_mut().zip(line) {
                            *acc += u32::from(v);
                        }
                    }
                    let count = to - from + 1;
                    for (slot, &sum) in row.iter_mut().zip(sums.iter()) {
                        *slot = ((sum + count / 2) / count) as u8;
                    }
                }
            },
        );
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

    /// 矩形の中だけで作った値が、全面の結果の切り出しと一致すること。
    /// 端に接する矩形と、半径が矩形や画像より大きい場合を含める。
    #[test]
    fn the_window_agrees_with_the_whole_feather() {
        let (w, h) = (37u32, 29u32);
        let mut mask = Mask::new(w, h, 0);
        let mut state = 0x9e37_79b9_u32;
        for y in 0..h {
            for x in 0..w {
                state ^= state << 13;
                state ^= state >> 17;
                state ^= state << 5;
                mask.set(
                    x,
                    y,
                    if state % 3 == 0 {
                        255
                    } else {
                        (state % 256) as u8
                    },
                );
            }
        }
        for radius in [0, 1, 2, 5, 40] {
            let whole = feather(&mask, radius);
            for rect in [
                (0, 0, w - 1, h - 1),
                (3, 4, 10, 20),
                (30, 0, 36, 5),
                (0, 25, 4, 28),
            ] {
                let got = feather_window(&mask, radius, rect);
                let (x0, y0, x1, y1) = rect;
                let rw = (x1 - x0 + 1) as usize;
                for y in y0..=y1 {
                    for x in x0..=x1 {
                        let at = ((y - y0) as usize) * rw + (x - x0) as usize;
                        assert_eq!(got[at], whole.get(x, y), "r {radius} ({x}, {y})");
                    }
                }
            }
        }
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
