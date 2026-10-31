//! マスクの形態素処理。
//!
//! フラッドフィルの結果には、背景に散った孤立点（センサーノイズが背景色から
//! 外れたもの）が残る。これを消すのが `remove_specks` の役目である。
//!
//! オープニング（収縮 → 膨張）では消さない。オープニングは**構造要素より
//! 細いものをすべて消す**。半径 2 のオープニングは幅 5px 未満の構造を無条件に
//! 落とすため、ストラップ・持ち手・ケーブルといった商品の一部まで巻き添えにする。
//! 孤立点と細い構造を分けるのは太さではなく面積なので、連結成分の面積で選ぶ。
//!
//! `erode` / `dilate` / `open` / `close` は境界帯の生成や比較のためにある。
//! パイプラインでは使わない。理由は `remove_specks` と `close` の項を参照。

use std::collections::VecDeque;

use rayon::prelude::*;

use crate::cutout::mask::{FOREGROUND_THRESHOLD, Mask};

/// 孤立ノイズとして消す連結成分の面積の下限(px²)。
///
/// `radius` は「**長辺 1000px 換算での**半径 px」と読む。面積を
/// `(2r+1)^2 * (長辺/1000)^2` と画像の大きさに比例させるのは、ゴミの大きさが
/// 解像度に比例するためである。同じ被写体を 4 倍で撮れば、同じ大きさに見える
/// ゴミは 16 倍の画素を占める。実写（4284x5712 の不織布）では織り目の 1 粒が
/// 150px² あり、固定の 25px² では一つも消えない。
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
///
/// # 塗り広げずに、連結成分で答える
///
/// 消すかどうかは成分ごとの芯の面積だけで決まり、成分をどの順で辿るかには
/// 依らない。行ごとのラン（0 でない画素の連続区間）を並列に切り出して芯の
/// 画素数を数え、上下の行で接するランを `Runs::components` で結ぶ。1 画素ずつ
/// キューへ出し入れすると前景の大きい 24.5MP で 1 回 50ms かかるが、前景は
/// 長いランになるので結ぶ相手はその数十分の 1 しか無い。ランが細かく割れる
/// 素材では表が膨らむので、1 画素ずつ辿る旧来の経路へ落とす。
pub fn remove_specks(mask: &Mask, radius: u32) -> Mask {
    remove_specks_limited(mask, radius, RUN_DENSITY_LIMIT)
}

/// `remove_specks` の、ランの表を使ってよい密度（`Runs::collect` の `divisor`）を
/// 呼び出し側が決める版。試験は 1 を渡して、ランで答える経路を必ず通す。
fn remove_specks_limited(mask: &Mask, radius: u32, divisor: usize) -> Mask {
    if radius == 0 {
        return mask.clone();
    }
    let (w, h) = (mask.width(), mask.height());
    let min_area = speck_min_area(radius, w, h);
    let stride = w as usize;
    if stride == 0 || h == 0 {
        return mask.clone();
    }
    let alpha = mask.as_slice();
    // ランが細かく割れる素材（1px の市松など）では、ランの表が画素数に比例して
    // 膨らみ、1 画素ずつ辿るより重くなる。そこでは 1 画素ずつ辿る旧来の経路へ
    // 落とす（`RUN_DENSITY_LIMIT`）
    let Some(runs) = Runs::collect(stride, h as usize, divisor, |i| alpha[i] != 0) else {
        return remove_specks_bfs(mask, radius);
    };
    let component = runs.components(true);
    // 芯（前景判定）の画素数をランごとに数え、成分の代表へ畳む。整数の和なので
    // 足す順序で値は動かない
    let core = runs.per_run(|y, run| {
        let row = &alpha[y * stride + run.x0 as usize..=y * stride + run.x1 as usize];
        row.iter().filter(|&&v| v >= FOREGROUND_THRESHOLD).count() as u32
    });
    let mut area = vec![0u32; component.len()];
    for (r, &c) in core.iter().enumerate() {
        area[component[r] as usize] += c;
    }
    drop(core);
    // 消す成分のランだけを 0 で塗る。行ごとに書き先が分かれるので並列に回す
    let mut out = mask.clone();
    out.as_mut_slice()
        .par_chunks_mut(stride)
        .enumerate()
        .for_each(|(y, row)| {
            for r in runs.first[y]..runs.first[y + 1] {
                let run = runs.runs[r];
                if (area[component[r] as usize] as usize) < min_area {
                    row[run.x0 as usize..=run.x1 as usize].fill(0);
                }
            }
        });
    out
}

/// `remove_specks` の、1 画素ずつ辿る旧来の経路（Phase 35 より前の実装を
/// 逐語のまま残したもの）。ランが細かく割れる素材で使う。
///
/// 試験の物差しも兼ねる。ランで答える経路と一致することを
/// `the_run_speck_filter_agrees_with_the_old_one` が見ている。
fn remove_specks_bfs(mask: &Mask, radius: u32) -> Mask {
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
            // 207MB から 400MB へ跳ねる
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

/// ランの表を使ってよい密度の上限。ランの総数が画素数のこの分の 1 を超えたら、
/// ランで答える経路は使わない。
///
/// ラン 1 本は 8 バイト（`Run`）、連結の親が 4 バイト、`remove_specks` の芯の
/// 数と成分の面積が 4 バイトずつで、上限ちょうどでも画素あたり 2.5 バイトに
/// 収まる。1px の市松のようにランが画素数の半分まで割れる素材で表を作ると、
/// 5000x4000 で 100MB を超え、1 画素ずつ辿る旧来の経路よりピーク RSS が上がる
/// （581 → 674MB と測れた）。
///
/// 8 は実写の分布から決めた。24.5MP の実写リモコンでいちばん割れるフィルの
/// 芯でも画素数の 1.1%、実写キーボードで 7.4% に
/// 留まり、市松は 41% ある。32 にするとキーボードの芯が旧来の経路へ落ちる。
pub(crate) const RUN_DENSITY_LIMIT: usize = 8;

/// 行の中の連続区間。`x0..=x1`（両端を含む）。
#[derive(Clone, Copy, Default, Debug, PartialEq, Eq)]
pub(crate) struct Run {
    pub x0: u32,
    pub x1: u32,
}

/// 行ごとのランを平坦に並べた表。行 `y` のランは `runs[first[y]..first[y + 1]]`
/// で、x の昇順に並ぶ。
///
/// `Vec<Vec<Run>>` にしないのは、行ごとの器の頭と倍々の余りが、ランが細かい
/// 素材でそのまま常駐量に乗るためである。
pub(crate) struct Runs {
    pub runs: Vec<Run>,
    pub first: Vec<usize>,
}

impl Runs {
    /// `inside(添字)` が真の画素の連続区間を、行ごとに並列に切り出す。
    ///
    /// 1 度目の走査で行ごとの本数を数えて表の大きさを決め、2 度目で書く。
    /// 総数が画素数の `divisor` 分の 1 を超えたら `None` を返し、表は作らない
    /// （本番は `RUN_DENSITY_LIMIT`）。
    pub(crate) fn collect(
        w: usize,
        h: usize,
        divisor: usize,
        inside: impl Fn(usize) -> bool + Sync,
    ) -> Option<Runs> {
        let scan = |y: usize, mut put: Option<&mut [Run]>| -> usize {
            let row = y * w;
            let mut n = 0;
            let mut start: Option<usize> = None;
            for x in 0..=w {
                let on = x < w && inside(row + x);
                match (on, start) {
                    (true, None) => start = Some(x),
                    (false, Some(x0)) => {
                        if let Some(out) = put.as_deref_mut() {
                            out[n] = Run {
                                x0: x0 as u32,
                                x1: (x - 1) as u32,
                            };
                        }
                        n += 1;
                        start = None;
                    }
                    _ => {}
                }
            }
            n
        };
        let counts: Vec<usize> = (0..h).into_par_iter().map(|y| scan(y, None)).collect();
        let mut first = Vec::with_capacity(h + 1);
        let mut total = 0usize;
        for &n in &counts {
            first.push(total);
            total += n;
        }
        first.push(total);
        drop(counts);
        if total > (w * h) / divisor.max(1) {
            return None;
        }
        let mut runs = vec![Run::default(); total];
        let mut slices = Vec::with_capacity(h);
        let mut rest = runs.as_mut_slice();
        for y in 0..h {
            let (head, tail) = rest.split_at_mut(first[y + 1] - first[y]);
            slices.push(head);
            rest = tail;
        }
        slices.into_par_iter().enumerate().for_each(|(y, out)| {
            scan(y, Some(out));
        });
        Some(Runs { runs, first })
    }

    /// 行の数。
    pub(crate) fn rows(&self) -> usize {
        self.first.len() - 1
    }

    /// ランごとに `f(行, ラン)` を求めた表。行ごとに並列に回す。
    pub(crate) fn per_run<T: Send + Default + Clone>(
        &self,
        f: impl Fn(usize, Run) -> T + Sync,
    ) -> Vec<T> {
        let mut out = vec![T::default(); self.runs.len()];
        let mut slices = Vec::with_capacity(self.rows());
        let mut rest = out.as_mut_slice();
        for y in 0..self.rows() {
            let (head, tail) = rest.split_at_mut(self.first[y + 1] - self.first[y]);
            slices.push(head);
            rest = tail;
        }
        slices.into_par_iter().enumerate().for_each(|(y, slots)| {
            for (k, slot) in slots.iter_mut().enumerate() {
                *slot = f(y, self.runs[self.first[y] + k]);
            }
        });
        out
    }

    /// 上下の行で接するランどうしをつないだ連結成分。ランごとに成分の番号
    /// （成分を代表するランの通し番号）を返す。`diagonal` が真なら 8 近傍
    /// （斜めに 1 画素ずれて接するランもつなぐ）、偽なら 4 近傍。
    ///
    /// **成分は結ぶ順序に依らない。** 連結は「接している」という対称な関係の
    /// 推移閉包でしかないので、どの順で union-find に掛けても同じ分け方になる。
    pub(crate) fn components(&self, diagonal: bool) -> Vec<u32> {
        let total = self.runs.len();
        let slack = u32::from(diagonal);
        let mut parent: Vec<u32> = (0..total as u32).collect();
        for y in 1..self.rows() {
            let above = &self.runs[self.first[y - 1]..self.first[y]];
            let below = &self.runs[self.first[y]..self.first[y + 1]];
            let (mut i, mut j) = (0, 0);
            while i < above.len() && j < below.len() {
                let (a, b) = (above[i], below[j]);
                if a.x0 <= b.x1 + slack && b.x0 <= a.x1 + slack {
                    union(
                        &mut parent,
                        (self.first[y - 1] + i) as u32,
                        (self.first[y] + j) as u32,
                    );
                }
                if a.x1 < b.x1 {
                    i += 1;
                } else {
                    j += 1;
                }
            }
        }
        for i in 0..total as u32 {
            let root = find(&mut parent, i);
            parent[i as usize] = root;
        }
        parent
    }
}

/// union-find の根。辿った道は半分ずつ縮める。
fn find(parent: &mut [u32], mut i: u32) -> u32 {
    while parent[i as usize] != i {
        let up = parent[parent[i as usize] as usize];
        parent[i as usize] = up;
        i = up;
    }
    i
}

/// 2 つの成分を結ぶ。根の小さいほうへ寄せる（どちらへ寄せても成分は同じ）。
fn union(parent: &mut [u32], a: u32, b: u32) {
    let (ra, rb) = (find(parent, a), find(parent, b));
    if ra != rb {
        let (lo, hi) = (ra.min(rb), ra.max(rb));
        parent[hi as usize] = lo;
    }
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

    /// 添字ごとの真偽から面を作る。語ごとに並列に組む。
    ///
    /// 面は画素の添字をそのまま詰めるので、1 語は 64 画素ぶんで、語どうしは
    /// 書き先が重ならない。`f` は添字だけで決まる純粋な判定であること。
    pub fn from_fn(len: usize, f: impl Fn(usize) -> bool + Sync) -> Self {
        let mut words = vec![0u64; len.div_ceil(64)];
        words.par_iter_mut().enumerate().for_each(|(k, word)| {
            let base = k * 64;
            let mut v = 0u64;
            for b in 0..(len - base).min(64) {
                if f(base + b) {
                    v |= 1u64 << b;
                }
            }
            *word = v;
        });
        Self { words, len }
    }

    pub fn len(&self) -> usize {
        self.len
    }

    /// 2 枚の面の積（両方で立っている画素）。語ごとに畳む。
    pub fn and(&self, other: &BitPlane) -> BitPlane {
        assert_eq!(self.len, other.len, "面の大きさが違う");
        let words = self
            .words
            .par_iter()
            .zip(&other.words)
            .map(|(a, b)| a & b)
            .collect();
        BitPlane {
            words,
            len: self.len,
        }
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

    /// 連続する `w` ビットを、語の列へ先頭詰めで写す。
    ///
    /// 行の morphology はビット列を語ごとにずらして畳むので、入口で
    /// 「行 1 本を語に揃えて取り出す」ことが要る。1 画素ずつ問うと
    /// 24.5MP で 2450 万回になるが、**どちらもビットの列なので、
    /// ずらして繋ぐだけで済む。**
    pub fn read_bits(&self, start: usize, w: usize, out: &mut [u64]) {
        let (sw, so) = (start / 64, start % 64);
        for (k, slot) in out.iter_mut().enumerate() {
            let lo = self.words.get(sw + k).copied().unwrap_or(0);
            *slot = if so == 0 {
                lo
            } else {
                let hi = self.words.get(sw + k + 1).copied().unwrap_or(0);
                (lo >> so) | (hi << (64 - so))
            };
        }
        // 行からはみ出したビットは残さない。呼ぶ側が窓の外の扱いを決める
        let used = w % 64;
        if used != 0 {
            let last = w.div_ceil(64) - 1;
            out[last] &= (1u64 << used) - 1;
        }
    }

    /// 語の列の先頭 `w` ビットを、`start` の位置へ重ねる。
    ///
    /// 面は画素の添字をそのまま詰める（行で揃えない）ので、行ごとに作った
    /// 結果を書き戻すにはこのずらしが要る。立っていないビットは触らない
    /// ——`new` 直後の面に順に重ねる使い方だけを想定している。
    pub fn insert_bits(&mut self, start: usize, w: usize, src: &[u64]) {
        let mut i = 0;
        while i < w {
            let n = (w - i).min(64);
            let (sw, so) = (i / 64, i % 64);
            let mut v = src[sw] >> so;
            if so > 0 {
                if let Some(&hi) = src.get(sw + 1) {
                    v |= hi << (64 - so);
                }
            }
            if n < 64 {
                v &= (1u64 << n) - 1;
            }
            let (dw, dof) = ((start + i) / 64, (start + i) % 64);
            self.words[dw] |= v << dof;
            if dof + n > 64 {
                self.words[dw + 1] |= v >> (64 - dof);
            }
            i += n;
        }
    }

    /// 1 ビットでも立っているか。**語ごとに見る**ので、空の面では
    /// 画素数の 1/64 の読みで答えが出る。
    pub fn any(&self) -> bool {
        self.words.iter().any(|&word| word != 0)
    }

    /// 連続する範囲を一度に立てる。
    ///
    /// **語ごとに埋める。** bbox の外側のように広い範囲を 1 画素ずつ立てると、
    /// 24.5MP では 2000 万回の読み・変更・書きになる。両端の語だけを画素で
    /// 扱い、間は 1 語 64 画素でまとめて埋める。
    pub fn insert_range(&mut self, start: usize, end: usize) {
        let end = end.min(self.len);
        if start >= end {
            return;
        }
        let (first, last) = (start / 64, (end - 1) / 64);
        if first == last {
            let mask = (!0u64 << (start % 64)) & (!0u64 >> (63 - (end - 1) % 64));
            self.words[first] |= mask;
            return;
        }
        self.words[first] |= !0u64 << (start % 64);
        for word in &mut self.words[first + 1..last] {
            *word = !0u64;
        }
        self.words[last] |= !0u64 >> (63 - (end - 1) % 64);
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

    /// 連続する `w` ビットを語の列へ先頭詰めで写す。`out` は全体を書き換える。
    ///
    /// 既定は 1 画素ずつ。ビットで持っている面はずらして繋ぐだけで済むので、
    /// そちらで上書きする。
    fn read_bits(&self, start: usize, w: usize, out: &mut [u64]) {
        out.fill(0);
        for x in 0..w {
            if self.at(start + x) {
                out[x / 64] |= 1u64 << (x % 64);
            }
        }
    }
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

    fn read_bits(&self, start: usize, w: usize, out: &mut [u64]) {
        BitPlane::read_bits(self, start, w, out);
    }
}

/// 正方形の構造要素による収縮（`take_max` が false）／膨張（true）。
///
/// 横と縦に分けて O(n * radius) に収める。**枠の外は窓に含めない**——外を
/// 前景とみなすと、画面の端で切れている背景まで削れてしまう。
///
/// `floodfill` の測地的オープニングと `reshape` の隙間の閉じ直しが同じ規約で
/// 使う。規約（枠の外の扱い）を 2 箇所に持つと必ず離れるので、ここ 1 本に置く。
pub fn separable<P: Plane + Sync + ?Sized>(
    w: usize,
    h: usize,
    src: &P,
    radius: u32,
    take_max: bool,
) -> BitPlane {
    let mut out = BitPlane::new(w * h);
    if w == 0 || h == 0 {
        return out;
    }
    if radius == 0 {
        for i in 0..w * h {
            out.set(i, src.at(i));
        }
        return out;
    }
    let wpr = w.div_ceil(64);
    let mut rows = vec![0u64; h * wpr];
    rows.par_chunks_mut(wpr)
        .enumerate()
        .for_each(|(y, row)| src.read_bits(y * w, w, row));
    let rows = separable_rows(w, h, rows, radius, take_max);
    for (y, row) in rows.chunks_exact(wpr).enumerate() {
        out.insert_bits(y * w, w, row);
    }
    out
}

/// `separable` を**行ごとに語へ揃えた面**の上でやる版。
///
/// 面は 1 行 `w.div_ceil(64)` 語で、行の端より先のビットは 0 であること。
/// 返す面も同じ形で、端より先は 0 に揃えて返す。
///
/// 行を語に揃えておくと、行ごとの処理を並列に回せる（書き先の語が行を
/// またがない）。`separable` の詰めた面と行のあいだの写しは、語をずらして
/// 繋ぐだけなので安い。
pub(crate) fn separable_rows(
    w: usize,
    h: usize,
    mut rows: Vec<u64>,
    radius: u32,
    take_max: bool,
) -> Vec<u64> {
    if w == 0 || h == 0 || radius == 0 {
        return rows;
    }
    let r = radius as usize;
    // **窓の外は単位元で埋める。** 収縮（AND）なら真、膨張（OR）なら偽を
    // 置けば、畳んだ答えが「窓に含めない」と同じになる。枠の外の扱いを
    // 分岐で書き分けずに済む
    let identity = !take_max;
    let filler: u64 = if identity { !0 } else { 0 };
    let wpr = w.div_ceil(64);
    let used = w % 64;
    // 行の端より先のビット。入口では単位元へ、出口では 0 へ揃える
    let tail = if used == 0 { 0 } else { !0u64 << used };

    // 横方向。**1 語 64 画素を一度に畳む。** 1 画素ずつ窓を舐めると
    // 24.5MP・半径 1 で 7350 万回の読みになる。行は互いに独立なので並列に回し、
    // 元の行は作業用の 1 行へ写してから同じ場所へ畳み込む
    rows.par_chunks_mut(wpr).for_each_init(
        || vec![0u64; wpr],
        |line, acc| {
            line.copy_from_slice(acc);
            if identity {
                line[wpr - 1] |= tail;
            }
            acc.copy_from_slice(line);
            for d in 1..=r as isize {
                for (i, slot) in acc.iter_mut().enumerate() {
                    let a = shifted(line, i, d, filler);
                    let b = shifted(line, i, -d, filler);
                    *slot = if take_max {
                        *slot | a | b
                    } else {
                        *slot & a & b
                    };
                }
            }
        },
    );
    let across = rows;

    // 縦方向。行どうしをそのまま語で畳む（ずらしが要らない）。出力の行は
    // 横方向の結果を読むだけなので、これも行ごとに並列に回せる
    let mut out = vec![0u64; h * wpr];
    out.par_chunks_mut(wpr).enumerate().for_each(|(y, acc)| {
        acc.copy_from_slice(&across[y * wpr..(y + 1) * wpr]);
        let from = y.saturating_sub(r);
        let to = (y + r).min(h - 1);
        for k in from..=to {
            if k == y {
                continue;
            }
            let other = &across[k * wpr..(k + 1) * wpr];
            for (slot, &v) in acc.iter_mut().zip(other) {
                *slot = if take_max { *slot | v } else { *slot & v };
            }
        }
        acc[wpr - 1] &= !tail;
    });
    out
}

/// 語の列を `d` 画素ずらして読んだときの `i` 番目の語。
///
/// `d > 0` は「右の画素を見る」（結果の x が元の x+d）。列の外は `filler`
/// ——収縮なら全ビット 1、膨張なら 0 で、畳んでも答えを動かさない値である。
#[inline]
fn shifted(src: &[u64], i: usize, d: isize, filler: u64) -> u64 {
    let at = |j: isize| -> u64 {
        if j < 0 {
            filler
        } else {
            src.get(j as usize).copied().unwrap_or(filler)
        }
    };
    let i = i as isize;
    if d >= 0 {
        let (s, o) = (d / 64, (d % 64) as u32);
        let lo = at(i + s);
        if o == 0 {
            lo
        } else {
            (lo >> o) | (at(i + s + 1) << (64 - o))
        }
    } else {
        let e = -d;
        let (s, o) = (e / 64, (e % 64) as u32);
        let hi = at(i - s);
        if o == 0 {
            hi
        } else {
            (hi << o) | (at(i - s - 1) >> (64 - o))
        }
    }
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

    /// ランで答える `remove_specks` が、Phase 35 より前の実装
    /// （`remove_specks_bfs`、逐語）と、8 近傍を 1 画素ずつ辿って芯の面積を
    /// 数える素直な実装の両方と同じマスクを返すこと。半透明の裾を混ぜ、
    /// 寸法を振る。ランの経路は密度の上限を外して（`divisor` 1）必ず通す。
    #[test]
    fn the_run_speck_filter_agrees_with_the_old_one() {
        fn plain(mask: &Mask, min_area: usize) -> Mask {
            let (w, h) = (mask.width() as i64, mask.height() as i64);
            let mut out = mask.clone();
            let mut label = vec![usize::MAX; (w * h) as usize];
            let mut members = Vec::new();
            for start in 0..(w * h) as usize {
                if label[start] != usize::MAX || mask.as_slice()[start] == 0 {
                    continue;
                }
                members.clear();
                label[start] = start;
                members.push(start);
                let mut k = 0;
                while k < members.len() {
                    let i = members[k] as i64;
                    k += 1;
                    for dy in -1..=1 {
                        for dx in -1..=1 {
                            let (nx, ny) = (i % w + dx, i / w + dy);
                            if nx < 0 || ny < 0 || nx >= w || ny >= h {
                                continue;
                            }
                            let j = (ny * w + nx) as usize;
                            if label[j] == usize::MAX && mask.as_slice()[j] != 0 {
                                label[j] = start;
                                members.push(j);
                            }
                        }
                    }
                }
                let core = members
                    .iter()
                    .filter(|&&i| mask.as_slice()[i] >= FOREGROUND_THRESHOLD)
                    .count();
                if core < min_area {
                    for &i in &members {
                        out.as_mut_slice()[i] = 0;
                    }
                }
            }
            out
        }

        let mut state = 0x1234_5678_u32;
        let sizes = [
            (53u32, 41u32),
            (1, 1),
            (1, 9),
            (9, 1),
            (63, 20),
            (64, 20),
            (65, 20),
            (127, 7),
            (129, 7),
            (12, 300),
        ];
        for (w, h) in sizes {
            for density in [20u32, 45, 60, 95] {
                let mut mask = Mask::new(w, h, 0);
                for y in 0..h {
                    for x in 0..w {
                        state ^= state << 13;
                        state ^= state >> 17;
                        state ^= state << 5;
                        if state % 100 < density {
                            mask.set(x, y, if state % 7 == 0 { 60 } else { 255 });
                        }
                    }
                }
                for radius in [1, 2, 3] {
                    let label = format!("{w}x{h} 密度 {density} 半径 {radius}");
                    let old = remove_specks_bfs(&mask, radius);
                    assert_eq!(remove_specks_limited(&mask, radius, 1), old, "{label}");
                    assert_eq!(remove_specks(&mask, radius), old, "{label}");
                    assert_eq!(plain(&mask, speck_min_area(radius, w, h)), old, "{label}");
                }
            }
        }
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

    /// 長辺 1000px 以下では換算せず、`(2r+1)²` の面積で消す。
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
