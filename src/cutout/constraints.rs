//! 空間的な指示（トライマップ / マスク画像 / ポリゴン）の内部表現。
//!
//! **入口が何であれ、内部は 1 つの表現にする。** トライマップ・マスク画像・
//! ポリゴン・`--fg-seed` は、どれも「この画素は確定前景」「この画素は確定背景」
//! しか言っていない。入口ごとに別の経路を作ると、フィル・診断・プレビューの
//! 3 箇所で同じ判断を書き直すことになり、片方だけ直したときに
//! 「指定したのに効かない」が生まれる。
//!
//! ファイルのことは知らない。パスの解決とマスク画像の読み込みは
//! `commands/cutout.rs` が行う（`--bbox` の `resolve_bbox` と同じ層）。
//!
//! 画素 1 つにつき 1 バイトのフラグで持つ。12MP で 12MB、`Constraint` を
//! そのまま `Vec` に積んでも同じ量だが、**前景と背景を別のビットにしておくと
//! 衝突をそのまま表現できる**。衝突は黙ってどちらかを選ばずにエラーへ回すので、
//! 「選べない状態」を持てること自体が要る。

use image::RgbaImage;

/// 画素ごとの制約。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Constraint {
    /// 何も強制しない（トライマップの不明帯もこれ）
    Free,
    /// 色によらず前景。bbox と確定背景より優先する
    ForcedFg,
    /// 色によらず背景。フィルの種にもなる
    ForcedBg,
}

/// トライマップで確定前景とみなす輝度の下限。
pub const TRIMAP_FOREGROUND: u8 = 192;
/// トライマップで確定背景とみなす輝度の上限。
///
/// この 2 つの間（64〜191）は「不明」で、何も強制しない。Phase 3 の matting は
/// この帯を作業領域にする。
pub const TRIMAP_BACKGROUND: u8 = 63;

/// `--fg-mask` / `--bg-mask` で指示とみなす輝度の下限。
///
/// **「0 でない」では JPEG のリンギングを拾う。** 白く塗った矩形を q85 で
/// 保存しただけで、黒いはずの周囲に 1〜数の値が散り、指示された面積が実測で
/// 2.4 倍になった。中点で切れば、可逆でない形式を経由しても指示は動かない。
///
/// トライマップの `TRIMAP_FOREGROUND` / `TRIMAP_BACKGROUND` と同じ向きで
/// 「中間は指示ではない」を表す値でもある。
pub const MASK_THRESHOLD: u8 = 128;

const FG: u8 = 1 << 0;
const BG: u8 = 1 << 1;

/// 制約の入口。結果 JSON の `constraints.sources` に出る名前でもある。
///
/// **`--bbox` はここに入れない。** 既存の `settings` と `applied_bbox` が
/// 同じことを既に言っており、2 箇所で名乗ると「別の指定が 2 つある」と読める。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConstraintSource {
    /// セグメンテーションモデルが出した提案（`--segment`）。
    ///
    /// **他の入口と違って、これは利用者が引いた線ではない。** 同じ画素で
    /// 衝突したら利用者の指示が勝つ（`overlay`）ので、`CONSTRAINT_CONFLICT`
    /// にはならない
    Segment,
    Trimap,
    FgMask,
    BgMask,
    FgPolygon,
    BgPolygon,
    FgSeed,
}

impl ConstraintSource {
    pub fn as_str(self) -> &'static str {
        match self {
            ConstraintSource::Segment => "segment",
            ConstraintSource::Trimap => "trimap",
            ConstraintSource::FgMask => "fg_mask",
            ConstraintSource::BgMask => "bg_mask",
            ConstraintSource::FgPolygon => "fg_polygon",
            ConstraintSource::BgPolygon => "bg_polygon",
            ConstraintSource::FgSeed => "fg_seed",
        }
    }
}

/// 確定前景と確定背景が同じ画素で重なった箇所。
///
/// 黙ってどちらかを選ぶと「指定が効いていない」という最も追いにくい失敗になる
/// ので、呼び出し側はこれをエラー（`CONSTRAINT_CONFLICT`）へ変える。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Conflict {
    pub count: u64,
    /// 重なりの外接矩形 (x1, y1, x2, y2)。どこを直せばよいかを示すために持つ
    pub bbox: (u32, u32, u32, u32),
}

/// 画素ごとの制約と、それを作った入口の名前。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Constraints {
    width: u32,
    height: u32,
    flags: Vec<u8>,
    sources: Vec<ConstraintSource>,
}

impl Constraints {
    pub fn new(width: u32, height: u32) -> Self {
        Self {
            width,
            height,
            flags: vec![0u8; (width as usize) * (height as usize)],
            sources: Vec::new(),
        }
    }

    pub fn width(&self) -> u32 {
        self.width
    }

    pub fn height(&self) -> u32 {
        self.height
    }

    /// 効いた入口として記録する。同じ入口は 1 度だけ、指定された順に並ぶ。
    pub fn note(&mut self, source: ConstraintSource) {
        if !self.sources.contains(&source) {
            self.sources.push(source);
        }
    }

    pub fn sources(&self) -> &[ConstraintSource] {
        &self.sources
    }

    /// 制約が 1 画素も置かれていないか。
    ///
    /// 空なら呼び出し側は `None` として扱う。12MP で 12MB の表を、何も
    /// 強制しないまま下流へ配る理由が無い。
    pub fn is_empty(&self) -> bool {
        self.flags.iter().all(|&f| f == 0)
    }

    #[inline]
    fn index(&self, x: u32, y: u32) -> usize {
        (y as usize) * (self.width as usize) + (x as usize)
    }

    #[inline]
    pub fn has_fg(&self, index: usize) -> bool {
        self.flags[index] & FG != 0
    }

    #[inline]
    pub fn has_bg(&self, index: usize) -> bool {
        self.flags[index] & BG != 0
    }

    /// 画素ごとの制約。衝突している画素では前景を返す（優先順位は前景 > 背景）。
    ///
    /// 衝突は `conflict()` でエラーにしてから使うのが正しい経路だが、
    /// 公開 API なので「どちらとも言えない」で panic させない。
    pub fn at(&self, x: u32, y: u32) -> Constraint {
        if x >= self.width || y >= self.height {
            return Constraint::Free;
        }
        let f = self.flags[self.index(x, y)];
        if f & FG != 0 {
            Constraint::ForcedFg
        } else if f & BG != 0 {
            Constraint::ForcedBg
        } else {
            Constraint::Free
        }
    }

    pub fn mark(&mut self, x: u32, y: u32, kind: Constraint) {
        if x >= self.width || y >= self.height {
            return;
        }
        let index = self.index(x, y);
        self.mark_index(index, kind);
    }

    #[inline]
    pub fn mark_index(&mut self, index: usize, kind: Constraint) {
        match kind {
            Constraint::Free => {}
            Constraint::ForcedFg => self.flags[index] |= FG,
            Constraint::ForcedBg => self.flags[index] |= BG,
        }
    }

    /// 別の寸法へ最近傍で写した表を作る。
    ///
    /// **`--optimize` の探索段のためだけにある。** 候補を縮小版で回すのに、
    /// 指示だけ原寸のままでは寸法が合わず、`foreground_mask` の規約に従って
    /// 丸ごと無かったことにされる——利用者がトライマップを渡した画像で、
    /// 探索だけが指示の無い世界で行われることになる。
    ///
    /// 最近傍にするのは、確定領域の意味を変えないためである。補間すると
    /// 「半分だけ確定前景」という中間状態が生まれ、どちらへ丸めても
    /// 指示が 1px 太るか痩せるかになる。
    ///
    /// **細い指示は丸ごと消えうる。** 最近傍は行と列を捨てるので、1px の線を
    /// 10 分の 1 に縮めれば残らないことがあり、そのとき探索段は指示が無い
    /// ものとして順位を付ける。呑んでいるのは、探索段が順位を付けるためだけの
    /// ものだからである——**最終段は原寸の指示そのまま**で回るので、書き出される
    /// 画素には影響しない。
    ///
    /// `sources` は引き継ぐ。写した表も「その入口が置いたもの」であることに
    /// 変わりはない。
    pub fn resampled(&self, width: u32, height: u32) -> Constraints {
        let mut out = Constraints::new(width, height);
        out.sources = self.sources.clone();
        if width == 0 || height == 0 || self.width == 0 || self.height == 0 {
            return out;
        }
        // 列の対応は行に依らない。内側で引き直すと 1500x1125 で 170 万回の
        // 除算と `round` を払うことになる
        let columns: Vec<u32> = (0..width).map(|x| nearest(x, width, self.width)).collect();
        for y in 0..height {
            let sy = nearest(y, height, self.height);
            let row = (y as usize) * (width as usize);
            for (x, &sx) in columns.iter().enumerate() {
                out.flags[row + x] = self.flags[self.index(sx, sy)];
            }
        }
        out
    }

    /// 確定前景が 1 画素でもあるか。
    ///
    /// 表を確保するかどうかの判断に使う。`--fg-mask` だけを渡した実行で
    /// 「守る画素の表」を無駄に積まないためで、12MP では 1 本 12MB になる。
    pub fn any_fg(&self) -> bool {
        self.flags.iter().any(|&f| f & FG != 0)
    }

    /// 確定背景が 1 画素でもあるか。
    pub fn any_bg(&self) -> bool {
        self.flags.iter().any(|&f| f & BG != 0)
    }

    /// (確定前景の画素数, 確定背景の画素数)。衝突している画素は両方に数える。
    ///
    /// **比率だけでは足りない。** `round4` は 12MP の 1000 画素を 0.0001 に
    /// 落とし、5712x4284 の 20x20 に至っては 0.0000 になる。`sources` に名前が
    /// 出ているのに `fg_ratio` が 0.0 という結果を、エージェントは「効かなかった」
    /// としか読めない。画素数は桁落ちしない。
    pub fn counts(&self) -> (u64, u64) {
        let mut fg = 0u64;
        let mut bg = 0u64;
        for &f in &self.flags {
            if f & FG != 0 {
                fg += 1;
            }
            if f & BG != 0 {
                bg += 1;
            }
        }
        (fg, bg)
    }

    /// 画像に占める (確定前景, 確定背景, 不明) の割合。
    ///
    /// 衝突が無ければ 3 つの和は 1 になる。衝突はエラーにしてから報告するので、
    /// 和が崩れた比率が結果 JSON に出ることはない。
    pub fn ratios(&self) -> (f64, f64, f64) {
        let total = self.flags.len().max(1) as f64;
        let (fg, bg) = self.counts();
        let free = self.flags.iter().filter(|&&f| f == 0).count() as f64;
        (fg as f64 / total, bg as f64 / total, free / total)
    }

    /// 確定前景と確定背景が重なっている画素を数え、外接矩形を返す。
    pub fn conflict(&self) -> Option<Conflict> {
        let mut count = 0u64;
        let (mut x1, mut y1, mut x2, mut y2) = (u32::MAX, u32::MAX, 0u32, 0u32);
        for (i, &f) in self.flags.iter().enumerate() {
            if f & FG == 0 || f & BG == 0 {
                continue;
            }
            let (x, y) = (
                (i % self.width as usize) as u32,
                (i / self.width as usize) as u32,
            );
            count += 1;
            x1 = x1.min(x);
            y1 = y1.min(y);
            x2 = x2.max(x);
            y2 = y2.max(y);
        }
        (count > 0).then_some(Conflict {
            count,
            bbox: (x1, y1, x2, y2),
        })
    }

    /// 別の指示を**上から**重ねる。重なった画素では上書き側だけが残る。
    ///
    /// **衝突をエラーにしない唯一の合成である。** `--trimap` どうしのように
    /// 対等な指示が重なれば `CONSTRAINT_CONFLICT` で断るが、片方が
    /// セグメンテーションモデルの**提案**で、もう片方が利用者の**決定**である
    /// 場合は話が違う。「モデルが背景だと言った画素を、利用者が前景だと指した」
    /// のは矛盾ではなく訂正であり、そこでエラーを返すのは
    /// 「モデルを使うと指示が出せなくなる」という筋の悪い規約になる。
    ///
    /// 上書きは**画素ごとに全部**である。利用者が前景と言った画素から、
    /// モデルが置いた背景の印も消える（ビットを OR すると衝突として残り、
    /// 後段の `conflict()` が拾ってしまう）。
    ///
    /// 入口の名前は自分のものの後ろへ、指定された順で並べる。寸法が違えば
    /// 何もしない（`mark_by_luma` と同じ規約）。
    pub fn overlay(&mut self, other: &Constraints) {
        if other.width != self.width || other.height != self.height {
            return;
        }
        for (slot, &top) in self.flags.iter_mut().zip(other.flags.iter()) {
            if top != 0 {
                *slot = top;
            }
        }
        for &source in &other.sources {
            self.note(source);
        }
    }

    /// 画素ごとの輝度から印を付ける。`decide` が `None` を返した画素は触らない。
    ///
    /// **アルファは見ない。** マスクは 1 チャンネルのグレーとして読む約束で、
    /// 「透過 PNG のアルファをマスクとして渡す」用途をここに混ぜると、
    /// 同じファイルが 2 通りに読まれることになる。
    ///
    /// 寸法が違えば何もしない。呼び出し側が先に検査して
    /// `MASK_SIZE_MISMATCH` を返す（`commands/cutout.rs`）。黙って拡縮すると
    /// 境界がずれるので、ここでも合わせにいかない。
    ///
    /// 印を付けた画素数を返す。**0 なら、その入口は渡されたが何も塗っていない。**
    /// `sources` に並べるかどうかの判断に使う
    pub fn mark_by_luma(
        &mut self,
        image: &RgbaImage,
        decide: impl Fn(u8) -> Option<Constraint>,
    ) -> u64 {
        if image.width() != self.width || image.height() != self.height {
            return 0;
        }
        let mut marked = 0u64;
        for (i, p) in image.pixels().enumerate() {
            if let Some(kind) = decide(luma(p.0)) {
                self.mark_index(i, kind);
                marked += 1;
            }
        }
        marked
    }

    /// 多角形の内部（偶奇規則）へ印を付ける。座標は画素座標で、原点は左上。
    ///
    /// **画像の外へ出た部分は捨てるが、面そのものは捨てない。** 頂点 1 つが
    /// 1px はみ出しただけで指示をまるごと無視するのは筋が悪く、かといって
    /// 頂点を画像の縁へ寄せると辺の傾きが変わって面の形が動く。走査線ごとに
    /// 画像の中へ切り詰めれば、指示された面の「画像に写っている部分」が
    /// そのまま残る。
    ///
    /// 自己交差は偶奇のまま扱う（交差の内側は穴になる）。凝ったことをしないのは、
    /// 規則が 1 つなら結果を予測できるからである。
    ///
    /// 印を付けた画素数を返す（`mark_by_luma` と同じ理由）。
    pub fn fill_polygon(&mut self, points: &[[f64; 2]], kind: Constraint) -> u64 {
        let mut marked = 0u64;
        if points.len() < 3 || self.width == 0 || self.height == 0 {
            return marked;
        }
        let top = points.iter().fold(f64::MAX, |a, p| a.min(p[1]));
        let bottom = points.iter().fold(f64::MIN, |a, p| a.max(p[1]));
        // 走査線は画素の中心（y + 0.5）で引く。`as` は飽和するので、
        // 遠くにある頂点でも添字が巻き戻らない
        let first = (top - 0.5).ceil().max(0.0) as u32;
        let last = ((bottom - 0.5).floor().max(0.0) as u32).min(self.height.saturating_sub(1));

        let mut crossings: Vec<f64> = Vec::new();
        for y in first..=last {
            let scan = f64::from(y) + 0.5;
            crossings.clear();
            for (a, b) in points.iter().zip(points.iter().cycle().skip(1)) {
                // 半開区間で数える。水平な辺は交差 0 本、頂点がちょうど走査線に
                // 載っても 1 本にしかならないので、偶奇が崩れない
                if (a[1] <= scan) == (b[1] <= scan) {
                    continue;
                }
                let t = (scan - a[1]) / (b[1] - a[1]);
                crossings.push(a[0] + t * (b[0] - a[0]));
            }
            crossings.sort_by(f64::total_cmp);
            for span in crossings.chunks(2) {
                let [left, right] = span else { continue };
                // 画素 x は中心 x+0.5 が [left, right) に入るとき内側。
                // 終端は開区間で持つ。画像の幅で切り詰めるのがそのまま
                // 「はみ出した分を捨てる」ことになる
                let from = (left - 0.5).ceil().max(0.0) as u32;
                let to = ((right - 0.5).ceil().max(0.0) as u32).min(self.width);
                for x in from..to {
                    let index = self.index(x, y);
                    self.mark_index(index, kind);
                    marked += 1;
                }
            }
        }
        marked
    }

    /// 半径 `radius` の円板へ印を付ける。`--fg-seed` の保護円がこれ。
    ///
    /// 印を付けた画素数を返す。画像の外にある種は 0 になる（`--fg-seed` の
    /// 「範囲外は無視」と同じ扱い）。
    pub fn mark_disc(&mut self, cx: u32, cy: u32, radius: u32, kind: Constraint) -> u64 {
        let (w, h) = (self.width, self.height);
        let mut marked = 0u64;
        disc_pixels(w, h, cx, cy, radius, |index| {
            self.mark_index(index, kind);
            marked += 1;
        });
        marked
    }
}

/// 寸法 `from` の座標 `v` を、寸法 `to` の格子で最も近い座標へ写す。
///
/// 画素の中心どうしを合わせる（`+0.5` して比を掛け、`-0.5` で戻す）。左上を
/// そのまま掛ける素朴な式だと、縮小のたびに半画素ずつ左上へ寄る。
///
/// **確定領域を寸法の間で往き来させる道はこの 1 本だけにしておく。**
/// `segment::grid_index`（確率マップ → 原寸）も `Constraints::resampled`
/// （原寸 → 探索段）もここを呼ぶ。式が 2 本に分かれると、片方だけ直した
/// ときに同じ指示が経路によって半画素ずれる。
///
/// `to` が 0 のときは 0 を返す。呼び出し側は寸法 0 を先に断っているが、
/// 引き算が build の種類で panic したりラップしたりするのは避ける。
pub(crate) fn nearest(v: u32, from: u32, to: u32) -> u32 {
    if from == 0 || to == 0 {
        return 0;
    }
    let mapped = ((f64::from(v) + 0.5) * f64::from(to) / f64::from(from) - 0.5).round();
    (mapped.max(0.0) as u32).min(to - 1)
}

/// 円板に含まれる画素の添字を渡す。
///
/// **`floodfill::protected_pixels` と `Constraints::mark_disc` が同じ円を
/// 描くために切り出してある。** `--fg-seed` は保護円としても制約としても
/// 効くので、2 つの実装が 1px でも食い違えば、報告される `fg_ratio` と
/// 実際に守られた画素が別のものになる。
pub(crate) fn disc_pixels(
    width: u32,
    height: u32,
    cx: u32,
    cy: u32,
    radius: u32,
    mut mark: impl FnMut(usize),
) {
    if cx >= width || cy >= height {
        return;
    }
    let r = i64::from(radius);
    for dy in -r..=r {
        for dx in -r..=r {
            if dx * dx + dy * dy > r * r {
                continue;
            }
            let (x, y) = (i64::from(cx) + dx, i64::from(cy) + dy);
            if x < 0 || y < 0 || x >= i64::from(width) || y >= i64::from(height) {
                continue;
            }
            mark((y as usize) * (width as usize) + (x as usize));
        }
    }
}

/// Rec.709 の輝度。`edges.rs` の堤防と同じ係数で測る。
///
/// マスクを「1 チャンネルのグレー」として読むための唯一の窓口である。
/// グレー PNG なら 3 チャンネルが同じ値なので、輝度はその値そのものになる。
fn luma(p: [u8; 4]) -> u8 {
    let v = 0.2126 * f32::from(p[0]) + 0.7152 * f32::from(p[1]) + 0.0722 * f32::from(p[2]);
    v.round().clamp(0.0, 255.0) as u8
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::Rgba;

    fn gray(w: u32, h: u32, v: u8) -> RgbaImage {
        RgbaImage::from_pixel(w, h, Rgba([v, v, v, 255]))
    }

    /// マスクの内側を数える。走査線充填の検査で使う
    fn marked(c: &Constraints, kind: Constraint) -> Vec<(u32, u32)> {
        let mut out = Vec::new();
        for y in 0..c.height() {
            for x in 0..c.width() {
                if c.at(x, y) == kind {
                    out.push((x, y));
                }
            }
        }
        out
    }

    #[test]
    fn a_trimap_splits_into_three_by_luminance() {
        let mut c = Constraints::new(3, 1);
        let mut image = gray(3, 1, 0);
        image.put_pixel(0, 0, Rgba([255, 255, 255, 255]));
        image.put_pixel(1, 0, Rgba([128, 128, 128, 255]));
        image.put_pixel(2, 0, Rgba([0, 0, 0, 255]));
        c.mark_by_luma(&image, |l| {
            if l >= TRIMAP_FOREGROUND {
                Some(Constraint::ForcedFg)
            } else if l <= TRIMAP_BACKGROUND {
                Some(Constraint::ForcedBg)
            } else {
                None
            }
        });
        assert_eq!(c.at(0, 0), Constraint::ForcedFg);
        assert_eq!(c.at(1, 0), Constraint::Free, "不明帯は何も強制しない");
        assert_eq!(c.at(2, 0), Constraint::ForcedBg);
    }

    /// **アルファは見ない。** 透明な白はマスクとして「白」である。
    #[test]
    fn the_alpha_channel_is_ignored() {
        let mut c = Constraints::new(1, 1);
        let image = RgbaImage::from_pixel(1, 1, Rgba([255, 255, 255, 0]));
        c.mark_by_luma(&image, |l| (l != 0).then_some(Constraint::ForcedFg));
        assert_eq!(c.at(0, 0), Constraint::ForcedFg);
    }

    #[test]
    fn a_mask_of_the_wrong_size_changes_nothing() {
        let mut c = Constraints::new(4, 4);
        c.mark_by_luma(&gray(3, 4, 255), |l| {
            (l != 0).then_some(Constraint::ForcedFg)
        });
        assert!(c.is_empty(), "寸法違いを黙って当てはめてはいけない");
    }

    #[test]
    fn a_convex_polygon_fills_its_interior() {
        let mut c = Constraints::new(10, 10);
        c.fill_polygon(
            &[[2.0, 2.0], [6.0, 2.0], [6.0, 5.0], [2.0, 5.0]],
            Constraint::ForcedFg,
        );
        // 画素中心が矩形に入るのは 2..=5 x 2..=4
        for y in 2..=4 {
            for x in 2..=5 {
                assert_eq!(c.at(x, y), Constraint::ForcedFg, "({x},{y}) が抜けている");
            }
        }
        assert_eq!(c.at(1, 3), Constraint::Free);
        assert_eq!(c.at(6, 3), Constraint::Free);
        assert_eq!(c.at(3, 5), Constraint::Free);
    }

    /// 凹んだ形でも、凹みの中は内側にならない。
    #[test]
    fn a_concave_polygon_keeps_its_notch_outside() {
        // コの字（右側が開いている）
        let mut c = Constraints::new(12, 12);
        c.fill_polygon(
            &[
                [1.0, 1.0],
                [9.0, 1.0],
                [9.0, 3.0],
                [4.0, 3.0],
                [4.0, 7.0],
                [9.0, 7.0],
                [9.0, 9.0],
                [1.0, 9.0],
            ],
            Constraint::ForcedBg,
        );
        assert_eq!(c.at(6, 5), Constraint::Free, "凹みの中が塗られている");
        assert_eq!(c.at(2, 5), Constraint::ForcedBg, "背の部分が抜けている");
        assert_eq!(c.at(6, 2), Constraint::ForcedBg, "上の腕が抜けている");
        assert_eq!(c.at(6, 8), Constraint::ForcedBg, "下の腕が抜けている");
    }

    /// 自己交差は偶奇のまま扱う。凝ったことをしないので結果は予測できる。
    #[test]
    fn a_self_intersecting_polygon_follows_the_even_odd_rule() {
        // 砂時計。2 本の対角線が中央で交わり、上下の三角形だけが内側になる
        let mut c = Constraints::new(20, 12);
        c.fill_polygon(
            &[[2.0, 2.0], [17.0, 9.0], [2.0, 9.0], [17.0, 2.0]],
            Constraint::ForcedFg,
        );
        assert_eq!(c.at(9, 3), Constraint::ForcedFg, "上の三角が抜けている");
        assert_eq!(c.at(9, 8), Constraint::ForcedFg, "下の三角が抜けている");
        assert_eq!(c.at(3, 5), Constraint::Free, "左の隙間が塗られている");
        assert_eq!(c.at(16, 5), Constraint::Free, "右の隙間が塗られている");
    }

    /// 画像の外へ出る頂点があっても、面ごと捨てない。
    #[test]
    fn a_polygon_reaching_outside_the_image_keeps_the_part_inside() {
        let mut c = Constraints::new(8, 8);
        c.fill_polygon(
            &[[-50.0, -50.0], [4.0, -50.0], [4.0, 4.0], [-50.0, 4.0]],
            Constraint::ForcedFg,
        );
        assert_eq!(c.at(0, 0), Constraint::ForcedFg, "画像内の部分が抜けている");
        assert_eq!(c.at(3, 3), Constraint::ForcedFg);
        assert_eq!(c.at(4, 4), Constraint::Free, "外側まで塗っている");
        // 走査線が画像の外にしか無い場合でも panic しない
        c.fill_polygon(
            &[[100.0, 100.0], [200.0, 100.0], [150.0, 200.0]],
            Constraint::ForcedBg,
        );
        assert!(marked(&c, Constraint::ForcedBg).is_empty());
    }

    #[test]
    fn a_degenerate_polygon_marks_nothing() {
        let mut c = Constraints::new(8, 8);
        c.fill_polygon(&[[1.0, 1.0], [5.0, 5.0]], Constraint::ForcedFg);
        assert!(c.is_empty(), "2 点では面にならない");
    }

    #[test]
    fn overlapping_foreground_and_background_is_a_conflict() {
        let mut c = Constraints::new(10, 10);
        // 画素中心で見て 1..=5 の正方形
        c.fill_polygon(
            &[[1.0, 1.0], [6.0, 1.0], [6.0, 6.0], [1.0, 6.0]],
            Constraint::ForcedFg,
        );
        assert_eq!(c.conflict(), None, "重なっていない段階で衝突と言っている");
        // 同じく 4..=8。重なるのは 4..=5 の 4 画素
        c.fill_polygon(
            &[[4.0, 4.0], [9.0, 4.0], [9.0, 9.0], [4.0, 9.0]],
            Constraint::ForcedBg,
        );
        let conflict = c.conflict().expect("重なりを見落としている");
        assert_eq!(conflict.count, 4);
        assert_eq!(conflict.bbox, (4, 4, 5, 5));
    }

    #[test]
    fn the_ratios_add_up_to_one() {
        let mut c = Constraints::new(10, 10);
        c.fill_polygon(
            &[[0.0, 0.0], [10.0, 0.0], [10.0, 2.0], [0.0, 2.0]],
            Constraint::ForcedFg,
        );
        c.fill_polygon(
            &[[0.0, 5.0], [10.0, 5.0], [10.0, 10.0], [0.0, 10.0]],
            Constraint::ForcedBg,
        );
        let (fg, bg, unknown) = c.ratios();
        assert_eq!(fg, 0.20);
        assert_eq!(bg, 0.50);
        assert_eq!(unknown, 0.30);
    }

    #[test]
    fn the_sources_are_listed_once_in_the_order_they_were_given() {
        let mut c = Constraints::new(4, 4);
        c.note(ConstraintSource::Trimap);
        c.note(ConstraintSource::FgPolygon);
        c.note(ConstraintSource::Trimap);
        let names: Vec<&str> = c.sources().iter().map(|s| s.as_str()).collect();
        assert_eq!(names, ["trimap", "fg_polygon"]);
    }

    /// **利用者の指示はモデルの提案に勝つ。** 衝突はエラーにならない。
    #[test]
    fn an_overlay_lets_the_later_instruction_win() {
        let mut model = Constraints::new(4, 1);
        model.note(ConstraintSource::Segment);
        for x in 0..4 {
            model.mark(x, 0, Constraint::ForcedBg);
        }
        let mut user = Constraints::new(4, 1);
        user.note(ConstraintSource::FgPolygon);
        user.mark(1, 0, Constraint::ForcedFg);

        model.overlay(&user);
        assert_eq!(
            model.at(0, 0),
            Constraint::ForcedBg,
            "触っていない画素が動いた"
        );
        assert_eq!(
            model.at(1, 0),
            Constraint::ForcedFg,
            "利用者の指示が勝っていない"
        );
        assert_eq!(
            model.conflict(),
            None,
            "上書きしたのに衝突として残っている（ビットを OR している）"
        );
        assert_eq!(
            model
                .sources()
                .iter()
                .map(|s| s.as_str())
                .collect::<Vec<_>>(),
            ["segment", "fg_polygon"]
        );
    }

    /// 寸法が違う指示は重ねない（`mark_by_luma` と同じ規約）。
    #[test]
    fn an_overlay_of_the_wrong_size_changes_nothing() {
        let mut base = Constraints::new(4, 4);
        base.mark(0, 0, Constraint::ForcedFg);
        let mut other = Constraints::new(2, 2);
        other.mark(0, 0, Constraint::ForcedBg);
        base.overlay(&other);
        assert_eq!(base.at(0, 0), Constraint::ForcedFg);
    }

    #[test]
    fn a_disc_is_round_and_clipped_to_the_image() {
        let mut c = Constraints::new(10, 10);
        c.mark_disc(1, 1, 3, Constraint::ForcedFg);
        assert_eq!(c.at(1, 1), Constraint::ForcedFg);
        assert_eq!(c.at(4, 1), Constraint::ForcedFg, "真横 3px は円の内側");
        assert_eq!(c.at(4, 4), Constraint::Free, "斜め 3px は円の外側");
        // 画像の外へ出る分は落ちるだけで、panic しない
        c.mark_disc(9, 9, 5, Constraint::ForcedFg);
        assert_eq!(c.at(9, 9), Constraint::ForcedFg);
    }

    /// 縮めても**確定前景は確定前景のまま**であること。
    ///
    /// `--optimize` の探索段はこれで指示を運ぶ。ここが意味を変えると、
    /// トライマップを渡した画像で探索だけが別の世界を見ることになる。
    #[test]
    fn a_resampled_instruction_keeps_its_meaning() {
        let mut c = Constraints::new(40, 20);
        for y in 4..16 {
            for x in 8..32 {
                c.mark(x, y, Constraint::ForcedFg);
            }
        }
        // 角の確定背景は 2x2 で置く。1px では**縮小で消えうる**
        // （`a_hairline_instruction_can_vanish_when_resampled` を参照）
        for (x, y) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
            c.mark(x, y, Constraint::ForcedBg);
        }
        c.note(ConstraintSource::Trimap);

        let small = c.resampled(20, 10);
        assert_eq!((small.width(), small.height()), (20, 10));
        assert_eq!(
            small.sources(),
            &[ConstraintSource::Trimap],
            "入口の名前は引き継ぐ"
        );
        // 塊の中は確定前景のまま。外は自由のまま
        assert_eq!(small.at(10, 5), Constraint::ForcedFg);
        assert_eq!(small.at(18, 8), Constraint::Free);
        assert_eq!(small.at(0, 0), Constraint::ForcedBg);
        // 面積はおおむね比で縮む（最近傍なので 1px の増減はありうる）
        let (fg, bg) = small.counts();
        assert!((66..=78).contains(&fg), "確定前景が縮小で消えている: {fg}");
        assert_eq!(bg, 1);
    }

    /// 寸法 0 は panic せず、空の表を返す。
    ///
    /// `Constraints::new(0, 0)` は `flags` が空になる。添字を引く前に
    /// 断っていなければ、**縮小の内側で範囲外参照になる。**
    #[test]
    fn a_resample_to_or_from_zero_never_panics() {
        let mut c = Constraints::new(4, 4);
        c.mark(1, 1, Constraint::ForcedFg);
        for (w, h) in [(0, 4), (4, 0), (0, 0)] {
            let out = c.resampled(w, h);
            assert_eq!((out.width(), out.height()), (w, h));
            assert_eq!(out.counts(), (0, 0));
        }
        assert_eq!(Constraints::new(0, 0).resampled(4, 4).counts(), (0, 0));
    }

    /// **細い指示は間引かれ切ることがある。** 最近傍は行と列を捨てるので、
    /// 1px の線が縮小版から丸ごと消えうる。
    ///
    /// これは探索段だけの話である（最終段は原寸の指示そのままで回る）が、
    /// 「指示を渡したのに候補の順位が指示無しと同じ」という形でしか現れない
    /// ので、起こりうることをここに固定しておく。
    #[test]
    fn a_hairline_instruction_can_vanish_when_resampled() {
        let mut c = Constraints::new(100, 4);
        for y in 0..4 {
            c.mark(1, y, Constraint::ForcedFg);
        }
        assert_eq!(c.counts().0, 4);
        // 10 分の 1 では x=1 を代表する列が無くなる（0 と 10 が選ばれる）
        assert_eq!(c.resampled(10, 4).counts().0, 0);
        assert!(c.resampled(10, 4).is_empty());
    }

    /// 最近傍の式は画素の**中心**どうしを合わせ、端で飽和する。
    ///
    /// **`segment::grid_index` と同じ式でなければならない。** 2 本に分かれると、
    /// 同じ指示が経路によって半画素ずれる。
    #[test]
    fn the_nearest_mapping_matches_pixel_centres() {
        // 4 幅を 2 幅へ。中心 0.5/1.5/2.5/3.5 が 0.25/0.75/1.25/1.75 に落ちる
        assert_eq!(
            (0..4).map(|v| nearest(v, 4, 2)).collect::<Vec<_>>(),
            vec![0, 0, 1, 1]
        );
        // 2 幅を 4 幅へ。中心はちょうど画素の境目に来るので、round の規約
        // （0.5 は 0 から遠い側）でどちらへ倒れるかが決まる
        assert_eq!(
            (0..2).map(|v| nearest(v, 2, 4)).collect::<Vec<_>>(),
            vec![1, 3]
        );
        // 端は飽和する
        assert_eq!(nearest(99, 100, 10), 9);
        assert_eq!(nearest(0, 100, 10), 0);
        // 寸法 0 は 0 を返す。**引き算が build の種類で panic したり
        // ラップしたりしてはいけない**
        assert_eq!(nearest(5, 0, 10), 0);
        assert_eq!(nearest(5, 10, 0), 0);
    }
}
