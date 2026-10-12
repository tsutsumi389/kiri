//! `kiri compose --profile` — **見出し画像の規格を、合成の側の表として持つ。**
//!
//! # `crate::profile` へ混ぜない
//!
//! あちらの `Rules` は EC のモール規格（占有率・背景色・透過）の表で、cutout /
//! batch / lint が読む。note をそこへ足すと `kiri cutout --profile note` が書式と
//! して通り、**占有率も背景色も規定しない規格が切り抜きに当たる**——通るのに何も
//! 起きない指定は、`compose::gate` が cutout の指標と表を分けたのと同じ形の事故で
//! ある。表を分ければ、綴りの段階で clap が断る。
//!
//! # 持つのは「規定」と「実測」の 2 種類である
//!
//! キャンバス・安全領域・容量は note のヘルプに書かれた規定で、`source` から
//! 辿れる。**一覧での表示幅（`displays`）は note が公表していない**ので、kiri が
//! note.com を開いて描画サイズを測った値である（12-phase-31.md §12.3）。改修で
//! 変わりうるので、`revision` に測った年月を持つ。

/// 縮小表示の 1 つ。**幅だけを持つ**——高さはキャンバスの比から決まる
/// （note の一覧は `object-fit: cover` で、1280x670 なら切られない）。
#[derive(Debug)]
pub struct Display {
    /// 結果 JSON の `layers[].display[].name` に出る綴り
    pub name: &'static str,
    /// 表示幅（CSS px）
    pub width: u32,
}

/// 1 つの規格。**`kiri schema` の `compose_profiles[]` はこれをそのまま配る。**
#[derive(Debug)]
pub struct ComposeProfile {
    /// `--profile` に書く名前
    pub name: &'static str,
    /// 規格の版。表示幅が実測なので、**測った年月**でもある
    pub revision: &'static str,
    pub summary: &'static str,
    /// 規定の出典。表示幅はここに無い（実測、モジュールの doc）
    pub source: &'static str,
    /// spec が `canvas.width` / `canvas.height` を省いたときの寸法
    pub canvas: (u32, u32),
    /// spec が `safe_area` を省いたときの範囲 `[x, y, 幅, 高さ]`。
    /// **`canvas` を基準にした値**で、キャンバスが違えば比で合わせる
    /// （`safe_area_for`）
    pub safe_area: [f64; 4],
    /// 書いたバイト数の上限。**これ以下が合格**（`ProfileRules::max_bytes` と
    /// 同じ読み方）。`None` は規定なし
    pub max_bytes: Option<u64>,
    /// 縮めて読みやすさを測る表示。**並びがそのまま `layers[].display[]` の並び**
    pub displays: &'static [Display],
}

/// キャンバスの比が規格の比からこれ以上外れたら言う。
///
/// **0 ではない。** note は 1280x670 と 1920x1006 を並べて推奨しており、両者の比は
/// 1.910 と 1.909 で 0.1% 違う。0.5% はその 5 倍で、16:9（1280x720 は 6.9% 外れる）
/// のような**別の比**とは十分に離れている。
pub const ASPECT_TOLERANCE: f64 = 0.005;

/// 規格の表。**行は note の 1 つだけである**（YouTube などは対象にしない。
/// 12-phase-31.md §12.1）。表の形は複数を持てるようにしてある。
pub const ALL: &[ComposeProfile] = &[ComposeProfile {
    name: "note",
    revision: "2026-10",
    summary: "note の記事の見出し画像（1280x670、一覧で見える中央 1280x454、10MB 以下）",
    source: "https://www.help-note.com/hc/ja/articles/360000231642",
    canvas: (1280, 670),
    // 「1280 × 454 px の範囲が表示される」を上下中央に置く。(670 − 454) / 2 = 108
    safe_area: [0.0, 108.0, 1280.0, 454.0],
    // 「各画像 最大 10MB」。**最大なので以下**で読み、k は 1000 進
    // （`--max-bytes` の決め方と同じ）
    max_bytes: Some(10_000_000),
    displays: &[
        // PC（横 1440）の一覧で最も多かった描画幅
        Display {
            name: "pc_list",
            width: 192,
        },
        // スマホ（横 375）の一覧で最も多かった描画幅。**96px は持たない**——
        // そこで 11px を要求すると 1280 幅で 147px の見出しが要る（§12.3）
        Display {
            name: "mobile_list",
            width: 144,
        },
    ],
}];

/// `ALL` の名前を並べたもの。clap の候補に渡す（`profile::PROFILE_NAMES` と
/// 同じ作法で、手で書き写した一覧を持たない）。
pub const NAMES: [&str; ALL.len()] = spellings();

const fn spellings() -> [&'static str; ALL.len()] {
    let mut out = [""; ALL.len()];
    let mut i = 0;
    while i < ALL.len() {
        out[i] = ALL[i].name;
        i += 1;
    }
    out
}

/// 名前で引く。
pub fn named(name: &str) -> Option<&'static ComposeProfile> {
    ALL.iter().find(|p| p.name == name)
}

impl ComposeProfile {
    /// そのキャンバスに当てる安全領域。**表示側に実際に残る範囲を返す。**
    ///
    /// # 比が許容の内なら、比で合わせる
    ///
    /// 1920x1006 は note が並べる推奨サイズで、同じ比として扱う（§12.2）。x と y を
    /// それぞれの倍率で合わせる。`PROFILE_ASPECT_MISMATCH` を出さないのと同じ
    /// 判定で、ここで「切り取りがある」とは言わない。
    ///
    /// # 外れたら、note が切り取った後の範囲を返す
    ///
    /// note は推奨と違う比の画像を**中央で規格の比へ切り取ってから**表示する
    /// （「推奨と異なるサイズ: トリミングされて表示される」）。切り取られて
    /// 残るのは、規格の比の矩形をキャンバスへ内接させたもの——倍率
    /// `s = min(w / pw, h / ph)` で、余った側を左右か上下へ等分に捨てる。
    /// 安全領域はその矩形の中で規格どおりの位置にある。1280x720 なら上下 25px
    /// ずつ捨てて [0, 133, 1280, 454] になる。
    ///
    /// x と y を別々の倍率で伸ばすと [0, 116.06, 1280, 487.88] になり、**捨てられる
    /// 帯の中まで「安全」と言う**（12-phase-31.md §12.9 の R2）。
    pub fn safe_area_for(&self, width: u32, height: u32) -> [f64; 4] {
        let (pw, ph) = (f64::from(self.canvas.0), f64::from(self.canvas.1));
        let (cw, ch) = (f64::from(width), f64::from(height));
        let [x, y, w, h] = self.safe_area;
        if !self.aspect_mismatch(width, height) {
            let (sx, sy) = (cw / pw, ch / ph);
            return [x * sx, y * sy, w * sx, h * sy];
        }
        let s = (cw / pw).min(ch / ph);
        let ox = (cw - pw * s) / 2.0;
        let oy = (ch - ph * s) / 2.0;
        [ox + x * s, oy + y * s, w * s, h * s]
    }

    /// キャンバスの比が規格の比から外れているか。`|(w/h) / (pw/ph) − 1| >= 0.005`。
    pub fn aspect_mismatch(&self, width: u32, height: u32) -> bool {
        let actual = f64::from(width) / f64::from(height);
        (actual / self.aspect() - 1.0).abs() >= ASPECT_TOLERANCE
    }

    /// 規格のキャンバスの比（幅 / 高さ）。
    pub fn aspect(&self) -> f64 {
        f64::from(self.canvas.0) / f64::from(self.canvas.1)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_published_names_are_the_table() {
        let from_table: Vec<&str> = ALL.iter().map(|p| p.name).collect();
        assert_eq!(NAMES.to_vec(), from_table);
    }

    /// **cutout / lint の表に note が混ざっていない。** 混ざると
    /// `kiri cutout --profile note` が書式として通る（モジュールの doc）。
    #[test]
    fn note_is_not_a_cutout_profile() {
        assert!(crate::profile::named("note").is_none());
        assert!(named("note").is_some());
    }

    /// 安全領域は規定どおりの上下中央である（§12.2）。
    #[test]
    fn the_note_safe_area_is_the_centred_band() {
        let note = named("note").unwrap();
        assert_eq!(note.safe_area_for(1280, 670), [0.0, 108.0, 1280.0, 454.0]);
        // (670 − 454) / 2 = 108 の関係そのもの
        let [_, y, _, h] = note.safe_area;
        assert_eq!(y * 2.0 + h, f64::from(note.canvas.1));
    }

    /// 大きいほうの推奨サイズでは同じ比で拡大する。
    #[test]
    fn the_safe_area_scales_with_the_canvas() {
        let note = named("note").unwrap();
        let [x, y, w, h] = note.safe_area_for(1920, 1006);
        assert_eq!(x, 0.0);
        assert_eq!(w, 1920.0);
        assert!((y - 108.0 * 1006.0 / 670.0).abs() < 1e-9, "{y}");
        assert!((h - 454.0 * 1006.0 / 670.0).abs() < 1e-9, "{h}");
    }

    /// **比の違うキャンバスでは、note が中央を切り取った後の範囲を返す**
    /// （§12.9 の R2）。1280x720 は上下 25px ずつ切られて 1280x670 になり、
    /// その中央の 454 が見える。x と y を別々の倍率で合わせると
    /// [0, 116.06, 1280, 487.88] になり、切られて見えない帯まで安全と言う。
    #[test]
    fn a_mismatched_canvas_gets_the_band_that_survives_the_crop() {
        let note = named("note").unwrap();
        assert_eq!(note.safe_area_for(1280, 720), [0.0, 133.0, 1280.0, 454.0]);
        // 横長に外れたら左右が切られる。2560x670 の見える幅は 1280
        assert_eq!(note.safe_area_for(2560, 670), [640.0, 108.0, 1280.0, 454.0]);
    }

    /// 比が許容の内なら切り取りは無いとみなす（`PROFILE_ASPECT_MISMATCH` を出さない
    /// のと同じ判定）。1920x1006 は Phase 31 の最初の実装と同じ値のまま。
    #[test]
    fn a_canvas_within_the_tolerance_is_scaled_not_cropped() {
        let note = named("note").unwrap();
        let [x, y, w, h] = note.safe_area_for(1920, 1006);
        assert_eq!([x, w], [0.0, 1920.0]);
        assert_eq!(y, 108.0 * (1006.0 / 670.0));
        assert_eq!(h, 454.0 * (1006.0 / 670.0));
    }

    /// note が並べる 2 つの推奨サイズは通り、16:9 は外れる（§12.5 の 3）。
    #[test]
    fn the_aspect_check_separates_the_two_recommendations_from_16_9() {
        let note = named("note").unwrap();
        assert!(!note.aspect_mismatch(1280, 670));
        assert!(!note.aspect_mismatch(1920, 1006));
        assert!(note.aspect_mismatch(1280, 720));
    }

    /// 表示は幅の大きい順に並び、名前は一意である。
    #[test]
    fn displays_are_unique_and_narrower_than_the_canvas() {
        for p in ALL {
            for (i, d) in p.displays.iter().enumerate() {
                assert!(d.width > 0 && d.width < p.canvas.0, "{}", d.name);
                assert!(
                    p.displays[..i].iter().all(|o| o.name != d.name),
                    "{} が重複",
                    d.name
                );
            }
        }
    }
}
