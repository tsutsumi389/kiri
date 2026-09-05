//! マスクの形態素処理。
//!
//! フラッドフィルの結果には、背景に散った孤立点（センサーノイズが背景色から
//! 外れたもの）と、商品内部の小さな穴（ハイライトが背景色に一致したもの）が残る。
//! オープニングで前者を、クロージングで後者を消す。

use crate::cutout::mask::Mask;

/// 収縮。前景を `radius` px 削る。
pub fn erode(mask: &Mask, radius: u32) -> Mask {
    filter(mask, radius, false)
}

/// 膨張。前景を `radius` px 広げる。
pub fn dilate(mask: &Mask, radius: u32) -> Mask {
    filter(mask, radius, true)
}

/// オープニング（収縮 → 膨張）。孤立した小さな前景を消す。
pub fn open(mask: &Mask, radius: u32) -> Mask {
    if radius == 0 {
        return mask.clone();
    }
    dilate(&erode(mask, radius), radius)
}

/// クロージング（膨張 → 収縮）。前景の小さな穴を埋める。
pub fn close(mask: &Mask, radius: u32) -> Mask {
    if radius == 0 {
        return mask.clone();
    }
    erode(&dilate(mask, radius), radius)
}

/// 正方形の構造要素による min/max フィルタ。
/// 横方向と縦方向に分けて掛けることで O(n * radius) に収めている。
fn filter(mask: &Mask, radius: u32, take_max: bool) -> Mask {
    if radius == 0 {
        return mask.clone();
    }
    let horizontal = pass(mask, radius, take_max, true);
    pass(&horizontal, radius, take_max, false)
}

fn pass(mask: &Mask, radius: u32, take_max: bool, horizontal: bool) -> Mask {
    let (w, h) = (mask.width(), mask.height());
    let mut out = Mask::new(w, h, 0);
    let r = radius as i64;

    for y in 0..h {
        for x in 0..w {
            let (limit, pos) = if horizontal { (w, x) } else { (h, y) };
            let from = (pos as i64 - r).max(0) as u32;
            let to = ((pos as i64 + r) as u32).min(limit - 1);

            let mut acc = if take_max { 0u8 } else { 255u8 };
            for k in from..=to {
                let v = if horizontal {
                    mask.get(k, y)
                } else {
                    mask.get(x, k)
                };
                acc = if take_max { acc.max(v) } else { acc.min(v) };
            }
            out.set(x, y, acc);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn from_ascii(rows: &[&str]) -> Mask {
        let h = rows.len() as u32;
        let w = rows[0].len() as u32;
        let mut mask = Mask::new(w, h, 0);
        for (y, row) in rows.iter().enumerate() {
            for (x, ch) in row.chars().enumerate() {
                mask.set(x as u32, y as u32, if ch == '#' { 255 } else { 0 });
            }
        }
        mask
    }

    fn render(mask: &Mask) -> String {
        (0..mask.height())
            .map(|y| {
                (0..mask.width())
                    .map(|x| if mask.is_foreground(x, y) { '#' } else { '.' })
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn opening_removes_an_isolated_speck() {
        let mask = from_ascii(&[
            ".........",
            "..#####..",
            "..#####..",
            "..#####..",
            ".........",
            ".......#.",
        ]);
        let out = open(&mask, 1);
        assert!(
            !out.is_foreground(7, 5),
            "孤立点が残っている\n{}",
            render(&out)
        );
        assert!(
            out.is_foreground(4, 2),
            "本体まで消えている\n{}",
            render(&out)
        );
    }

    #[test]
    fn closing_fills_a_pinhole() {
        let mask = from_ascii(&[
            ".........",
            "..#####..",
            "..##.##..",
            "..#####..",
            ".........",
        ]);
        let out = close(&mask, 1);
        assert!(
            out.is_foreground(4, 2),
            "穴が埋まっていない\n{}",
            render(&out)
        );
    }

    #[test]
    fn closing_keeps_a_large_hole() {
        let mask = from_ascii(&[
            "...........",
            "..#######..",
            "..#.....#..",
            "..#.....#..",
            "..#.....#..",
            "..#######..",
            "...........",
        ]);
        let out = close(&mask, 1);
        assert!(
            !out.is_foreground(5, 3),
            "大きな穴まで埋めている\n{}",
            render(&out)
        );
    }

    #[test]
    fn opening_keeps_a_large_blob() {
        let mask = from_ascii(&[
            "........", "..####..", "..####..", "..####..", "..####..", "........",
        ]);
        let out = open(&mask, 1);
        assert_eq!(out.stats().bbox, Some((2, 1, 5, 4)));
    }

    #[test]
    fn radius_zero_is_the_identity() {
        let mask = from_ascii(&["..#..", ".###.", "..#.."]);
        assert_eq!(open(&mask, 0), mask);
        assert_eq!(close(&mask, 0), mask);
        assert_eq!(erode(&mask, 0), mask);
        assert_eq!(dilate(&mask, 0), mask);
    }

    #[test]
    fn erosion_shrinks_and_dilation_grows() {
        let mask = from_ascii(&[".......", "..###..", "..###..", "..###..", "......."]);
        assert_eq!(erode(&mask, 1).stats().bbox, Some((3, 2, 3, 2)));
        assert_eq!(dilate(&mask, 1).stats().bbox, Some((1, 0, 5, 4)));
    }

    #[test]
    fn a_product_at_the_border_is_not_eroded_by_the_frame() {
        // 端の外側を背景として扱うと、見切れた商品が縁から削られてしまう
        let mask = from_ascii(&["#####", "#####", "#####"]);
        let out = erode(&mask, 1);
        assert!(
            out.is_foreground(0, 0),
            "画像の縁で誤って収縮している\n{}",
            render(&out)
        );
    }
}
