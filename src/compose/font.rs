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
//! 使った字体の素性（パスと SHA-256）は結果 JSON に載せる。モデルの素性を
//! `settings` に載せているのと同じ扱いで、**後から「何で組まれたか」を辿れる**
//! ようにするためである。

use std::path::Path;

use resvg::usvg::fontdb;
use serde::Serialize;

use crate::compose::FontSpec;
use crate::error::{Error, ErrorCode, Result};
use crate::segment::sha256::Sha256;

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
pub fn resolve(spec: &FontSpec, base: &Path) -> Result<(fontdb::Database, ResolvedFont)> {
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

    let source = query(&db, &spec.family).and_then(|id| file_of(&db, id));
    let sha256 = source.as_deref().map(digest).transpose()?;
    Ok((
        db,
        ResolvedFont {
            family: spec.family.clone(),
            path: source.map(|p| p.display().to_string()),
            sha256,
        },
    ))
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
