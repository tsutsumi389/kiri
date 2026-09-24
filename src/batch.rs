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
    /// 全項目に適用する既定値。項目側の指定が優先される
    #[serde(default)]
    pub defaults: ItemSettings,
    pub items: Vec<BatchItem>,
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
    /// （反時計回り）ので、他の数値と違って 0 以上の検査は掛けない
    pub rotate: Option<f64>,
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
const ROOT_KEYS: &[&str] = &["defaults", "items"];

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
    Ok(spec)
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
