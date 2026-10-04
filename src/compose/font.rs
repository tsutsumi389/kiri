//! フォントの解決。**黙って代替へ落ちない。**
//!
//! usvg は指定の字体が見つからなければ既定の字体で組む。**それを通すと、同じ
//! spec が機械ごとに違う絵になる**——kiri の中核の約束（同じ版が同じ入力から
//! 同じ結果を出す）に正面から触る。計画 §10.2 で実測したときも、存在しない
//! `NoSuchFontXYZ` を指定した行が指定どおりの行高で描けてしまった。
//!
//! だから kiri は**描く前に自分で引き、見つからなければ `FONT_NOT_FOUND` で
//! 断る。** `SEGMENT_UNAVAILABLE` が feature の無い build で黙って off へ
//! 落ちないのと同じ向きで、新しい作法ではない。
//!
//! # family を確かめるだけでは足りない
//!
//! 「family が在る」ことと「その family で組まれる」ことは別である。システムの
//! フォント群をまるごと usvg へ渡すと、**グリフ単位で他の字体へ落ちる**——
//! `Helvetica` を指定した spec の日本語が、別の和文フォントで描かれ、しかも
//! 結果 JSON は Helvetica のパスとダイジェストを名乗る（計画 §10.11 の H4）。
//! 素性の記録が嘘になるので、**渡す前に家族以外を落とす。**
//!
//! 絞ったうえで無い文字は豆腐（.notdef）として描けてしまうので、**グリフの
//! 有無も描く前に見る**（同 H3）。
//!
//! 使った字体の素性（パスと SHA-256）は結果 JSON に載せる。モデルの素性を
//! `settings` に載せているのと同じ扱いで、**後から「何で組まれたか」を辿れる**
//! ようにするためである。

use std::path::Path;
use std::sync::Arc;

use resvg::usvg::fontdb;
use serde::Serialize;

use crate::compose::{ComposeSpec, FontSpec, Layer};
use crate::error::{Error, ErrorCode, Result};
use crate::segment::sha256::Sha256;
use crate::warning::{Warning, WarningCode};

/// 解決した字体。**結果 JSON にそのまま出る。**
#[derive(Debug, Clone, Serialize)]
pub struct ResolvedFont {
    /// spec が求めた family。usvg へもこの名前で渡す
    pub family: String,
    /// 実際に読んだファイル。システムから引いた場合もここに実体が出る。
    ///
    /// **`None` になるのは、字体がファイルとして辿れない場合だけである**
    /// （メモリ上の face）。キーは常に出す——省くと「辿れなかった」と
    /// 「そもそも報告していない」が同じ形になる
    pub path: Option<String>,
    /// 読んだファイルの SHA-256。`path` が `None` なら `None`
    pub sha256: Option<String>,
}

/// 字体を 1 つに決める。
///
/// **`path` があればそれしか読まない。** システムのフォント群を一緒に積むと、
/// spec が指した family がそのファイルに無いときに、システム側の同名の字体で
/// 黙って組まれる。「このファイルで組む」と書いた指定の意味が消える。
pub fn resolve(
    spec: &FontSpec,
    base: &Path,
    document: &ComposeSpec,
    warnings: &mut Vec<Warning>,
) -> Result<(Arc<fontdb::Database>, ResolvedFont)> {
    let mut db = fontdb::Database::new();

    match &spec.path {
        Some(path) => {
            let path = base.join(path);
            db.load_font_file(&path).map_err(|e| {
                Error::new(
                    ErrorCode::FontUnreadable,
                    format!("{} を字体として読めません: {e}", path.display()),
                )
                .with_hint("TrueType / OpenType のファイルを指してください")
            })?;
            // **ファイルは読めたが、求めた family がその中に無い**ことがある。
            // ここを見ないと、名前の綴り違いが「読めたので成功」になる
            if query(&db, &spec.family).is_none() {
                return Err(Error::new(
                    ErrorCode::FontNotFound,
                    format!(
                        "{} に family '{}' がありません",
                        path.display(),
                        spec.family
                    ),
                )
                .with_hint(format!(
                    "このファイルが名乗る family: {}",
                    families(&db).join(", ")
                )));
            }
        }
        None => {
            db.load_system_fonts();
            if query(&db, &spec.family).is_none() {
                return Err(Error::new(
                    ErrorCode::FontNotFound,
                    format!("family '{}' がこの環境にありません", spec.family),
                )
                .with_hint(
                    "font.path でファイルを直接指すと、環境に依らず同じ字体で組めます\
                     （kiri は代替の字体へ落としません）",
                ));
            }
        }
    }

    // **ここから先、db にはこの family しか残らない。** 残すとグリフ単位で
    // 他の字体へ落ちる（モジュールの doc）
    retain_family(&mut db, &spec.family);

    let source = query(&db, &spec.family).and_then(|id| file_of(&db, id));
    let sha256 = source.as_deref().map(digest).transpose()?;

    report_missing_glyphs(&db, spec, document, warnings);

    Ok((
        Arc::new(db),
        ResolvedFont {
            family: spec.family.clone(),
            path: source.map(|p| p.display().to_string()),
            sha256,
        },
    ))
}

/// 求めた family 以外の face を落とす。
fn retain_family(db: &mut fontdb::Database, family: &str) {
    let strangers: Vec<fontdb::ID> = db
        .faces()
        .filter(|face| {
            !face
                .families
                .iter()
                .any(|(name, _)| name.eq_ignore_ascii_case(family))
        })
        .map(|face| face.id)
        .collect();
    for id in strangers {
        db.remove_face(id);
    }
}

/// spec が書いた文字のうち、この字体に無いものを報せる。
///
/// **断らずに警告にする。** 1 文字でも欠けたら組めない、とするには強すぎる
/// ——記号 1 つのために spec 全体が止まる。一方で黙って豆腐を並べるのは、
/// この段が最も避けたい「黙って別のものが出る」そのものなので、
/// `FONT_GLYPHS_MISSING` で名指しする。
fn report_missing_glyphs(
    db: &fontdb::Database,
    spec: &FontSpec,
    document: &ComposeSpec,
    warnings: &mut Vec<Warning>,
) {
    let Some(id) = query(db, &spec.family) else {
        return;
    };
    let mut wanted: Vec<char> = document
        .layers
        .iter()
        .filter_map(|l| match l {
            Layer::Text(t) => Some(t.lines.iter()),
            Layer::Image(_) => None,
        })
        .flatten()
        .flat_map(|line| line.chars())
        // 空白は字面を持たない字体が普通にある。**欠けとして数えない**
        .filter(|c| !c.is_whitespace())
        .collect();
    wanted.sort_unstable();
    wanted.dedup();

    let missing = db
        .with_face_data(id, |data, index| {
            let Ok(face) = ttf_parser::Face::parse(data, index) else {
                return Vec::new();
            };
            wanted
                .iter()
                .filter(|&&c| face.glyph_index(c).is_none())
                .copied()
                .collect::<Vec<char>>()
        })
        .unwrap_or_default();

    if missing.is_empty() {
        return;
    }
    let shown: String = missing.iter().collect();
    warnings.push(
        Warning::new(
            WarningCode::FontGlyphsMissing,
            format!(
                "'{}' に {} 文字のグリフがありません: {shown}",
                spec.family,
                missing.len()
            ),
        )
        .with_hint("描くと豆腐（.notdef）が並びます。その文字を持つ字体を指してください")
        .with_data("missing", shown)
        .with_data("count", missing.len()),
    );
}

/// この字体の、em に対する ascent の比。
///
/// **1 行目の天を `rect` の上端へ合わせるために要る。** ベースラインを
/// 「上端 + size」に置くと、ascent が em を超える字体（珍しくない）で
/// 字の天が枠の上へ出る——`text_overflow` は 4 辺を見るので、**枠の上端に
/// 置いただけの文字が毎回はみ出しを報告する。**
///
/// 読めなければ 0.8 を使う。ここで断らないのは、字体が在ることは既に
/// 確かめてあり、**比が読めないことは組めないことではない**ためである。
pub fn ascent_ratio(db: &fontdb::Database, family: &str) -> f64 {
    const FALLBACK: f64 = 0.8;
    let Some(id) = query(db, family) else {
        return FALLBACK;
    };
    db.with_face_data(id, |data, index| {
        let Ok(face) = ttf_parser::Face::parse(data, index) else {
            return FALLBACK;
        };
        let em = f64::from(face.units_per_em());
        if em <= 0.0 {
            return FALLBACK;
        }
        f64::from(face.ascender()) / em
    })
    .unwrap_or(FALLBACK)
}

/// family をそのまま引く。**太さは見ない。**
///
/// 引きたいのは「この family がこの環境に在るか」で、太さの合う face が在るかでは
/// ない。可変フォント 1 本で 100〜900 を賄う字体では、`Weight::BOLD` で引くと
/// 在るのに無いと答えることがある。太さの当てはめは usvg が自分で行う。
fn query(db: &fontdb::Database, family: &str) -> Option<fontdb::ID> {
    db.query(&fontdb::Query {
        families: &[fontdb::Family::Name(family)],
        weight: fontdb::Weight::NORMAL,
        stretch: fontdb::Stretch::Normal,
        style: fontdb::Style::Normal,
    })
}

/// その face の実体のパス。メモリ上の face なら `None`。
fn file_of(db: &fontdb::Database, id: fontdb::ID) -> Option<std::path::PathBuf> {
    match db.face(id).map(|f| &f.source) {
        Some(fontdb::Source::File(path)) => Some(path.clone()),
        _ => None,
    }
}

/// この db が名乗る family の一覧。**断るときの手がかりに添える。**
fn families(db: &fontdb::Database) -> Vec<String> {
    let mut names: Vec<String> = db
        .faces()
        .flat_map(|f| f.families.iter().map(|(name, _)| name.clone()))
        .collect();
    names.sort();
    names.dedup();
    names
}

fn digest(path: &Path) -> Result<String> {
    let bytes = std::fs::read(path).map_err(|e| {
        Error::new(
            ErrorCode::FontUnreadable,
            format!("{} を読めません: {e}", path.display()),
        )
    })?;
    let mut hasher = Sha256::new();
    hasher.update(&bytes);
    Ok(hasher.finish())
}
