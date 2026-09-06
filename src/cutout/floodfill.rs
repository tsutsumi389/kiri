//! 外周からの連結フラッドフィルによる前景マスクの生成。
//!
//! kiri の中核。単に「背景色に近い画素」を透明にするのではなく、
//! **画像の外周から到達できる背景色領域だけ**を透明にする。
//!
//! これが白背景に白い商品を置いた場合（EC で最頻出かつ最難の状況）の答えになる。
//! 商品内部の白は外周から到達できないため、色が背景と同じでも生き残る。

use std::collections::VecDeque;

use image::RgbaImage;

use crate::color::lab::delta_e_rgb;
use crate::cutout::edges::gradient_magnitude;
use crate::cutout::mask::Mask;

/// `--fg-seed` が保護する円の半径(px)。
pub const FG_SEED_RADIUS: u32 = 5;

#[derive(Debug, Clone, Default)]
pub struct FloodOptions {
    /// 背景色との色差(ΔE)がこの値以下なら背景候補とみなす
    pub tolerance: f64,
    /// 指定された場合、この矩形の外側は無条件に背景とする (x1, y1, x2, y2)
    pub bbox: Option<(u32, u32, u32, u32)>,
    /// 「ここは必ず前景」と指定された座標。周囲を保護し、フィルの侵入を防ぐ
    pub fg_seeds: Vec<(u32, u32)>,
    /// 1px あたりの輝度変化がこの値を超える画素にはフィルを侵入させない。
    /// 0 で無効。商品の輪郭は急峻、落ち影はなだらかという差を使って両者を分ける
    pub edge_threshold: f64,
}

/// 前景マスクを生成する。255 = 前景、0 = 背景。
pub fn foreground_mask(image: &RgbaImage, background: [u8; 3], opts: &FloodOptions) -> Mask {
    let (w, h) = (image.width(), image.height());
    if w == 0 || h == 0 {
        return Mask::new(w, h, 0);
    }

    let protected = protected_pixels(w, h, &opts.fg_seeds);
    let candidate = background_candidates(image, background, opts, &protected);
    let mut is_background = fill_from_border(w, h, &candidate, opts.bbox);

    // bbox の外側は、色に関わらず背景として扱う。
    // AI が「商品はここにある」と判断した結果をここで効かせる。
    if let Some((x1, y1, x2, y2)) = opts.bbox {
        for y in 0..h {
            for x in 0..w {
                if x < x1 || x > x2 || y < y1 || y > y2 {
                    is_background[(y as usize) * (w as usize) + (x as usize)] = true;
                }
            }
        }
    }

    // 保護された画素は最後に前景へ戻す（bbox 指定より優先する）
    let foreground: Vec<bool> = is_background
        .iter()
        .zip(protected.iter())
        .map(|(&bg, &prot)| prot || !bg)
        .collect();

    Mask::from_bools(w, h, &foreground)
}

/// 背景色に十分近い画素に印を付ける。既に透明な画素も背景として扱う。
///
/// `edge_threshold` が有効なとき、輪郭上の画素は色が背景に近くても候補から外す。
/// これがなければ、淡い色の商品は落ち影を消せる許容量の下で必ず飲み込まれてしまう。
fn background_candidates(
    image: &RgbaImage,
    background: [u8; 3],
    opts: &FloodOptions,
    protected: &[bool],
) -> Vec<bool> {
    let gradient = (opts.edge_threshold > 0.0).then(|| gradient_magnitude(image));

    image
        .pixels()
        .enumerate()
        .zip(protected.iter())
        .map(|((i, p), &prot)| {
            if prot {
                return false;
            }
            if p[3] == 0 {
                return true;
            }
            // let-chain は Rust 1.88 以降。MSRV 1.85 を保つためネストで書く
            if let Some(g) = &gradient {
                if f64::from(g[i]) > opts.edge_threshold {
                    return false;
                }
            }
            delta_e_rgb([p[0], p[1], p[2]], background) <= opts.tolerance
        })
        .collect()
}

/// 外周を起点に 4 近傍で塗り広げる。
///
/// 4 近傍にしているのは、8 近傍だと斜めの隙間を通って商品内部へ漏れるため。
fn fill_from_border(
    w: u32,
    h: u32,
    candidate: &[bool],
    bbox: Option<(u32, u32, u32, u32)>,
) -> Vec<bool> {
    let mut filled = vec![false; candidate.len()];
    let mut queue = VecDeque::new();
    let idx = |x: u32, y: u32| (y as usize) * (w as usize) + (x as usize);

    let seed = |x: u32, y: u32, filled: &mut Vec<bool>, queue: &mut VecDeque<(u32, u32)>| {
        let i = idx(x, y);
        if candidate[i] && !filled[i] {
            filled[i] = true;
            queue.push_back((x, y));
        }
    };

    // bbox が与えられていれば、その矩形の縁から塗り始める。
    //
    // bbox の外は色によらず背景と確定しているので、フィルの起点として画像の
    // 外周より内側にある矩形の縁のほうが正しい。画像の外周からしか塗れないと、
    // 途中に背景色から外れた領域（照明ムラや別の物体）があるだけでフィルが
    // 遮られ、bbox の内側に一切届かなくなる。それでは bbox が「商品はここに
    // ある」という指示ではなく、単なる切り取り枠に留まってしまう。
    let (sx1, sy1, sx2, sy2) = bbox.unwrap_or((0, 0, w - 1, h - 1));
    for x in sx1..=sx2 {
        seed(x, sy1, &mut filled, &mut queue);
        seed(x, sy2, &mut filled, &mut queue);
    }
    for y in sy1..=sy2 {
        seed(sx1, y, &mut filled, &mut queue);
        seed(sx2, y, &mut filled, &mut queue);
    }

    while let Some((x, y)) = queue.pop_front() {
        let visit = |nx: u32, ny: u32, filled: &mut Vec<bool>, q: &mut VecDeque<(u32, u32)>| {
            let i = idx(nx, ny);
            if candidate[i] && !filled[i] {
                filled[i] = true;
                q.push_back((nx, ny));
            }
        };
        if x > 0 {
            visit(x - 1, y, &mut filled, &mut queue);
        }
        if y > 0 {
            visit(x, y - 1, &mut filled, &mut queue);
        }
        if x + 1 < w {
            visit(x + 1, y, &mut filled, &mut queue);
        }
        if y + 1 < h {
            visit(x, y + 1, &mut filled, &mut queue);
        }
    }

    filled
}

/// `--fg-seed` の周囲を保護領域として塗る。
///
/// 前景の色を推定して領域ごと救い出す方式は、種が背景と同色だった場合に
/// 背景全体を巻き込む危険がある。ここでは半径を固定した円に限定し、
/// 挙動が予測できることを優先している。
fn protected_pixels(w: u32, h: u32, seeds: &[(u32, u32)]) -> Vec<bool> {
    let mut protected = vec![false; (w as usize) * (h as usize)];
    let r = FG_SEED_RADIUS as i64;
    for &(sx, sy) in seeds {
        if sx >= w || sy >= h {
            continue;
        }
        for dy in -r..=r {
            for dx in -r..=r {
                if dx * dx + dy * dy > r * r {
                    continue;
                }
                let (x, y) = (sx as i64 + dx, sy as i64 + dy);
                if x < 0 || y < 0 || x >= w as i64 || y >= h as i64 {
                    continue;
                }
                protected[(y as usize) * (w as usize) + (x as usize)] = true;
            }
        }
    }
    protected
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::Rgba;

    const BG: [u8; 3] = [250, 250, 250];

    /// ASCII で試験画像を組む。
    /// `.` 背景(白) / `o` 商品内部(背景と同色) / `#` 濃い商品 / `-` 淡い輪郭 / ` ` 透明
    ///
    /// `.` と `o` は同じ色である点が肝。連結性だけがこの2つを区別する。
    fn ascii(rows: &[&str]) -> RgbaImage {
        let h = rows.len() as u32;
        let w = rows[0].len() as u32;
        let mut img = RgbaImage::new(w, h);
        for (y, row) in rows.iter().enumerate() {
            assert_eq!(row.len() as u32, w, "行の長さが揃っていない");
            for (x, ch) in row.chars().enumerate() {
                let px = match ch {
                    '.' => [250, 250, 250, 255],
                    'o' => [250, 250, 250, 255],
                    '#' => [40, 40, 40, 255],
                    '-' => [215, 215, 215, 255],
                    ' ' => [0, 0, 0, 0],
                    other => panic!("未知の文字 '{other}'"),
                };
                img.put_pixel(x as u32, y as u32, Rgba(px));
            }
        }
        img
    }

    fn opts(tolerance: f64) -> FloodOptions {
        FloodOptions {
            tolerance,
            ..Default::default()
        }
    }

    /// マスクを ASCII に戻す。失敗時に目で見て分かるようにするため。
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
    fn a_dark_product_on_white_is_isolated() {
        let img = ascii(&[
            "........", "........", "..####..", "..####..", "........", "........",
        ]);
        let mask = foreground_mask(&img, BG, &opts(5.0));
        assert!(
            mask.is_foreground(3, 2),
            "商品が背景にされている\n{}",
            render(&mask)
        );
        assert!(
            !mask.is_foreground(0, 0),
            "背景が残っている\n{}",
            render(&mask)
        );
        assert_eq!(mask.stats().bbox, Some((2, 2, 5, 3)));
    }

    /// kiri の中核。商品内部が背景と同一の色でも、外周から到達できなければ残る。
    #[test]
    fn an_enclosed_background_colored_region_survives() {
        let img = ascii(&[
            "........", "..####..", "..#oo#..", "..#oo#..", "..####..", "........",
        ]);
        let mask = foreground_mask(&img, BG, &opts(5.0));

        assert!(
            mask.is_foreground(3, 2) && mask.is_foreground(4, 3),
            "商品内部の白に穴が空いている\n{}",
            render(&mask)
        );
        assert!(
            !mask.is_foreground(0, 0),
            "外側の白が残っている\n{}",
            render(&mask)
        );
    }

    /// 上の対照実験。同じ色の領域でも外周とつながっていれば消える。
    #[test]
    fn a_background_colored_region_connected_to_the_border_is_removed() {
        let img = ascii(&[
            "........", "..####..", "..#oo#..", "..#oo#..", "..#..#..", "........",
        ]);
        let mask = foreground_mask(&img, BG, &opts(5.0));
        assert!(
            !mask.is_foreground(3, 2),
            "外周につながった白が残っている\n{}",
            render(&mask)
        );
    }

    /// 白背景に白い商品。淡い輪郭さえあれば内部は保たれる。
    #[test]
    fn a_white_product_on_a_white_background_keeps_its_interior() {
        let img = ascii(&[
            "..........",
            "..------..",
            "..-oooo-..",
            "..-oooo-..",
            "..------..",
            "..........",
        ]);
        // 淡い輪郭(215)と背景(250)の色差は ΔE で 10 程度あるため、
        // tolerance 5 ならフィルはここで止まる
        let mask = foreground_mask(&img, BG, &opts(5.0));

        for (x, y) in [(3u32, 2u32), (4, 2), (5, 3), (6, 3)] {
            assert!(
                mask.is_foreground(x, y),
                "白い商品の内部({x},{y})に穴が空いている\n{}",
                render(&mask)
            );
        }
        assert!(!mask.is_foreground(0, 0));
        assert_eq!(mask.stats().bbox, Some((2, 1, 7, 4)));
    }

    #[test]
    fn a_product_touching_the_border_is_kept_and_flagged() {
        let img = ascii(&["..####..", "..####..", "..####.."]);
        let mask = foreground_mask(&img, BG, &opts(5.0));
        let stats = mask.stats();
        assert!(mask.is_foreground(3, 0));
        assert!(stats.touches_edge, "見切れが検出されていない");
    }

    #[test]
    fn tolerance_controls_how_much_is_removed() {
        // 淡い輪郭(215)は tolerance を上げると背景として飲まれる
        let img = ascii(&["........", "..----..", "..-##-..", "..----..", "........"]);
        let tight = foreground_mask(&img, BG, &opts(5.0));
        assert!(
            tight.is_foreground(2, 1),
            "tolerance 5 で輪郭まで消えている"
        );

        let loose = foreground_mask(&img, BG, &opts(20.0));
        assert!(
            !loose.is_foreground(2, 1),
            "tolerance 20 でも輪郭が残っている"
        );
        assert!(loose.is_foreground(3, 2), "濃い商品まで消えている");
    }

    #[test]
    fn already_transparent_pixels_count_as_background() {
        let img = ascii(&["        ", "  ####  ", "  ####  ", "        "]);
        let mask = foreground_mask(&img, BG, &opts(5.0));
        assert!(mask.is_foreground(3, 1));
        assert!(!mask.is_foreground(0, 0));
    }

    #[test]
    fn bbox_forces_everything_outside_to_background() {
        let img = ascii(&["........", "..####..", "..####..", "..####..", "........"]);
        let mut o = opts(5.0);
        // 商品の左半分だけを囲う
        o.bbox = Some((2, 1, 3, 3));
        let mask = foreground_mask(&img, BG, &o);

        assert!(
            mask.is_foreground(2, 1),
            "bbox 内が消えている\n{}",
            render(&mask)
        );
        assert!(
            !mask.is_foreground(5, 2),
            "bbox 外が残っている\n{}",
            render(&mask)
        );
        assert_eq!(mask.stats().bbox, Some((2, 1, 3, 3)));
    }

    #[test]
    fn the_bbox_edge_seeds_the_fill_when_the_image_border_is_blocked() {
        // 画像の外周と商品の間に背景色から外れた領域（照明ムラや別の物体）が
        // あると、外周からのフィルはそこで止まり bbox の内側に届かない。
        // bbox の縁を起点に加えることで、その内側の背景を消せるようにする。
        let bg = [250, 250, 250];
        let mut img = RgbaImage::from_pixel(40, 40, Rgba([bg[0], bg[1], bg[2], 255]));

        // 外周からのフィルを遮る枠（背景色から大きく外れた色）
        for i in 8..32 {
            for (x, y) in [(i, 8), (i, 31), (8, i), (31, i)] {
                img.put_pixel(x, y, Rgba([20, 90, 160, 255]));
            }
        }
        // 枠の内側は背景色。その中央に商品を置く
        for y in 16..24 {
            for x in 16..24 {
                img.put_pixel(x, y, Rgba([200, 40, 30, 255]));
            }
        }

        let opts = FloodOptions {
            tolerance: 12.0,
            bbox: Some((9, 9, 30, 30)),
            edge_threshold: 0.0,
            ..Default::default()
        };
        let mask = foreground_mask(&img, bg, &opts);

        assert!(mask.is_foreground(20, 20), "商品は残る");
        assert!(
            !mask.is_foreground(12, 12),
            "bbox の内側の背景色は消える（外周からは到達できない位置）"
        );
        assert!(!mask.is_foreground(2, 2), "bbox の外は背景");
    }

    #[test]
    fn fg_seed_protects_its_neighbourhood() {
        // 全面が背景色。何も指定しなければ全部消える
        let img = ascii(&[
            "..............................",
            "..............................",
            "..............................",
            "..............................",
            "..............................",
            "..............................",
            "..............................",
            "..............................",
            "..............................",
            "..............................",
            "..............................",
            "..............................",
        ]);
        let bare = foreground_mask(&img, BG, &opts(5.0));
        assert_eq!(bare.stats().foreground_ratio, 0.0);

        let mut o = opts(5.0);
        o.fg_seeds = vec![(15, 6)];
        let seeded = foreground_mask(&img, BG, &o);

        assert!(seeded.is_foreground(15, 6), "種そのものが保護されていない");
        assert!(
            seeded.is_foreground(15 + FG_SEED_RADIUS, 6),
            "保護円の縁が守られていない"
        );
        assert!(
            !seeded.is_foreground(15 + FG_SEED_RADIUS + 2, 6),
            "保護円が広すぎる"
        );
    }

    #[test]
    fn fg_seed_wins_over_bbox() {
        let img = ascii(&[
            "..............................",
            "..............................",
            "..............................",
            "..............................",
            "..............................",
            "..............................",
            "..............................",
            "..............................",
            "..............................",
            "..............................",
            "..............................",
            "..............................",
        ]);
        let mut o = opts(5.0);
        o.bbox = Some((0, 0, 2, 2));
        o.fg_seeds = vec![(20, 6)];
        let mask = foreground_mask(&img, BG, &o);
        assert!(mask.is_foreground(20, 6), "bbox 外でも種は前景であるべき");
    }

    #[test]
    fn out_of_range_seeds_are_ignored() {
        let img = ascii(&["....", "....", "...."]);
        let mut o = opts(5.0);
        o.fg_seeds = vec![(100, 100)];
        let mask = foreground_mask(&img, BG, &o);
        assert_eq!(mask.stats().foreground_ratio, 0.0);
    }
}
