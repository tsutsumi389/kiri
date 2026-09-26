//! バッチ仕様ファイル（spec.json）の読み込みと検証。
//!
//! 「AI が画像ごとに座標を出す」設計は、画像ごとに引数が変わるため CLI 引数とは
//! 相性が悪い。JSON を流し込む形にすることで、AI が最も得意な出力形式のまま
//! 数百点を一括処理できる。
//!
//! 仕様の誤りは黙って無視しない。AI が生成した JSON に綴り違いのキーがあれば
//! エラーにして知らせる。無視すると「指定したはずの設定が効いていない」という
//! 最も気づきにくい失敗を生むため。

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::error::{Error, ErrorCode, Result};

#[derive(Debug, Deserialize, Default)]
pub struct BatchSpec {
    /// セット内でスケールと余白を揃える指定。書かなければ何も変わらない
    pub set: Option<SetSpec>,
    /// 全項目に適用する既定値。項目側の指定が優先される
    #[serde(default)]
    pub defaults: ItemSettings,
    pub items: Vec<BatchItem>,
}

/// セット内でスケールと余白を揃える指定。
///
/// **`defaults` の隣ではなく最上位に置く。** これは項目ごとの設定ではない
/// ——揃える相手は「このセットの全点」で、1 点だけを見ても決まらない。
/// `defaults` に置けば項目側で上書きできる形になり、「自分だけ別の基準で
/// 揃える」という意味を持たない指定が書けてしまう。
#[derive(Debug, Deserialize, Clone)]
pub struct SetSpec {
    /// 何を揃えるか（`"height"` / `"bbox"`）。**必須**
    pub align: String,
    /// 目標の占有率 T。**省けるのが普通の使い方**で、省くと pass 1 で測った
    /// 代表寸法（全点の占有率の中央値）が T になる
    pub fill_ratio: Option<f64>,
}

/// 何を揃えるか。
///
/// **2 つは別の問いに答える。** `Height` は「出力上の商品の高さ」を共通の
/// `T * CH` にする——撮影距離の違いを正規化する側である。`Bbox` は外接矩形を
/// 共通の枠 `(T*CW, T*CH)` へ長辺基準で収める——縦長と横長が混ざったセットで
/// 「はみ出さない」ことを優先する側になる。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SetAlign {
    Height,
    Bbox,
}

impl SetAlign {
    /// 書ける綴り。断るときの hint がここから組まれる
    pub const NAMES: &'static [&'static str] = &["height", "bbox"];

    pub fn parse(name: &str) -> Option<Self> {
        match name {
            "height" => Some(Self::Height),
            "bbox" => Some(Self::Bbox),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Height => "height",
            Self::Bbox => "bbox",
        }
    }
}

impl SetSpec {
    /// `align` を解く。**綴りを外したら断る。**
    ///
    /// 未知の値を既定へ落とすと、頼んだのと別の基準でセット全体が揃う。
    /// しかも結果 JSON は「揃えた」と言うので、仕上がりを並べて見るまで
    /// 気づけない。
    pub fn align(&self) -> Result<SetAlign> {
        SetAlign::parse(&self.align).ok_or_else(|| {
            Error::new(
                ErrorCode::InvalidSet,
                format!("'{}' は未対応の set.align です", self.align),
            )
            .with_hint(format!(
                "{} のいずれかを指定してください",
                SetAlign::NAMES.join(" / ")
            ))
        })
    }

    /// 書かれた目標 T を取る。**書かれていなければ `None`**（中央値で決める）。
    ///
    /// 値域は `canvas::plan()` が `fill_ratio` に課すものと同じ `(0, 1]` に
    /// する。ここで断らないと、1.2 を書いた spec が全項目そろって
    /// `INVALID_FILL_RATIO` で落ちる——同じ 1 つの誤りが項目数ぶんの
    /// エラーになって返る。
    pub fn target(&self) -> Result<Option<f64>> {
        let Some(value) = self.fill_ratio else {
            return Ok(None);
        };
        if !(value > 0.0 && value <= 1.0) {
            return Err(Error::new(
                ErrorCode::InvalidSet,
                format!(
                    "set.fill_ratio は 0.0 より大きく 1.0 以下である必要があります（{value} が指定されました）"
                ),
            )
            .with_hint("省けば全点の占有率の中央値が目標になります"));
        }
        Ok(Some(value))
    }
}

/// pass 1 を終えた `set`。**項目ごとの `CutoutArgs` はこの形で受け取る。**
///
/// spec の `SetSpec` と分けるのは、こちらが**測り終えた後の姿**だからである。
/// `target` は「書かれた値」か「測った中央値」のどちらかで、使う側はその
/// 違いを知る必要が無い——知る必要があるのは結果 JSON の読み手だけなので、
/// 出どころは `BatchReport.set.source` が言う。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SetPlacement {
    pub align: SetAlign,
    /// 目標の占有率 T。`(0, 1]`
    pub target: f64,
}

impl SetPlacement {
    /// `canvas::plan()` へ渡す占有率 `f_i` を綴る。
    ///
    /// # なぜ `canvas.rs` に手を入れないのか
    ///
    /// `plan()` は `scale = min(CW*f/cw, CH*f/ch)` を返す。狙った倍率 `s_i` を
    /// 出したいなら、**渡す `f` のほうを組み替えれば足りる**——
    ///
    /// ```text
    /// f_i = s_i * max(cw_i/CW, ch_i/CH)
    /// ```
    ///
    /// を代入すると `min` の中が両方 `s_i` になる。配置の算術は 1 つのままで、
    /// セット統一は「何を渡すか」だけの話に畳める。
    ///
    /// - `bbox` は `s_i = T*min(CW/cw, CH/ch)` なので **`f_i = T`（全点同じ）**
    /// - `height` は `s_i = T*CH/ch` なので **`f_i = T * max(1, (CH*cw)/(CW*ch))`**
    ///
    /// **`(0, 1]` を外れうるのは `height` だけである。** 正方キャンバスで
    /// 横長の商品に高さ `T*CH` を与えると、横幅が `T*CH*(cw/ch)` を要求する
    /// ——縦横比が `1/T` を超えればキャンバスの幅を越える。止めるかどうかは
    /// 呼ぶ側（`place_on_canvas`）が決める。ここは**要求そのもの**を返す。
    pub fn fill_ratio(self, content: (u32, u32), canvas: (u32, u32)) -> f64 {
        let (cw, ch) = (f64::from(content.0), f64::from(content.1));
        let (canvas_w, canvas_h) = (f64::from(canvas.0), f64::from(canvas.1));
        match self.align {
            SetAlign::Bbox => self.target,
            SetAlign::Height => self.target * 1.0_f64.max((canvas_h * cw) / (canvas_w * ch)),
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct BatchItem {
    pub input: PathBuf,
    pub output: PathBuf,
    #[serde(flatten)]
    pub settings: ItemSettings,
}

/// 1 項目分の設定。すべて任意で、未指定なら defaults、それも無ければ CLI の既定値。
#[derive(Debug, Deserialize, Default, Clone)]
pub struct ItemSettings {
    pub bbox: Option<[f64; 4]>,
    pub normalized: Option<bool>,
    pub fg_seeds: Option<Vec<[f64; 2]>>,
    /// 確定前景・確定背景・不明を表すグレー画像。`input` と同じ規則で
    /// 仕様ファイルの場所を基準に解決する
    pub trimap: Option<PathBuf>,
    /// 切り抜き済み画像のアルファを指示として読む。`trimap` と同じ規則で
    /// 仕様ファイルの場所を基準に解決する
    pub alpha_trimap: Option<PathBuf>,
    pub fg_mask: Option<PathBuf>,
    pub bg_mask: Option<PathBuf>,
    /// 内部を確定前景にする多角形。`[[x,y,x,y,...], ...]` の形で複数書ける
    pub fg_polygons: Option<Vec<Vec<f64>>>,
    pub bg_polygons: Option<Vec<Vec<f64>>>,
    pub tolerance: Option<f64>,
    pub border: Option<u32>,
    pub cleanup: Option<u32>,
    pub feather: Option<u32>,
    pub despill: Option<bool>,
    pub refine: Option<bool>,
    /// 境界のアルファの解き方（"projection" / "guided"）
    pub matting: Option<String>,
    /// 帯の中の二値輪郭に掛けるメディアンの半径(px, 長辺 1000px 換算)
    pub smooth_contour: Option<f64>,
    pub reclassify: Option<bool>,
    /// 背景のモデル（"auto" / "flat" / "field"）
    pub background_model: Option<String>,
    /// セグメンテーションモデルの使い方（"off" / "auto" / "isnet"）。
    ///
    /// **数百点に一律で付ける値ではない。** 1 件あたり推論だけで 1.3 秒
    /// かかる。`auto` なら色で解ける画像は素通りするので、混ざった素材には
    /// そちらが向く（`defaults` に書いて、効いた件は `segment_ran` で分かる）
    pub segment: Option<String>,
    /// モデルの ONNX ファイル。`input` と同じ規則で仕様ファイルの場所を
    /// 基準に解決する
    pub model_path: Option<PathBuf>,
    /// 探索を kiri に任せるか。**書いた設定はその値に固定される**——
    /// tolerance を書いた項目では tolerance を探索しない（CLI で
    /// `--tolerance` を明示したときと同じ規約）
    pub optimize: Option<bool>,
    /// 埋め込み ICC を sRGB へ変換するか（既定 true）
    pub color_convert: Option<bool>,
    pub edge_threshold: Option<f64>,
    pub step_tolerance: Option<f64>,
    /// 実写に写っている影を**消す**側の許容量。足す側は `shadow` から下の 5 つ
    pub shadow_tolerance: Option<f64>,
    /// 落ち影を合成するか（"off" / "synth"）
    pub shadow: Option<String>,
    /// 影をずらす量 `[dx, dy]`(px、長辺 1000px 換算)
    pub shadow_offset: Option<[f64; 2]>,
    pub shadow_blur: Option<f64>,
    /// 影の色（"#RRGGBB"）
    pub shadow_color: Option<String>,
    pub shadow_opacity: Option<f64>,
    pub seal: Option<u32>,
    /// 切り抜いた後に時計回りへ回す角度(度)。**負値に意味がある**
    /// （反時計回り）ので、他の数値と違って 0 以上の検査は掛けない。
    ///
    /// **数値でも文字列でも書ける**（`90` と `"90"`、それに `"auto"`）。
    /// 型を `f64` に決めないのは `max_bytes` とまったく同じ事情で、エージェントが
    /// 書く JSON には両方が現れるうえ、`auto` は数値では表せない。`"90"` を
    /// SPEC_INVALID で断ると、CLI では通る書き方が spec でだけ通らない。解くのは
    /// `commands::batch`——CLI と同じ `cli::parse_rotate` を通し、読めない値は
    /// INVALID_ROTATE にする
    pub rotate: Option<serde_json::Value>,
    /// 規格の複合指定に名前を付けたもの（"amazon" など）。**未知の名前は
    /// `UNKNOWN_PROFILE` でその項目を落とす**——解くのは `commands::batch` で、
    /// CLI と同じ `profile::named` を通る。
    ///
    /// **`canvas` / `fill_ratio` / `format` / `background` / `flatten` /
    /// `max_bytes` を書いた項目では、書いたほうが勝つ**（CLI で明示したときと
    /// 同じ規約）。上書きが起きたら `PROFILE_OVERRIDDEN` が言う
    pub profile: Option<String>,
    pub canvas: Option<String>,
    pub fill_ratio: Option<f64>,
    pub format: Option<String>,
    pub quality: Option<f32>,
    pub effort: Option<u8>,
    /// 合否の条件。`--fail-on` とまったく同じ書式の文字列
    /// （`"default,halo_ratio>0.05"`）。**読めない値は `INVALID_FAIL_ON` で
    /// その項目を落とす**——解くのは `commands::batch` で、CLI と同じ
    /// `FailOn::parse` を通る
    pub fail_on: Option<String>,
    /// 出力の上限バイト数。**数値でも文字列でも書ける**（`512000` と `"500k"`）。
    ///
    /// 型を `u64` に決めないのは、エージェントが書く JSON に両方が現れるため
    /// である。`"500k"` を SPEC_INVALID で断ると、CLI では通る書き方が spec
    /// でだけ通らない。解くのは `commands::batch`——CLI と同じ
    /// `cli::parse_max_bytes` を通し、読めない値は INVALID_MAX_BYTES にする
    pub max_bytes: Option<serde_json::Value>,
    pub background: Option<String>,
    pub flatten: Option<bool>,
    /// 書き出す派生の並び。各要素は `--derive` と同じキーを持つオブジェクト
    /// （`{"width":1600,"format":"jpeg","quality":82,"max_bytes":"500k"}`）。
    ///
    /// **値は数値でも文字列でも読める。** `max_bytes` と同じ理由で、エージェントが
    /// 書く JSON には両方が現れる。解くのは `commands::batch`——CLI と同じ
    /// `DeriveSpec::set` を通し、読めない値は INVALID_DERIVATION にする。
    ///
    /// `sizes` / `formats` との同時指定は CLI と同じく断る
    pub derive: Option<Vec<serde_json::Value>>,
    /// 幅の並び。`formats` との直積で派生を組む
    pub sizes: Option<Vec<u32>>,
    /// 形式の並び（"avif" / "png" / "jpeg" / "jpg"）
    pub formats: Option<Vec<String>>,
    /// 派生のファイル名の付け方。既定は `{stem}_{width}.{ext}`
    pub naming: Option<String>,
}

impl ItemSettings {
    /// 項目の指定を優先しつつ既定値で埋める。
    pub fn merged_over(&self, defaults: &ItemSettings) -> ItemSettings {
        macro_rules! pick {
            ($($field:ident),+ $(,)?) => {
                ItemSettings {
                    $($field: self.$field.clone().or_else(|| defaults.$field.clone()),)+
                }
            };
        }
        pick!(
            bbox,
            normalized,
            fg_seeds,
            trimap,
            alpha_trimap,
            fg_mask,
            bg_mask,
            fg_polygons,
            bg_polygons,
            tolerance,
            border,
            cleanup,
            feather,
            despill,
            refine,
            matting,
            smooth_contour,
            reclassify,
            background_model,
            segment,
            model_path,
            optimize,
            color_convert,
            edge_threshold,
            step_tolerance,
            shadow_tolerance,
            shadow,
            shadow_offset,
            shadow_blur,
            shadow_color,
            shadow_opacity,
            seal,
            rotate,
            profile,
            canvas,
            fill_ratio,
            format,
            quality,
            effort,
            fail_on,
            max_bytes,
            background,
            flatten,
            derive,
            sizes,
            formats,
            naming,
        )
    }
}

/// 設定として受け付けるキー。綴り違いの検出に使う。
const SETTING_KEYS: &[&str] = &[
    "bbox",
    "normalized",
    "fg_seeds",
    "trimap",
    "alpha_trimap",
    "fg_mask",
    "bg_mask",
    "fg_polygons",
    "bg_polygons",
    "tolerance",
    "border",
    "cleanup",
    "feather",
    "despill",
    "refine",
    "matting",
    "smooth_contour",
    "reclassify",
    "background_model",
    "segment",
    "model_path",
    "optimize",
    "color_convert",
    "edge_threshold",
    "step_tolerance",
    "shadow_tolerance",
    "shadow",
    "shadow_offset",
    "shadow_blur",
    "shadow_color",
    "shadow_opacity",
    "seal",
    "rotate",
    "profile",
    "canvas",
    "fill_ratio",
    "format",
    "quality",
    "effort",
    "fail_on",
    "max_bytes",
    "background",
    "flatten",
    "derive",
    "sizes",
    "formats",
    "naming",
];
const ROOT_KEYS: &[&str] = &["set", "defaults", "items"];

/// `set` の中に書けるキー。**`SETTING_KEYS` は増えない**——`set` は項目の
/// 設定ではなく最上位の指定なので、`defaults` や項目に書かれたら未知のキーである。
const SET_KEYS: &[&str] = &["align", "fill_ratio"];

/// 仕様ファイルを読み込む。
pub fn load(path: &Path) -> Result<BatchSpec> {
    let text = std::fs::read_to_string(path).map_err(|e| {
        Error::new(
            ErrorCode::SpecUnreadable,
            format!("{} を読めません: {e}", path.display()),
        )
    })?;

    let raw: serde_json::Value = serde_json::from_str(&text).map_err(|e| {
        Error::new(
            ErrorCode::SpecInvalidJson,
            format!("{} は JSON として不正です: {e}", path.display()),
        )
    })?;

    validate_keys(&raw)?;

    let spec: BatchSpec = serde_json::from_value(raw).map_err(|e| {
        Error::new(
            ErrorCode::SpecInvalid,
            format!("{} の内容が不正です: {e}", path.display()),
        )
    })?;

    if spec.items.is_empty() {
        return Err(Error::new(ErrorCode::SpecEmpty, "items が空です")
            .with_hint("処理する画像を items に列挙してください"));
    }
    validate_set(&spec)?;
    Ok(spec)
}

/// `set` が噛み合わない spec を、**1 バイトも読む前に**断る。
///
/// 優先順位は **明示指定 > set > profile > 既定** の 1 本である。`fill_ratio` を
/// 書いた人はその値を望んでいるので、`set` が上から別の値を配るなら
/// 「指定したのに効かない」になる——数百点を書き切ってから仕上がりで気づく
/// 種類の失敗で、`cli.rs` 全体が避けてきたものである。**どちらを消すかは
/// 書いた人にしか決められない**ので、黙ってどちらかを勝たせずに断る。
///
/// 綴りと値域をここで見るのも同じ理由による。項目ごとに解くと、同じ 1 つの
/// 誤りが項目数ぶんのエラーになって返る。
fn validate_set(spec: &BatchSpec) -> Result<()> {
    let Some(set) = spec.set.as_ref() else {
        return Ok(());
    };
    set.align()?;
    set.target()?;

    let refuse = |location: &str| {
        Err(Error::new(
            ErrorCode::InvalidSet,
            format!("set と {location} の fill_ratio は同時に指定できません"),
        )
        .with_hint(
            "set は全点の占有率を 1 つの目標へ揃えるものなので、\
             項目ごとの fill_ratio と両立しません。どちらか一方を消してください",
        ))
    };
    if spec.defaults.fill_ratio.is_some() {
        return refuse("defaults");
    }
    if let Some(i) = spec
        .items
        .iter()
        .position(|item| item.settings.fill_ratio.is_some())
    {
        return refuse(&format!("items[{i}]"));
    }
    Ok(())
}

/// 未知のキーを拾ってエラーにする。serde の flatten では検出できないため自前で行う。
fn validate_keys(raw: &serde_json::Value) -> Result<()> {
    let object = raw.as_object().ok_or_else(|| {
        Error::new(
            ErrorCode::SpecInvalid,
            "仕様ファイルの最上位はオブジェクトである必要があります",
        )
    })?;

    check(object.keys(), ROOT_KEYS, "最上位")?;

    // **`set` の中も同じ `check` を通す。** 綴り違いを黙って無視すると、
    // `fill_ration` と書いた spec が「中央値で揃えた」結果を返してしまう
    if let Some(set) = object.get("set") {
        let s = set.as_object().ok_or_else(|| {
            Error::new(
                ErrorCode::SpecInvalid,
                "set はオブジェクトである必要があります",
            )
        })?;
        check(s.keys(), SET_KEYS, "set")?;
    }

    if let Some(defaults) = object.get("defaults") {
        let d = defaults.as_object().ok_or_else(|| {
            Error::new(
                ErrorCode::SpecInvalid,
                "defaults はオブジェクトである必要があります",
            )
        })?;
        check(d.keys(), SETTING_KEYS, "defaults")?;
    }

    let items = object.get("items").and_then(|v| v.as_array());
    for (i, item) in items.into_iter().flatten().enumerate() {
        let o = item.as_object().ok_or_else(|| {
            Error::new(
                ErrorCode::SpecInvalid,
                format!("items[{i}] はオブジェクトである必要があります"),
            )
        })?;
        let mut allowed: Vec<&str> = SETTING_KEYS.to_vec();
        allowed.extend_from_slice(&["input", "output"]);
        check(o.keys(), &allowed, &format!("items[{i}]"))?;
    }
    Ok(())
}

fn check<'a>(
    keys: impl Iterator<Item = &'a String>,
    allowed: &[&str],
    location: &str,
) -> Result<()> {
    for key in keys {
        if allowed.contains(&key.as_str()) {
            continue;
        }
        let suggestion = closest(key, allowed);
        let mut err = Error::new(
            ErrorCode::SpecUnknownField,
            format!("{location} に未知のキー '{key}' があります"),
        );
        err = match suggestion {
            Some(s) => err.with_hint(format!("'{s}' の綴り違いではありませんか")),
            None => err.with_hint(format!(
                "指定できるキー: {}",
                allowed
                    .iter()
                    .collect::<BTreeSet<_>>()
                    .iter()
                    .copied()
                    .copied()
                    .collect::<Vec<_>>()
                    .join(", ")
            )),
        };
        return Err(err);
    }
    Ok(())
}

/// 綴り違いの候補を探す。編集距離 2 以内で最も近いものを返す。
fn closest(key: &str, allowed: &[&str]) -> Option<String> {
    allowed
        .iter()
        .map(|c| (edit_distance(key, c), *c))
        .filter(|(d, _)| *d <= 2)
        .min_by_key(|(d, _)| *d)
        .map(|(_, c)| c.to_string())
}

fn edit_distance(a: &str, b: &str) -> usize {
    let (a, b): (Vec<char>, Vec<char>) = (a.chars().collect(), b.chars().collect());
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    let mut cur = vec![0usize; b.len() + 1];
    for (i, ca) in a.iter().enumerate() {
        cur[0] = i + 1;
        for (j, cb) in b.iter().enumerate() {
            let cost = usize::from(ca != cb);
            cur[j + 1] = (prev[j + 1] + 1).min(cur[j] + 1).min(prev[j] + cost);
        }
        std::mem::swap(&mut prev, &mut cur);
    }
    prev[b.len()]
}

/// 仕様ファイル中の相対パスを解決する基準ディレクトリ。
///
/// 既定では仕様ファイルのある場所を基準にする。AI は画像の隣に仕様を書き出すのが
/// 自然であり、実行時のカレントディレクトリに依存しないほうが再現性が高いため。
pub fn base_dir(spec_path: &Path, override_dir: Option<&Path>) -> PathBuf {
    match override_dir {
        Some(d) => d.to_path_buf(),
        None => spec_path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .map(Path::to_path_buf)
            .unwrap_or_else(|| PathBuf::from(".")),
    }
}

pub fn resolve(base: &Path, path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        base.join(path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec_from(json: &str) -> Result<BatchSpec> {
        let raw: serde_json::Value = serde_json::from_str(json).unwrap();
        validate_keys(&raw)?;
        Ok(serde_json::from_value(raw).unwrap())
    }

    #[test]
    fn a_minimal_spec_parses() {
        let s = spec_from(r#"{"items":[{"input":"a.jpg","output":"a.avif"}]}"#).unwrap();
        assert_eq!(s.items.len(), 1);
        assert_eq!(s.items[0].input, PathBuf::from("a.jpg"));
        assert!(s.items[0].settings.tolerance.is_none());
    }

    #[test]
    fn item_settings_override_defaults() {
        let s = spec_from(
            r#"{"defaults":{"tolerance":12,"canvas":"1000x1000"},
                 "items":[{"input":"a.jpg","output":"a.avif","tolerance":5}]}"#,
        )
        .unwrap();
        let merged = s.items[0].settings.merged_over(&s.defaults);
        assert_eq!(merged.tolerance, Some(5.0), "項目の指定が優先されるべき");
        assert_eq!(
            merged.canvas.as_deref(),
            Some("1000x1000"),
            "既定値が引き継がれるべき"
        );
    }

    #[test]
    fn defaults_apply_when_the_item_says_nothing() {
        let s = spec_from(
            r#"{"defaults":{"tolerance":9},"items":[{"input":"a.jpg","output":"a.avif"}]}"#,
        )
        .unwrap();
        assert_eq!(
            s.items[0].settings.merged_over(&s.defaults).tolerance,
            Some(9.0)
        );
    }

    /// 画像で渡す指示は 2 つの入口を別のキーで受ける。
    ///
    /// **`trimap` と `alpha_trimap` は同じファイルの 2 通りの読み方**なので、
    /// 片方だけが spec から渡せると「CLI では選べるのに batch では選べない」
    /// 差が生まれる。
    #[test]
    fn both_trimap_entries_come_through_the_spec() {
        let s = spec_from(
            r#"{"defaults":{"alpha_trimap":"cut.png"},
                 "items":[{"input":"a.jpg","output":"a.avif","trimap":"t.png"}]}"#,
        )
        .unwrap();
        let merged = s.items[0].settings.merged_over(&s.defaults);
        assert_eq!(merged.trimap, Some(PathBuf::from("t.png")));
        assert_eq!(merged.alpha_trimap, Some(PathBuf::from("cut.png")));
    }

    /// AI が生成した JSON の綴り違いを黙って無視しないこと。
    #[test]
    fn a_misspelled_key_is_rejected_with_a_suggestion() {
        let err = spec_from(r#"{"items":[{"input":"a.jpg","output":"a.avif","tolerence":5}]}"#)
            .unwrap_err();
        assert_eq!(err.code.as_str(), "SPEC_UNKNOWN_FIELD");
        assert!(err.message.contains("tolerence"));
        assert!(
            err.hint.unwrap().contains("tolerance"),
            "綴り違いの候補を示すべき"
        );
    }

    #[test]
    fn an_unknown_key_without_a_near_match_lists_the_valid_ones() {
        let err = spec_from(r#"{"items":[{"input":"a.jpg","output":"a.avif","sharpen":true}]}"#)
            .unwrap_err();
        assert_eq!(err.code.as_str(), "SPEC_UNKNOWN_FIELD");
        let hint = err.hint.unwrap();
        assert!(hint.contains("tolerance") && hint.contains("canvas"));
    }

    #[test]
    fn unknown_keys_are_caught_at_every_level() {
        for json in [
            r#"{"item":[]}"#,
            r#"{"defaults":{"nope":1},"items":[]}"#,
            r#"{"items":[{"input":"a","output":"b","nope":1}]}"#,
        ] {
            let err = spec_from(json).unwrap_err();
            assert_eq!(err.code.as_str(), "SPEC_UNKNOWN_FIELD", "見逃した: {json}");
        }
    }

    #[test]
    fn edit_distance_behaves() {
        assert_eq!(edit_distance("tolerance", "tolerance"), 0);
        assert_eq!(edit_distance("tolerence", "tolerance"), 1);
        assert_eq!(edit_distance("", "abc"), 3);
    }

    #[test]
    fn relative_paths_resolve_against_the_spec_directory() {
        let base = base_dir(Path::new("/work/shoot/spec.json"), None);
        assert_eq!(base, PathBuf::from("/work/shoot"));
        assert_eq!(
            resolve(&base, Path::new("a.jpg")),
            PathBuf::from("/work/shoot/a.jpg")
        );
    }

    #[test]
    fn absolute_paths_are_left_alone() {
        let base = base_dir(Path::new("/work/spec.json"), None);
        assert_eq!(
            resolve(&base, Path::new("/other/a.jpg")),
            PathBuf::from("/other/a.jpg")
        );
    }

    #[test]
    fn a_bare_spec_filename_resolves_against_the_current_directory() {
        let base = base_dir(Path::new("spec.json"), None);
        assert_eq!(base, PathBuf::from("."));
    }

    #[test]
    fn base_dir_can_be_overridden() {
        let base = base_dir(Path::new("/work/spec.json"), Some(Path::new("/images")));
        assert_eq!(
            resolve(&base, Path::new("a.jpg")),
            PathBuf::from("/images/a.jpg")
        );
    }

    /// serde の derive が `deserialize_struct` へ渡すフィールド名（rename 適用後）を
    /// 横取りする。serde に reflection は無いが、derive は受け付ける名前の一覧を
    /// `&'static [&'static str]` で必ず渡してくるので、それを読んで即座に止める
    fn serde_field_names<T: for<'de> Deserialize<'de>>() -> &'static [&'static str] {
        use serde::de::{self, Deserializer, Visitor};

        struct Probe(Option<&'static [&'static str]>);

        #[derive(Debug)]
        struct Stop;
        impl std::fmt::Display for Stop {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str("stop")
            }
        }
        impl std::error::Error for Stop {}
        impl de::Error for Stop {
            fn custom<M: std::fmt::Display>(_: M) -> Self {
                Stop
            }
        }

        impl<'de> Deserializer<'de> for &mut Probe {
            type Error = Stop;
            fn deserialize_any<V: Visitor<'de>>(self, _: V) -> std::result::Result<V::Value, Stop> {
                Err(Stop)
            }
            fn deserialize_struct<V: Visitor<'de>>(
                self,
                _: &'static str,
                fields: &'static [&'static str],
                _: V,
            ) -> std::result::Result<V::Value, Stop> {
                self.0 = Some(fields);
                Err(Stop)
            }
            serde::forward_to_deserialize_any! {
                bool i8 i16 i32 i64 i128 u8 u16 u32 u64 u128 f32 f64 char str string
                bytes byte_buf option unit unit_struct newtype_struct seq tuple
                tuple_struct map enum identifier ignored_any
            }
        }

        let mut probe = Probe(None);
        let _ = T::deserialize(&mut probe);
        probe.0.expect("derive(Deserialize) の構造体であるべき")
    }

    /// `set` のキー表も serde が読むものと一致すること。
    ///
    /// `SETTING_KEYS` と同じ理由でずれを止める。ずれると「検査は通るのに値が
    /// 入らない」か「正しいキーが未知と言われる」になる。
    #[test]
    fn set_keys_are_exactly_what_serde_reads() {
        let mut serde_keys = serde_field_names::<SetSpec>().to_vec();
        let mut ours = SET_KEYS.to_vec();
        serde_keys.sort_unstable();
        ours.sort_unstable();
        assert_eq!(ours, serde_keys);
    }

    /// `align` の綴りは `NAMES` が配るものと過不足なく一致する。
    ///
    /// 断るときの hint はこの表から組まれるので、**表に無い綴りが通ると
    /// 「指定できる値」の案内が嘘になる。**
    #[test]
    fn every_named_alignment_parses_and_spells_itself_back() {
        for name in SetAlign::NAMES {
            let parsed = SetAlign::parse(name).unwrap_or_else(|| panic!("{name} が解けない"));
            assert_eq!(parsed.as_str(), *name);
        }
        assert!(SetAlign::parse("heigth").is_none(), "綴り違いが通った");
        assert!(SetAlign::parse("HEIGHT").is_none(), "大文字が通った");
    }

    /// `f_i` の式が `canvas::plan()` の狙いどおりの倍率を返すこと。
    ///
    /// **ここが式そのものの検査である。** `plan()` は
    /// `scale = min(CW*f/cw, CH*f/ch)` を返すので、`f_i` を入れた結果が
    /// 狙った `s_i` に一致するかを両方の揃え方で確かめる——
    ///
    /// - `bbox` は `s_i = T*min(CW/cw, CH/ch)`（外接矩形が枠へ収まる）
    /// - `height` は `s_i = T*CH/ch`（高さが `T*CH` になる）
    ///
    /// 出力を測る検査（tests/cli.rs）は丸めの後しか見られないので、
    /// 式の誤りを 1px の差として受け取ることになる。こちらは実数のまま見る。
    #[test]
    fn the_fill_ratio_formula_produces_the_intended_scale() {
        let plan_scale = |f: f64, content: (u32, u32), canvas: (u32, u32)| -> f64 {
            let (cw, ch) = (f64::from(content.0), f64::from(content.1));
            (f64::from(canvas.0) * f / cw).min(f64::from(canvas.1) * f / ch)
        };
        let canvas = (1000u32, 1000u32);
        // 縦長・正方・横長・極端な横長
        for content in [(300u32, 900u32), (500, 500), (800, 400), (900, 200)] {
            let (cw, ch) = (f64::from(content.0), f64::from(content.1));
            for target in [0.3, 0.5, 0.85, 1.0] {
                let bbox = SetPlacement {
                    align: SetAlign::Bbox,
                    target,
                };
                assert!(
                    (bbox.fill_ratio(content, canvas) - target).abs() < 1e-12,
                    "bbox の f_i が T と違う: {content:?} T={target}"
                );
                let want = target * (f64::from(canvas.0) / cw).min(f64::from(canvas.1) / ch);
                let got = plan_scale(bbox.fill_ratio(content, canvas), content, canvas);
                assert!((got - want).abs() < 1e-9, "bbox: {got} != {want}");

                let height = SetPlacement {
                    align: SetAlign::Height,
                    target,
                };
                let f = height.fill_ratio(content, canvas);
                let want = target * f64::from(canvas.1) / ch;
                let got = plan_scale(f, content, canvas);
                assert!(
                    (got - want).abs() < 1e-9,
                    "height: {content:?} T={target} f={f} -> {got} != {want}"
                );
                // 高さは狙いどおり `T*CH` になる
                assert!((got * ch - target * f64::from(canvas.1)).abs() < 1e-9);
            }
        }
    }

    /// `set` と `fill_ratio` の同時指定は spec を読んだ時点で断る。
    #[test]
    fn a_set_next_to_a_fill_ratio_is_refused() {
        for json in [
            r#"{"set":{"align":"height"},"defaults":{"fill_ratio":0.8},
                 "items":[{"input":"a.jpg","output":"a.avif"}]}"#,
            r#"{"set":{"align":"height"},
                 "items":[{"input":"a.jpg","output":"a.avif","fill_ratio":0.8}]}"#,
        ] {
            let spec = spec_from(json).unwrap();
            let err = validate_set(&spec).unwrap_err();
            assert_eq!(err.code.as_str(), "INVALID_SET", "見逃した: {json}");
            assert_eq!(err.exit_code(), 2);
        }
    }

    /// `set.fill_ratio` の値域は `canvas::plan()` が課すものと同じ `(0, 1]`。
    #[test]
    fn a_set_target_outside_the_unit_interval_is_refused() {
        for bad in [0.0, -0.5, 1.5] {
            let set = SetSpec {
                align: "height".to_string(),
                fill_ratio: Some(bad),
            };
            assert_eq!(set.target().unwrap_err().code.as_str(), "INVALID_SET");
        }
        assert_eq!(
            SetSpec {
                align: "bbox".to_string(),
                fill_ratio: Some(0.85)
            }
            .target()
            .unwrap(),
            Some(0.85)
        );
    }

    /// 綴りの検査に使うキー表と、serde が実際に読むキーが一致すること。
    ///
    /// `pick!` は `..Default` 無しの構造体リテラルなので、フィールドの読み漏れは
    /// コンパイラが止める。残るずれ——キー表にだけある / フィールドにだけある /
    /// `#[serde(rename)]` で名前が変わった——はこの 1 本が止める。ずれると
    /// 「検査は通るのに値が入らない」か「正しいキーが未知と言われる」になる
    #[test]
    fn setting_keys_are_exactly_what_serde_reads() {
        let mut serde_keys = serde_field_names::<ItemSettings>().to_vec();
        let mut ours = SETTING_KEYS.to_vec();
        serde_keys.sort_unstable();
        ours.sort_unstable();
        assert_eq!(ours, serde_keys);
    }
}
