//! 悪い区間だけ境界を解き直す（`--edge-repair auto`）。
//!
//! 実写（白い不織布の上の黒いリモコン）では、1 回目の輪郭がリモコンの黒い縁より
//! 5〜10px 外を通り、その内側に布の織り目と落ち影が残る。1 回目のマスクから
//! **非対称の**トライマップ——確定背景は輪郭の少し内側から、確定前景はもっと
//! 内側から、そのあいだを不明帯——を作って 2 回目を切ると、真の縁は必ず帯の中に
//! あるので matting がそこを解き直す（手作業で `contour_roughness` 0.254 → 0.087）。
//!
//! これを kiri の中で、警告の `data.regions` が名指した区間の周りだけに掛ける。
//! 区間の外は 1 回目のマスクと画素をそのまま使い、継ぎ目は重みを滑らかに
//! 変えて混ぜるので段差が出ない。

use image::RgbaImage;

use crate::cutout::constraints::{Constraint, Constraints};
use crate::cutout::diagnostics::{
    CONTOUR_ROUGH_WARN, Diagnostics, RIM_CONTAMINATION_WARN, scale_at_1000,
};
use crate::cutout::mask::{FOREGROUND_THRESHOLD, Mask};
use crate::cutout::{
    BackgroundSeen, CutoutOptions, CutoutResult, collect_warnings, cutout_seen, judge,
};
use crate::warning::{Warning, WarningCode};

/// `--edge-repair` の指定。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, clap::ValueEnum)]
pub enum EdgeRepairMode {
    /// 解き直さない（既定）。成果物も結果 JSON も、この指定を足す前と 1 バイトも変わらない
    #[default]
    Off,
    /// 警告の区間だけ境界を解き直す
    Auto,
}

/// 帯の幅（どれも長辺 1000px 換算の px）。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Bands {
    /// 確定背景を 1 回目の輪郭の何 px 内側から取るか
    pub background_inset: f64,
    /// 確定前景を 1 回目の輪郭の何 px 内側から取るか
    pub foreground_inset: f64,
    /// 区間の矩形を外へ何 px 広げて解き直すか
    pub pad: f64,
    /// 継ぎ目で 1 回目と 2 回目を混ぜる幅
    pub blend: f64,
}

impl Default for Bands {
    fn default() -> Self {
        // 背景側は実写リモコン（長辺 5712px、換算 5.712 倍）で手作業が効いた 2px を
        // 換算したもの。前景側はベンチ 27 点の掃引で、欠けを 1 点も増やさない
        // 最も狭い幅。掃引は design.md 4.18
        Self {
            background_inset: 0.35,
            foreground_inset: 6.0,
            pad: 8.0,
            blend: 4.0,
        }
    }
}

/// 解き直す窓（`[x1, y1, x2, y2]`、両端を含む）。
pub type Window = (u32, u32, u32, u32);

/// 解き直しを見送る警告。**縁の位置ではなく切り抜きそのものが外れている**ことを言う。
///
/// どれも帯を解き直しても直らず、帯の手前で別の手（`--bbox`、`--tolerance`、
/// 撮り直し）が要る。`HALO_REMAINS` は「背景色のまま」の塊が帯の幅を超えて
/// 残っていることの現れで、ベンチで解き直しが悪化させた 2 点（S9 の輪郭誤差、
/// R3 assisted の商品の欠け）はどちらもこれを出していた。`SUBJECT_TOUCHES_EDGE` は
/// 本当の見切れでも出るが、ベンチで唯一これだけを出していた R3 既定は前景が
/// 画像の外周まで広がった壊れた切り抜きで、解き直すと輪郭の誤差が 115 → 121 へ
/// 悪化した。見切れた商品の縁を直せないのは惜しいが、悪化させないほうを採る。
pub const GATES: &[WarningCode] = &[
    WarningCode::HaloRemains,
    WarningCode::BboxRecommended,
    WarningCode::NotSeparable,
    WarningCode::ForegroundTooSmall,
    WarningCode::ForegroundTooLarge,
    WarningCode::SubjectTouchesEdge,
];

/// 警告が出た指標の区間を集め、広げた窓にする。解き直す区間が無ければ空。
///
/// 区間を出すのは `CONTOUR_ROUGH` と `RIM_CONTAMINATED` だけである。どちらも
/// 「輪郭の位置が真の縁から少し外れている」ことの別の現れで、直す手（帯を
/// 解き直す）が同じだからである。`GATES` の警告が 1 つでも出ていれば空を返す。
pub fn windows(d: &Diagnostics, warnings: &[Warning], bands: &Bands) -> Vec<Window> {
    let (w, h) = (d.local.grid.width, d.local.grid.height);
    if w == 0 || h == 0 || warnings.iter().any(|x| GATES.contains(&x.code)) {
        return Vec::new();
    }
    let mut regions = Vec::new();
    if d.contour_roughness.is_some_and(|v| v > CONTOUR_ROUGH_WARN) {
        regions.extend(d.roughness_regions());
    }
    if d.rim_contamination
        .is_some_and(|v| v > RIM_CONTAMINATION_WARN)
    {
        regions.extend(d.rim_regions());
    }
    let pad = (bands.pad * scale_at_1000(w, h)).ceil() as u32;
    let mut out: Vec<Window> = regions
        .iter()
        .map(|r| {
            let (x1, y1, x2, y2) = r.bbox;
            (
                x1.saturating_sub(pad),
                y1.saturating_sub(pad),
                (x2 + pad).min(w - 1),
                (y2 + pad).min(h - 1),
            )
        })
        .collect();
    // 並びを決めておく。指標ごとの順に積んだままだと、同じ窓の集合でも
    // 積み方で順が変わる（結果の画素は変わらないが、報告の並びが揺れる）
    out.sort_unstable();
    out.dedup();
    out
}

/// 窓の和の外接矩形。
fn hull(windows: &[Window]) -> Option<Window> {
    windows
        .iter()
        .copied()
        .reduce(|a, b| (a.0.min(b.0), a.1.min(b.1), a.2.max(b.2), a.3.max(b.3)))
}

/// 窓の中で、前景の画素が輪郭から何 px 内側にあるか（3-4 チャンファー、px 単位の
/// 1/3 刻みを u16 で持つ）。前景でない画素は 0。
///
/// 測る範囲は `roi` に絞る。roi の縁は前景が続いていても「輪郭」とは数えない
/// ——呼ぶ側は roi を窓より帯の幅以上広く取るので、窓の中の値は全面で測るのと
/// 変わらない。
fn depth(mask: &Mask, roi: Window) -> (Vec<u16>, usize) {
    let (x0, y0, x1, y1) = (
        roi.0 as usize,
        roi.1 as usize,
        roi.2 as usize,
        roi.3 as usize,
    );
    let w = x1 - x0 + 1;
    let h = y1 - y0 + 1;
    let alpha = mask.as_slice();
    let stride = mask.width() as usize;
    let mut d = vec![u16::MAX; w * h];
    for y in 0..h {
        for x in 0..w {
            if alpha[(y + y0) * stride + x + x0] < FOREGROUND_THRESHOLD {
                d[y * w + x] = 0;
            }
        }
    }
    let step = |v: u16, by: u16| v.saturating_add(by);
    for y in 0..h {
        for x in 0..w {
            let i = y * w + x;
            let mut best = d[i];
            if y > 0 {
                best = best.min(step(d[i - w], 3));
                if x > 0 {
                    best = best.min(step(d[i - w - 1], 4));
                }
                if x + 1 < w {
                    best = best.min(step(d[i - w + 1], 4));
                }
            }
            if x > 0 {
                best = best.min(step(d[i - 1], 3));
            }
            d[i] = best;
        }
    }
    for y in (0..h).rev() {
        for x in (0..w).rev() {
            let i = y * w + x;
            let mut best = d[i];
            if y + 1 < h {
                best = best.min(step(d[i + w], 3));
                if x > 0 {
                    best = best.min(step(d[i + w - 1], 4));
                }
                if x + 1 < w {
                    best = best.min(step(d[i + w + 1], 4));
                }
            }
            if x + 1 < w {
                best = best.min(step(d[i + 1], 3));
            }
            d[i] = best;
        }
    }
    (d, w)
}

/// 2 回目に渡す指示。窓の中は非対称の帯、窓の外は 1 回目の結果に釘付けにする。
///
/// **窓の外も釘付けにする。** 外はどうせ 1 回目の画素を使うが、指示を空けておくと
/// 2 回目のフィルが窓の外まで動き、背景の場の材料も変わる。0 と 255 を確定させて
/// おけば、2 回目は窓の帯と 1 回目の遷移の画素だけを解く。
///
/// `user` は利用者の指示。**利用者は決定**（`--segment` の規約と同じ）なので、
/// 最後に上から重ねる。
pub fn trimap(
    first: &Mask,
    windows: &[Window],
    bands: &Bands,
    user: Option<&Constraints>,
) -> Constraints {
    let (w, h) = (first.width(), first.height());
    let scale = scale_at_1000(w, h);
    // チャンファーの単位（1px = 3）で比べる
    let bg = ((bands.background_inset * scale).round() as u32) * 3;
    let fg = ((bands.foreground_inset * scale).ceil() as u32).max(2) * 3;
    let mut out = Constraints::new(w, h);
    let alpha = first.as_slice();
    for (i, &a) in alpha.iter().enumerate() {
        if a == 0 {
            out.mark_index(i, Constraint::ForcedBg);
        } else if a == u8::MAX {
            out.mark_index(i, Constraint::ForcedFg);
        }
    }
    if let Some(all) = hull(windows) {
        // 窓の中の深さは、窓より帯の幅だけ広い枠で測る
        let reach = fg / 3 + 2;
        let roi = (
            all.0.saturating_sub(reach),
            all.1.saturating_sub(reach),
            (all.2 + reach).min(w - 1),
            (all.3 + reach).min(h - 1),
        );
        let (field, fw) = depth(first, roi);
        for &(x1, y1, x2, y2) in windows {
            for y in y1..=y2 {
                for x in x1..=x2 {
                    let i = (y as usize) * (w as usize) + (x as usize);
                    let d = u32::from(field[((y - roi.1) as usize) * fw + (x - roi.0) as usize]);
                    // 前の印を消してから付け直す（ビットの OR は衝突になる）
                    out.clear_index(i);
                    let kind = if d <= bg {
                        Constraint::ForcedBg
                    } else if d > fg {
                        Constraint::ForcedFg
                    } else {
                        Constraint::Free
                    };
                    out.mark_index(i, kind);
                }
            }
        }
    }
    if let Some(user) = user {
        out.overlay(user);
    }
    out
}

/// 窓の和の中で、和の縁までのチェビシェフ距離（px）。和の外は 0、縁の画素は 1。
///
/// **窓ごとの縁からの距離の最大では足りない。** 2 つの窓が混ぜる幅の 2 倍より
/// 浅く重なると、重なりの画素はどちらの窓の縁にも近く、和の奥にあるのに重みが
/// 下がる——継ぎ目に 1 回目へ戻る筋が残る（CONTOUR_ROUGH と RIM_CONTAMINATED の
/// 窓は同じ辺で範囲が少しずつ違うので、普通に起きる）。和そのものの距離を測る。
///
/// 外接矩形の外は和の外なので、1 画素ぶん外側に 0 の縁を付けて測る。
fn union_depth(windows: &[Window], hull: Window) -> (Vec<u16>, usize) {
    let (x0, y0) = (hull.0 as usize, hull.1 as usize);
    let w = (hull.2 - hull.0) as usize + 3;
    let h = (hull.3 - hull.1) as usize + 3;
    let mut d = vec![0u16; w * h];
    for &(x1, y1, x2, y2) in windows {
        for y in y1 as usize..=y2 as usize {
            let row = (y - y0 + 1) * w;
            for x in x1 as usize..=x2 as usize {
                d[row + x - x0 + 1] = u16::MAX;
            }
        }
    }
    for y in 1..h - 1 {
        for x in 1..w - 1 {
            let i = y * w + x;
            if d[i] == 0 {
                continue;
            }
            let near = d[i - 1].min(d[i - w - 1]).min(d[i - w]).min(d[i - w + 1]);
            d[i] = d[i].min(near.saturating_add(1));
        }
    }
    for y in (1..h - 1).rev() {
        for x in (1..w - 1).rev() {
            let i = y * w + x;
            if d[i] == 0 {
                continue;
            }
            let near = d[i + 1].min(d[i + w + 1]).min(d[i + w]).min(d[i + w - 1]);
            d[i] = d[i].min(near.saturating_add(1));
        }
    }
    (d, w)
}

/// 2 回目を 1 回目の上へ混ぜる（1 回目をその場で書き換える）。窓の外は
/// 1 回目の画素そのもので、1 バイトも動かない。
///
/// アルファも色も同じ重みで混ぜる。2 回目の色は 2 回目のアルファで復元した
/// ものなので、アルファだけを混ぜると縁の色が 1 回目のアルファと食い違う。
///
/// 書き込むアルファは元画像のアルファと小さいほう（`apply_alpha` と同じ規約）。
/// 混ぜた値をそのまま書くと、透過つきの入力で窓の中だけ透明な画素が戻ってくる。
///
/// **その場で書き換える。** 24.5MP の画像は 1 枚 98MB あり、混ぜた結果を
/// 別に確保すると 1 回目・2 回目・結果の 3 枚が同時に生きる。
pub fn composite(
    source: &RgbaImage,
    first: (&mut RgbaImage, &mut Mask),
    second: (&RgbaImage, &Mask),
    windows: &[Window],
    bands: &Bands,
) {
    let (image, mask) = first;
    let (w, h) = (mask.width(), mask.height());
    let blend = (bands.blend * scale_at_1000(w, h)).max(1.0);
    let Some((hx1, hy1, hx2, hy2)) = hull(windows) else {
        return;
    };
    let (depth, dw) = union_depth(windows, (hx1, hy1, hx2, hy2));
    for y in hy1..=hy2 {
        for x in hx1..=hx2 {
            // 窓の縁から `blend` px かけて 0 → 1。縁の画素（距離 1）は 0.5 / blend
            let d = depth[((y - hy1) as usize + 1) * dw + (x - hx1) as usize + 1];
            if d == 0 {
                continue;
            }
            let t = ((f64::from(d) - 0.5) / blend).min(1.0);
            let i = (y as usize) * (w as usize) + (x as usize);
            let mix = |a: u8, b: u8| (f64::from(a) * (1.0 - t) + f64::from(b) * t).round() as u8;
            let a = mix(mask.as_slice()[i], second.1.as_slice()[i]);
            mask.as_mut_slice()[i] = a;
            let p1 = image.get_pixel(x, y).0;
            let p2 = second.0.get_pixel(x, y).0;
            let shown = a.min(source.get_pixel(x, y).0[3]);
            image.put_pixel(
                x,
                y,
                image::Rgba([
                    mix(p1[0], p2[0]),
                    mix(p1[1], p2[1]),
                    mix(p1[2], p2[2]),
                    shown,
                ]),
            );
        }
    }
}

/// 解き直しの前後。結果 JSON の `edge_repair` ブロックの中身になる。
#[derive(Debug, Clone, PartialEq)]
pub struct Outcome {
    /// 解き直した窓（`[x1, y1, x2, y2]`、入力の画素）。空なら何もしていない
    pub windows: Vec<Window>,
    /// 見送った理由。`GATES` の警告が出ていればその code、警告の区間が
    /// 無ければ None（直すものが無かった）
    pub gated_by: Option<WarningCode>,
    /// 1 回目の指標
    pub before: Diagnostics,
    /// 混ぜた後の指標。解き直していなければ `before` と同じ
    pub after: Diagnostics,
}

/// 1 回目の結果から、警告の区間だけを解き直した結果を作る。
///
/// **`subject` と設定の値（堤防・帯幅）は 1 回目のまま**である。どちらも
/// 「どう切ったか」の側の値で、差し替えるのは窓の中の輪郭の数 px だけなので、
/// 主体の矩形は動かない。
///
/// 解き直す区間が無ければ（警告が無い、`GATES` に当たった）、`first` を
/// **そのまま**返す——画素も警告も 1 バイトも動かない。
///
/// 2 回目は `opts` に指示（`trimap`）だけを差し替えて同じ `cutout_seen` を
/// 通す。混ぜた後の分離度・診断・警告は、**1 回目の背景の場で**測り直す
/// （`background` ブロックが言う背景と、数値を測った背景を食い違わせない）。
pub fn repair(
    image: &RgbaImage,
    opts: &CutoutOptions,
    first: CutoutResult,
    seen: Option<&BackgroundSeen>,
    bands: &Bands,
) -> (CutoutResult, Outcome) {
    // **直すものがあったときだけ「見送った」と言う。** 縁の警告が出ていない
    // 切り抜きで門の code を返すと、直す必要があったのに止めたと読まれる
    let wanted = first
        .diagnostics
        .contour_roughness
        .is_some_and(|v| v > CONTOUR_ROUGH_WARN)
        || first
            .diagnostics
            .rim_contamination
            .is_some_and(|v| v > RIM_CONTAMINATION_WARN);
    let gated_by = first
        .warnings
        .iter()
        .map(|w| w.code)
        .find(|c| wanted && GATES.contains(c));
    let windows = windows(&first.diagnostics, &first.warnings, bands);
    if windows.is_empty() {
        let outcome = Outcome {
            windows,
            gated_by,
            before: first.diagnostics.clone(),
            after: first.diagnostics.clone(),
        };
        return (first, outcome);
    }
    // 指示の表（24.5MP で 24.5MB）を複製してから捨てないよう、表を抜いた
    // 設定を複製してからトライマップを差す
    let mut again = opts.clone_without_constraints();
    again.constraints = Some(trimap(
        &first.mask,
        &windows,
        bands,
        opts.constraints.as_ref(),
    ));
    let second = cutout_seen(image, &again, seen);
    drop(again);
    let CutoutResult {
        image: mut out,
        mut mask,
        background,
        background_model,
        field_range,
        residual,
        edge_threshold,
        band_min_radius,
        band_width_histogram,
        smooth_radius_px,
        diagnostics: before,
        subject,
        mut warnings,
        judged_from,
        field,
        ..
    } = first;
    composite(
        image,
        (&mut out, &mut mask),
        (&second.image, &second.mask),
        &windows,
        bands,
    );
    // **2 回目の設定の警告も持ち越す。** 窓の中は 2 回目の画素なので、
    // 2 回目の matting が解き切れなかった（`MATTING_NOT_CONVERGED`）なら、
    // そう言わないと黙って使ったことになる。1 回目と同じ code は重ねない
    let carried: Vec<Warning> = second.warnings[..second.judged_from]
        .iter()
        .filter(|w| !warnings[..judged_from].iter().any(|x| x.code == w.code))
        .cloned()
        .collect();
    // 2 回目はここで手放す。24.5MP では画像だけで 98MB ある
    drop(second);
    let stats = mask.stats();
    let (separability, diagnostics) = judge(image, &mask, &field, &residual, opts);
    // 設定の警告（堤防・場・matting）は 1 回目のまま正しい。結果の警告だけを
    // 混ぜたマスクで判定し直して入れ替える
    warnings.truncate(judged_from);
    warnings.extend(carried);
    let judged_from = warnings.len();
    warnings.extend(collect_warnings(
        &background,
        &residual,
        &stats,
        separability,
        &diagnostics,
        subject.as_ref(),
        opts.bbox.is_some(),
    ));
    let outcome = Outcome {
        windows,
        gated_by: None,
        before,
        after: diagnostics.clone(),
    };
    (
        CutoutResult {
            image: out,
            mask,
            background,
            background_model,
            field_range,
            residual,
            edge_threshold,
            band_min_radius,
            band_width_histogram,
            smooth_radius_px,
            stats,
            separability,
            diagnostics,
            subject,
            warnings,
            judged_from,
            field,
        },
        outcome,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cutout::regions::Grid;

    /// 200x200 の中央に 50..150 の正方形（不透明）。外は透明。
    fn square() -> Mask {
        let mut m = Mask::new(200, 200, 0);
        for y in 50..150 {
            for x in 50..150 {
                m.set(x, y, 255);
            }
        }
        m
    }

    #[test]
    fn the_trimap_bands_inside_the_window_and_pins_outside() {
        let mut first = square();
        // 窓の外の遷移の画素（アルファ 0 でも 255 でもない）
        first.set(149, 140, 100);
        let windows = [(40, 40, 160, 100)];
        let t = trimap(&first, &windows, &Bands::default(), None);
        // 長辺 200px なので換算は 1 倍。確定背景は前景でない画素だけ（0.35px は
        // 0px に丸まる）、確定前景は輪郭から 6px より内側
        assert_eq!(t.at(100, 45), Constraint::ForcedBg, "窓の中の背景");
        assert_eq!(t.at(100, 50), Constraint::Free, "輪郭の画素は不明帯");
        assert_eq!(t.at(100, 54), Constraint::Free, "6px 以内は不明帯");
        assert_eq!(
            t.at(100, 60),
            Constraint::ForcedFg,
            "6px より深ければ確定前景"
        );
        // 窓の外は 1 回目に釘付け
        assert_eq!(t.at(100, 149), Constraint::ForcedFg);
        assert_eq!(t.at(100, 152), Constraint::ForcedBg);
        assert_eq!(t.at(149, 140), Constraint::Free, "遷移の画素は解き直させる");
    }

    /// 長辺 5712px（換算 5.712 倍）では、手作業で効いた 2px と、35px の帯になる。
    #[test]
    fn the_bands_scale_with_the_long_side() {
        let (w, h) = (5712u32, 400u32);
        let mut first = Mask::new(w, h, 0);
        for y in 100..400 {
            for x in 0..w {
                first.set(x, y, 255);
            }
        }
        let t = trimap(&first, &[(0, 0, w - 1, h - 1)], &Bands::default(), None);
        assert_eq!(
            t.at(10, 100),
            Constraint::ForcedBg,
            "輪郭の画素（深さ 1px）"
        );
        assert_eq!(
            t.at(10, 101),
            Constraint::ForcedBg,
            "深さ 2px までが確定背景"
        );
        assert_eq!(t.at(10, 102), Constraint::Free);
        assert_eq!(t.at(10, 133), Constraint::Free, "深さ 35px までが不明帯");
        assert_eq!(t.at(10, 135), Constraint::ForcedFg);
    }

    #[test]
    fn the_users_instruction_wins_over_the_trimap() {
        let first = square();
        let mut user = Constraints::new(200, 200);
        user.mark(100, 60, Constraint::ForcedBg);
        let t = trimap(
            &first,
            &[(40, 40, 160, 100)],
            &Bands::default(),
            Some(&user),
        );
        assert_eq!(t.at(100, 60), Constraint::ForcedBg);
        assert!(t.conflict().is_none(), "付け替えが衝突として残っている");
    }

    #[test]
    fn outside_the_windows_not_a_byte_moves_and_the_seam_is_gradual() {
        let source = RgbaImage::from_pixel(200, 200, image::Rgba([10, 20, 30, 255]));
        let first_mask = square();
        let mut first_image = RgbaImage::from_fn(200, 200, |x, y| {
            image::Rgba([10, 20, 30, first_mask.get(x, y)])
        });
        // 2 回目は右の辺を 4px 内側へ動かした
        let mut second_mask = Mask::new(200, 200, 0);
        for y in 50..150 {
            for x in 50..146 {
                second_mask.set(x, y, 255);
            }
        }
        let second_image = RgbaImage::from_pixel(200, 200, image::Rgba([200, 100, 50, 0]));
        let mut mask = first_mask.clone();
        let before = first_image.clone();
        let windows = [(120, 40, 160, 100)];
        composite(
            &source,
            (&mut first_image, &mut mask),
            (&second_image, &second_mask),
            &windows,
            &Bands::default(),
        );
        for y in 0..200 {
            for x in 0..200 {
                let inside = (120..=160).contains(&x) && (40..=100).contains(&y);
                if !inside {
                    assert_eq!(mask.get(x, y), first_mask.get(x, y), "({x},{y})");
                    assert_eq!(first_image.get_pixel(x, y), before.get_pixel(x, y));
                }
            }
        }
        // 窓の芯は 2 回目そのもの
        assert_eq!(mask.get(147, 70), 0);
        assert_eq!(mask.get(144, 70), 255);
        // 窓の下の縁（y = 100）へ向かって、x = 147 の列は 0 から 1 回目の 255 へ
        // 滑らかに戻る。隣どうしの段差は混ぜる幅（換算 4px）で割った量に収まる
        let column: Vec<i32> = (70..=105).map(|y| i32::from(mask.get(147, y))).collect();
        assert_eq!(*column.last().unwrap(), 255);
        for pair in column.windows(2) {
            assert!(
                (pair[0] - pair[1]).abs() <= 255 / 4 + 1,
                "段差がある: {column:?}"
            );
        }
    }

    /// 浅く重なった 2 つの窓の重なりは、和の奥なので 2 回目そのものになる。
    /// 窓ごとの縁からの距離で重みを決めると、ここに 1 回目へ戻る筋が残る。
    #[test]
    fn a_shallow_overlap_of_two_windows_leaves_no_seam() {
        let source = RgbaImage::from_pixel(200, 200, image::Rgba([0, 0, 0, 255]));
        let mut image = RgbaImage::from_pixel(200, 200, image::Rgba([0, 0, 0, 255]));
        let mut mask = Mask::new(200, 200, 255);
        let second = (
            RgbaImage::from_pixel(200, 200, image::Rgba([0, 0, 0, 0])),
            Mask::new(200, 200, 0),
        );
        composite(
            &source,
            (&mut image, &mut mask),
            (&second.0, &second.1),
            &[(10, 20, 100, 180), (98, 20, 150, 180)],
            &Bands::default(),
        );
        for x in 20..140 {
            assert_eq!(mask.get(x, 100), 0, "x = {x} に筋が残った");
        }
    }

    /// 透過つきの入力では、窓の中でも元画像のアルファを上回らない。
    #[test]
    fn the_source_alpha_still_caps_the_window() {
        let source = RgbaImage::from_pixel(200, 200, image::Rgba([10, 20, 30, 40]));
        let mut image = RgbaImage::from_pixel(200, 200, image::Rgba([10, 20, 30, 40]));
        let mut mask = square();
        let second_image = RgbaImage::from_pixel(200, 200, image::Rgba([10, 20, 30, 40]));
        composite(
            &source,
            (&mut image, &mut mask),
            (&second_image, &square()),
            &[(40, 40, 160, 160)],
            &Bands::default(),
        );
        assert_eq!(image.get_pixel(100, 100).0[3], 40);
    }

    #[test]
    fn a_failed_cut_is_not_repaired() {
        let mut d = Diagnostics {
            halo_ratio: Some(0.5),
            edge_width: Some(1.0),
            contour_roughness: Some(1.0),
            rim_contamination: Some(0.5),
            local: Default::default(),
        };
        d.local.grid = Grid::new(200, 200, 1.0);
        for code in GATES {
            let warnings = [Warning::new(*code, "外れています")];
            assert!(
                windows(&d, &warnings, &Bands::default()).is_empty(),
                "{code:?}"
            );
        }
    }
}
