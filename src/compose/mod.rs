//! `kiri compose` — **素材と文字を 1 枚へ組む。組版器ではなく合成器である。**
//!
//! どこに何を置くかは spec が決める。kiri は置いて、**測って、言う**
//! （計画 §10）。収まらない文字を黙って縮めることも、長い行を黙って折り返すことも
//! しない——どちらも「揃えたくて spec を書いた側の指定が消える」向きの親切で、
//! `--fill-ratio` が商品を枠へ合わせるのとは**約束の向きが逆**である。
//!
//! # 役割の分担
//!
//! **画像は kiri が読んで自分で合成し、resvg へ渡すのは文字だけである。**
//! resvg の `raster-images` を有効にすれば `<image>` を読ませられるが、kiri は
//! EXIF の向きと ICC → sRGB を `image_io/load.rs` で正規化している。そちらを
//! 迂回すると、**色と向きの契約が compose でだけ崩れる**——同じ素材が `cutout`
//! を通ったときと `compose` を通ったときで違う色になる。`Cargo.toml` の resvg の
//! doc に同じことを書いてある。
//!
//! # spec を読む関門は 1 つである
//!
//! `batch` の spec と同じく、生の JSON を一度 `Value` で受けて**キーを自分で
//! 検査してから**型へ落とす。serde の `deny_unknown_fields` に任せると、綴り違いの
//! 候補（`batch::closest`）を添えられない。未知のキーは `SPEC_UNKNOWN_FIELD` で
//! 断る——spec を数百行書いてから仕上がりで気づく種類の失敗を作らないためである。

pub mod font;
pub mod gate;
pub mod measure;
pub mod text;

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::batch::check;
use crate::error::{Error, ErrorCode, Result};
use crate::transform::FitMode;

/// spec に書けるキー。**ここが唯一の定義である。**
///
/// 未知のキーを断る側（`validate_keys`）と、形を配る側（`kiri schema` の
/// `compose_spec[]`）が同じ表を見る。別々に持つと、キーを 1 つ足した日に
/// schema だけが古い形を配る——`profile::PROFILE_NAMES` が手で書き写した一覧を
/// 増やさないのと同じ作法である。
pub mod keys {
    pub const SPEC: &[&str] = &["canvas", "font", "safe_area", "layers"];
    pub const CANVAS: &[&str] = &["width", "height", "background"];
    pub const FONT: &[&str] = &["family", "path"];
    pub const IMAGE_LAYER: &[&str] = &["id", "type", "role", "source", "rect", "fit"];
    pub const TEXT_LAYER: &[&str] = &[
        "id",
        "type",
        "role",
        "lines",
        "rect",
        "size",
        "weight",
        "color",
        "line_height",
        "align",
    ];
}

/// キャンバスの 1 辺の上限(px)。
///
/// **note の「画像拡大時の長辺は 4,000px まで」とは別の数である。** こちらは
/// 規格ではなく、**spec の桁違いが即座にメモリを食い潰すのを止める**ための門で
/// ある。16384 は RGBA で約 1GB にあたり、ここを超える合成物に用途が思い当たらない。
/// 規格の寸法は `profile.rs` が持つ——**出典のある数と kiri の都合の数を同じ表に
/// 混ぜない**（`Rules` の doc と同じ理由）。
pub const MAX_CANVAS_SIDE: u32 = 16384;

/// 文字の大きさの下限(px)。0 や負を通すと usvg がグリフを 1 つも返さず、
/// **「書いたのに何も出ない」が無言で成立する。**
pub const MIN_FONT_SIZE: f64 = 1.0;

/// 1 枚の組み立て方。
#[derive(Debug, Clone, Deserialize)]
pub struct ComposeSpec {
    pub canvas: Canvas,
    /// 文字のレイヤが 1 つでもあれば要る。**既定のフォントは持たない**
    /// （`font::resolve` の doc を参照）
    #[serde(default)]
    pub font: Option<FontSpec>,
    /// 表示側で切られずに残る範囲 `[x, y, 幅, 高さ]`。
    ///
    /// **規格の数を kiri が持たない。** note のヘッダーが一覧で 1280x454 しか
    /// 見えない、のような条件は媒体ごとに違い、出典も媒体の側にある。spec に
    /// 書いてもらい、kiri は「その外へ出たか」だけを測る
    #[serde(default)]
    pub safe_area: Option<[f64; 4]>,
    pub layers: Vec<Layer>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Canvas {
    pub width: u32,
    pub height: u32,
    /// 下地。**無ければ透明のまま**——`--canvas` の `background` と同じ約束である
    #[serde(default)]
    pub background: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct FontSpec {
    pub family: String,
    /// 読むファイルを 1 つに固定する。**与えられたらシステムは見に行かない**
    #[serde(default)]
    pub path: Option<PathBuf>,
}

/// レイヤの役割。**何を測るかがここで決まる。**
///
/// 役割を持たない図形の集まり（生の SVG）では「見出しが商品に重なっている」が
/// 言えない。それが spec を受けて内部で組む形を採った理由そのものである
/// （計画 §10.9）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    /// 商品。**文字が重なってはいけない相手**として数えられる
    Subject,
    Heading,
    Body,
    Caption,
    /// 飾り。コントラストも重なりも測らない——**測らないことを spec の側から
    /// 言える口**が無いと、意図した薄い文字が毎回警告を出す
    Decoration,
}

impl Role {
    pub const ALL: [Role; 5] = [
        Role::Subject,
        Role::Heading,
        Role::Body,
        Role::Caption,
        Role::Decoration,
    ];

    pub const fn as_str(self) -> &'static str {
        match self {
            Role::Subject => "subject",
            Role::Heading => "heading",
            Role::Body => "body",
            Role::Caption => "caption",
            Role::Decoration => "decoration",
        }
    }

    /// 読めることを要求する役割か。`decoration` だけが外れる。
    pub const fn wants_contrast(self) -> bool {
        !matches!(self, Role::Decoration)
    }
}

/// 行揃え。**枠の中での基準点**であって、折り返しの指示ではない。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Align {
    #[default]
    Start,
    Center,
    End,
}

impl Align {
    pub const ALL: [Align; 3] = [Align::Start, Align::Center, Align::End];

    pub const fn as_str(self) -> &'static str {
        match self {
            Align::Start => "start",
            Align::Center => "center",
            Align::End => "end",
        }
    }

    /// SVG の `text-anchor`。
    pub const fn anchor(self) -> &'static str {
        match self {
            Align::Start => "start",
            Align::Center => "middle",
            Align::End => "end",
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Layer {
    Image(ImageLayer),
    Text(TextLayer),
}

impl Layer {
    pub fn id(&self) -> &str {
        match self {
            Layer::Image(l) => &l.id,
            Layer::Text(l) => &l.id,
        }
    }

    pub fn role(&self) -> Role {
        match self {
            Layer::Image(l) => l.role,
            Layer::Text(l) => l.role,
        }
    }

    pub fn rect(&self) -> [f64; 4] {
        match self {
            Layer::Image(l) => l.rect,
            Layer::Text(l) => l.rect,
        }
    }

    pub const fn kind(&self) -> &'static str {
        match self {
            Layer::Image(_) => "image",
            Layer::Text(_) => "text",
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct ImageLayer {
    pub id: String,
    pub role: Role,
    /// 読み込む画像。spec からの相対パスで解く
    pub source: PathBuf,
    /// 枠 `[x, y, 幅, 高さ]`
    pub rect: [f64; 4],
    /// 枠への当てはめ方。既定は `contain`（枠に収まるまで縮める）
    #[serde(default = "default_fit")]
    pub fit: FitMode,
}

fn default_fit() -> FitMode {
    FitMode::Contain
}

#[derive(Debug, Clone, Deserialize)]
pub struct TextLayer {
    pub id: String,
    pub role: Role,
    /// **行の配列である。kiri は折り返さない。**
    ///
    /// 自動折り返しは禁則処理（行頭に句読点を置かない、など）と字詰めを伴い、
    /// それを持つとこのモジュールは合成器ではなく組版器になる。どこで割るかは
    /// 文意の問題でもあり、呼ぶ側が決めるべき側にある。**割る材料（行ごとの
    /// 実測幅）は返すが、割らない**（計画 §10.3.2）
    pub lines: Vec<String>,
    /// 枠 `[x, y, 幅, 高さ]`。**収まらなければ縮めずに言う**
    pub rect: [f64; 4],
    pub size: f64,
    /// 字の太さ（100〜900）。既定 400
    #[serde(default = "default_weight")]
    pub weight: u16,
    pub color: String,
    /// 行送り。`size` に対する倍率。既定 1.4
    #[serde(default = "default_line_height")]
    pub line_height: f64,
    #[serde(default)]
    pub align: Align,
}

fn default_weight() -> u16 {
    400
}

fn default_line_height() -> f64 {
    1.4
}

/// spec を読む。
pub fn load(path: &Path) -> Result<ComposeSpec> {
    let text = std::fs::read_to_string(path).map_err(|e| {
        Error::new(
            ErrorCode::SpecUnreadable,
            format!("{} を読めません: {e}", path.display()),
        )
    })?;
    parse(&text, &path.display().to_string())
}

/// 読んだ文字列を spec にする。**ファイルを読む経路とテストが同じ関門を通る。**
pub fn parse(text: &str, origin: &str) -> Result<ComposeSpec> {
    let raw: serde_json::Value = serde_json::from_str(text).map_err(|e| {
        Error::new(
            ErrorCode::SpecInvalidJson,
            format!("{origin} は JSON として不正です: {e}"),
        )
    })?;

    validate_keys(&raw)?;

    let spec: ComposeSpec = serde_json::from_value(raw).map_err(|e| {
        Error::new(
            ErrorCode::SpecInvalid,
            format!("{origin} の内容が不正です: {e}"),
        )
    })?;

    validate(&spec)?;
    Ok(spec)
}

/// 未知のキーを、型へ落とす前に断る。
fn validate_keys(raw: &serde_json::Value) -> Result<()> {
    let object = raw.as_object().ok_or_else(|| {
        Error::new(
            ErrorCode::SpecInvalid,
            "spec の最上位はオブジェクトである必要があります",
        )
    })?;
    check(object.keys(), keys::SPEC, "spec")?;

    if let Some(canvas) = object.get("canvas").and_then(|v| v.as_object()) {
        check(canvas.keys(), keys::CANVAS, "canvas")?;
    }
    if let Some(font) = object.get("font").and_then(|v| v.as_object()) {
        check(font.keys(), keys::FONT, "font")?;
    }

    let layers = object.get("layers").and_then(|v| v.as_array());
    for (i, layer) in layers.into_iter().flatten().enumerate() {
        let o = layer.as_object().ok_or_else(|| {
            Error::new(
                ErrorCode::SpecInvalid,
                format!("layers[{i}] はオブジェクトである必要があります"),
            )
        })?;
        // **`type` を見てから候補を選ぶ。** 両方の和集合で検査すると、画像の
        // レイヤに `line_height` を書いた spec が素通りして、効かない指定が
        // 結果にも現れないまま残る
        let allowed = match o.get("type").and_then(|v| v.as_str()) {
            Some("image") => keys::IMAGE_LAYER,
            Some("text") => keys::TEXT_LAYER,
            Some(other) => {
                return Err(Error::new(
                    ErrorCode::SpecInvalid,
                    format!("layers[{i}] の type '{other}' は image か text ではありません"),
                ));
            }
            None => {
                return Err(Error::new(
                    ErrorCode::SpecInvalid,
                    format!("layers[{i}] に type がありません"),
                )
                .with_hint("\"type\": \"image\" か \"type\": \"text\" を書いてください"));
            }
        };
        check(o.keys(), allowed, &format!("layers[{i}]"))?;
    }
    Ok(())
}

/// 値そのものを見る。**1 枚も読まず 1 画素も描かないうちに断る。**
fn validate(spec: &ComposeSpec) -> Result<()> {
    if spec.canvas.width == 0 || spec.canvas.height == 0 {
        return Err(Error::new(
            ErrorCode::InvalidCanvas,
            "キャンバスの寸法に 0 は指定できません",
        ));
    }
    if spec.canvas.width > MAX_CANVAS_SIDE || spec.canvas.height > MAX_CANVAS_SIDE {
        return Err(Error::new(
            ErrorCode::InvalidCanvas,
            format!(
                "キャンバスの 1 辺は {MAX_CANVAS_SIDE}px までです（指定: {}x{}）",
                spec.canvas.width, spec.canvas.height
            ),
        ));
    }
    if let Some(color) = &spec.canvas.background {
        parse_color(color)?;
    }
    if spec.layers.is_empty() {
        return Err(Error::new(ErrorCode::SpecEmpty, "layers が空です")
            .with_hint("組む素材を layers に列挙してください"));
    }
    if let Some(area) = spec.safe_area {
        check_rect(area, "safe_area")?;
    }

    // **id は結果 JSON で層を指す鍵である。** 重複すると、返した測りがどの層の
    // ものか呼ぶ側から決められない
    for (i, layer) in spec.layers.iter().enumerate() {
        let id = layer.id();
        if id.is_empty() {
            return Err(Error::new(
                ErrorCode::SpecInvalid,
                format!("layers[{i}] の id が空です"),
            ));
        }
        if spec.layers[..i].iter().any(|other| other.id() == id) {
            return Err(Error::new(
                ErrorCode::SpecInvalid,
                format!("id '{id}' が 2 つのレイヤで使われています"),
            )
            .with_hint("id は結果 JSON で層を指す鍵なので、1 枚の中で重複できません"));
        }
        check_rect(layer.rect(), &format!("layers[{i}] ({id}) の rect"))?;

        if let Layer::Text(t) = layer {
            if t.lines.is_empty() {
                return Err(Error::new(
                    ErrorCode::SpecInvalid,
                    format!("layers[{i}] ({id}) の lines が空です"),
                ));
            }
            if !(t.size.is_finite() && t.size >= MIN_FONT_SIZE) {
                return Err(Error::new(
                    ErrorCode::SpecInvalid,
                    format!(
                        "layers[{i}] ({id}) の size は {MIN_FONT_SIZE} 以上である必要が\
                         あります（指定: {}）",
                        t.size
                    ),
                ));
            }
            if !(t.line_height.is_finite() && t.line_height > 0.0) {
                return Err(Error::new(
                    ErrorCode::SpecInvalid,
                    format!(
                        "layers[{i}] ({id}) の line_height は 0 より大きい必要が\
                         あります（指定: {}）",
                        t.line_height
                    ),
                ));
            }
            if !(100..=900).contains(&t.weight) {
                return Err(Error::new(
                    ErrorCode::SpecInvalid,
                    format!(
                        "layers[{i}] ({id}) の weight は 100〜900 です（指定: {}）",
                        t.weight
                    ),
                ));
            }
            parse_color(&t.color)?;
        }
    }

    // **文字があるのにフォントが無い spec は、読む前に断る。** 既定のフォントへ
    // 落とすと、同じ spec が機械ごとに違う絵になる（`font::resolve` の doc）
    if spec.layers.iter().any(|l| matches!(l, Layer::Text(_))) && spec.font.is_none() {
        return Err(Error::new(
            ErrorCode::FontNotFound,
            "文字のレイヤがありますが font が指定されていません",
        )
        .with_hint(
            "font.family か font.path を書いてください。kiri は既定のフォントを持ちません\
             ——持つと同じ spec が機械ごとに違う絵になります",
        ));
    }
    Ok(())
}

fn check_rect(rect: [f64; 4], location: &str) -> Result<()> {
    if !rect.iter().all(|v| v.is_finite()) {
        return Err(Error::new(
            ErrorCode::SpecInvalid,
            format!("{location} に有限でない数があります"),
        ));
    }
    if rect[2] <= 0.0 || rect[3] <= 0.0 {
        return Err(Error::new(
            ErrorCode::SpecInvalid,
            format!(
                "{location} の幅と高さは 0 より大きい必要があります（指定: {}x{}）",
                rect[2], rect[3]
            ),
        ));
    }
    Ok(())
}

/// `#rrggbb` を解く。
///
/// **3 桁の短縮形（`#fff`）を受けない。** 受ける形を増やすほど、spec を書く側が
/// 「どちらで書いたか」を覚えることになる。`--background` が `r,g,b` の 1 通りしか
/// 受けないのと同じ判断で、綴りは 1 つに保つ。
pub fn parse_color(text: &str) -> Result<[u8; 3]> {
    let body = text.strip_prefix('#').filter(|b| b.len() == 6);
    let parsed = body.and_then(|b| {
        let r = u8::from_str_radix(&b[0..2], 16).ok()?;
        let g = u8::from_str_radix(&b[2..4], 16).ok()?;
        let b = u8::from_str_radix(&b[4..6], 16).ok()?;
        Some([r, g, b])
    });
    parsed.ok_or_else(|| {
        Error::new(
            ErrorCode::InvalidColor,
            format!("色は #rrggbb の形式です（指定: {text}）"),
        )
        .with_hint("3 桁の短縮形は受けません。#ffffff のように 6 桁で書いてください")
    })
}
