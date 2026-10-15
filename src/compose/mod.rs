//! `kiri compose` — **素材と文字を 1 枚へ組む。組版器ではなく合成器である。**
//!
//! どこに何を置くかは spec が決める。kiri は置いて、**測って、言う**。
//! 収まらない文字を黙って縮めることも、長い行を黙って折り返すことも
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

pub mod display;
pub mod font;
pub mod gate;
pub mod measure;
pub mod profile;
pub mod shape;
pub mod text;

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::batch::check;
use crate::compose::profile::ComposeProfile;
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
    pub const IMAGE_LAYER: &[&str] = &["id", "type", "role", "source", "rect", "fit", "trim"];
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
        "font",
        "stroke",
    ];
    pub const STROKE: &[&str] = &["color", "width"];
    /// **`role` を持たない。** 図形は情報を持たないので測る理由が無く、結果では
    /// 常に `decoration` を名乗る。書けば `SPEC_UNKNOWN_FIELD` で断る——書いた
    /// role が効いたと思わせない
    pub const SHAPE_LAYER: &[&str] = &["id", "type", "rect", "color", "opacity", "radius"];
}

/// キャンバスの 1 辺の上限(px)。
///
/// **note の「画像拡大時の長辺は 4,000px まで」とは別の数である。** こちらは
/// 規格ではなく、**spec の桁違いが即座にメモリを食い潰すのを止める**ための門で
/// ある。16384 は RGBA で約 1GB にあたり、ここを超える合成物に用途が思い当たらない。
/// 規格の寸法は `profile.rs` が持つ——**出典のある数と kiri の都合の数を同じ表に
/// 混ぜない**（`Rules` の doc と同じ理由）。
pub const MAX_CANVAS_SIDE: u32 = 16384;

/// 縁取りの幅の下限(px)。
///
/// **見えない縁を信じない。** 0.01px の縁は 1 画素も塗らないのに、原寸の
/// `text_contrast` は塗りと縁の色の比を返す——白地に白の文字が縁 0.01 で 21 を
/// 名乗る。1px は原寸で 1 画素を確実に塗る最小の幅である。
pub const MIN_STROKE_WIDTH: f64 = 1.0;

/// 文字の大きさの下限(px)。0 や負を通すと usvg がグリフを 1 つも返さず、
/// **「書いたのに何も出ない」が無言で成立する。**
pub const MIN_FONT_SIZE: f64 = 1.0;

/// 1 枚の組み立て方。
#[derive(Debug, Clone, Deserialize)]
pub struct ComposeSpec {
    pub canvas: Canvas,
    /// 既定の字体。文字の層が自分の `font` を持たなければこれで組む。
    /// 文字の層のどれかが `font` を持たなければ要る。**kiri は既定のフォントを
    /// 持たない**（`font::resolve` の doc を参照）
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
    /// **`--profile` を渡したときだけ省ける**（`parse` が埋める）。
    /// 渡さずに省けば今までどおり `SPEC_INVALID` で断る
    pub width: u32,
    pub height: u32,
    /// 下地。**無ければ透明のまま**——`--canvas` の `background` と同じ約束である
    #[serde(default)]
    pub background: Option<String>,
}

/// 字体の指定。**family と path の組で同じ指定かが決まる**（`font::resolve` は
/// 同じ指定を 1 度しか読まない）。
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct FontSpec {
    pub family: String,
    /// 読むファイルを 1 つに固定する。**与えられたらシステムは見に行かない**
    #[serde(default)]
    pub path: Option<PathBuf>,
}

/// レイヤの役割。**何を測るかがここで決まる。**
///
/// 役割を持たない図形の集まり（生の SVG）では「見出しが商品に重なっている」が
/// 言えない。それが spec を受けて内部で組む形を採った理由そのものである。
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
    Shape(ShapeLayer),
}

impl Layer {
    pub fn id(&self) -> &str {
        match self {
            Layer::Image(l) => &l.id,
            Layer::Text(l) => &l.id,
            Layer::Shape(l) => &l.id,
        }
    }

    pub fn role(&self) -> Role {
        match self {
            Layer::Image(l) => l.role,
            Layer::Text(l) => l.role,
            Layer::Shape(_) => Role::Decoration,
        }
    }

    pub fn rect(&self) -> [f64; 4] {
        match self {
            Layer::Image(l) => l.rect,
            Layer::Text(l) => l.rect,
            Layer::Shape(l) => l.rect,
        }
    }

    pub const fn kind(&self) -> &'static str {
        match self {
            Layer::Image(_) => "image",
            Layer::Text(_) => "text",
            Layer::Shape(_) => "shape",
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
    /// 当てはめる前に何を切り詰めるか。既定は `alpha`（透過の余白）
    #[serde(default)]
    pub trim: Trim,
}

fn default_fit() -> FitMode {
    FitMode::Contain
}

/// 素材を枠へ当てはめる前の切り詰め方。
///
/// # 既定が `alpha` である理由
///
/// **`cutout` の出力は、商品の外が透過の余白である。** `--canvas` は余白を
/// 意図して足し、`--rotate` は回した四隅のぶんだけ余白を広げる（-91 度で
/// 回した 5788x4386 の出力では、商品は横幅の 24% しか占めない）。余白ごと
/// `contain` で内接させると、**枠を書いた側が置きたかった商品が枠の中で
/// 豆粒になる**——枠は商品を置く場所として書かれるのであって、素材の
/// キャンバスを置く場所として書かれるのではない。
///
/// 切り詰めは**見た目を 1 画素も消さない**：不透明度が 1 でもある画素の
/// 外接矩形で切るので、捨てるのは完全に透明な画素だけである。不透明な写真
/// （背景の層）では何も起きない。ただし `fit` の後は別で、`cover` が枠から
/// はみ出して切り落とすのは余白ではなく商品になる。
///
/// 素材の余白そのものを構図として使いたいとき（余白込みで位置を揃えた
/// 素材など）は `none` を書く。0.x までの振る舞いはこちらである。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Trim {
    /// 不透明度が 0 でない画素の外接矩形で切り詰めてから当てはめる
    #[default]
    Alpha,
    /// 素材をそのまま当てはめる（透過の余白も枠に内接させる）
    None,
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
    /// 実測幅）は返すが、割らない**
    pub lines: Vec<String>,
    /// 枠 `[x, y, 幅, 高さ]`。**収まらなければ縮めずに言う**
    pub rect: [f64; 4],
    pub size: f64,
    /// 字の太さ（100〜900 の 100 刻み）。既定 400
    #[serde(default = "default_weight")]
    pub weight: u16,
    pub color: String,
    /// 行送り。`size` に対する倍率。既定 1.4
    #[serde(default = "default_line_height")]
    pub line_height: f64,
    #[serde(default)]
    pub align: Align,
    /// この層だけの字体。**書けば最上位の `font` より強い。** 見出しを W8、
    /// 補足を W4 にするような強弱は、1 つの字体の weight では書けないことがある
    /// （1 ウェイトのファイルでは weight が効かない。`font` の doc）
    #[serde(default)]
    pub font: Option<FontSpec>,
    /// 字の縁取り。**書かなければ縁の無い SVG を出す**——Phase 31 までと
    /// 1 画素も変わらない
    #[serde(default)]
    pub stroke: Option<Stroke>,
}

/// 文字の縁取り（13-phase-32.md §13.2.1）。
///
/// **外側にだけ付く。** SVG の stroke は輪郭の内外に半分ずつ乗るので、内側の
/// 半分は塗りに隠す（`text::to_svg`）。内側まで見せると字の線が細り、縁を
/// 付けた分だけ縮小で `strokes_lost` に近づく——読めるようにするための手が、
/// 読めなくする向きに効く。
#[derive(Debug, Clone, Deserialize)]
pub struct Stroke {
    /// 縁の色。**不透明だけを受ける**——`#rrggbb` に不透明度を書く口は無い
    pub color: String,
    /// 字の輪郭から外へ見える幅(px)。`MIN_STROKE_WIDTH` 以上 `size` 以下
    pub width: f64,
}

/// 図形の層（13-phase-32.md §13.2.3）。**矩形だけを持つ。**
///
/// 帯・座布団・丸い札はこれで足りる。円や多角形は要る日に種類を足す——既定を
/// 矩形にしておけば、足しても書いた spec の意味は変わらない。
#[derive(Debug, Clone, Deserialize)]
pub struct ShapeLayer {
    pub id: String,
    /// 枠 `[x, y, 幅, 高さ]`。**置かれる矩形そのもの**で、`placed` もこれになる
    pub rect: [f64; 4],
    pub color: String,
    /// 塗りの不透明度。0 < x ≤ 1、既定 1
    #[serde(default = "default_opacity")]
    pub opacity: f64,
    /// 角の丸み(px)。幅か高さの半分を超えれば SVG の規則どおり半分で止まる
    #[serde(default)]
    pub radius: f64,
}

fn default_opacity() -> f64 {
    1.0
}

fn default_weight() -> u16 {
    400
}

fn default_line_height() -> f64 {
    1.4
}

/// spec を読み、`--profile` の値で**書かれていない項目だけを**埋める。
pub fn load(path: &Path, profile: Option<&ComposeProfile>) -> Result<ComposeSpec> {
    let text = std::fs::read_to_string(path).map_err(|e| {
        Error::new(
            ErrorCode::SpecUnreadable,
            format!("{} を読めません: {e}", path.display()),
        )
    })?;
    parse(&text, &path.display().to_string(), profile)
}

/// 読んだ文字列を spec にする。**ファイルを読む経路とテストが同じ関門を通る。**
///
/// `profile` があれば書かれていない項目だけを埋める。**明示 > profile の順で
/// 勝つ**（cutout の `--profile` と同じ約束）。
///
/// # 埋めるのは型へ落とす前である
///
/// `canvas.width` / `canvas.height` は型の上では必須のまま置く。profile が
/// あるときだけ、**生の JSON の欠けたキーへ profile の値を書き込んでから**
/// 型へ落とす。型を `Option` にすると、profile の無い経路で欠けた寸法を
/// どこで断るかという分岐が増える——いまは serde が「width が無い」と言って
/// `SPEC_INVALID` で断っており、**profile を渡さない spec の断り方は 1 文字も
/// 変わらない。**
///
/// `safe_area` は型へ落とした後に埋める。キャンバスの寸法が決まらないと
/// 比で合わせられないからである。どちらも `validate` の前なので、profile が
/// 埋めた値も spec に書いた値と同じ関門を通る。
pub fn parse(text: &str, origin: &str, profile: Option<&ComposeProfile>) -> Result<ComposeSpec> {
    let mut raw: serde_json::Value = serde_json::from_str(text).map_err(|e| {
        Error::new(
            ErrorCode::SpecInvalidJson,
            format!("{origin} は JSON として不正です: {e}"),
        )
    })?;

    validate_keys(&raw)?;
    if let Some(profile) = profile {
        fill_canvas(&mut raw, profile);
    }

    let mut spec: ComposeSpec = serde_json::from_value(raw).map_err(|e| {
        Error::new(
            ErrorCode::SpecInvalid,
            format!("{origin} の内容が不正です: {e}"),
        )
    })?;

    if let Some(profile) = profile {
        if spec.safe_area.is_none() {
            spec.safe_area = Some(profile.safe_area_for(spec.canvas.width, spec.canvas.height));
        }
    }

    validate(&spec)?;
    Ok(spec)
}

/// spec が書かなかったキャンバスの寸法を profile から埋める。
///
/// **1 辺ずつ埋める。** 幅だけを書いた spec は、高さを profile から受け取る
/// ——比が外れていれば `PROFILE_ASPECT_MISMATCH` が言う。`canvas` そのものが
/// 無ければ作る。`canvas` がオブジェクトでなければ触らない（型へ落とすところで
/// 今までどおり断られる）。
fn fill_canvas(raw: &mut serde_json::Value, profile: &ComposeProfile) {
    let Some(object) = raw.as_object_mut() else {
        return;
    };
    let canvas = object
        .entry("canvas")
        .or_insert_with(|| serde_json::Value::Object(serde_json::Map::new()));
    let Some(canvas) = canvas.as_object_mut() else {
        return;
    };
    let (width, height) = profile.canvas;
    canvas.entry("width").or_insert_with(|| width.into());
    canvas.entry("height").or_insert_with(|| height.into());
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
            Some("shape") => keys::SHAPE_LAYER,
            Some(other) => {
                return Err(Error::new(
                    ErrorCode::SpecInvalid,
                    format!("layers[{i}] の type '{other}' は image / text / shape ではありません"),
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
        if let Some(font) = o.get("font").and_then(|v| v.as_object()) {
            check(font.keys(), keys::FONT, &format!("layers[{i}].font"))?;
        }
        if let Some(stroke) = o.get("stroke").and_then(|v| v.as_object()) {
            check(stroke.keys(), keys::STROKE, &format!("layers[{i}].stroke"))?;
        }
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
            // **100 刻みでない weight は断る。** usvg は "100"〜"900" の 9 つしか
            // 太さとして読まず、450 は黙って 400 で組まれる——`FONT_WEIGHT_UNAVAILABLE`
            // が塞いだ穴が、綴りの側から開く
            if !(100..=900).contains(&t.weight) || t.weight % 100 != 0 {
                return Err(Error::new(
                    ErrorCode::SpecInvalid,
                    format!(
                        "layers[{i}] ({id}) の weight は 100〜900 の 100 刻みです（指定: {}）",
                        t.weight
                    ),
                ));
            }
            parse_color(&t.color)?;
            if let Some(stroke) = &t.stroke {
                parse_color(&stroke.color)?;
                // **上限は size である。** 字の大きさを超える縁は字そのものより
                // 太く、桁違いの書き損じとしか読めない。下限は `MIN_STROKE_WIDTH`
                // （見えない縁に比を名乗らせない）。有限でない値は usvg が黙って
                // 捨て、縁の無い字が「縁を付けた」結果として返る
                if !(stroke.width.is_finite()
                    && stroke.width >= MIN_STROKE_WIDTH
                    && stroke.width <= t.size)
                {
                    return Err(Error::new(
                        ErrorCode::SpecInvalid,
                        format!(
                            "layers[{i}] ({id}) の stroke.width は {MIN_STROKE_WIDTH} 以上 \
                             size（{}）以下である必要があります（指定: {}）",
                            t.size, stroke.width
                        ),
                    ));
                }
            }

            // **文字があるのにフォントが無い層は、読む前に断る。** 既定のフォントへ
            // 落とすと、同じ spec が機械ごとに違う絵になる（`font::resolve` の doc）
            if t.font.is_none() && spec.font.is_none() {
                return Err(Error::new(
                    ErrorCode::FontNotFound,
                    format!("layers[{i}] ({id}) は文字のレイヤですが font が指定されていません"),
                )
                .with_hint(
                    "最上位の font か layers[].font に family か path を書いてください。\
                     kiri は既定のフォントを持ちません——持つと同じ spec が機械ごとに\
                     違う絵になります",
                ));
            }
        }

        if let Layer::Shape(s) = layer {
            parse_color(&s.color)?;
            // **0 は断る。** 見えない図形は何も敷かず、何も覆わない——書いた側の
            // 意図は「薄く敷く」のほうで、0 は書き損じである
            if !(s.opacity.is_finite() && s.opacity > 0.0 && s.opacity <= 1.0) {
                return Err(Error::new(
                    ErrorCode::SpecInvalid,
                    format!(
                        "layers[{i}] ({id}) の opacity は 0 より大きく 1 以下である必要が\
                         あります（指定: {}）",
                        s.opacity
                    ),
                ));
            }
            if !(s.radius.is_finite() && s.radius >= 0.0) {
                return Err(Error::new(
                    ErrorCode::SpecInvalid,
                    format!(
                        "layers[{i}] ({id}) の radius は 0 以上の有限の数である必要が\
                         あります（指定: {}）",
                        s.radius
                    ),
                ));
            }
        }
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
    // **枠にもキャンバスと同じ上限を置く。** 枠はリサイズの要求そのもので、
    // `100000x200000` と書かれると `fast_image_resize` がその大きさの画像を
    // 確保しようとする——キャンバスが 400x400 でも、である。`f64` から `u32` への
    // キャストは飽和するので、`1e12` は黙って `u32::MAX` になり、約 73TB を
    // 要求したうえで応答が返らなくなる。**上限で黙って切るのではなく断る**のは、
    // 切ると「書いた枠と違う寸法で組まれた」ことが結果からしか分からないため
    let limit = f64::from(MAX_CANVAS_SIDE);
    if rect[2] > limit || rect[3] > limit {
        return Err(Error::new(
            ErrorCode::SpecInvalid,
            format!(
                "{location} の幅と高さは {MAX_CANVAS_SIDE}px までです（指定: {}x{}）",
                rect[2], rect[3]
            ),
        ));
    }
    // 原点も同じ幅で縛る。**キャンバスの外は見えないので、離れた位置を
    // 許しても意味が無い**——許すと配置の算術が `i64` の端へ寄るだけである
    if rect[0].abs() > limit || rect[1].abs() > limit {
        return Err(Error::new(
            ErrorCode::SpecInvalid,
            format!(
                "{location} の原点は ±{MAX_CANVAS_SIDE}px までです（指定: {},{}）",
                rect[0], rect[1]
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
    // **`len()` はバイト数で、`&b[0..2]` もバイトで切る。** 16 進かどうかを先に
    // 見ないと、`#日本`（`#` + 6 バイト）が長さの検査を通り、文字の途中で
    // 切って panic する。**利用者の spec から届く値なので、panic は契約の外へ
    // 出る失敗**になる——code も exit code も名乗らずに 101 で落ちる
    let body = text
        .strip_prefix('#')
        .filter(|b| b.len() == 6 && b.bytes().all(|c| c.is_ascii_hexdigit()));
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

#[cfg(test)]
mod tests {
    use super::*;

    const IMAGE_ONLY: &str = r#"{"layers":[{"id":"a","type":"image","role":"subject",
        "source":"a.png","rect":[0,0,10,10]}]}"#;

    fn note() -> &'static ComposeProfile {
        profile::named("note").unwrap()
    }

    /// `canvas` を省いた spec は profile の寸法と安全領域で組む（§12.5 の 2）。
    #[test]
    fn a_missing_canvas_takes_the_profile_values() {
        let spec = parse(IMAGE_ONLY, "t", Some(note())).unwrap();
        assert_eq!((spec.canvas.width, spec.canvas.height), (1280, 670));
        assert_eq!(spec.safe_area, Some([0.0, 108.0, 1280.0, 454.0]));
        assert_eq!(spec.canvas.background, None);
    }

    /// **profile を渡さなければ、省いた寸法は今までどおり断る。**
    #[test]
    fn without_a_profile_a_missing_canvas_is_still_refused() {
        let err = parse(IMAGE_ONLY, "t", None).unwrap_err();
        assert_eq!(err.code, ErrorCode::SpecInvalid);
        let err = parse(
            r#"{"canvas":{"width":10},"layers":[{"id":"a","type":"image",
               "role":"subject","source":"a.png","rect":[0,0,10,10]}]}"#,
            "t",
            None,
        )
        .unwrap_err();
        assert_eq!(err.code, ErrorCode::SpecInvalid);
    }

    /// 書いた値は profile より強い。安全領域は書いたキャンバスへ比で合わせる。
    #[test]
    fn written_values_win_over_the_profile() {
        let spec = parse(
            r##"{"canvas":{"width":1920,"height":1006,"background":"#ffffff"},
                "layers":[{"id":"a","type":"image","role":"subject",
                           "source":"a.png","rect":[0,0,10,10]}]}"##,
            "t",
            Some(note()),
        )
        .unwrap();
        assert_eq!((spec.canvas.width, spec.canvas.height), (1920, 1006));
        assert_eq!(spec.safe_area, Some(note().safe_area_for(1920, 1006)));

        let spec = parse(
            r#"{"canvas":{"width":1280,"height":670},"safe_area":[10,10,100,100],
                "layers":[{"id":"a","type":"image","role":"subject",
                           "source":"a.png","rect":[0,0,10,10]}]}"#,
            "t",
            Some(note()),
        )
        .unwrap();
        assert_eq!(spec.safe_area, Some([10.0, 10.0, 100.0, 100.0]));
    }

    /// 文字の層 1 つに `stroke` を足した spec（13-phase-32.md §13.4 の 9）。
    fn stroked(stroke: &str) -> String {
        format!(
            r##"{{"canvas":{{"width":100,"height":100}},"font":{{"family":"X"}},
                 "layers":[{{"id":"t","type":"text","role":"heading","lines":["A"],
                            "rect":[0,0,100,100],"size":40,"color":"#ffffff",
                            "stroke":{stroke}}}]}}"##
        )
    }

    /// 図形の層 1 つの spec。`extra` は `"color"` の後ろへ足すキー。
    fn shaped(extra: &str) -> String {
        format!(
            r##"{{"canvas":{{"width":100,"height":100}},
                 "layers":[{{"id":"s","type":"shape","rect":[0,0,50,50],
                            "color":"#000000"{extra}}}]}}"##
        )
    }

    #[test]
    fn a_valid_stroke_and_shape_are_accepted() {
        let spec = parse(&stroked(r##"{"color":"#000000","width":40}"##), "t", None).unwrap();
        let Layer::Text(t) = &spec.layers[0] else {
            panic!("文字の層");
        };
        let stroke = t.stroke.as_ref().unwrap();
        assert_eq!((stroke.color.as_str(), stroke.width), ("#000000", 40.0));

        let spec = parse(&shaped(""), "t", None).unwrap();
        let Layer::Shape(s) = &spec.layers[0] else {
            panic!("図形の層");
        };
        assert_eq!((s.opacity, s.radius), (1.0, 0.0), "既定は不透明・角なし");
        assert_eq!(spec.layers[0].role(), Role::Decoration);
        assert_eq!(spec.layers[0].kind(), "shape");
        parse(&shaped(r#","opacity":0.6,"radius":1000"#), "t", None).unwrap();
    }

    #[test]
    fn a_stroke_width_outside_one_to_size_is_refused() {
        for width in ["0", "-1", "0.01", "0.99", "40.5", "1e9"] {
            let err = parse(
                &stroked(&format!(r##"{{"color":"#000000","width":{width}}}"##)),
                "t",
                None,
            )
            .unwrap_err();
            assert_eq!(err.code, ErrorCode::SpecInvalid, "width {width}");
        }
        let err = parse(&stroked(r##"{"color":"#000","width":2}"##), "t", None).unwrap_err();
        assert_eq!(err.code, ErrorCode::InvalidColor);
        let err = parse(&stroked(r#"{"width":2}"#), "t", None).unwrap_err();
        assert_eq!(err.code, ErrorCode::SpecInvalid, "color は必須");
    }

    #[test]
    fn an_unknown_stroke_key_is_refused() {
        let err = parse(
            &stroked(r##"{"color":"#000000","width":2,"opacity":0.5}"##),
            "t",
            None,
        )
        .unwrap_err();
        assert_eq!(err.code, ErrorCode::SpecUnknownField);
    }

    #[test]
    fn shape_values_outside_their_range_are_refused() {
        for extra in [
            r#","opacity":0"#,
            r#","opacity":1.01"#,
            r#","opacity":-0.5"#,
            r#","radius":-1"#,
        ] {
            let err = parse(&shaped(extra), "t", None).unwrap_err();
            assert_eq!(err.code, ErrorCode::SpecInvalid, "{extra}");
        }
        let err = parse(
            r#"{"canvas":{"width":100,"height":100},
                "layers":[{"id":"s","type":"shape","rect":[0,0,50,50],"color":"black"}]}"#,
            "t",
            None,
        )
        .unwrap_err();
        assert_eq!(err.code, ErrorCode::InvalidColor);
    }

    /// **`role` は図形に書けない。** 書けば効いたと思わせるので、未知のキーとして断る。
    #[test]
    fn a_role_on_a_shape_is_an_unknown_field() {
        for extra in [
            r#","role":"heading""#,
            r#","stroke":{}"#,
            r#","lines":["a"]"#,
        ] {
            let err = parse(&shaped(extra), "t", None).unwrap_err();
            assert_eq!(err.code, ErrorCode::SpecUnknownField, "{extra}");
        }
    }

    /// 1 辺だけ書けば、もう 1 辺を profile から受け取る。
    #[test]
    fn one_written_side_keeps_the_other_from_the_profile() {
        let spec = parse(
            r#"{"canvas":{"height":720},"layers":[{"id":"a","type":"image",
               "role":"subject","source":"a.png","rect":[0,0,10,10]}]}"#,
            "t",
            Some(note()),
        )
        .unwrap();
        assert_eq!((spec.canvas.width, spec.canvas.height), (1280, 720));
    }
}
