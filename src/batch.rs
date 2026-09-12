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
    /// 埋め込み ICC を sRGB へ変換するか（既定 true）
    pub color_convert: Option<bool>,
    pub edge_threshold: Option<f64>,
    pub step_tolerance: Option<f64>,
    pub shadow_tolerance: Option<f64>,
    pub seal: Option<u32>,
    pub canvas: Option<String>,
    pub fill_ratio: Option<f64>,
    pub format: Option<String>,
    pub quality: Option<f32>,
    pub effort: Option<u8>,
    pub background: Option<String>,
    pub flatten: Option<bool>,
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
            color_convert,
            edge_threshold,
            step_tolerance,
            shadow_tolerance,
            seal,
            canvas,
            fill_ratio,
            format,
            quality,
            effort,
            background,
            flatten,
        )
    }
}

/// 設定として受け付けるキー。綴り違いの検出に使う。
const SETTING_KEYS: &[&str] = &[
    "bbox",
    "normalized",
    "fg_seeds",
    "trimap",
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
    "color_convert",
    "edge_threshold",
    "step_tolerance",
    "shadow_tolerance",
    "seal",
    "canvas",
    "fill_ratio",
    "format",
    "quality",
    "effort",
    "background",
    "flatten",
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
}
