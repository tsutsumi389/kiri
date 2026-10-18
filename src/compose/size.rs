//! `kiri compose --size-range` — **文字の層ごとに、収まって読める `size` の範囲を返す。**
//!
//! compose は縮めない、折り返さない。収まらない文字は `TEXT_OVERFLOW` が上から、
//! 縮小で読めない文字は `too_small` が下から `size` を押さえるが、**両方を同時に
//! 満たす値は誰も言わない**（15-phase-34.md）。ここは両側の境目を整数で返す。
//! **kiri は `size` を書き換えない**——書くのは spec である。
//!
//! # 上限：線形に見積もり、組んで確かめる
//!
//! `size` 以外を固定すると、`placed` の 4 辺は `size` の 1 次式になる（行の送りは
//! advance の和で、usvg は輪郭をヒンティングしない。縦は ascent / descent と
//! 行送りの倍数で、縁の幅は `size` に依らない定数項である。§15.2.2）。そこで
//!
//! 1. spec の `size`（`s0`）の `placed` は組んだ結果をそのまま使い、`s1` で
//!    幾何だけを組み直す（`text::placed_at`。ラスタライズはしない）
//! 2. 辺ごとの はみ出し量 `o(s) = a + b × s` を 2 点から解き、
//!    `o(s) ≤ SUBPIXEL_TOLERANCE` となる最大の `s` の最小を切り捨てて候補 `c` とする
//! 3. **`c` と `c + 1` を組んで確かめる。** `c` が収まり `c + 1` がはみ出せば
//!    それが答えで、外れていれば 1 ずつ `STEPS` 回まで歩き、それでも決まらなければ
//!    二分探索に落ちる（f32 の端数や字体の癖で直線から外れたときの保険）
//!
//! **最後に必ず組んで確かめる**ので、正しさは二分探索と同じで、普通の層では
//! 組み直しが 3 回で済む。`c + 1` まで確かめるのは、返した `max` に 1 を足すと
//! 必ずはみ出す、という約束（§15.4 の 2）を見積もりの端数に預けないためである
//! （§15.7 の 1）。
//!
//! # 下限は `display::required_size` そのもの
//!
//! `TEXT_ILLEGIBLE_AT_DISPLAY` の `too_small` の hint が言う N と、同じ式・同じ
//! 丸めで一致させる（§15.2.3）。最も狭い測る表示の N が、全表示で届く最小になる。

use std::sync::Arc;

use resvg::usvg;
use serde::Serialize;

use crate::compose::display;
use crate::compose::measure::{self, SUBPIXEL_TOLERANCE};
use crate::compose::{Canvas, MIN_FONT_SIZE, TextLayer};
use crate::error::Result;

/// 2 点目の `size` の下駄(px)。**`s1 = max(S1_FACTOR × s0, S1_FLOOR)`**（§15.6 の 7）。
///
/// 小さい `size` で 2 点を近く取ると、f32 の端数（`SUBPIXEL_TOLERANCE` の doc）が
/// 傾きに対して相対的に大きくなる。2 倍より遠くへ取っても見積もりは良くならず、
/// 組む文字が大きくなるだけである（§15.7 の 2 に実測）。
pub const S1_FLOOR: f64 = 64.0;

/// 2 点目の `size` の倍率。**`s1 = max(S1_FACTOR × s0, S1_FLOOR)`**。
/// `kiri schema` の notes もこの値を差し込む。
pub const S1_FACTOR: f64 = 2.0;

/// 見積もりが外れたとき、1 ずつ歩いて確かめる回数。超えたら二分探索へ落ちる。
pub const STEPS: u32 = 3;

/// 1 層あたりの組み直しの最悪回数。**見積もりが最も低く外れたとき**で、
/// `s1`・`c`・上へ歩く `STEPS + 1` 回・倍々に上げる 14 回（5 → 65536）・
/// 二分探索 15 回（40960〜65536 の間）。普通の層は 3 回である（§15.7 の 9）。
pub const WORST_LAYOUTS: u32 = 35;

/// 傾きがこれ以下の辺は、`size` を大きくしても はみ出しが増えないと見る(px / size)。
///
/// 0 で切ると、動かないはずの辺（`end` 揃えの右端）の f32 の揺れ（1e-6 程度）を
/// 傾きと読み、上限を 1 万倍先に見積もる。実の傾きは 1 文字でも 0.1 を下回らない。
/// 見積もりを外しても確かめで拾うので、ここは正しさではなく回数にだけ効く。
const MIN_SLOPE: f64 = 1e-3;

/// 上へ探す天井。**これを組んでも収まるなら「上限が無い」と言う。**
///
/// `rect` の辺は `MAX_CANVAS_SIDE`（16384）までで、字が 1 つでも組める行は
/// 行の天地が `size` 以上あるので、収まる `size` もそれを超えない。4 倍の余裕を
/// 置き、ここまで収まるのは字が 1 つも組めなかった層だけにする。
const SEARCH_CEILING: u32 = 4 * crate::compose::MAX_CANVAS_SIDE;

/// 1 つの文字の層の `size_range`。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SizeRange {
    /// `--profile` の最も狭い測る表示で、表示上 `MIN_DISPLAY_SIZE` に届く最小の
    /// 整数 `size`。`--profile` が無いか、測る表示が 1 つも無ければ `None`
    pub min: Option<u32>,
    /// `text_overflow` が 0 になる最大の整数 `size`。収まる整数が無いか、
    /// 上限が無ければ `None`（`limited_by` で分ける）
    pub max: Option<u32>,
    /// 上限を決めた向き。収まる整数が無ければ `None`
    pub limited_by: Option<LimitedBy>,
}

/// 上限を決めた向き。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum LimitedBy {
    /// 左右の辺（最も長い行の幅）
    Width,
    /// 上下の辺（行の積み上げ）
    Height,
    /// どれだけ大きくしても はみ出さない（字が 1 つも組めない層など）
    None,
}

/// 1 つの文字の層の範囲を求める。
///
/// `placed` は spec の `size` で組んだ外接矩形（`layers[].placed`）で、見積もりの
/// 1 点目に使う——同じ SVG から組むので、組み直した値とビットまで一致する。
/// `narrowest` は `--profile` の測る表示のうち最も狭い幅（無ければ `None`）。
pub fn range(
    layer: &TextLayer,
    canvas: &Canvas,
    family: &str,
    db: &Arc<usvg::fontdb::Database>,
    placed: [f64; 4],
    narrowest: Option<u32>,
) -> Result<SizeRange> {
    let floor = layer
        .stroke
        .as_ref()
        .map_or(MIN_FONT_SIZE, |s| s.width.max(MIN_FONT_SIZE));
    let upper = upper(layer.rect, layer.size, placed, floor, estimate, |size| {
        crate::compose::text::placed_at(layer, size, canvas, family, db)
    })?;
    Ok(SizeRange {
        min: narrowest.map(|w| lower(canvas.width, w)),
        max: upper.max,
        limited_by: upper.limited_by,
    })
}

/// 下限。`display::required_size` を整数にしただけである。
pub fn lower(canvas_w: u32, narrowest: u32) -> u32 {
    // `required_size` は切り上げた整数を f64 で返す。`11 × 16384 / 1` でも
    // u32 に収まる
    display::required_size(canvas_w, narrowest) as u32
}

/// 上限を求めた結果。`route` はテストが経路を確かめるためにだけ持つ。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Upper {
    max: Option<u32>,
    limited_by: Option<LimitedBy>,
    route: Route,
}

/// どの経路で決まったか。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Route {
    /// 見積もり `c` が収まり、`c + 1` がはみ出した
    Estimate,
    /// 1 ずつ歩いて決まった
    Stepped,
    /// 二分探索に落ちた
    Searched,
    /// 組み直すまでもなく決まった（上限が無い）
    Unbounded,
}

/// 見積もりの関数の形。`(rect, s0, placed(s0), s1, placed(s1))` から、
/// 候補の実数 `size` を返す。どの辺も効かなければ `None`。
type Estimator = fn([f64; 4], f64, [f64; 4], f64, [f64; 4]) -> Option<f64>;

/// 2 点から辺ごとの 1 次式を解き、`o(s) ≤ SUBPIXEL_TOLERANCE` を満たす最大の `s` の
/// 最小を返す（§15.2.2 の 2〜3）。
fn estimate(rect: [f64; 4], s0: f64, p0: [f64; 4], s1: f64, p1: [f64; 4]) -> Option<f64> {
    let (e0, e1) = (measure::edges(rect, p0), measure::edges(rect, p1));
    (0..4)
        .filter_map(|k| {
            let b = (e1[k] - e0[k]) / (s1 - s0);
            (b > MIN_SLOPE).then(|| {
                let a = e0[k] - b * s0;
                (SUBPIXEL_TOLERANCE - a) / b
            })
        })
        .reduce(f64::min)
}

/// 上限を求める。`place` は `size` を差し替えて組んだ外接矩形を返す。
///
/// **見積もりの関数を引数に取る**のは、外れて二分探索へ落ちる経路を、字体に
/// 依らずテストで通すためである（§15.4 の 9）。
fn upper(
    rect: [f64; 4],
    s0: f64,
    p0: [f64; 4],
    floor: f64,
    estimator: Estimator,
    place: impl FnMut(f64) -> Result<[f64; 4]>,
) -> Result<Upper> {
    // **書ける最小の整数。** `stroke.width ≤ size` は `compose::validate` の検査で、
    // それを破る `size` は spec に書けない（§15.2.2 の 5）。天井を超える縁
    // （`size` 100000 に縁 70000 など）の層は、天井の内側に書ける整数が無い
    let lowest = (floor.ceil().min(f64::from(u32::MAX)) as u32).max(1);
    if lowest > SEARCH_CEILING {
        return Ok(Upper::none_fits());
    }
    let s1 = (S1_FACTOR * s0).max(S1_FLOOR);
    let mut probe = Probe {
        rect,
        place,
        seen: Vec::new(),
    };
    let p1 = (probe.place)(s1)?;
    // 組んだ 2 点は、整数なら覚えておく——確かめで同じ `size` を組み直さない
    for (s, p) in [(s0, p0), (s1, p1)] {
        if s.fract() == 0.0 && s <= f64::from(SEARCH_CEILING) {
            probe.seen.push((s as u32, p));
        }
    }
    let fits = |p: [f64; 4]| measure::overflow(rect, p) == 0.0;
    let candidate = match estimator(rect, s0, p0, s1, p1) {
        // **NaN は候補にしない。** `as u32` が 0 に落ち、`max: 0` を返しうる。
        // spec の `size` から探して確かめる
        Some(s) if s.is_nan() => s0,
        Some(s) => s,
        // **どの辺も伸びない。** 2 点とも収まっていれば、どれだけ大きくしても
        // はみ出さない（§15.6 の 4）。収まっていなければ定数項がはみ出しており、
        // 探して確かめる
        None if fits(p0) && fits(p1) => {
            return Ok(Upper {
                max: None,
                limited_by: Some(LimitedBy::None),
                route: Route::Unbounded,
            });
        }
        None => s0,
    };
    // `lowest ≤ SEARCH_CEILING` は上で確かめてある。cast の後にも `lowest` で
    // 押さえ、候補が何であれ書けない整数から歩き始めない
    let c = (candidate
        .floor()
        .clamp(f64::from(lowest), f64::from(SEARCH_CEILING)) as u32)
        .max(lowest);
    probe.settle(c, lowest)
}

/// 整数の `size` ごとに組んで確かめる器。同じ `size` を 2 度組まない。
struct Probe<F> {
    rect: [f64; 4],
    place: F,
    seen: Vec<(u32, [f64; 4])>,
}

impl<F: FnMut(f64) -> Result<[f64; 4]>> Probe<F> {
    fn at(&mut self, size: u32) -> Result<[f64; 4]> {
        if let Some((_, p)) = self.seen.iter().find(|(s, _)| *s == size) {
            return Ok(*p);
        }
        let p = (self.place)(f64::from(size))?;
        self.seen.push((size, p));
        Ok(p)
    }

    fn fits(&mut self, size: u32) -> Result<bool> {
        let p = self.at(size)?;
        Ok(measure::overflow(self.rect, p) == 0.0)
    }

    /// 候補 `c` の近くで、収まる最大の整数を決める。**大きくするほど はみ出しは
    /// 減らない**（辺が `size` の 1 次式で、傾きが正の辺だけが効く）ことを前提に
    /// 二分探索する。
    fn settle(&mut self, mut c: u32, lowest: u32) -> Result<Upper> {
        if self.fits(c)? {
            // 上へ歩く。`c + 1` が はみ出せば見積もりどおり
            for step in 0..=STEPS {
                if c >= SEARCH_CEILING {
                    return Ok(Upper {
                        max: None,
                        limited_by: Some(LimitedBy::None),
                        route: Route::Searched,
                    });
                }
                if !self.fits(c + 1)? {
                    let route = if step == 0 {
                        Route::Estimate
                    } else {
                        Route::Stepped
                    };
                    return self.done(c, route);
                }
                c += 1;
            }
            // 見積もりが低すぎた。倍々に上げて はみ出す点を掴んでから詰める
            let mut lo = c;
            let mut hi = c;
            loop {
                hi = hi.saturating_mul(2).min(SEARCH_CEILING);
                if !self.fits(hi)? {
                    break;
                }
                if hi == SEARCH_CEILING {
                    return Ok(Upper {
                        max: None,
                        limited_by: Some(LimitedBy::None),
                        route: Route::Searched,
                    });
                }
                lo = hi;
            }
            self.search(lo, hi)
        } else {
            // 下へ歩く
            for _ in 0..STEPS {
                if c <= lowest {
                    return Ok(Upper::none_fits());
                }
                c -= 1;
                if self.fits(c)? {
                    return self.done(c, Route::Stepped);
                }
            }
            if c <= lowest || !self.fits(lowest)? {
                return Ok(Upper::none_fits());
            }
            self.search(lowest, c)
        }
    }

    /// `lo` は収まり、`hi` は はみ出す。間を詰める。
    fn search(&mut self, mut lo: u32, mut hi: u32) -> Result<Upper> {
        while hi - lo > 1 {
            let mid = lo + (hi - lo) / 2;
            if self.fits(mid)? {
                lo = mid;
            } else {
                hi = mid;
            }
        }
        self.done(lo, Route::Searched)
    }

    /// `max` で決まった。**向きは `max + 1` で最も大きく はみ出した辺から言う**
    /// ——見積もりで効いた辺ではなく、組んで確かめた辺である。
    fn done(&mut self, max: u32, route: Route) -> Result<Upper> {
        let over = self.at(max + 1)?;
        let e = measure::edges(self.rect, over);
        // [右, 下, 左, 上]。同じなら先の辺（幅）を採る
        let worst = (0..4).fold(0, |best, k| if e[k] > e[best] { k } else { best });
        let limited_by = if worst % 2 == 0 {
            LimitedBy::Width
        } else {
            LimitedBy::Height
        };
        Ok(Upper {
            max: Some(max),
            limited_by: Some(limited_by),
            route,
        })
    }
}

impl Upper {
    /// 収まる整数が 1 つも無い。`max` も `limited_by` も null
    fn none_fits() -> Upper {
        Upper {
            max: None,
            limited_by: None,
            route: Route::Searched,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `start` 揃えの 1 行を真似た幾何。幅は `w × s`、高さは `h × s`、縁は定数。
    fn linear(w: f64, h: f64, stroke: f64) -> impl Fn(f64) -> [f64; 4] {
        move |s| [10.0, 20.0, w * s + 2.0 * stroke, h * s + 2.0 * stroke]
    }

    const RECT: [f64; 4] = [10.0, 20.0, 400.0, 100.0];

    fn run(
        place: impl Fn(f64) -> [f64; 4],
        s0: f64,
        floor: f64,
        estimator: Estimator,
        calls: &mut u32,
    ) -> Upper {
        let p0 = place(s0);
        upper(RECT, s0, p0, floor, estimator, |s| {
            *calls += 1;
            Ok(place(s))
        })
        .unwrap()
    }

    /// 直線どおりなら見積もりで決まり、組み直しは `s1`・`c`・`c + 1` の 3 回。
    #[test]
    fn a_linear_layer_is_settled_by_the_estimate_in_three_layouts() {
        let mut calls = 0;
        // 幅 2.5 × s ≤ 400 → 160、高さ 1.2 × s ≤ 100 → 83.33
        let up = run(linear(2.5, 1.2, 0.0), 40.0, 1.0, estimate, &mut calls);
        assert_eq!(up.max, Some(83));
        assert_eq!(up.limited_by, Some(LimitedBy::Height));
        assert_eq!(up.route, Route::Estimate);
        assert_eq!(calls, 3);
    }

    /// 縁の幅は定数項として効き、上限を下げる。
    #[test]
    fn a_stroke_lowers_the_limit_by_its_constant_term() {
        let mut calls = 0;
        // 幅 (400 - 12) / 5 = 77.6 → 77。高さ (100 - 12) / 0.9 = 97.7
        let up = run(linear(5.0, 0.9, 6.0), 40.0, 6.0, estimate, &mut calls);
        assert_eq!(up.max, Some(77));
        assert_eq!(up.limited_by, Some(LimitedBy::Width));
    }

    /// **見積もりが外れても、組んで確かめた値を返す**（§15.4 の 9）。見積もりを
    /// 遠くへ外す関数へ差し替え、上下どちらへ外れても二分探索で正しい値に着く。
    #[test]
    fn a_wrong_estimate_falls_back_to_the_binary_search() {
        fn too_high(_: [f64; 4], _: f64, _: [f64; 4], _: f64, _: [f64; 4]) -> Option<f64> {
            Some(5000.0)
        }
        fn too_low(_: [f64; 4], _: f64, _: [f64; 4], _: f64, _: [f64; 4]) -> Option<f64> {
            Some(2.0)
        }
        for estimator in [too_high as Estimator, too_low] {
            let mut calls = 0;
            let up = run(linear(2.5, 1.2, 0.0), 40.0, 1.0, estimator, &mut calls);
            assert_eq!(up.max, Some(83));
            assert_eq!(up.route, Route::Searched);
            assert_eq!(up.limited_by, Some(LimitedBy::Height));
        }
    }

    /// 直線から外れる幾何（高さが `size` の 2 乗で伸びる）でも、正しい値に着く。
    #[test]
    fn a_curved_layer_is_still_answered_exactly() {
        let mut calls = 0;
        // 高さ s² / 50 ≤ 100 → s ≤ 70.7。2 点（40, 80）の直線は 62.2 と見積もる
        let up = run(
            |s| [10.0, 20.0, s, s * s / 50.0],
            40.0,
            1.0,
            estimate,
            &mut calls,
        );
        assert_eq!(up.max, Some(70));
        assert_ne!(up.route, Route::Estimate);
    }

    /// 字が 1 つも組めず `placed` が潰れた層は、上限が無い。
    #[test]
    fn a_collapsed_layer_has_no_upper_limit() {
        let mut calls = 0;
        let up = run(|_| [10.0, 20.0, 0.0, 0.0], 40.0, 1.0, estimate, &mut calls);
        assert_eq!(up.max, None);
        assert_eq!(up.limited_by, Some(LimitedBy::None));
        assert_eq!(calls, 1);
    }

    /// `stroke.width`（`floor`）を下回る値は返さない。1 も入らなければ null。
    #[test]
    fn nothing_below_the_floor_is_returned() {
        let mut calls = 0;
        // 高さ 1.2 × s + 2 × 6 ≤ 100 → 73。floor 80 なら入る整数が無い
        let up = run(linear(0.5, 1.2, 6.0), 90.0, 80.0, estimate, &mut calls);
        assert_eq!(up.max, None);
        assert_eq!(up.limited_by, None);
        // 縁が rect より太い（定数項だけで はみ出す）
        let up = run(linear(0.5, 0.5, 60.0), 90.0, 1.0, estimate, &mut calls);
        assert_eq!(up.max, None);
        assert_eq!(up.limited_by, None);
    }

    /// **天井を超える縁の層は、組む前に「入る整数が無い」と言う。** `lowest` が
    /// 天井より上だと `clamp` の下限が上限を超えて panic した（レビューの指摘）。
    #[test]
    fn a_stroke_wider_than_the_ceiling_fits_nothing() {
        let mut calls = 0;
        let up = run(
            linear(0.5, 0.5, 0.0),
            100_000.0,
            70_000.0,
            estimate,
            &mut calls,
        );
        assert_eq!(up.max, None);
        assert_eq!(up.limited_by, None);
        assert_eq!(calls, 0, "組み直す前に断る");
    }

    /// **NaN の見積もりは `max: 0` にならない。** spec の `size` から探す。
    #[test]
    fn a_nan_estimate_is_not_taken_as_zero() {
        fn nan(_: [f64; 4], _: f64, _: [f64; 4], _: f64, _: [f64; 4]) -> Option<f64> {
            Some(f64::NAN)
        }
        let mut calls = 0;
        let up = run(linear(2.5, 1.2, 0.0), 40.0, 1.0, nan, &mut calls);
        assert_eq!(up.max, Some(83));
    }

    /// 見積もりが最も低く外れても、組み直しは `WORST_LAYOUTS` 回に収まる。
    /// 真の上限を倍々の最後の区間（40960〜65536）に置き、二分探索を最も長くする。
    #[test]
    fn the_worst_case_stays_within_the_published_count() {
        fn lowest(_: [f64; 4], _: f64, _: [f64; 4], _: f64, _: [f64; 4]) -> Option<f64> {
            Some(1.0)
        }
        let mut calls = 0;
        // 高さ 0.002 × s ≤ 100 + 1/64 → 50007.8。spec の size は整数でない（覚えない）
        let up = run(linear(0.0, 0.002, 0.0), 40.5, 1.0, lowest, &mut calls);
        assert_eq!(up.max, Some(50_007));
        assert!(calls <= WORST_LAYOUTS, "{calls} 回");
        assert!(
            calls >= WORST_LAYOUTS - 2,
            "最悪の経路を通っていない: {calls} 回"
        );
    }

    /// spec の `size` が整数なら、その `placed` は組み直さない。
    #[test]
    fn the_spec_size_is_not_laid_out_again() {
        let mut calls = 0;
        // 見積もり c = 83、c + 1 = 84。spec の size を 84 にすれば c + 1 は覚えた値
        let up = run(linear(2.5, 1.2, 0.0), 84.0, 1.0, estimate, &mut calls);
        assert_eq!(up.max, Some(83));
        assert_eq!(calls, 2, "s1 と c だけを組む");
    }

    /// 下限は `too_small` の hint の N と同じ式で出る。
    #[test]
    fn the_lower_bound_is_the_too_small_hint() {
        // note の最も狭い表示は 144 幅。1280 幅なら ceil(11 × 1280 / 144) = 98
        assert_eq!(lower(1280, 144), 98);
        assert_eq!(
            f64::from(lower(1280, 144)),
            display::required_size(1280, 144)
        );
    }
}
