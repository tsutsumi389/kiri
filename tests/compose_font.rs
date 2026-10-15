//! `kiri compose` の字体を**層ごとに**選ぶ口と、太さの当てはめの検査。
//!
//! # 起きていたこと（2026-10-14 のサムネ検証）
//!
//! 1 ウェイトしか持たないファイル（`ヒラギノ角ゴシック W4.ttc`）を `font.path` で
//! 指し、weight を 100 と 900 にして組むと、**出力がバイト単位で一致し、警告も
//! 出なかった。** usvg は合う太さの face が無ければ最も近い face で組み、太さを
//! 合成しない。`FONT_NOT_FOUND` が名乗る「黙って代替しない」の、太さの側の穴である。
//!
//! 字体は `tests/compose.rs` と同じく**この機械に在るものを借りる。** 在る字体は
//! 機械ごとに違うので、条件に合うものが無ければその検査だけが黙って飛ぶ
//! （`KIRI_REQUIRE_FONT=1` で落とせるのは `common::any_font` を通る検査だけ）。

mod common;

use std::path::{Path, PathBuf};

use assert_cmd::Command;
use resvg::usvg::fontdb;
use serde_json::Value;
use tempfile::TempDir;

fn kiri() -> Command {
    Command::new(env!("CARGO_BIN_EXE_kiri"))
}

fn json_stdout(output: &std::process::Output) -> Value {
    let stdout = String::from_utf8_lossy(&output.stdout);
    serde_json::from_str(&stdout)
        .unwrap_or_else(|e| panic!("stdout が JSON として解釈できません: {e}\n---\n{stdout}"))
}

fn write_spec(dir: &Path, json: &str) -> PathBuf {
    let path = dir.join("layout.json");
    std::fs::write(&path, json).unwrap();
    path
}

fn compose(spec: &Path, out: &Path, extra: &[&str]) -> std::process::Output {
    let mut args = vec![
        "compose",
        spec.to_str().unwrap(),
        "-o",
        out.to_str().unwrap(),
        "--json",
    ];
    args.extend_from_slice(extra);
    kiri().args(&args).output().unwrap()
}

fn layer<'a>(report: &'a Value, id: &str) -> &'a Value {
    report["layers"]
        .as_array()
        .expect("layers が配列")
        .iter()
        .find(|l| l["id"] == id)
        .unwrap_or_else(|| panic!("レイヤ '{id}' が結果に無い: {report}"))
}

fn warnings_of<'a>(report: &'a Value, code: &str) -> Vec<&'a Value> {
    report["warnings"]
        .as_array()
        .map(|w| w.iter().filter(|x| x["code"] == code).collect())
        .unwrap_or_default()
}

const WEIGHT_CODE: &str = "FONT_WEIGHT_UNAVAILABLE";

/// 字体の 1 面について、検査が要る事実だけを抜いたもの。
#[derive(Debug, Clone)]
struct Face {
    family: String,
    path: PathBuf,
    weight: u16,
    /// wght 軸の `[最小, 最大]`。静的な字体では `None`
    wght: Option<(u16, u16)>,
    draws_ab: bool,
    draws_kana: bool,
}

fn system_faces() -> Vec<Face> {
    let mut db = fontdb::Database::new();
    db.load_system_fonts();
    let ids: Vec<_> = db.faces().map(|f| f.id).collect();
    ids.into_iter()
        .filter_map(|id| {
            let info = db.face(id)?;
            let fontdb::Source::File(path) = &info.source else {
                return None;
            };
            let family = info.families.first()?.0.clone();
            // `.SF NS Mono` のような隠し字体は、SVG の font-family で名指しできない
            if family.starts_with('.') {
                return None;
            }
            let weight = info.weight.0;
            let path = path.clone();
            db.with_face_data(id, |data, index| {
                let face = ttf_parser::Face::parse(data, index).ok()?;
                let wght = face
                    .variation_axes()
                    .into_iter()
                    .find(|a| a.tag == ttf_parser::Tag::from_bytes(b"wght"))
                    // **CSS の単位の軸だけを軸として扱う**（`compose::font::drawn_weight`
                    // と同じ線）。0.48〜3.2 のような軸を u16 へ落とすと 0〜3 になる
                    .filter(|a| a.min_value >= 1.0 && a.max_value <= 1000.0)
                    .map(|a| (a.min_value.round() as u16, a.max_value.round() as u16));
                Some(Face {
                    family,
                    path,
                    weight,
                    wght,
                    draws_ab: ['A', 'B'].iter().all(|&c| face.glyph_index(c).is_some()),
                    draws_kana: face.glyph_index('あ').is_some(),
                })
            })?
        })
        .collect()
}

/// **1 ウェイトだけの静的な字体を、ファイルごと 1 つ。**
///
/// ファイルの中に同じ family の別の太さが入っていない（TTC で W3 と W6 を
/// 抱えるものがある）ことまで確かめる——入っていると、遠い weight でも
/// 合う face が見つかってしまう。
fn single_weight_font() -> Option<Face> {
    let faces = system_faces();
    faces
        .iter()
        // spec に書ける太さ（100 刻み）の面だけ。275 の面では「同じ weight の層」が書けない
        .filter(|f| f.wght.is_none() && f.draws_ab && f.weight % 100 == 0)
        .filter(|f| (100..=900).contains(&f.weight))
        .find(|f| {
            faces
                .iter()
                .filter(|o| o.path == f.path && o.family == f.family)
                .all(|o| o.weight == f.weight && o.wght.is_none())
        })
        .cloned()
}

/// wght 軸を持つ可変フォント。**軸の中に 100 刻みの太さが 2 つ以上あるもの**
/// ——狭い軸では、両端で組んだ絵が違うことを確かめられない。
fn variable_font() -> Option<Face> {
    system_faces()
        .into_iter()
        .find(|f| f.draws_ab && f.wght.is_some_and(|(lo, hi)| hundreds(lo, hi).is_some()))
}

/// 軸 `[lo, hi]` の中で最も軽い 100 刻みと最も重い 100 刻み。2 つ取れなければ `None`。
fn hundreds(lo: u16, hi: u16) -> Option<(u16, u16)> {
    let light = lo.div_ceil(100).max(1) * 100;
    let heavy = (hi / 100).min(9) * 100;
    (heavy > light).then_some((light, heavy))
}

/// 遠い weight。その面の太さから 300 以上離れた端を選ぶ。
fn far_weight(weight: u16) -> u16 {
    if weight <= 500 { 900 } else { 100 }
}

/// 字体のファイルを spec の隣へ写す（検証で踏んだ形そのもの）。
///
/// **`std::fs::copy` は使わない。** macOS のそれは `fcopyfile` で、システム字体の
/// 透過圧縮（decmpfs）を**圧縮のまま**写す。並列に走る検査の下でその写しを
/// 別プロセス（kiri）が読むと、ときどき `Attribute not found (os error 93)` で
/// 読めず `FONT_UNREADABLE` になった。中身だけを書けば普通のファイルになる。
fn copy_beside(dir: &Path, font: &Path) -> String {
    let name = font.file_name().unwrap().to_str().unwrap().to_string();
    std::fs::write(dir.join(&name), std::fs::read(font).unwrap()).unwrap();
    name
}

fn text_layer(id: &str, weight: u16, y: u32, font: Option<&str>) -> String {
    let font = font.map(|f| format!(r#","font":{f}"#)).unwrap_or_default();
    format!(
        r##"{{"id":"{id}","type":"text","role":"heading","lines":["AB"],
             "rect":[10,{y},280,60],"size":40,"weight":{weight},
             "color":"#000000"{font}}}"##
    )
}

/// **合う太さの face が無い weight は、黙って別の太さで組まない。**
///
/// 1 ウェイトのファイルで weight を遠くへ動かすと、`FONT_WEIGHT_UNAVAILABLE` が
/// その層を名指しし、指定と実際の太さを数で返す。面と同じ weight の層は言わない。
#[test]
fn a_weight_the_font_cannot_draw_is_named() {
    let Some(face) = single_weight_font() else {
        eprintln!("1 ウェイトの静的な字体が無いので飛ばす");
        return;
    };
    let dir = TempDir::new().unwrap();
    let file = copy_beside(dir.path(), &face.path);
    let far = far_weight(face.weight);
    let spec = write_spec(
        dir.path(),
        &format!(
            r##"{{"canvas":{{"width":300,"height":200,"background":"#ffffff"}},
                 "font":{{"family":"{family}","path":{file}}},
                 "layers":[{exact},{far_layer}]}}"##,
            family = face.family,
            file = serde_json::to_string(&file).unwrap(),
            exact = text_layer("exact", face.weight, 10, None),
            far_layer = text_layer("far", far, 100, None),
        ),
    );
    let out = dir.path().join("out.png");
    let result = compose(&spec, &out, &["--dry-run"]);
    assert!(
        result.status.success(),
        "{}{}",
        String::from_utf8_lossy(&result.stderr),
        String::from_utf8_lossy(&result.stdout)
    );
    let report = json_stdout(&result);

    let found = warnings_of(&report, WEIGHT_CODE);
    assert_eq!(found.len(), 1, "遠い層だけを言うはず: {found:?}");
    let data = &found[0]["data"];
    assert_eq!(data["layer"], "far");
    assert_eq!(data["requested"], far);
    assert_eq!(data["actual"], face.weight);
    assert_eq!(data["family"], face.family.as_str());

    // **実際に描かれた太さは層の結果にも出る。** 警告を読まない受け手でも
    // 指定との差が分かる
    assert_eq!(layer(&report, "far")["font"]["weight"], face.weight);
    assert_eq!(layer(&report, "exact")["font"]["weight"], face.weight);
}

/// **可変フォントは軸の範囲の中なら言わない**——`query` が太さを見ない理由と
/// 同じ罠を、警告の側で踏まない。両端で組んだ絵が違うことも確かめる
/// （usvg が font-weight を wght 軸へ当てていなければ、黙らせるのは嘘になる）。
#[test]
fn a_variable_font_within_its_axis_is_not_reported() {
    let Some(face) = variable_font() else {
        eprintln!("wght 軸を持つ可変フォントが無いので飛ばす");
        return;
    };
    let (lo, hi) = face.wght.unwrap();
    let (lo, hi) = hundreds(lo, hi).unwrap();
    let dir = TempDir::new().unwrap();
    let file = copy_beside(dir.path(), &face.path);
    let render = |weight: u16, name: &str| -> (Value, Vec<u8>) {
        let spec = write_spec(
            dir.path(),
            &format!(
                r##"{{"canvas":{{"width":300,"height":100,"background":"#ffffff"}},
                     "font":{{"family":"{family}","path":{file}}},
                     "layers":[{t}]}}"##,
                family = face.family,
                file = serde_json::to_string(&file).unwrap(),
                t = text_layer("t", weight, 10, None),
            ),
        );
        let out = dir.path().join(name);
        let result = compose(&spec, &out, &[]);
        assert!(
            result.status.success(),
            "{}{}",
            String::from_utf8_lossy(&result.stderr),
            String::from_utf8_lossy(&result.stdout)
        );
        (json_stdout(&result), std::fs::read(&out).unwrap())
    };

    let (light, light_png) = render(lo, "lo.png");
    let (heavy, heavy_png) = render(hi, "hi.png");
    assert!(
        warnings_of(&light, WEIGHT_CODE).is_empty() && warnings_of(&heavy, WEIGHT_CODE).is_empty(),
        "'{}' の wght 軸 {lo}〜{hi} の中なのに言った",
        face.family
    );
    assert!(
        light_png != heavy_png,
        "'{}' の軸の両端で同じ絵になった",
        face.family
    );
    assert_eq!(layer(&heavy, "t")["font"]["weight"], hi);
}

/// 可変フォントでも、**軸の外の weight は端で止まる**ので言う。
#[test]
fn a_variable_font_outside_its_axis_is_reported() {
    let Some(face) = system_faces()
        .into_iter()
        .find(|f| f.draws_ab && f.wght.is_some_and(|(lo, hi)| lo > 100 || hi < 900))
    else {
        eprintln!("wght 軸が 100〜900 より狭い可変フォントが無いので飛ばす");
        return;
    };
    let (lo, hi) = face.wght.unwrap();
    // 実際に描かれる太さは軸の端を 100 刻みへ丸めた値で返る
    let round = |w: u16| ((w + 50) / 100 * 100).clamp(100, 900);
    let (requested, actual) = if lo > 100 {
        (100, round(lo))
    } else {
        (900, round(hi))
    };
    let dir = TempDir::new().unwrap();
    let file = copy_beside(dir.path(), &face.path);
    let spec = write_spec(
        dir.path(),
        &format!(
            r##"{{"canvas":{{"width":300,"height":100,"background":"#ffffff"}},
                 "font":{{"family":"{family}","path":{file}}},
                 "layers":[{t}]}}"##,
            family = face.family,
            file = serde_json::to_string(&file).unwrap(),
            t = text_layer("t", requested, 10, None),
        ),
    );
    let out = dir.path().join("out.png");
    let report = json_stdout(&compose(&spec, &out, &["--dry-run"]));
    let found = warnings_of(&report, WEIGHT_CODE);
    assert_eq!(found.len(), 1, "{report}");
    assert_eq!(found[0]["data"]["requested"], requested);
    assert_eq!(found[0]["data"]["actual"], actual);
}

/// **層ごとに字体を選べる。** 層の `font` は最上位の `font` より強く、書かない層は
/// 最上位を継ぐ。どの層が何で組まれたかは `layers[].font` が辿らせる。
#[test]
fn each_text_layer_can_name_its_own_font() {
    let faces = system_faces();
    let mut drawable = faces.iter().filter(|f| f.draws_ab);
    let Some(first) = drawable.next().cloned() else {
        eprintln!("字体が無いので飛ばす");
        return;
    };
    let Some(second) = drawable.find(|f| f.family != first.family).cloned() else {
        eprintln!("family の違う字体が 2 つ無いので飛ばす");
        return;
    };
    let dir = TempDir::new().unwrap();
    let own = format!(r#"{{"family":"{}"}}"#, second.family);
    let spec = write_spec(
        dir.path(),
        &format!(
            r##"{{"canvas":{{"width":300,"height":200,"background":"#ffffff"}},
                 "font":{{"family":"{family}"}},
                 "layers":[{inherit},{own_layer},
                   {{"id":"img","type":"image","role":"decoration",
                     "source":"dot.png","rect":[0,0,4,4]}}]}}"##,
            family = first.family,
            inherit = text_layer("inherit", 400, 10, None),
            own_layer = text_layer("own", 400, 100, Some(&own)),
        ),
    );
    common::write_png(dir.path(), "dot.png", &image::RgbaImage::new(4, 4));
    let out = dir.path().join("out.png");
    let result = compose(&spec, &out, &["--dry-run"]);
    assert!(
        result.status.success(),
        "{}{}",
        String::from_utf8_lossy(&result.stderr),
        String::from_utf8_lossy(&result.stdout)
    );
    let report = json_stdout(&result);

    // 最上位の `font` は今までどおり最上位の指定を名乗る（互換）
    assert_eq!(report["font"]["family"], first.family.as_str());
    let inherit = &layer(&report, "inherit")["font"];
    let own = &layer(&report, "own")["font"];
    assert_eq!(inherit["family"], first.family.as_str());
    assert_eq!(own["family"], second.family.as_str());
    for font in [inherit, own] {
        assert!(font["path"].is_string(), "実体のパスが出る: {font}");
        assert_eq!(font["sha256"].as_str().unwrap().len(), 64);
    }
    // **画像の層でもキーは出る**（null）
    let img = layer(&report, "img");
    assert!(img.get("font").is_some());
    assert_eq!(img["font"], Value::Null);
}

/// 最上位に `font` が無くても、**文字の層がすべて自分の `font` を持てば組める。**
/// 結果の最上位の `font` は null（最上位に指定が無い）。
#[test]
fn a_spec_with_only_layer_fonts_needs_no_top_level_font() {
    let Some(face) = system_faces().into_iter().find(|f| f.draws_ab) else {
        eprintln!("字体が無いので飛ばす");
        return;
    };
    let dir = TempDir::new().unwrap();
    let own = format!(r#"{{"family":"{}"}}"#, face.family);
    let spec = write_spec(
        dir.path(),
        &format!(
            r##"{{"canvas":{{"width":300,"height":100,"background":"#ffffff"}},
                 "layers":[{t}]}}"##,
            t = text_layer("t", 400, 10, Some(&own)),
        ),
    );
    let out = dir.path().join("out.png");
    let result = compose(&spec, &out, &["--dry-run"]);
    assert!(
        result.status.success(),
        "{}{}",
        String::from_utf8_lossy(&result.stderr),
        String::from_utf8_lossy(&result.stdout)
    );
    let report = json_stdout(&result);
    assert_eq!(report["font"], Value::Null);
    assert_eq!(layer(&report, "t")["font"]["family"], face.family.as_str());
}

/// 層の `font` が見つからなければ、最上位と同じく `FONT_NOT_FOUND` で断る。
/// **どの層の指定かを言う。**
#[test]
fn a_missing_layer_font_is_refused_and_names_the_layer() {
    let dir = TempDir::new().unwrap();
    let spec = write_spec(
        dir.path(),
        &format!(
            r##"{{"canvas":{{"width":300,"height":100}},
                 "layers":[{t}]}}"##,
            t = text_layer("lead", 400, 10, Some(r#"{"family":"NoSuchFontXYZ"}"#)),
        ),
    );
    let out = dir.path().join("out.png");
    let result = compose(&spec, &out, &[]);
    assert_eq!(result.status.code(), Some(3));
    let report = json_stdout(&result);
    assert_eq!(report["error"]["code"], "FONT_NOT_FOUND");
    let message = report["error"]["message"].as_str().unwrap();
    assert!(message.contains("NoSuchFontXYZ"), "{message}");
    assert!(!out.exists());
}

/// 層の `font` の綴り違いは、**読む前に**断る（最上位の `font` と同じ関門）。
#[test]
fn an_unknown_key_in_a_layer_font_is_refused() {
    let dir = TempDir::new().unwrap();
    let spec = write_spec(
        dir.path(),
        &format!(
            r##"{{"canvas":{{"width":300,"height":100}},
                 "font":{{"family":"NoSuchFontXYZ"}},
                 "layers":[{t}]}}"##,
            t = text_layer("t", 400, 10, Some(r#"{"famly":"X"}"#)),
        ),
    );
    let out = dir.path().join("out.png");
    let result = compose(&spec, &out, &[]);
    assert_eq!(result.status.code(), Some(3));
    let report = json_stdout(&result);
    assert_eq!(report["error"]["code"], "SPEC_UNKNOWN_FIELD");
    assert!(
        report["error"]["message"]
            .as_str()
            .unwrap()
            .contains("layers[0].font"),
        "{report}"
    );
}

/// 画像の層に `font` は書けない。**効かない指定を黙って受けない。**
#[test]
fn an_image_layer_cannot_take_a_font() {
    let dir = TempDir::new().unwrap();
    let spec = write_spec(
        dir.path(),
        r##"{"canvas":{"width":100,"height":100},
             "layers":[{"id":"a","type":"image","role":"subject","source":"a.png",
                        "rect":[0,0,10,10],"font":{"family":"X"}}]}"##,
    );
    let out = dir.path().join("out.png");
    let result = compose(&spec, &out, &[]);
    assert_eq!(json_stdout(&result)["error"]["code"], "SPEC_UNKNOWN_FIELD");
}

/// **グリフの欠けはその字体を使う層の文字だけで見る。** 仮名を持たない字体の層と、
/// 仮名を持つ字体の層を並べたとき、前者の字体に仮名の欠けを言わない。
#[test]
fn missing_glyphs_are_checked_per_font() {
    let faces = system_faces();
    let Some(latin) = faces.iter().find(|f| f.draws_ab && !f.draws_kana) else {
        eprintln!("仮名を持たない字体が無いので飛ばす");
        return;
    };
    let Some(kana) = faces.iter().find(|f| f.draws_kana) else {
        eprintln!("仮名を持つ字体が無いので飛ばす");
        return;
    };
    let dir = TempDir::new().unwrap();
    let spec = write_spec(
        dir.path(),
        &format!(
            r##"{{"canvas":{{"width":300,"height":200,"background":"#ffffff"}},
                 "font":{{"family":"{latin}"}},
                 "layers":[{t},
                   {{"id":"jp","type":"text","role":"body","lines":["あ"],
                     "rect":[10,100,280,60],"size":40,"color":"#000000",
                     "font":{{"family":"{kana}"}}}}]}}"##,
            latin = latin.family,
            kana = kana.family,
            t = text_layer("en", 400, 10, None),
        ),
    );
    let out = dir.path().join("out.png");
    let report = json_stdout(&compose(&spec, &out, &["--dry-run"]));
    assert!(
        warnings_of(&report, "FONT_GLYPHS_MISSING").is_empty(),
        "別の字体で組む層の文字を '{}' の欠けとして数えた: {report}",
        latin.family
    );
}

/// 契約の配布。**新しい警告は schema の warnings[] に remedy 付きで載り、
/// 層の `font` は compose_spec[] に載る。**
#[test]
fn the_schema_names_the_weight_warning_and_the_layer_font() {
    let output = kiri().args(["schema", "--json"]).output().unwrap();
    assert!(output.status.success());
    let schema = json_stdout(&output);
    let entry = schema["warnings"]
        .as_array()
        .unwrap()
        .iter()
        .find(|w| w["code"] == WEIGHT_CODE)
        .expect("warnings[] に載る");
    assert!(entry["remedy"].is_string());

    let spec = schema["compose_spec"].as_array().unwrap();
    let text = spec
        .iter()
        .find(|e| e["at"] == "layers[type=text]")
        .unwrap();
    assert!(
        text["keys"].as_array().unwrap().iter().any(|k| k == "font"),
        "{text}"
    );
    assert!(
        spec.iter().any(|e| e["at"] == "layers[type=text].font"),
        "層の font の形が配られていない"
    );
}

/// **100 刻みでない weight は断る。** usvg は "100"〜"900" しか太さとして読まず、
/// 450 は黙って 400 で組まれる（可変フォントでも、である）。字体は要らない。
#[test]
fn a_weight_that_is_not_a_multiple_of_100_is_refused() {
    let dir = TempDir::new().unwrap();
    let spec = write_spec(
        dir.path(),
        &format!(
            r##"{{"canvas":{{"width":300,"height":100}},
                 "font":{{"family":"NoSuchFontXYZ"}},
                 "layers":[{t}]}}"##,
            t = text_layer("t", 450, 10, None),
        ),
    );
    let out = dir.path().join("out.png");
    let result = compose(&spec, &out, &[]);
    assert_eq!(result.status.code(), Some(3));
    assert_eq!(json_stdout(&result)["error"]["code"], "SPEC_INVALID");
}

/// 層が自分の `font` を持っていても、**書いた最上位の `font` が無ければ断る。**
/// 使わない既定でも、書いた指定を黙って捨てない。
#[test]
fn a_missing_top_level_font_is_refused_even_when_every_layer_overrides_it() {
    let Some(face) = system_faces().into_iter().find(|f| f.draws_ab) else {
        eprintln!("字体が無いので飛ばす");
        return;
    };
    let dir = TempDir::new().unwrap();
    let own = format!(r#"{{"family":"{}"}}"#, face.family);
    let spec = write_spec(
        dir.path(),
        &format!(
            r##"{{"canvas":{{"width":300,"height":100}},
                 "font":{{"family":"NoSuchFontXYZ"}},
                 "layers":[{t}]}}"##,
            t = text_layer("t", 400, 10, Some(&own)),
        ),
    );
    let out = dir.path().join("out.png");
    let result = compose(&spec, &out, &[]);
    assert_eq!(json_stdout(&result)["error"]["code"], "FONT_NOT_FOUND");
}
