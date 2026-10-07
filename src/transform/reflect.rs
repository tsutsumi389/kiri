//! 切り抜いた商品の鏡像を下に敷く（台に映った反射）。
//!
//! EC の商品写真では「艶のある台の上に置いた」見た目が定番で、その反射は
//! 撮影で作ると台の反射率と光源の向きで濃さがばらつく。落ち影
//! （`transform/shadow.rs`）とまったく同じ問題なので、答えも同じにする——
//! **切り抜いた後の商品から合成し、指定どおりに揃える。**
//!
//! **影とは写すものが違う。** 影はアルファだけを使って `--shadow-color` で
//! 塗るが、反射は**画素をそのまま写す**（RGB も）。台に映るのは商品の色で
//! あって影ではない。色を塗る形にすると `--reflect-color` を持たない
//! この段は影の別名になってしまう（design.md 4.16 の却下案）。
//!
//! **商品の層は 1 画素も変えない。** 反射のアルファが 0 の画素と、商品の
//! アルファが 255 の画素は、`--reflect off` の出力とビット一致する。合成の
//! 算術は `transform/canvas.rs` の `over` を共有しているので、半透明の縁で
//! 1 ずつ食い違う 2 種類目の合成が生まれる余地も無い。

use image::RgbaImage;

use crate::transform::canvas::over;
use crate::transform::shadow::grow_rect;

/// 反射を合成するか。
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum ReflectMode {
    /// 合成しない（既定）
    Off,
    /// 商品の鏡像を下に敷く
    On,
}

impl ReflectMode {
    pub fn as_str(self) -> &'static str {
        match self {
            ReflectMode::Off => "off",
            ReflectMode::On => "on",
        }
    }
}

/// 合成する反射の姿。**すべて実寸の px** で、長辺 1000px 換算からの掛け戻しは
/// 呼び出し側（`commands/cutout.rs`）が済ませてある。
///
/// `ShadowSpec` と同じ約束である。換算をここへ持ち込むと「最終画像の長辺」を
/// 知るためにキャンバスの寸法までこの層へ渡すことになる。反射の形は画像 1 枚で
/// 閉じた話なので、外の事情は入口で解いておく。
#[derive(Debug, Clone)]
pub struct ReflectSpec {
    /// 鏡像を何行ぶん敷くか
    pub height: u32,
    /// 商品の下端と反射の 1 行目のあいだに空ける行数
    pub gap: u32,
    /// 商品に接する側（反射の 1 行目）の不透明度 0.0-1.0
    pub opacity: f64,
}

/// 反射が実際にどこを占めたか。
///
/// **`ShadowBounds` と同型にしてある。** `rect` が `None` でも `clipped` は
/// 真になりうる、という規約もそのまま同じである——隙間が画像より大きければ
/// 反射は 1 画素も残らないが、それは「反射を敷かなかった」のではなく
/// 「全部はみ出した」である。2 つを 1 つの `Option` に畳むと、結果の JSON で
/// 両者が同じ形になり、エージェントは指定が効かなかった理由を追えない
/// （`shadow.rs` の `ShadowBounds` と `report.rs` の `ShadowReport::clipped`）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReflectBounds {
    /// 反射のアルファが 0 より大きい画素の外接矩形 [x1, y1, x2, y2]。
    /// 1 画素も無ければ None
    pub rect: Option<[u32; 4]>,
    /// 反射の一部が画像の外にあるか。
    ///
    /// # 左右の縁では**切られていなくても真になる**
    ///
    /// 判定は「インクの出る行が画像の外へ落ちた」か「外周にインクが残って
    /// いる」で、後者には左右の 1 列も入る。反射は横へ広がらないので、
    /// 左右にインクが残るのは**商品が画像の左右の縁に触れている**という
    /// 意味であって、反射が切られたわけではない。`--shadow` の
    /// `offset (0, 0)` がまったく同じ振る舞いをするので、規約をそちらへ
    /// 揃えてある（`docs/commands/05-5-finishing.md` の反射の節と `kiri schema` の
    /// `reflect.clipped` が同じことを言う）。
    ///
    /// 縦方向——下端の行にインクが残っている、または行が画像の外へ落ちた
    /// ——だけが「その先へ続いていた」の直接の証拠である。
    pub clipped: bool,
}

/// 商品の下に反射を敷いた画像を返す。寸法は入力と同じ。
///
/// **`product` を値で受けてその場に書く。** 24.5MP の RGBA は 98MB あり、
/// 複製する理由が無い。**その場で書いても材料は壊れない**——読むのは基準線
/// （商品のアルファの下端）より上の行だけ、書くのは基準線より下の行だけで、
/// 2 つは交わらない。
///
/// 反射が画像の外へ出る部分は切る。**商品の配置は反射のために動かさない**
/// ——影とまったく同じ理由で、`--canvas --fill-ratio` で揃えたはずの占有率が
/// 反射の有無で変わってしまう。切ったことは `clipped` が言う。
pub fn synth(mut product: RgbaImage, spec: &ReflectSpec) -> (RgbaImage, ReflectBounds) {
    let (w, h) = (product.width(), product.height());
    let empty = ReflectBounds {
        rect: None,
        clipped: false,
    };
    if w == 0 || h == 0 {
        return (product, empty);
    }
    // 商品が 1 画素も無ければ写すものが無い。「敷かなかった」のであって
    // 「はみ出した」のではないので `clipped` は偽である
    let Some(baseline) = baseline(&product) else {
        return (product, empty);
    };

    // **写せる行は基準線より上にある `baseline + 1` 行しかない。** ここで頭を
    // 抑えておくので、桁外れの `height`（`cli::REFLECT_HEIGHT_MAX` を通らない
    // 経路から来た値）でも走る行数は画像の高さを超えず、時間も溢れもしない。
    // 抑えた行は素材が無い＝インクも出ないので、`clipped` の判定にも効かない
    let rows = u64::from(spec.height).min(u64::from(baseline) + 1) as u32;

    let mut rect: Option<[u32; 4]> = None;
    // **はみ出しは最終のアルファで決める**（`clipped` の組み立てを参照）。
    // 下端の行に反射のインクが残っていれば、その先へ続いていた。左右の縁は
    // 「商品が縁に触れている」の言い換えにすぎない（`ReflectBounds::clipped`）
    let mut touches_border = false;
    // インクの出る行が画像の外へ落ちたか
    let mut dropped = false;
    // 1 行ぶんの控え。**その場で書くために要る**——同じ画像から読みながら
    // 書くので、借用を分けるには行を 1 本抜き出しておくのがいちばん素直である
    // （`w * 4` バイトで、24.5MP でも 17KB にしかならない）
    let mut row = vec![image::Rgba([0u8; 4]); w as usize];

    for j in 0..rows {
        // **減衰の割り算は行ごとに 1 度だけ。** 画素あたりは乗算 1 回と丸め
        // 1 回で閉じる（`shadow.rs` の不透明度の段と同じ書き方）。実数の総和を
        // 作らないので、足す順序で最下位ビットが動く余地が無い＝決定性が立つ
        let fade = spec.opacity * f64::from(spec.height - j) / f64::from(spec.height);
        let src_y = baseline - j;
        // 置く行。桁外れの `gap` でも i64 の中に収まる
        let dst = i64::from(baseline) + i64::from(spec.gap) + 1 + i64::from(j);

        if dst >= i64::from(h) {
            // **置き場所が無い行は写さない。** 行の複製も合成も要らず、見るのは
            // 「インクが出たか」の 1 点だけである（24.5MP に
            // `--reflect-gap 1000` を渡すと、複製だけで 24M 画素ぶん無駄になる）。
            //
            // **`dropped` が立ったら以降は 1 画素も読まない。** `dst` は j に
            // ついて単調増加なので、ここから下はすべて画像の外である。
            //
            // **それでも loop は抜けない。** 抜くと、上に透明な帯を持つ商品で
            // 嘘をつく——この行にインクが無くても、さらに上の行（j が大きい側）に
            // インクがありうる。`dropped` は「落ちた画素があったか」であって
            // 「最初に落ちた行にインクがあったか」ではない
            if !dropped {
                dropped = (0..w).any(|x| {
                    (f64::from(product.get_pixel(x, src_y).0[3]) * fade).round() as u8 > 0
                });
            }
            continue;
        }
        let y = dst as u32;

        for x in 0..w {
            row[x as usize] = *product.get_pixel(x, src_y);
        }
        for x in 0..w {
            let p = row[x as usize].0;
            let a = (f64::from(p[3]) * fade).round() as u8;
            if a == 0 {
                continue;
            }
            // **上の縁は見ない。** 反射は基準線より下にしか置かないので
            // `y == 0` は構造的に起こらず、条件に残すと「起こりうる」と
            // 読ませてしまう。左右の縁は起こりうる——ただしそれは商品が縁に
            // 触れているという意味で、反射が切られたわけではない
            // （`ReflectBounds::clipped` の doc）
            touches_border |= x == 0 || x == w - 1 || y == h - 1;
            grow_rect(&mut rect, x, y);
            let q = product.get_pixel_mut(x, y);
            // **商品のアルファが 255 の画素には触れない。**
            //
            // ここと下の `over` の混色は、いまの呼び出し方では**構造的に
            // 到達しない**——基準線はアルファ > 0 の最下行なので、その下の行は
            // どれもアルファ 0 であり、`over` は `sa == 0` の枝で反射の画素を
            // そのまま返す。守っているのは実行時の値ではなく**合成の形の
            // 一貫性**で、`shadow::compose` と同じ 2 行を同じ順で持つ。層を
            // 増やす日に、ここだけ別の算術になっていないことが要点である
            if q.0[3] == 255 {
                continue;
            }
            q.0 = over(q.0, [p[0], p[1], p[2], a]);
        }
    }

    // **不透明度 0 と高さ 0 は「反射を敷かない」指定**なので、はみ出しようが
    // ない。ここを通さないと、`rect` が `None` になる 2 つの理由——敷かなかった
    // ／全部はみ出した——を `clipped` が分けられなくなる（`ShadowBounds` と
    // まったく同じ規約で、そこでは `--shadow-opacity 0` が同じ役を演じている）
    let clipped = spec.opacity > 0.0 && spec.height > 0 && (dropped || touches_border);

    (product, ReflectBounds { rect, clipped })
}

/// 反射の基準線——商品のアルファが 0 より大きい最下行。
///
/// **外接矩形の下端だけを使う。** 左右の端は要らない（反射は横へ広がらない）
/// ので、下から探して最初に当たった行を返す。商品が下端寄りにある EC の素材
/// では最初の数行で見つかり、画像全体を舐めるのは商品が 1 画素も無いときだけ
/// である。
fn baseline(product: &RgbaImage) -> Option<u32> {
    (0..product.height())
        .rev()
        .find(|&y| (0..product.width()).any(|x| product.get_pixel(x, y).0[3] > 0))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec() -> ReflectSpec {
        ReflectSpec {
            height: 10,
            gap: 0,
            opacity: 1.0,
        }
    }

    /// 中央に不透明な正方形を置いた画像。
    fn block(w: u32, h: u32, x1: u32, y1: u32, x2: u32, y2: u32) -> RgbaImage {
        let mut img = RgbaImage::new(w, h);
        for y in y1..=y2 {
            for x in x1..=x2 {
                img.put_pixel(x, y, image::Rgba([200, 60, 40, 255]));
            }
        }
        img
    }

    /// 行ごとに色と幅が違う階段状の商品。**どの行がどこへ写ったかが読める。**
    fn staircase(w: u32, h: u32, top: u32, rows: u32) -> RgbaImage {
        let mut img = RgbaImage::new(w, h);
        for i in 0..rows {
            let y = top + i;
            // 行ごとに違う色にしたいだけなので、巡回してもかまわない
            let tint = (10 + i * 20) as u8;
            for x in 0..=i {
                img.put_pixel(x + 1, y, image::Rgba([tint, 100, 200, 255]));
            }
        }
        img
    }

    /// 反射の j 行目は商品の `baseline - j` 行目の写しである。
    ///
    /// **見ているのは鏡像の対応そのもの**（どの行がどこへ落ちたか）なので、
    /// 減衰で動かない量——インクの出る x の集合と RGB——で確かめる。
    /// 減衰の側は `the_reflection_fades_monotonically_to_its_foot` が見る。
    #[test]
    fn the_reflection_mirrors_the_product_row_by_row() {
        // 階段は y=10..=13（幅 1, 2, 3, 4）。基準線は 13
        let product = staircase(40, 40, 10, 4);
        let (out, bounds) = synth(
            product.clone(),
            &ReflectSpec {
                height: 4,
                ..spec()
            },
        );
        assert_eq!(bounds.rect, Some([1, 14, 4, 17]), "反射の矩形が違う");
        assert!(!bounds.clipped);

        let inked = |img: &RgbaImage, y: u32| -> Vec<u32> {
            (0..img.width())
                .filter(|&x| img.get_pixel(x, y).0[3] > 0)
                .collect()
        };
        for j in 0..4u32 {
            let src = 13 - j;
            let dst = 14 + j;
            assert_eq!(
                inked(&out, dst),
                inked(&product, src),
                "反射の {dst} 行が商品の {src} 行の形になっていない"
            );
            for x in inked(&product, src) {
                assert_eq!(
                    out.get_pixel(x, dst).0[..3],
                    product.get_pixel(x, src).0[..3],
                    "反射の ({x},{dst}) が商品の ({x},{src}) の色でない"
                );
            }
        }
    }

    /// 1 行目は商品の下端から `gap + 1` 行目に来る。
    #[test]
    fn the_reflection_starts_one_gap_below_the_product() {
        let product = block(60, 60, 10, 10, 19, 29);
        for gap in [0u32, 1, 5] {
            let (out, bounds) = synth(
                product.clone(),
                &ReflectSpec {
                    height: 6,
                    gap,
                    ..spec()
                },
            );
            let first = 30 + gap;
            assert_eq!(
                bounds.rect.map(|r| r[1]),
                Some(first),
                "gap={gap} で 1 行目の位置が違う"
            );
            // 隙間の行には 1 画素も置かない
            for y in 30..first {
                for x in 0..60 {
                    assert_eq!(
                        out.get_pixel(x, y).0,
                        [0, 0, 0, 0],
                        "gap={gap} の隙間 ({x},{y}) に反射が漏れている"
                    );
                }
            }
            assert_eq!(
                out.get_pixel(15, first).0[3],
                255,
                "gap={gap} で 1 行目が空"
            );
        }
    }

    /// 上（商品に接する側）が最も濃く、足元へ単調に薄くなって 0 へ向かう。
    #[test]
    fn the_reflection_fades_monotonically_to_its_foot() {
        let product = block(80, 80, 20, 10, 39, 39);
        let height = 20u32;
        let (out, _) = synth(
            product,
            &ReflectSpec {
                height,
                gap: 0,
                opacity: 0.8,
            },
        );

        let alpha: Vec<u8> = (0..height)
            .map(|j| out.get_pixel(30, 40 + j).0[3])
            .collect();
        assert_eq!(
            alpha[0],
            (255.0f64 * 0.8).round() as u8,
            "1 行目が不透明度と違う"
        );
        for j in 1..height as usize {
            assert!(
                alpha[j] <= alpha[j - 1],
                "{j} 行目で濃くなっている: {alpha:?}"
            );
        }
        // 最後の行は係数 1/height。足元は 0 へ向かう
        assert_eq!(
            *alpha.last().unwrap(),
            (255.0 * 0.8 / f64::from(height)).round() as u8,
            "足元の減衰が線形でない: {alpha:?}"
        );
        assert!(
            alpha[0] > alpha[height as usize - 1] + 100,
            "上下でほとんど変わっていない: {alpha:?}"
        );
    }

    /// 反射は影と違って**商品の色を写す**。
    #[test]
    fn the_reflection_takes_the_product_colours() {
        let mut product = RgbaImage::new(30, 30);
        product.put_pixel(10, 10, image::Rgba([12, 200, 250, 255]));
        let (out, _) = synth(
            product,
            &ReflectSpec {
                height: 1,
                gap: 0,
                opacity: 1.0,
            },
        );
        // 高さ 1・不透明度 1 なら減衰の係数も 1 で、真下の 1 画素は写しそのもの
        assert_eq!(out.get_pixel(10, 11).0, [12, 200, 250, 255]);
    }

    /// 商品が不透明な画素と、反射が届かない画素は入力とビット一致する。
    #[test]
    fn opaque_product_pixels_and_reflection_free_pixels_are_untouched() {
        let product = block(80, 80, 30, 20, 49, 39);
        let (out, _) = synth(
            product.clone(),
            &ReflectSpec {
                height: 12,
                gap: 2,
                opacity: 0.5,
            },
        );
        for (x, y, p) in product.enumerate_pixels() {
            if p[3] == 255 {
                assert_eq!(out.get_pixel(x, y).0, p.0, "商品の層が変わった ({x},{y})");
            }
        }
        // 反射から遠い角は透明のまま
        assert_eq!(out.get_pixel(0, 0).0, [0, 0, 0, 0]);
        assert_eq!(out.get_pixel(79, 79).0, [0, 0, 0, 0]);
    }

    /// 不透明度 0 は反射の層が空になり、入力とビット一致する。
    #[test]
    fn a_zero_opacity_leaves_the_image_untouched() {
        let product = block(64, 64, 20, 20, 43, 43);
        let (out, bounds) = synth(
            product.clone(),
            &ReflectSpec {
                height: 15,
                gap: 3,
                opacity: 0.0,
            },
        );
        assert_eq!(out.as_raw(), product.as_raw(), "画素が変わっている");
        assert_eq!(bounds.rect, None, "反射が無いのに矩形が出ている");
    }

    /// 不透明度 0 は「反射を敷かない」指定なので、はみ出しようがない。
    ///
    /// `rect` が None になる 2 つの理由——敷かなかった／全部はみ出した——を
    /// `clipped` が分ける、という契約そのものの検査である。
    #[test]
    fn a_zero_opacity_is_never_reported_as_clipped() {
        let product = block(64, 64, 10, 10, 29, 29);
        let far = ReflectSpec {
            height: 20,
            gap: 300,
            opacity: 0.0,
        };
        let (_, bounds) = synth(product.clone(), &far);
        assert_eq!(bounds.rect, None);
        assert!(!bounds.clipped, "反射を敷いていないのに切れたと報告した");

        // 同じ隙間でも、反射を敷いたなら切れたと言う
        let (_, bounds) = synth(
            product,
            &ReflectSpec {
                opacity: 1.0,
                ..far
            },
        );
        assert_eq!(bounds.rect, None);
        assert!(bounds.clipped);
    }

    /// 高さ 0 も「反射を敷かない」指定である。
    #[test]
    fn a_zero_height_leaves_the_image_untouched() {
        let product = block(48, 48, 8, 8, 23, 23);
        let (out, bounds) = synth(
            product.clone(),
            &ReflectSpec {
                height: 0,
                gap: 0,
                opacity: 1.0,
            },
        );
        assert_eq!(out.as_raw(), product.as_raw(), "画素が変わっている");
        assert_eq!(bounds.rect, None);
        assert!(!bounds.clipped, "敷いていないのに切れたと報告した");
    }

    /// 画像の端で反射が切れても寸法は変わらず、切れたことが報告される。
    #[test]
    fn a_reflection_running_off_the_image_is_clipped_and_reported() {
        let product = block(40, 40, 10, 10, 29, 29);
        let (out, bounds) = synth(
            product,
            &ReflectSpec {
                height: 20,
                gap: 0,
                opacity: 1.0,
            },
        );
        assert_eq!((out.width(), out.height()), (40, 40), "寸法が変わった");
        assert!(bounds.clipped, "はみ出したのに clipped が偽");
        assert_eq!(
            bounds.rect.map(|r| r[3]),
            Some(39),
            "矩形が画像の中に収まっていない"
        );
    }

    /// 全部はみ出しても「反射を敷かなかった」とは報告しない。
    #[test]
    fn a_reflection_pushed_entirely_off_the_image_still_reports_the_clipping() {
        let product = block(32, 32, 4, 4, 11, 11);
        let (_, bounds) = synth(
            product,
            &ReflectSpec {
                height: 8,
                gap: 100,
                opacity: 1.0,
            },
        );
        assert_eq!(bounds.rect, None);
        assert!(bounds.clipped, "全部はみ出したのに clipped が偽");
    }

    /// 寸法 0 の画像でも落ちず、何も報告しない。
    ///
    /// `RgbaImage::new(0, h)` は `get_pixel` が必ず panic する形なので、
    /// **入口で断つ以外に守る方法が無い。** 呼び出し側（`commands/cutout.rs`）は
    /// 切り抜きが空でないことを先に確かめているが、この層はそれを前提に
    /// していない——`baseline` の走査も行の複製も 0 幅で壊れる。
    #[test]
    fn a_zero_sized_image_is_left_alone() {
        for (w, h) in [(0u32, 0u32), (0, 16), (16, 0)] {
            let product = RgbaImage::new(w, h);
            let (out, bounds) = synth(product, &spec());
            assert_eq!(
                (out.width(), out.height()),
                (w, h),
                "{w}x{h} で寸法が変わった"
            );
            assert_eq!(bounds.rect, None, "{w}x{h}");
            assert!(!bounds.clipped, "{w}x{h}");
        }
    }

    /// 左右の縁では、切られていなくても `clipped` が真になる。
    ///
    /// **これは規約であって不具合ではない**（`--shadow` の `offset (0, 0)` と
    /// 同じ振る舞い）。反射は横へ広がらないので、左右にインクが残るのは
    /// 「商品が縁に触れている」の言い換えである。**文面がそう言っていること**
    /// （README / `kiri schema` の `reflect.clipped`）が契約の側の担保で、
    /// ここは振る舞いを固定して、黙って変わらないようにしておく。
    #[test]
    fn a_product_touching_the_side_edges_is_reported_as_clipped() {
        // 商品が左右の縁いっぱいに広がる。反射は画像の中に収まる
        let full = block(40, 40, 0, 10, 39, 19);
        let (_, wide) = synth(
            full,
            &ReflectSpec {
                height: 5,
                gap: 0,
                opacity: 1.0,
            },
        );
        let rect = wide.rect.expect("反射があるはず");
        assert_eq!(
            [rect[1], rect[3]],
            [20, 24],
            "反射が画像の中に収まっていない"
        );
        assert!(
            wide.clipped,
            "左右の縁に触れているのに clipped が偽（規約が変わっている）"
        );

        // 1px 内側へ寄せれば偽になる。縦にも収まっているので真になる理由が無い
        let inset = block(40, 40, 1, 10, 38, 19);
        let (_, narrow) = synth(
            inset,
            &ReflectSpec {
                height: 5,
                gap: 0,
                opacity: 1.0,
            },
        );
        assert!(
            !narrow.clipped,
            "縁から離れて収まっている反射が切れたと報告された: {narrow:?}"
        );
    }

    /// 上に透明な帯を持つ商品でも、落ちたインクを見落とさない。
    ///
    /// **`dst` が画像の外へ出た行で loop を抜けると嘘になる。** 抜けた時点の
    /// 行にインクが無くても、さらに上（j が大きい側）の行にインクがありうる。
    /// `dropped` は「落ちた画素があったか」であって「最初に落ちた行にインクが
    /// あったか」ではない。
    #[test]
    fn ink_above_a_transparent_band_still_counts_as_dropped() {
        // 商品は y=20..=21 と y=10..=11 の 2 段。あいだの 12..=19 は透明
        let mut product = RgbaImage::new(20, 24);
        for y in [10u32, 11, 20, 21] {
            for x in 4..=15 {
                product.put_pixel(x, y, image::Rgba([200, 60, 40, 255]));
            }
        }
        // 基準線は 21。反射の 1 行目は 22（画像の中）、2 行目は 23（中）、
        // 3 行目から外。j=2,3 の源は y=19,18 で透明、j=10,11 の源は y=11,10 で
        // インクがある——そこを見落とさないこと
        let (_, bounds) = synth(
            product,
            &ReflectSpec {
                height: 12,
                gap: 0,
                opacity: 1.0,
            },
        );
        assert_eq!(bounds.rect, Some([4, 22, 15, 23]), "収まった 2 行が違う");
        assert!(
            bounds.clipped,
            "透明な帯の先にあるインクが落ちたのに clipped が偽"
        );
    }

    /// アルファが 1 画素も無い画像には何も敷かない。
    #[test]
    fn an_empty_product_lays_no_reflection() {
        let product = RgbaImage::new(24, 24);
        let (out, bounds) = synth(product.clone(), &spec());
        assert_eq!(out.as_raw(), product.as_raw());
        assert_eq!(bounds.rect, None);
        assert!(
            !bounds.clipped,
            "写すものが無いのに「はみ出した」と報告した"
        );
    }

    /// 桁外れの高さ・隙間でも落ちず、時間も溢れも起こさない。
    ///
    /// `cli::REFLECT_HEIGHT_MAX` / `REFLECT_GAP_MAX` が実際には断るが、
    /// **断らない経路ができたときに panic したり行数が化けたりしない**ことを
    /// 保つ（`shadow.rs` の `an_absurd_sigma_neither_panics_nor_produces_a_bogus_width`
    /// と同じ役の検査である）。走る行数は `baseline + 1` で頭を抑えてあるので、
    /// `u32::MAX` を渡しても画像 1 枚ぶんしか回らない。
    #[test]
    fn an_absurd_height_neither_panics_nor_overflows() {
        let product = block(32, 32, 8, 8, 19, 19);
        for (height, gap) in [
            (u32::MAX, 0u32),
            (u32::MAX, u32::MAX),
            (1, u32::MAX),
            (u32::MAX, 1),
        ] {
            let (out, bounds) = synth(
                product.clone(),
                &ReflectSpec {
                    height,
                    gap,
                    opacity: 1.0,
                },
            );
            assert_eq!((out.width(), out.height()), (32, 32), "h={height} g={gap}");
            // 置き場所が画像の外なら 1 画素も残らないが、切れたことは言う
            if gap >= 32 {
                assert_eq!(bounds.rect, None, "h={height} g={gap}");
                assert!(bounds.clipped, "h={height} g={gap}");
            }
        }
    }

    /// 同じ入力からは同じバイト列が出る。
    #[test]
    fn the_same_input_produces_the_same_bytes() {
        let product = staircase(120, 120, 30, 40);
        let s = ReflectSpec {
            height: 37,
            gap: 5,
            opacity: 0.37,
        };
        let a = synth(product.clone(), &s);
        let b = synth(product.clone(), &s);
        assert_eq!(a.0.as_raw(), b.0.as_raw());
        assert_eq!(a.1, b.1);
    }

    /// 24.5MP での合成そのものの所要時間を出す。
    ///
    /// **`cutout` 全体の `elapsed_ms` では測れない。** 切り抜きが 5 秒かかる
    /// ので、その中の 0.1 秒は実行ごとのばらつきに埋もれる
    /// （`shadow.rs` の `print_the_blur_cost_on_a_large_alpha` と同じ事情）。
    /// design.md 4.16 の数値はここから取る。
    ///
    /// ```
    /// cargo test --release --lib transform::reflect -- --ignored --nocapture
    /// ```
    #[test]
    #[ignore]
    fn print_the_reflection_cost_on_a_large_product() {
        use std::time::Instant;

        // 実写素材と同じ 4284x5712（24.5MP）。中央に商品を置く
        let (w, h) = (4284u32, 5712u32);
        let product = block(w, h, w / 4, h / 4, w * 3 / 4, h * 3 / 4);

        println!("24.5MP ({w}x{h})");
        for height in [0u32, 857, 2856] {
            let spec = ReflectSpec {
                height,
                gap: 0,
                opacity: 0.25,
            };
            // 1 回目は確保で振れるので 2 回測って速いほうを採る
            let mut best = f64::MAX;
            for _ in 0..2 {
                let input = product.clone();
                let started = Instant::now();
                let (out, _) = synth(input, &spec);
                let elapsed = started.elapsed().as_secs_f64() * 1000.0;
                std::hint::black_box(&out);
                best = best.min(elapsed);
            }
            println!("  高さ {height:>5}px  synth {best:>7.1} ms");
        }
    }
}
