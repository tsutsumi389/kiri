//! フォントの解決。**黙って代替へ落ちない。**
//!
//! usvg は指定の字体が見つからなければ既定の字体で組む。**それを通すと、同じ
//! spec が機械ごとに違う絵になる**——kiri の中核の約束（同じ版が同じ入力から
//! 同じ結果を出す）に正面から触る。実際、存在しない `NoSuchFontXYZ` を
//! 指定した行も、指定どおりの行高で描けてしまう。
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
//! 結果 JSON は Helvetica のパスとダイジェストを名乗る。
//! 素性の記録が嘘になるので、**渡す前に家族以外を落とす。**
//!
//! 絞ったうえで無い文字は豆腐（.notdef）として描けてしまうので、**グリフの
//! 有無も描く前に見る**。
//!
//! 使った字体の素性（パスと SHA-256）は結果 JSON に載せる。モデルの素性を
//! `settings` に載せているのと同じ扱いで、**後から「何で組まれたか」を辿れる**
//! ようにするためである。字体は層ごとに選べるので、素性も層ごとに
//! （`layers[].font`）載せる。最上位の `font` は最上位の指定の素性で、互換のために残す。
//!
//! # 太さも黙って代替しない
//!
//! usvg は weight に合う face が無ければ**最も近い face で組み、太さを合成しない。**
//! 1 ウェイトのファイル（`ヒラギノ角ゴシック W4.ttc`）を指して weight を 100 と
//! 900 にした 2 枚は、バイト単位で一致した（2026-10-14 のサムネ検証）。
//! family の代替と同じ穴なので、**実際に描かれる太さが spec と違えば
//! `FONT_WEIGHT_UNAVAILABLE` で名指しする。** 断らないのは、近い太さで組んだ絵は
//! 読めるもので、太さ 1 つのために spec 全体を止めるのは強すぎるからである
//! （`FONT_GLYPHS_MISSING` と同じ判断）。
//!
//! 可変フォントは wght 軸の範囲の中なら指定どおりに描かれる（usvg が
//! font-weight を wght へ当てる）。**軸を見ずに face の太さだけで比べると、
//! 可変フォントで毎回誤検出する**——`query` が太さを見ないのと同じ罠である。

use std::path::{Path, PathBuf};
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

/// 1 つの文字の層が実際に組まれた字体。**結果 JSON の `layers[].font` である。**
///
/// 最上位の `font`（`ResolvedFont`）と違い、**その層の weight で引いた face** を
/// 名乗る。同じ family でも W3 と W6 が別ファイルの字体では、層ごとに読む
/// ファイルが違う——最上位の 1 つだけでは「何で組まれたか」が辿れない。
#[derive(Debug, Clone, Serialize)]
pub struct UsedFont {
    /// `family` / `path` / `sha256`。**`ResolvedFont` と同じ約束で、同じ並びに出る**
    #[serde(flatten)]
    pub font: ResolvedFont,
    /// **実際に描かれる太さ**を 100 刻みへ丸めたもの。spec の weight ではない。
    ///
    /// 静的な字体ではその face の太さ（OS/2 の usWeightClass）、可変フォントでは
    /// spec の weight を wght 軸の範囲へ収めた値になる（`drawn_weight`）。spec の
    /// weight と違えば `FONT_WEIGHT_UNAVAILABLE` が出ている
    pub weight: u16,
}

/// 文字の層 1 つぶんの、組むための字体。
pub struct LayerFont {
    /// この層の字体だけを積んだ db（`retain_family` の後）
    pub db: Arc<fontdb::Database>,
    /// family は `used.font.family` が持つ
    pub used: UsedFont,
}

/// spec 全体の字体。
pub struct Fonts {
    /// 最上位の `font`。**書かれていなければ `None`**——層がすべて自分の
    /// `font` を持つ spec で、最上位の結果を作り話で埋めない
    pub top: Option<ResolvedFont>,
    /// `spec.layers` と同じ並び。文字の層にだけ `Some`
    pub layers: Vec<Option<LayerFont>>,
}

/// spec が書いた字体を全部決める。**1 画素も描く前に呼ぶ。**
///
/// 層の `font` は最上位の `font` より強く、書かない層は最上位を継ぐ。同じ指定
/// （family と path の組）は 1 度しか読まない——見出しと本文が同じ字体を
/// 書いても、ファイルを 2 度読まない。システムの字体も 1 度しか積まない。
pub fn resolve(document: &ComposeSpec, base: &Path, warnings: &mut Vec<Warning>) -> Result<Fonts> {
    let mut opened: Vec<(&FontSpec, Arc<fontdb::Database>)> = Vec::new();
    let mut system: Option<fontdb::Database> = None;

    // 同じファイルを 2 度読んで 2 度ハッシュしない。ヒラギノの TTC は数十 MB ある
    let mut digests: Vec<(PathBuf, String)> = Vec::new();

    let top = match &document.font {
        Some(spec) => {
            let db = open_once(spec, "font", base, &mut opened, &mut system)?;
            let face = query(&db, &spec.family);
            Some(provenance(&spec.family, &db, face, &mut digests)?)
        }
        None => None,
    };

    // 層ごとの指定を先に全部開き、**その層の weight で組まれる face** まで決める。
    // グリフの欠けは face ごとに、その face で組む層の文字だけで見るので、
    // どの層がどの face かが決まってからでないと数えられない
    let mut chosen: Vec<Option<Chosen>> = Vec::with_capacity(document.layers.len());
    for (i, layer) in document.layers.iter().enumerate() {
        let Layer::Text(text) = layer else {
            chosen.push(None);
            continue;
        };
        // `compose::validate` が「どちらにも無い」を既に断っている。**それでも
        // unwrap しない**——届かないはずの経路が届いたときに、code で言う
        let spec = text
            .font
            .as_ref()
            .or(document.font.as_ref())
            .ok_or_else(|| {
                Error::new(
                    ErrorCode::FontNotFound,
                    format!("'{}' を組むための字体が決まっていません", text.id),
                )
            })?;
        let whose = format!("layers[{i}] ({}) の font", text.id);
        let db = open_once(spec, &whose, base, &mut opened, &mut system)?;
        let face = query_weight(&db, &spec.family, text.weight);
        chosen.push(Some(Chosen {
            family: &spec.family,
            db,
            face,
        }));
    }

    // **W3 と W6 が別ファイルの字体では、収める文字が face ごとに違いうる。**
    // weight 400 の face だけで見ると、太い層の豆腐を見逃す
    let mut checked: Vec<(*const fontdb::Database, fontdb::ID)> = Vec::new();
    for c in chosen.iter().flatten() {
        let Some(face) = c.face else {
            continue;
        };
        let key = (Arc::as_ptr(&c.db), face);
        if checked.contains(&key) {
            continue;
        }
        checked.push(key);
        let lines = document
            .layers
            .iter()
            .zip(&chosen)
            .filter_map(|(layer, other)| match (layer, other) {
                (Layer::Text(t), Some(o))
                    if (Arc::as_ptr(&o.db), o.face) == (key.0, Some(face)) =>
                {
                    Some(t.lines.iter())
                }
                _ => None,
            })
            .flatten();
        report_missing_glyphs(&c.db, face, c.family, lines, warnings);
    }

    let mut layers = Vec::with_capacity(document.layers.len());
    for (layer, c) in document.layers.iter().zip(chosen) {
        let (Layer::Text(text), Some(c)) = (layer, c) else {
            layers.push(None);
            continue;
        };
        let font = provenance(c.family, &c.db, c.face, &mut digests)?;
        let weight = c
            .face
            .map_or(text.weight, |id| drawn_weight(&c.db, id, text.weight));
        if weight != text.weight {
            warnings.push(weight_warning(
                &text.id,
                c.family,
                text.weight,
                weight,
                font.path.as_deref(),
            ));
        }
        layers.push(Some(LayerFont {
            used: UsedFont { font, weight },
            db: c.db,
        }));
    }

    Ok(Fonts { top, layers })
}

/// 1 つの文字の層について、組む前に決めたもの。
struct Chosen<'a> {
    family: &'a str,
    db: Arc<fontdb::Database>,
    /// その層の weight で引いた face。family は `open` が確かめてあるので、
    /// `None` は字体がメモリ上にしか無いような届かない経路である
    face: Option<fontdb::ID>,
}

/// face の素性（実体のパスと SHA-256）を、結果 JSON に出す形に組む。
fn provenance(
    family: &str,
    db: &fontdb::Database,
    face: Option<fontdb::ID>,
    digests: &mut Vec<(PathBuf, String)>,
) -> Result<ResolvedFont> {
    let source = face.and_then(|id| file_of(db, id));
    let sha256 = source
        .as_deref()
        .map(|p| cached_digest(p, digests))
        .transpose()?;
    Ok(ResolvedFont {
        family: family.to_string(),
        path: source.map(|p| p.display().to_string()),
        sha256,
    })
}

/// 読んだことのあるファイルならそのダイジェストを返す。
fn cached_digest(path: &Path, cache: &mut Vec<(PathBuf, String)>) -> Result<String> {
    if let Some((_, d)) = cache.iter().find(|(p, _)| p == path) {
        return Ok(d.clone());
    }
    let d = digest(path)?;
    cache.push((path.to_path_buf(), d.clone()));
    Ok(d)
}

/// 同じ指定を既に開いていればそれを返し、無ければ開いて覚える。
fn open_once<'a>(
    spec: &'a FontSpec,
    whose: &str,
    base: &Path,
    opened: &mut Vec<(&'a FontSpec, Arc<fontdb::Database>)>,
    system: &mut Option<fontdb::Database>,
) -> Result<Arc<fontdb::Database>> {
    if let Some((_, db)) = opened.iter().find(|(s, _)| *s == spec) {
        return Ok(Arc::clone(db));
    }
    let db = Arc::new(open(spec, whose, base, system)?);
    opened.push((spec, Arc::clone(&db)));
    Ok(db)
}

/// 字体を 1 つ開き、**その family だけを残した db** を返す。
///
/// **`path` があればそれしか読まない。** システムのフォント群を一緒に積むと、
/// spec が指した family がそのファイルに無いときに、システム側の同名の字体で
/// 黙って組まれる。「このファイルで組む」と書いた指定の意味が消える。
fn open(
    spec: &FontSpec,
    whose: &str,
    base: &Path,
    system: &mut Option<fontdb::Database>,
) -> Result<fontdb::Database> {
    let mut db = match &spec.path {
        Some(path) => {
            let path = base.join(path);
            let mut db = fontdb::Database::new();
            db.load_font_file(&path).map_err(|e| {
                Error::new(
                    ErrorCode::FontUnreadable,
                    format!("{whose}: {} を字体として読めません: {e}", path.display()),
                )
                .with_hint("TrueType / OpenType のファイルを指してください")
            })?;
            // **ファイルは読めたが、求めた family がその中に無い**ことがある。
            // ここを見ないと、名前の綴り違いが「読めたので成功」になる
            if query(&db, &spec.family).is_none() {
                return Err(Error::new(
                    ErrorCode::FontNotFound,
                    format!(
                        "{whose}: {} に family '{}' がありません",
                        path.display(),
                        spec.family
                    ),
                )
                .with_hint(format!(
                    "このファイルが名乗る family: {}",
                    families(&db).join(", ")
                )));
            }
            db
        }
        None => {
            // **システムの字体は 1 度だけ積み、指定ごとに複製する。** 積むのは
            // 数百のファイルを開く仕事で、層の数だけ繰り返す理由が無い
            let db = system
                .get_or_insert_with(|| {
                    let mut db = fontdb::Database::new();
                    db.load_system_fonts();
                    db
                })
                .clone();
            if query(&db, &spec.family).is_none() {
                return Err(Error::new(
                    ErrorCode::FontNotFound,
                    format!("{whose}: family '{}' がこの環境にありません", spec.family),
                )
                .with_hint(format!(
                    "{}.path でファイルを直接指すと、環境に依らず同じ字体で組めます\
                     （kiri は代替の字体へ落としません）",
                    if whose == "font" {
                        "font"
                    } else {
                        "layers[].font"
                    }
                )));
            }
            db
        }
    };

    // **ここから先、db にはこの family しか残らない。** 残すとグリフ単位で
    // 他の字体へ落ちる（モジュールの doc）
    retain_family(&mut db, &spec.family);
    Ok(db)
}

/// 指定と違う太さで描かれることを報せる。
fn weight_warning(
    layer: &str,
    family: &str,
    requested: u16,
    actual: u16,
    path: Option<&str>,
) -> Warning {
    let mut warning = Warning::new(
        WarningCode::FontWeightUnavailable,
        format!(
            "'{layer}' の weight {requested} に合う face が '{family}' に無く、\
             {actual} で描かれます"
        ),
    )
    .with_hint(format!(
        "weight {requested} の face を持つファイルを layers[].font で指すか、\
         weight を {actual} にしてください（kiri は太さを合成しません）"
    ))
    .with_data("layer", layer)
    .with_data("family", family)
    .with_data("requested", requested)
    .with_data("actual", actual);
    if let Some(path) = path {
        warning = warning.with_data("path", path);
    }
    warning
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
///
/// `lines` は**この face で組む層の行だけ**である。別の字体で組む層の文字を
/// 数えると、和文の層を持つ spec で欧文の字体に毎回欠けを言う。
fn report_missing_glyphs<'a>(
    db: &fontdb::Database,
    id: fontdb::ID,
    family: &str,
    lines: impl Iterator<Item = &'a String>,
    warnings: &mut Vec<Warning>,
) {
    let mut wanted: Vec<char> = lines
        .flat_map(|line| line.chars())
        // 空白は字面を持たない字体が普通にある。**欠けとして数えない**
        .filter(|c| !c.is_whitespace())
        .collect();
    wanted.sort_unstable();
    wanted.dedup();
    if wanted.is_empty() {
        return;
    }

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
                "'{family}' に {} 文字のグリフがありません: {shown}",
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
    query_weight(db, family, fontdb::Weight::NORMAL.0)
}

/// usvg が組むときと**同じ引き方**で、その weight の face を引く。
///
/// usvg は font-weight をそのまま `fontdb::Query` の weight へ渡し、CSS の規則で
/// 最も近い face を選ぶ（`usvg::text::default_font_selector`）。db にはこの family
/// しか残っていないので、ここで引いた face が実際に組まれる face である。
fn query_weight(db: &fontdb::Database, family: &str, weight: u16) -> Option<fontdb::ID> {
    db.query(&fontdb::Query {
        families: &[fontdb::Family::Name(family)],
        weight: fontdb::Weight(weight),
        stretch: fontdb::Stretch::Normal,
        style: fontdb::Style::Normal,
    })
}

/// その face で `requested` を求めたとき、**実際に描かれる太さ**を、spec に
/// 書ける 100 刻みへ丸めて返す。
///
/// wght 軸を持つ face は、軸の範囲へ収めた値で描かれる（usvg が font-weight を
/// wght の位置へ当て、範囲の外は端で止まる）。軸を持たない face は、face
/// そのものの太さ（usWeightClass）でしか描けない。
///
/// **丸めるのは、返した値が spec に書ける値でなければならないから**である。
/// 275（Avenir Next の Light）をそのまま返すと、weight 300 の層——実際に最も近い
/// face で描かれている——が毎回警告を出し、しかも「275 にせよ」という書けない手を
/// 勧める。
///
/// **CSS の単位でない wght 軸（Apple の GX 由来で 0.48〜3.2 のようなもの）は
/// 軸として読まない。** clamp すると「3」が返り、書けない手を勧める。その face の
/// 太さで代える。
fn drawn_weight(db: &fontdb::Database, id: fontdb::ID, requested: u16) -> u16 {
    let axis = db
        .with_face_data(id, |data, index| {
            let face = ttf_parser::Face::parse(data, index).ok()?;
            face.variation_axes()
                .into_iter()
                .find(|a| a.tag == ttf_parser::Tag::from_bytes(b"wght"))
                .map(|a| (a.min_value, a.max_value))
        })
        .flatten()
        .filter(|&(lo, hi)| lo >= 1.0 && hi <= 1000.0);
    let raw = match axis {
        Some((lo, hi)) => f32::from(requested).clamp(lo, hi),
        None => db
            .face(id)
            .map_or(f32::from(requested), |f| f32::from(f.weight.0)),
    };
    ((raw / 100.0).round() * 100.0).clamp(100.0, 900.0) as u16
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
