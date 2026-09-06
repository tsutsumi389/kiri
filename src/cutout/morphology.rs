//! マスクの形態素処理。
//!
//! フラッドフィルの結果には、背景に散った孤立点（センサーノイズが背景色から
//! 外れたもの）が残る。これを消すのが `remove_specks` の役目である。
//!
//! かつてはオープニング（収縮 → 膨張）で消していたが、オープニングは
//! **構造要素より細いものをすべて消す**。半径 2 のオープニングは幅 5px 未満の
//! 構造を無条件に落とすため、ストラップ・持ち手・ケーブルといった商品の一部まで
//! 巻き添えにしていた。孤立点と細い構造を分けるのは太さではなく面積なので、
//! 連結成分の面積で選ぶ。
//!
//! `erode` / `dilate` / `open` / `close` は境界帯の生成や比較のために残している。
//! ただしパイプラインからは外した。理由は `remove_specks` と `close` の項を参照。

use std::collections::VecDeque;

use crate::cutout::mask::Mask;

/// 孤立ノイズの除去。連結成分のうち、不透明な芯の面積が `(2*radius+1)^2` に
/// 満たないものを消す。`radius` 0 で無効。
///
/// 「半径 r のオープニングが消すのは、r の構造要素が入らないもの」という直感を
/// 面積で置き換えている。同じ `--cleanup` の値で、消える孤立点の大きさは
/// おおむね従来どおりのまま、細い構造だけが生き残る。
///
/// 連結は 8 近傍で、**アルファが 0 より大きい画素**をたどる。面積は
/// **前景判定(128以上)の画素だけ**で数える。半透明の裾ごと消さないと、
/// 芯を消した跡に薄い輪だけが残るためである。二値マスクではどちらも同じになる。
///
/// 連結を 8 近傍にするのは、4 近傍だと斜めに走る 1px の構造が細切れになり、
/// 面積で判定した途端にすべて消えてしまうためである。
pub fn remove_specks(mask: &Mask, radius: u32) -> Mask {
    if radius == 0 {
        return mask.clone();
    }
    let side = 2 * radius + 1;
    let min_area = (side as usize) * (side as usize);
    let (w, h) = (mask.width(), mask.height());
    let stride = w as usize;

    let mut out = mask.clone();
    let mut visited = vec![false; stride * (h as usize)];
    let mut component: Vec<(u32, u32)> = Vec::new();
    let mut queue: VecDeque<(u32, u32)> = VecDeque::new();

    for y in 0..h {
        for x in 0..w {
            let start = (y as usize) * stride + (x as usize);
            if visited[start] || mask.get(x, y) == 0 {
                continue;
            }
            visited[start] = true;
            component.clear();
            queue.clear();
            queue.push_back((x, y));
            let mut core = 0usize;

            while let Some((cx, cy)) = queue.pop_front() {
                component.push((cx, cy));
                if mask.is_foreground(cx, cy) {
                    core += 1;
                }
                for dy in -1i64..=1 {
                    for dx in -1i64..=1 {
                        if dx == 0 && dy == 0 {
                            continue;
                        }
                        let (nx, ny) = (cx as i64 + dx, cy as i64 + dy);
                        if nx < 0 || ny < 0 || nx >= w as i64 || ny >= h as i64 {
                            continue;
                        }
                        let (nx, ny) = (nx as u32, ny as u32);
                        let i = (ny as usize) * stride + (nx as usize);
                        if visited[i] || mask.get(nx, ny) == 0 {
                            continue;
                        }
                        visited[i] = true;
                        queue.push_back((nx, ny));
                    }
                }
            }

            if core < min_area {
                for &(px, py) in &component {
                    out.set(px, py, 0);
                }
            }
        }
    }
    out
}

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
///
/// **切り抜きのパイプラインでは使っていない。** 前景は「外周から到達できなかった
/// 領域」として定義されるため、到達できない穴はそもそも前景になっている。
/// 残るのは細い隙間を通って外周とつながった領域だけで、それを埋めるのは
/// 形の改変にあたる。穴を埋める効果より、商品の隙間（取っ手の内側など）を
/// 塞いでしまう害のほうが大きい。
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
    fn an_area_filter_removes_a_speck() {
        let mask = from_ascii(&[
            ".........",
            "..#####..",
            "..#####..",
            "..#####..",
            ".........",
            ".......#.",
        ]);
        let out = remove_specks(&mask, 1);
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

    /// オープニングとの決定的な違い。細い構造を消さない。
    #[test]
    fn an_area_filter_keeps_a_thin_limb_that_opening_would_erase() {
        let mask = from_ascii(&[
            "...#......",
            "...#......",
            "...#......",
            ".######...",
            ".######...",
            ".######...",
            ".######...",
            "..........",
        ]);
        let opened = open(&mask, 1);
        assert!(
            !opened.is_foreground(3, 0),
            "前提が崩れている: オープニングは幅 1px の腕を消すはず\n{}",
            render(&opened)
        );
        let filtered = remove_specks(&mask, 1);
        assert!(
            filtered.is_foreground(3, 0),
            "面積フィルタが細い腕を消している\n{}",
            render(&filtered)
        );
    }

    #[test]
    fn an_area_filter_measures_area_not_thickness() {
        // 幅 1px でも、面積が下限を超えていれば残る
        let mask = from_ascii(&[
            "..........",
            ".#########",
            "..........",
            "..#.......",
            "..........",
        ]);
        let out = remove_specks(&mask, 1);
        assert!(out.is_foreground(5, 1), "面積 9 の線が消えている");
        assert!(!out.is_foreground(2, 3), "面積 1 の点が残っている");
    }

    #[test]
    fn an_area_filter_follows_diagonal_connections() {
        // 4 近傍で見ると斜めの線が細切れになり、面積判定で全部消えてしまう
        let mask = from_ascii(&[
            "#.........",
            ".#........",
            "..#.......",
            "...#......",
            "....#.....",
            ".....#....",
            "......#...",
            ".......#..",
            "........#.",
        ]);
        let out = remove_specks(&mask, 1);
        assert!(
            out.is_foreground(4, 4),
            "斜めの線が消えている\n{}",
            render(&out)
        );
    }

    #[test]
    fn an_area_filter_with_radius_zero_is_the_identity() {
        let mask = from_ascii(&["..#..", ".###.", "..#.."]);
        assert_eq!(remove_specks(&mask, 0), mask);
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
