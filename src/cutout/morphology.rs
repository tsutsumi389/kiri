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

/// 孤立ノイズとして消す連結成分の面積の下限(px²)。
///
/// `radius` は「**長辺 1000px 換算での**半径 px」と読む。面積を
/// `(2r+1)^2 * (長辺/1000)^2` と画像の大きさに比例させるのは、ゴミの大きさが
/// 解像度に比例するためである。同じ被写体を 4 倍で撮れば、同じ大きさに見える
/// ゴミは 16 倍の画素を占める。実写（4284x5712 の不織布）では織り目の 1 粒が
/// 150px² あり、固定の 25px² では一つも消えなかった。
///
/// 長辺 1000px 以下では倍率を 1 に留める。ここを下回るのはサムネイルや
/// テストの合成画像であり、既に妥当な値で運用されているものを動かす理由が無い。
///
/// 半径 0 は「無効」の意味なので 0 を返す。比例させると面積 1 以上、つまり
/// 「1 画素の成分は消す」という別の意味になってしまう。
///
/// `2 * radius` を整数で組まないのは、公開 API に `u32::MAX` を渡されても
/// debug ビルドで溢れさせないためである。CLI と batch は `MAX_CLEANUP` で
/// 上限を掛けるが、ライブラリとして呼ばれる経路にはその関門が無い。
pub fn speck_min_area(radius: u32, width: u32, height: u32) -> usize {
    if radius == 0 {
        return 0;
    }
    let side = 2.0 * f64::from(radius) + 1.0;
    (side * speck_scale(width, height)).powi(2).round() as usize
}

/// 実際の画素数での半径。`speck_min_area` と同じ大きさの正方形の半径にあたる。
///
/// `--cleanup` は長辺 1000px 換算の値なので、そのままでは「実寸で何 px か」を
/// 語らない。境界の探索深さのように**実寸の距離**が要る場所では、面積の下限から
/// 逆算したこちらを使う。
pub fn speck_radius(radius: u32, width: u32, height: u32) -> u32 {
    if radius == 0 {
        return 0;
    }
    let side = (2.0 * f64::from(radius) + 1.0) * speck_scale(width, height);
    ((side - 1.0) / 2.0).round() as u32
}

/// 面積と半径に掛ける解像度の倍率。長辺 1000px 以下では 1 に留める。
fn speck_scale(width: u32, height: u32) -> f64 {
    (f64::from(width.max(height)) / 1000.0).max(1.0)
}

/// 孤立ノイズの除去。連結成分のうち、不透明な芯の面積が
/// `speck_min_area(radius, 幅, 高さ)` に満たないものを消す。`radius` 0 で無効。
///
/// 「半径 r のオープニングが消すのは、r の構造要素が入らないもの」という直感を
/// 面積で置き換えている。同じ `--cleanup` の値で、消える孤立点の見た目の
/// 大きさは解像度によらず一定になり、細い構造だけが生き残る。
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
    let (w, h) = (mask.width(), mask.height());
    let min_area = speck_min_area(radius, w, h);
    let stride = w as usize;

    let mut out = mask.clone();
    let mut visited = vec![false; stride * (h as usize)];
    let mut queue: VecDeque<(u32, u32)> = VecDeque::new();

    for y in 0..h {
        for x in 0..w {
            let start = (y as usize) * stride + (x as usize);
            if visited[start] || mask.get(x, y) == 0 {
                continue;
            }

            // 1 周目は面積を数えるだけにする。成分の座標を Vec に実体化すると、
            // 前景がほぼ全面の 12MP で 1 成分が 1200 万画素になり、それだけで
            // 96MB を余計に抱える。パイプラインは 2 回呼ぶので、ピーク RSS が
            // 207MB から 400MB へ跳ねていた
            visited[start] = true;
            queue.clear();
            queue.push_back((x, y));
            let mut core = 0usize;
            while let Some((cx, cy)) = queue.pop_front() {
                if mask.is_foreground(cx, cy) {
                    core += 1;
                }
                let (x0, x1) = (cx.saturating_sub(1), (cx + 1).min(w - 1));
                let (y0, y1) = (cy.saturating_sub(1), (cy + 1).min(h - 1));
                for ny in y0..=y1 {
                    for nx in x0..=x1 {
                        let i = (ny as usize) * stride + (nx as usize);
                        if visited[i] || mask.get(nx, ny) == 0 {
                            continue;
                        }
                        visited[i] = true;
                        queue.push_back((nx, ny));
                    }
                }
            }
            if core >= min_area {
                continue;
            }

            // 2 周目で 0 を書きながら同じ成分をたどる。書き込んだ 0 がそのまま
            // 訪問済みの印になるので、座標を覚えておく必要がない
            queue.clear();
            out.set(x, y, 0);
            queue.push_back((x, y));
            while let Some((cx, cy)) = queue.pop_front() {
                let (x0, x1) = (cx.saturating_sub(1), (cx + 1).min(w - 1));
                let (y0, y1) = (cy.saturating_sub(1), (cy + 1).min(h - 1));
                for ny in y0..=y1 {
                    for nx in x0..=x1 {
                        if out.get(nx, ny) == 0 {
                            continue;
                        }
                        out.set(nx, ny, 0);
                        queue.push_back((nx, ny));
                    }
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

/// 画素ごとの真偽値をビットで持つ面。
///
/// **`Vec<bool>` は 1 画素 1 バイトである。** 測地的オープニングは元の面・芯・
/// 到達済み・膨張の 4 枚を同時に生かすので、24.5MP の実写では 98MB になる。
/// 形態素処理も連結性の探索も「立っているか」しか問わないのだから、ビットで
/// 持てば同じ答えを 1/8 の常駐量で出せる。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BitPlane {
    words: Vec<u64>,
    len: usize,
}

impl BitPlane {
    pub fn new(len: usize) -> Self {
        Self {
            words: vec![0u64; len.div_ceil(64)],
            len,
        }
    }

    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    #[inline]
    pub fn get(&self, index: usize) -> bool {
        (self.words[index / 64] >> (index % 64)) & 1 == 1
    }

    /// 空の面には何も入っていない、として問う。
    ///
    /// 印が要らない経路（`--matting projection` など）に 24.5MP で 3MB を
    /// 確保させないための窓口である。`get` と分けてあるのは、**空かどうかを
    /// 気にしない呼び出し側が黙って添字を外す**のを防ぐため。
    #[inline]
    pub fn contains(&self, index: usize) -> bool {
        !self.words.is_empty() && self.get(index)
    }

    #[inline]
    pub fn insert(&mut self, index: usize) {
        self.words[index / 64] |= 1u64 << (index % 64);
    }

    /// 立っている添字だけを昇順に渡す。0 のワードをまとめて飛ばすので、
    /// **ほとんど立っていない面**を全画素走査するより桁で安い。
    pub fn for_each_set(&self, mut body: impl FnMut(usize)) {
        for (w, &word) in self.words.iter().enumerate() {
            let mut bits = word;
            while bits != 0 {
                let bit = bits.trailing_zeros() as usize;
                body(w * 64 + bit);
                bits &= bits - 1;
            }
        }
    }

    #[inline]
    pub fn set(&mut self, index: usize, value: bool) {
        let bit = 1u64 << (index % 64);
        let word = &mut self.words[index / 64];
        if value {
            *word |= bit;
        } else {
            *word &= !bit;
        }
    }
}

/// 形態素処理に渡せる真偽値の面。
///
/// `Vec<bool>` とビットセットを **同じ 1 本の実装**へ通すためだけに存在する。
/// 2 つの表現に 2 つの収縮／膨張を持つと、片方だけ直したときに黙って食い違う。
pub trait Plane {
    fn at(&self, index: usize) -> bool;
}

impl Plane for [bool] {
    #[inline]
    fn at(&self, index: usize) -> bool {
        self[index]
    }
}

impl Plane for BitPlane {
    #[inline]
    fn at(&self, index: usize) -> bool {
        self.get(index)
    }
}

/// 正方形の構造要素による収縮（`take_max` が false）／膨張（true）。
///
/// 横と縦に分けて O(n * radius) に収める。**枠の外は窓に含めない**——外を
/// 前景とみなすと、画面の端で切れている背景まで削れてしまう。
///
/// `floodfill` の測地的オープニングと `refine` の隙間の閉じ直しが、同じ規約を
/// 別々に書き写していた。規約（枠の外の扱い）を 2 箇所に持つと必ず離れる。
pub fn separable<P: Plane + ?Sized>(
    w: usize,
    h: usize,
    src: &P,
    radius: u32,
    take_max: bool,
) -> BitPlane {
    let mut out = BitPlane::new(w * h);
    if radius == 0 {
        for i in 0..w * h {
            out.set(i, src.at(i));
        }
        return out;
    }
    let r = radius as usize;
    let combine = |acc: bool, v: bool| if take_max { acc || v } else { acc && v };

    let mut horizontal = BitPlane::new(w * h);
    for y in 0..h {
        for x in 0..w {
            let (from, to) = (x.saturating_sub(r), (x + r).min(w - 1));
            let mut acc = !take_max;
            for k in from..=to {
                acc = combine(acc, src.at(y * w + k));
            }
            horizontal.set(y * w + x, acc);
        }
    }
    for y in 0..h {
        for x in 0..w {
            let (from, to) = (y.saturating_sub(r), (y + r).min(h - 1));
            let mut acc = !take_max;
            for k in from..=to {
                acc = combine(acc, horizontal.get(k * w + x));
            }
            out.set(y * w + x, acc);
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

    /// 長辺 1000px 以下では従来どおりの面積で消す。
    #[test]
    fn the_area_threshold_is_unchanged_for_small_images() {
        assert_eq!(speck_min_area(2, 120, 120), 25, "面積 (2*2+1)^2");
        assert_eq!(speck_min_area(2, 1000, 800), 25, "長辺ちょうど 1000px");
        assert_eq!(speck_min_area(1, 400, 400), 9);
        assert_eq!(speck_min_area(0, 4000, 4000), 0, "半径 0 は無効");
    }

    /// 長辺が伸びれば面積のしきい値も同じ比で伸びる。
    ///
    /// 同じ被写体を 4 倍の解像度で撮れば、同じ大きさに見えるゴミの面積は
    /// 16 倍になる。固定値では 20MP の織り目が消せない。
    #[test]
    fn the_area_threshold_follows_the_resolution() {
        assert_eq!(speck_min_area(2, 3000, 4000), 400, "(2*2+1)^2 * 4^2");
        assert_eq!(speck_min_area(2, 1600, 1600), 64, "(5*1.6)^2");
        // 実写の 4284x5712。既定の cleanup 2 で 150px² の織り目が消える
        assert!(
            speck_min_area(2, 4284, 5712) > 400,
            "20MP で織り目(150px²)を超えていない: {}",
            speck_min_area(2, 4284, 5712)
        );
    }

    /// 実効半径は面積の下限と同じ大きさを指すこと。
    ///
    /// 境界の探索深さがこれを使うので、面積の式とずれると
    /// 「高解像度でだけ縁を跨げない」という形で静かに壊れる。
    #[test]
    fn the_effective_radius_matches_the_area_threshold() {
        for (r, w, h) in [(2u32, 600u32, 600u32), (2, 3000, 4000), (2, 4284, 5712)] {
            // 半径は整数なので一辺は必ず奇数になる。ずれは丸めのぶんの 1px まで
            let from_radius = f64::from(2 * speck_radius(r, w, h) + 1);
            let from_area = (speck_min_area(r, w, h) as f64).sqrt();
            assert!(
                (from_radius - from_area).abs() <= 1.0,
                "{w}x{h} 半径 {r}: 実効半径からの一辺 {from_radius} と面積からの一辺 {from_area} が食い違う"
            );
        }
        assert_eq!(
            speck_radius(2, 600, 600),
            2,
            "長辺 1000px 以下では換算しない"
        );
        assert_eq!(speck_radius(0, 4000, 4000), 0, "半径 0 は無効");
    }

    /// 半径に極端な値が来ても溢れないこと。CLI と batch は上限で断るが、
    /// ライブラリとして呼ばれる経路にはその関門が無い。
    #[test]
    fn an_absurd_radius_does_not_overflow() {
        assert!(speck_min_area(u32::MAX, 4000, 4000) > 0);
        assert!(speck_radius(u32::MAX, 4000, 4000) > 0);
    }

    /// 大きな画像では、小さな画像なら残る大きさのゴミが消えること。
    #[test]
    fn a_speck_that_survives_at_low_resolution_is_removed_at_high_resolution() {
        // 7x7 = 49px²。長辺 1000px 以下では下限 25px² を超えるので残るが、
        // 長辺 2000px 相当では下限が 100px² になるので消える
        let speck = |w: u32, h: u32| -> bool {
            let mut mask = Mask::new(w, h, 0);
            for y in 10..17 {
                for x in 10..17 {
                    mask.set(x, y, 255);
                }
            }
            remove_specks(&mask, 2).is_foreground(13, 13)
        };
        assert!(speck(500, 500), "長辺 500px で 7x7 が消えている");
        assert!(!speck(2000, 2000), "長辺 2000px で 7x7 が残っている");
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
