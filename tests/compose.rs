//! `kiri compose` の統合テスト（計画 §10）。
//!
//! # 字体をリポジトリに置いていない
//!
//! **`tests/segment.rs` がモデルに対して採ったのと同じ扱いである。** 日本語の
//! 字体は 5〜20MB あり、ライセンスの表示義務も付く。置いていない機械では、
//! 字体を要する検査だけが黙って飛ぶ（`#[ignore]` を付けて回らない検査にすると、
//! 置いてある機械でも走らなくなる）。
//!
//! **字体の綴りに依存する数は表明しない。** 「この見出しの外接矩形は 1344px」は
//! 字体ごとに違う値になるので、機械をまたいで固定できる約束ではない。固定するのは
//! **関係**である——枠より広ければはみ出しが正になる、`line_widths` の最大が
//! `placed` の幅と一致する、黒と白のコントラストは 21 に近い、など。
//! §10.2 が記録した実測値は設計文書の側に置いてある。
//!
//! 字体が要らない検査——画像の内接、断り方、spec の形、契約の配布、決定性の
//! 一部——は、どの機械でも必ず走る。

mod common;

use std::path::{Path, PathBuf};

use assert_cmd::Command;
use common::{ProductSpec, product_image, write_png};
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

/// このファイルの表明が使う文字を**実際に描ける**字体を 1 つ借りる。
///
/// 返すのは「この機械に在る family」で、どれが在るかは機械ごとに違う。
/// 在るものを 1 つ借りて**関係だけ**を確かめる、というのがこのファイルの作法で
/// ある（モジュールの doc を参照）。
///
/// **グリフの有無まで見る。** 1 面目をそのまま採ると記号だけの字体を掴むことが
/// あり、そのとき `A` は豆腐にも成らずに消える——`FONT_GLYPHS_MISSING` と
/// `TEXT_NOT_RENDERED` が出て、字体とは関係のない表明まで落ちる。
fn any_font() -> Option<String> {
    common::any_font(&TEST_CHARS)
}

/// このファイルが組ませる文字。`any_font` が覆いを確かめる相手である。
const TEST_CHARS: [char; 2] = ['A', 'B'];

/// 縦長の素材を 1 枚置く。**縦横比が枠と違うことが要点である**——
/// 枠にそのまま収まる素材では「内接して縮んだ」ことを確かめられない。
fn portrait(dir: &Path) -> PathBuf {
    let image = product_image(&ProductSpec {
        width: 200,
        height: 400,
        ..Default::default()
    });
    write_png(dir, "product.png", &image)
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

fn rect_of(value: &Value) -> [f64; 4] {
    let a = value.as_array().expect("矩形は配列");
    [
        a[0].as_f64().unwrap(),
        a[1].as_f64().unwrap(),
        a[2].as_f64().unwrap(),
        a[3].as_f64().unwrap(),
    ]
}

fn codes(report: &Value) -> Vec<String> {
    report["warnings"]
        .as_array()
        .map(|w| {
            w.iter()
                .map(|x| x["code"].as_str().unwrap().to_string())
                .collect()
        })
        .unwrap_or_default()
}

/// **書いた枠と実際に置かれた矩形は違う**（§10.8 の (a)）。
///
/// これが返らなければ、呼ぶ側は余白を計算できない。枠を書いた側は素材の縦横比を
/// 知らないからである——200x400 の素材を 160x160 の枠へ `contain` で入れると
/// 80x160 になり、枠の中央へ寄る。
#[test]
fn the_placed_rect_is_not_the_rect_that_was_written() {
    let dir = TempDir::new().unwrap();
    portrait(dir.path());
    let spec = write_spec(
        dir.path(),
        r##"{"canvas":{"width":400,"height":400,"background":"#ffffff"},
             "layers":[{"id":"subject","type":"image","role":"subject",
                        "source":"product.png","rect":[100,100,160,160]}]}"##,
    );
    let out = dir.path().join("out.png");
    let result = compose(&spec, &out, &[]);
    assert!(
        result.status.success(),
        "compose: {}",
        String::from_utf8_lossy(&result.stderr)
    );
    let report = json_stdout(&result);
    let subject = layer(&report, "subject");

    assert_eq!(
        rect_of(&subject["rect"]),
        [100.0, 100.0, 160.0, 160.0],
        "rect は spec が書いたままである"
    );
    assert_eq!(
        rect_of(&subject["placed"]),
        [140.0, 100.0, 80.0, 160.0],
        "縦長の素材は枠へ内接し、横 80px に縮んで中央へ寄る"
    );
}

/// 文字が枠に収まらなければ、**縮めずに数で言う**（§10.8 の (b)(i)）。
#[test]
fn text_that_does_not_fit_is_reported_in_pixels_not_shrunk() {
    let Some(family) = any_font() else {
        return;
    };
    let dir = TempDir::new().unwrap();
    let spec = write_spec(
        dir.path(),
        &format!(
            r##"{{"canvas":{{"width":400,"height":200,"background":"#ffffff"}},
                 "font":{{"family":"{family}"}},
                 "layers":[{{"id":"heading","type":"text","role":"heading",
                            "lines":["AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA"],
                            "rect":[10,10,40,60],"size":40,"color":"#000000"}}]}}"##
        ),
    );
    let out = dir.path().join("out.png");
    let report = json_stdout(&compose(&spec, &out, &[]));
    let heading = layer(&report, "heading");

    let overflow = heading["text_overflow"]
        .as_f64()
        .expect("はみ出しは数で出る");
    assert!(overflow > 0.0, "幅 40px の枠に 32 文字が収まるはずがない");

    // **縮んでいないことの裏。** 外接矩形の高さが size より小さければ、
    // どこかで字が小さくされている
    let placed = rect_of(&heading["placed"]);
    assert!(
        placed[3] >= 40.0 * 0.9,
        "字が縮められている（高さ {}）",
        placed[3]
    );

    let widths = heading["line_widths"].as_array().expect("行ごとの幅が出る");
    assert_eq!(widths.len(), 1, "1 行書いたので 1 つ");
    assert!(
        (widths[0].as_f64().unwrap() - placed[2]).abs() < 0.01,
        "1 行のときは行の幅と外接矩形の幅が一致する"
    );

    assert!(codes(&report).contains(&"TEXT_OVERFLOW".to_string()));
}

/// はみ出しは `--fail-on` で exit 5 になる（§10.8 の (b)）。
#[test]
fn the_gate_returns_exit_five_for_an_overflowing_line() {
    let Some(family) = any_font() else {
        return;
    };
    let dir = TempDir::new().unwrap();
    let spec = write_spec(
        dir.path(),
        &format!(
            r##"{{"canvas":{{"width":400,"height":200,"background":"#ffffff"}},
                 "font":{{"family":"{family}"}},
                 "layers":[{{"id":"heading","type":"text","role":"heading",
                            "lines":["AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA"],
                            "rect":[10,10,40,60],"size":40,"color":"#000000"}}]}}"##
        ),
    );
    let out = dir.path().join("out.png");
    let result = compose(&spec, &out, &["--fail-on", "text_overflow>0"]);

    assert_eq!(result.status.code(), Some(5), "規格違反は exit 5");
    // **成果物はある。** exit 5 は「人が見る対象」であって失敗ではない
    assert!(out.exists(), "exit 5 でも書いたものは残る");

    let report = json_stdout(&result);
    assert_eq!(report["compliance"]["passed"], Value::Bool(false));
    assert_eq!(report["compliance"]["code"], "QUALITY_GATE_FAILED");
    let check = report["compliance"]["checks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["metric"] == "text_overflow")
        .expect("書いた指標が checks[] に出る");
    assert_eq!(check["status"], "fail");
    assert!(check["actual"].as_f64().unwrap() > 0.0);
}

/// 無い字体は**黙って代替へ落ちず**、1 枚も書かない（§10.8 の (c)）。
///
/// §10.2 の実測では、存在しない family を指定した行が指定どおりの行高で
/// 描けてしまった。それを通すと同じ spec が機械ごとに違う絵になる。
#[test]
fn a_missing_font_is_refused_and_nothing_is_written() {
    let dir = TempDir::new().unwrap();
    let spec = write_spec(
        dir.path(),
        r##"{"canvas":{"width":200,"height":100,"background":"#ffffff"},
             "font":{"family":"NoSuchFontXYZ"},
             "layers":[{"id":"t","type":"text","role":"heading",
                        "lines":["あ"],"rect":[10,10,180,80],
                        "size":20,"color":"#000000"}]}"##,
    );
    let out = dir.path().join("out.png");
    let result = compose(&spec, &out, &[]);

    assert_eq!(result.status.code(), Some(3), "入力の失敗は exit 3");
    let report = json_stdout(&result);
    assert_eq!(report["error"]["code"], "FONT_NOT_FOUND");
    assert!(
        !out.exists(),
        "断ったのにファイルが残っている（半端な成果物を作らない）"
    );
}

/// 文字があるのに `font` を書いていない spec は、**1 バイトも読む前に**断る。
#[test]
fn text_without_a_font_is_refused_before_anything_is_read() {
    let dir = TempDir::new().unwrap();
    let spec = write_spec(
        dir.path(),
        r##"{"canvas":{"width":200,"height":100},
             "layers":[{"id":"t","type":"text","role":"heading",
                        "lines":["あ"],"rect":[10,10,180,80],
                        "size":20,"color":"#000000"}]}"##,
    );
    let out = dir.path().join("out.png");
    let result = compose(&spec, &out, &[]);
    assert_eq!(result.status.code(), Some(3));
    assert_eq!(json_stdout(&result)["error"]["code"], "FONT_NOT_FOUND");
}

/// コントラストは**背後の実測**である（§10.8 の (d)）。
///
/// 下に暗い画像が敷いてあれば、背景色の指定（白）から計算した値とは必ず違う。
/// ちょうどそこが読めなくなる場所なので、**最も知りたい場合に外れる**計算では
/// 意味が無い。
#[test]
fn contrast_is_measured_against_what_is_actually_behind_the_text() {
    let Some(family) = any_font() else {
        return;
    };
    let dir = TempDir::new().unwrap();
    // 下地は白。その上に**暗い矩形**を敷き、さらにその上へ黒い文字を置く
    let dark = image::RgbaImage::from_pixel(200, 200, image::Rgba([20, 20, 20, 255]));
    write_png(dir.path(), "dark.png", &dark);

    let spec = write_spec(
        dir.path(),
        &format!(
            r##"{{"canvas":{{"width":200,"height":200,"background":"#ffffff"}},
                 "font":{{"family":"{family}"}},
                 "layers":[
                   {{"id":"plate","type":"image","role":"decoration",
                     "source":"dark.png","rect":[0,0,200,200],"fit":"exact"}},
                   {{"id":"on_dark","type":"text","role":"body",
                     "lines":["AAA"],"rect":[10,20,180,60],
                     "size":40,"color":"#000000"}}]}}"##
        ),
    );
    let out = dir.path().join("out.png");
    let report = json_stdout(&compose(&spec, &out, &[]));
    let measured = layer(&report, "on_dark")["text_contrast"]
        .as_f64()
        .expect("コントラストが出る");

    // 白地に黒なら 21 に近い。暗い板の上なので、そこからは大きく離れる
    assert!(
        measured < 3.0,
        "暗い板の上の黒文字が {measured:.2}。背景色の指定（白）から計算していないか"
    );
    assert!(
        codes(&report).contains(&"TEXT_CONTRAST_LOW".to_string()),
        "読めない組み合わせなのに警告が出ていない: {:?}",
        codes(&report)
    );
}

/// 白地に黒は 21 に近い。**字体に依らない関係である。**
#[test]
fn black_on_white_is_near_the_maximum_contrast() {
    let Some(family) = any_font() else {
        return;
    };
    let dir = TempDir::new().unwrap();
    let spec = write_spec(
        dir.path(),
        &format!(
            r##"{{"canvas":{{"width":200,"height":120,"background":"#ffffff"}},
                 "font":{{"family":"{family}"}},
                 "layers":[{{"id":"t","type":"text","role":"body",
                            "lines":["AAA"],"rect":[10,20,180,60],
                            "size":40,"color":"#000000"}}]}}"##
        ),
    );
    let out = dir.path().join("out.png");
    let report = json_stdout(&compose(&spec, &out, &[]));
    let measured = layer(&report, "t")["text_contrast"].as_f64().unwrap();
    assert!(measured > 20.0, "白地に黒が {measured:.2}（21 に近いはず）");
    assert!(!codes(&report).contains(&"TEXT_CONTRAST_LOW".to_string()));
}

/// 同じ spec は 2 回ともバイト一致する（§10.8 の (e)）。
///
/// **`family` で引いた実行と、`path` で固定した実行の両方を見る。** 字体の
/// ファイルは 1 回目の結果 JSON が教えてくれるので、リポジトリに置かずに
/// 2 通りを確かめられる。
#[test]
fn the_same_spec_produces_the_same_bytes_twice() {
    let Some(family) = any_font() else {
        return;
    };
    let dir = TempDir::new().unwrap();
    portrait(dir.path());
    let body = |font: String| {
        format!(
            r##"{{"canvas":{{"width":300,"height":200,"background":"#f5f2ec"}},
                 "layers":[
                   {{"id":"subject","type":"image","role":"subject",
                     "source":"product.png","rect":[180,20,100,160]}},
                   {{"id":"t","type":"text","role":"heading",
                     "lines":["AB"],"rect":[20,40,140,60],
                     "size":32,"color":"#1f2328"}}],
                 "font":{font}}}"##
        )
    };

    let spec = write_spec(dir.path(), &body(format!(r#"{{"family":"{family}"}}"#)));
    let first = dir.path().join("a.png");
    let report = json_stdout(&compose(&spec, &first, &[]));
    let path = report["font"]["path"]
        .as_str()
        .expect("字体の実体が結果に出る")
        .to_string();

    let second = dir.path().join("b.png");
    assert!(compose(&spec, &second, &[]).status.success());
    assert_eq!(
        std::fs::read(&first).unwrap(),
        std::fs::read(&second).unwrap(),
        "family で引いた 2 回がバイト一致しない"
    );

    // **同じ字体をファイルで固定しても同じ絵になる。** ここが割れるなら、
    // `path` 指定と `family` 指定で別の face が選ばれている
    let pinned = write_spec(
        dir.path(),
        &body(format!(
            r#"{{"family":"{family}","path":{}}}"#,
            serde_json::to_string(&path).unwrap()
        )),
    );
    let third = dir.path().join("c.png");
    assert!(
        compose(&pinned, &third, &[]).status.success(),
        "path で固定した spec が通らない"
    );
    assert_eq!(
        std::fs::read(&first).unwrap(),
        std::fs::read(&third).unwrap(),
        "family で引いた実行と path で固定した実行が一致しない"
    );
}

/// `--dry-run` は幾何を返して**画素を書かない**。
#[test]
fn a_dry_run_returns_the_geometry_without_writing() {
    let dir = TempDir::new().unwrap();
    portrait(dir.path());
    let spec = write_spec(
        dir.path(),
        r##"{"canvas":{"width":300,"height":300,"background":"#ffffff"},
             "layers":[{"id":"subject","type":"image","role":"subject",
                        "source":"product.png","rect":[50,50,200,200]}]}"##,
    );
    let out = dir.path().join("out.png");
    let report = json_stdout(&compose(&spec, &out, &["--dry-run"]));

    assert_eq!(report["dry_run"], Value::Bool(true));
    assert!(!out.exists(), "--dry-run なのに書かれている");
    // **バイト数は返る。** エンコードまでは通すので、本番で初めて形式の
    // 制約に当たることにならない
    assert!(report["output"]["bytes"].as_u64().unwrap() > 0);
    assert_eq!(rect_of(&layer(&report, "subject")["placed"])[2], 100.0);
}

/// `safe_area` の外へ出た要素を言う。
#[test]
fn an_element_outside_the_safe_area_is_named() {
    let dir = TempDir::new().unwrap();
    portrait(dir.path());
    let spec = write_spec(
        dir.path(),
        r##"{"canvas":{"width":300,"height":300,"background":"#ffffff"},
             "safe_area":[100,100,100,100],
             "layers":[{"id":"subject","type":"image","role":"subject",
                        "source":"product.png","rect":[0,0,300,300]}]}"##,
    );
    let out = dir.path().join("out.png");
    let report = json_stdout(&compose(&spec, &out, &["--dry-run"]));

    assert_eq!(
        layer(&report, "subject")["outside_safe_area"],
        Value::Bool(true)
    );
    assert!(codes(&report).contains(&"OUTSIDE_SAFE_AREA".to_string()));
}

/// **描画の端数ははみ出しではない**（2026-10-14 のサムネ検証の再現）。
///
/// ヒラギノ角ゴ W8 の size 104 を y=190 に置くと、外接矩形の y が f32 の
/// 丸めで 189.99998 になる。これを 1.5e-5px のはみ出しと数えると、
/// `--fail-on text_overflow>0` が見た目に何も起きていない構図で exit 5 を返す。
/// `safe_area` の上辺をちょうど 190 にして、`outside` の側も同じ端数で見る。
///
/// 字体はこの機械の綴りに依る値なので、**在る機械でだけ走る**（モジュールの doc）。
#[test]
fn a_rounding_error_in_the_glyph_box_is_neither_overflow_nor_outside() {
    const W8: &str = "/System/Library/Fonts/ヒラギノ角ゴシック W8.ttc";
    if !Path::new(W8).exists() {
        return;
    }
    let dir = TempDir::new().unwrap();
    let spec = write_spec(
        dir.path(),
        &format!(
            r##"{{"canvas":{{"width":1280,"height":670,"background":"#ffffff"}},
                 "safe_area":[0,190,1280,300],
                 "font":{{"family":"Hiragino Sans","path":"{W8}"}},
                 "layers":[{{"id":"h1","type":"text","role":"heading",
                            "lines":["切り抜き"],
                            "rect":[70,190,780,120],"size":104,"color":"#000000"}}]}}"##
        ),
    );
    let out = dir.path().join("out.png");
    let result = compose(&spec, &out, &["--fail-on", "text_overflow>0"]);
    let report = json_stdout(&result);
    let h1 = layer(&report, "h1");
    // **前提の確かめ。** 字体や `usvg` の版で y がちょうど 190 に戻ると、
    // 端数の経路を通らないまま緑になる
    assert_ne!(
        h1["placed"][1].as_f64(),
        Some(190.0),
        "端数が再現していない: {h1}"
    );

    assert_eq!(h1["text_overflow"].as_f64(), Some(0.0), "{h1}");
    assert_eq!(h1["outside_safe_area"], Value::Bool(false), "{h1}");
    let codes = codes(&report);
    assert!(!codes.contains(&"TEXT_OVERFLOW".to_string()), "{codes:?}");
    assert!(
        !codes.contains(&"OUTSIDE_SAFE_AREA".to_string()),
        "{codes:?}"
    );
    assert_eq!(result.status.code(), Some(0), "{report}");
}

/// `safe_area` を書いていなければ測らない。**null であってキーは出る。**
#[test]
fn without_a_safe_area_the_key_is_null_but_present() {
    let dir = TempDir::new().unwrap();
    portrait(dir.path());
    let spec = write_spec(
        dir.path(),
        r##"{"canvas":{"width":300,"height":300},
             "layers":[{"id":"subject","type":"image","role":"subject",
                        "source":"product.png","rect":[0,0,300,300]}]}"##,
    );
    let out = dir.path().join("out.png");
    let report = json_stdout(&compose(&spec, &out, &["--dry-run"]));
    let subject = layer(&report, "subject");
    assert!(
        subject.get("outside_safe_area").is_some(),
        "キーごと消すと「測らなかった」と「古い版で走った」が同じ形になる"
    );
    assert_eq!(subject["outside_safe_area"], Value::Null);
}

/// 未知のキーは綴り違いの候補を添えて断る。
#[test]
fn an_unknown_key_is_refused_with_a_suggestion() {
    let dir = TempDir::new().unwrap();
    let spec = write_spec(
        dir.path(),
        r##"{"canvas":{"width":10,"heigth":10},
             "layers":[{"id":"a","type":"image","role":"subject",
                        "source":"x.png","rect":[0,0,1,1]}]}"##,
    );
    let out = dir.path().join("out.png");
    let result = compose(&spec, &out, &[]);
    assert_eq!(result.status.code(), Some(3));
    let report = json_stdout(&result);
    assert_eq!(report["error"]["code"], "SPEC_UNKNOWN_FIELD");
    assert!(
        report["error"]["hint"]
            .as_str()
            .unwrap_or_default()
            .contains("height"),
        "綴り違いの候補が添えられていない: {report}"
    );
}

/// `type` ごとに書けるキーが違う。**和集合で検査しない。**
///
/// 画像のレイヤに `line_height` を書いた spec が素通りすると、効かない指定が
/// 結果にも現れないまま残る。
#[test]
fn a_text_only_key_on_an_image_layer_is_refused() {
    let dir = TempDir::new().unwrap();
    let spec = write_spec(
        dir.path(),
        r##"{"canvas":{"width":10,"height":10},
             "layers":[{"id":"a","type":"image","role":"subject",
                        "source":"x.png","rect":[0,0,1,1],"line_height":1.4}]}"##,
    );
    let out = dir.path().join("out.png");
    let result = compose(&spec, &out, &[]);
    assert_eq!(result.status.code(), Some(3));
    assert_eq!(json_stdout(&result)["error"]["code"], "SPEC_UNKNOWN_FIELD");
}

/// `id` は結果で層を指す鍵なので、1 枚の中で重複できない。
#[test]
fn duplicate_layer_ids_are_refused() {
    let dir = TempDir::new().unwrap();
    let spec = write_spec(
        dir.path(),
        r##"{"canvas":{"width":10,"height":10},
             "layers":[{"id":"a","type":"image","role":"subject",
                        "source":"x.png","rect":[0,0,1,1]},
                       {"id":"a","type":"image","role":"decoration",
                        "source":"x.png","rect":[0,0,1,1]}]}"##,
    );
    let out = dir.path().join("out.png");
    let result = compose(&spec, &out, &[]);
    assert_eq!(result.status.code(), Some(3));
    assert_eq!(json_stdout(&result)["error"]["code"], "SPEC_INVALID");
}

/// `--fail-on` の綴り違いは、**組む前に**断る。
#[test]
fn a_misspelled_metric_is_refused_before_anything_is_composed() {
    let dir = TempDir::new().unwrap();
    portrait(dir.path());
    let spec = write_spec(
        dir.path(),
        r##"{"canvas":{"width":50,"height":50},
             "layers":[{"id":"a","type":"image","role":"subject",
                        "source":"product.png","rect":[0,0,50,50]}]}"##,
    );
    let out = dir.path().join("out.png");
    let result = compose(&spec, &out, &["--fail-on", "text_overflw>0"]);
    // 書式の誤りは clap が code 無しの exit 2 で断る（kiri 全体の規約）
    assert_eq!(result.status.code(), Some(2));
    assert!(!out.exists());
    let stderr = String::from_utf8_lossy(&result.stderr);
    assert!(
        stderr.contains("text_overflow"),
        "綴り違いの候補が出ていない: {stderr}"
    );
}

/// cutout の指標を compose へ、compose の指標を cutout へは書けない。
///
/// **混ぜると永久に発火しない門ができる。** cutout に文字は 1 つも無いので、
/// `text_overflow` の条件は何をしても満たされない——書いた本人は合格が
/// 出続けるのを見て「通っている」と読む。
#[test]
fn the_two_fail_on_vocabularies_do_not_accept_each_other() {
    let dir = TempDir::new().unwrap();
    let input = portrait(dir.path());
    let out = dir.path().join("out.png");
    let spec = write_spec(
        dir.path(),
        r##"{"canvas":{"width":50,"height":50},
             "layers":[{"id":"a","type":"image","role":"subject",
                        "source":"product.png","rect":[0,0,50,50]}]}"##,
    );

    let to_cutout = kiri()
        .args([
            "cutout",
            input.to_str().unwrap(),
            "-o",
            out.to_str().unwrap(),
            "--dry-run",
            "--fail-on",
            "text_overflow>0",
        ])
        .output()
        .unwrap();
    assert_eq!(
        to_cutout.status.code(),
        Some(2),
        "cutout が compose の指標を受けた"
    );

    let to_compose = compose(&spec, &out, &["--fail-on", "halo_ratio>0.1"]);
    assert_eq!(
        to_compose.status.code(),
        Some(2),
        "compose が cutout の指標を受けた"
    );
}

/// `kiri schema` が spec の形を配る（§10.8 の (g)）。
///
/// ここが無いと、エージェントは spec の形を推測で書く。
#[test]
fn the_schema_publishes_the_shape_of_the_spec() {
    let out = kiri().args(["schema", "--json"]).output().unwrap();
    let schema = json_stdout(&out);

    let entries = schema["compose_spec"]
        .as_array()
        .expect("compose_spec が配られていない");
    let at = |name: &str| -> Vec<String> {
        entries
            .iter()
            .find(|e| e["at"] == name)
            .unwrap_or_else(|| panic!("compose_spec に '{name}' が無い"))["keys"]
            .as_array()
            .unwrap()
            .iter()
            .map(|k| k.as_str().unwrap().to_string())
            .collect()
    };
    assert!(at("spec").contains(&"layers".to_string()));
    assert!(at("layers[type=text]").contains(&"lines".to_string()));
    assert!(
        !at("layers[type=image]").contains(&"lines".to_string()),
        "画像のレイヤに lines を書けると配っている"
    );

    // コマンド一覧にも出る
    let names: Vec<&str> = schema["commands"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["name"].as_str().unwrap())
        .collect();
    assert!(names.contains(&"compose"), "commands[] に compose が無い");
}

/// 重なりは**字が実際に覆う画素**で数える（外接矩形ではない）。
///
/// 矩形で数えると、行間と字間の空白まで商品に重なったことになる。
#[test]
fn the_overlap_counts_glyph_coverage_not_the_bounding_box() {
    let Some(family) = any_font() else {
        return;
    };
    let dir = TempDir::new().unwrap();
    let plate = image::RgbaImage::from_pixel(200, 200, image::Rgba([200, 30, 30, 255]));
    write_png(dir.path(), "plate.png", &plate);

    // 商品はキャンバス全面。文字はその上に重ねる——**外接矩形で数えれば 1.0 に
    // なるが、字が覆うのは矩形の一部**なので、1.0 未満かつ 0 より大きくなる
    let spec = write_spec(
        dir.path(),
        &format!(
            r##"{{"canvas":{{"width":200,"height":200,"background":"#ffffff"}},
                 "font":{{"family":"{family}"}},
                 "layers":[
                   {{"id":"subject","type":"image","role":"subject",
                     "source":"plate.png","rect":[0,0,200,200],"fit":"exact"}},
                   {{"id":"t","type":"text","role":"heading",
                     "lines":["AB"],"rect":[20,40,160,60],
                     "size":40,"color":"#ffffff"}}]}}"##
        ),
    );
    let out = dir.path().join("out.png");
    let report = json_stdout(&compose(&spec, &out, &["--dry-run"]));
    let ratio = layer(&report, "t")["layer_overlap"]
        .as_f64()
        .expect("重なりが出る");

    assert!(
        ratio > 0.99,
        "商品が全面なので、字が覆う画素はすべて商品の上にある（{ratio}）"
    );
    assert!(codes(&report).contains(&"LAYERS_OVERLAP".to_string()));

    // subject が 1 つも無ければ測らない。**null であってキーは出る**
    let alone = write_spec(
        dir.path(),
        &format!(
            r##"{{"canvas":{{"width":200,"height":200,"background":"#ffffff"}},
                 "font":{{"family":"{family}"}},
                 "layers":[{{"id":"t","type":"text","role":"heading",
                            "lines":["AB"],"rect":[20,40,160,60],
                            "size":40,"color":"#000000"}}]}}"##
        ),
    );
    let report = json_stdout(&compose(&alone, &out, &["--dry-run", "--force"]));
    assert_eq!(layer(&report, "t")["layer_overlap"], Value::Null);
}

/// `decoration` はコントラストを測らない。
///
/// 測らないことを spec の側から言える口が無いと、意図した薄い文字が毎回
/// 警告を出す。
#[test]
fn a_decoration_is_not_asked_to_be_readable() {
    let Some(family) = any_font() else {
        return;
    };
    let dir = TempDir::new().unwrap();
    let spec = write_spec(
        dir.path(),
        &format!(
            r##"{{"canvas":{{"width":200,"height":120,"background":"#ffffff"}},
                 "font":{{"family":"{family}"}},
                 "layers":[{{"id":"t","type":"text","role":"decoration",
                            "lines":["AAA"],"rect":[10,20,180,60],
                            "size":40,"color":"#fafafa"}}]}}"##
        ),
    );
    let out = dir.path().join("out.png");
    let report = json_stdout(&compose(&spec, &out, &["--dry-run"]));
    assert_eq!(layer(&report, "t")["text_contrast"], Value::Null);
    assert!(!codes(&report).contains(&"TEXT_CONTRAST_LOW".to_string()));
}

/// 字体の素性（パスとダイジェスト）を結果に載せる。
///
/// **後から「何で組まれたか」を辿れる**ようにするためで、モデルの素性を
/// `settings` に載せているのと同じ扱いである。
#[test]
fn the_font_that_was_actually_used_is_named_in_the_result() {
    let Some(family) = any_font() else {
        return;
    };
    let dir = TempDir::new().unwrap();
    let spec = write_spec(
        dir.path(),
        &format!(
            r##"{{"canvas":{{"width":200,"height":120,"background":"#ffffff"}},
                 "font":{{"family":"{family}"}},
                 "layers":[{{"id":"t","type":"text","role":"body",
                            "lines":["A"],"rect":[10,20,180,60],
                            "size":30,"color":"#000000"}}]}}"##
        ),
    );
    let out = dir.path().join("out.png");
    let report = json_stdout(&compose(&spec, &out, &["--dry-run"]));
    assert_eq!(report["font"]["family"], family.as_str());
    let digest = report["font"]["sha256"]
        .as_str()
        .expect("ダイジェストが出る");
    assert_eq!(digest.len(), 64, "SHA-256 は 64 桁の 16 進である");
}

/// 文字が 1 つも無い spec では `font` が null になる。**キーは出る。**
#[test]
fn a_spec_without_text_reports_a_null_font() {
    let dir = TempDir::new().unwrap();
    portrait(dir.path());
    let spec = write_spec(
        dir.path(),
        r##"{"canvas":{"width":100,"height":100},
             "layers":[{"id":"a","type":"image","role":"subject",
                        "source":"product.png","rect":[0,0,100,100]}]}"##,
    );
    let out = dir.path().join("out.png");
    let report = json_stdout(&compose(&spec, &out, &["--dry-run"]));
    assert!(report.get("font").is_some());
    assert_eq!(report["font"], Value::Null);
}

// 以下は計画 §10.11 のレビューが見つけた欠陥の回帰。**どれも実装で踏んだ**
// ので、形を変えて戻ってこないように 1 件ずつ固定する。

/// 非 ASCII の色で panic しない（§10.11 の C1）。
///
/// `"#日本"` は `#` + 6 バイトなので長さの検査を通る。16 進かどうかを先に
/// 見ないと、文字の途中でバイト列を切って panic する——**利用者の spec から
/// 届く値なので、code も exit code も名乗らずに落ちる**ことになる。
#[test]
fn a_non_ascii_colour_is_refused_not_a_panic() {
    let dir = TempDir::new().unwrap();
    portrait(dir.path());
    for colour in ["#日本語", "#あ", "#zzzzzz"] {
        let spec = write_spec(
            dir.path(),
            &format!(
                r##"{{"canvas":{{"width":50,"height":50,"background":"{colour}"}},
                     "layers":[{{"id":"a","type":"image","role":"subject",
                                "source":"product.png","rect":[0,0,50,50]}}]}}"##
            ),
        );
        let out = dir.path().join("out.png");
        let result = compose(&spec, &out, &["--force"]);
        assert_eq!(
            result.status.code(),
            Some(2),
            "{colour} が exit 2 以外で終わった（panic は 101）"
        );
        assert_eq!(json_stdout(&result)["error"]["code"], "INVALID_COLOR");
    }
}

/// `--background` も同じ関門を通る。**CLI 側にも同じ切り方があった。**
#[test]
fn a_non_ascii_background_flag_is_refused_not_a_panic() {
    let dir = TempDir::new().unwrap();
    portrait(dir.path());
    let spec = write_spec(
        dir.path(),
        r##"{"canvas":{"width":50,"height":50},
             "layers":[{"id":"a","type":"image","role":"subject",
                        "source":"product.png","rect":[0,0,50,50]}]}"##,
    );
    let out = dir.path().join("out.jpg");
    let result = compose(&spec, &out, &["--background", "#日本"]);
    // clap の value_parser が断るので code 無しの exit 2（kiri 全体の規約）
    assert_eq!(result.status.code(), Some(2));
}

/// 枠が桁違いなら断る。**飽和させて走り出さない**（§10.11 の C3）。
///
/// `1e12` を `u32` へ飽和させると `u32::MAX` になり、約 73TB の確保を要求して
/// 応答が返らなくなる。キャンバスが 400x400 でも、である。
#[test]
fn an_oversized_layer_rect_is_refused_before_any_allocation() {
    let dir = TempDir::new().unwrap();
    portrait(dir.path());
    for rect in ["[0,0,100000,200000]", "[1e12,0,200,400]"] {
        let spec = write_spec(
            dir.path(),
            &format!(
                r##"{{"canvas":{{"width":400,"height":400,"background":"#ffffff"}},
                     "layers":[{{"id":"p","type":"image","role":"subject",
                                "source":"product.png","rect":{rect},"fit":"exact"}}]}}"##
            ),
        );
        let out = dir.path().join("out.png");
        let result = compose(&spec, &out, &["--force"]);
        assert_eq!(result.status.code(), Some(3), "{rect} が断られていない");
        assert_eq!(json_stdout(&result)["error"]["code"], "SPEC_INVALID");
    }
}

/// 枠の外へ置いた層は、**切り落として、置いた位置を正直に返す**（§10.11 の H1）。
///
/// 原点を 0 で止めると、`placed` が spec と違う位置を名乗り、
/// `outside_safe_area` もその嘘の位置から計算される。
#[test]
fn a_layer_placed_off_canvas_reports_where_it_actually_went() {
    let dir = TempDir::new().unwrap();
    portrait(dir.path());
    let spec = write_spec(
        dir.path(),
        r##"{"canvas":{"width":400,"height":400,"background":"#ffffff"},
             "safe_area":[0,0,400,400],
             "layers":[{"id":"p","type":"image","role":"subject",
                        "source":"product.png","rect":[-150,-200,200,400]}]}"##,
    );
    let out = dir.path().join("out.png");
    let report = json_stdout(&compose(&spec, &out, &["--dry-run"]));
    let p = layer(&report, "p");

    assert_eq!(
        rect_of(&p["placed"]),
        [-150.0, -200.0, 200.0, 400.0],
        "0 で止めると、置いた場所と違う位置を名乗ることになる"
    );
    assert_eq!(p["outside_safe_area"], Value::Bool(true));
    assert!(codes(&report).contains(&"OUTSIDE_SAFE_AREA".to_string()));
}

/// キャンバスの外にある `subject` では、重なりを**測れたことにしない**
/// （§10.11 の M1）。
#[test]
fn a_subject_entirely_off_canvas_leaves_the_overlap_unmeasured() {
    let Some(family) = any_font() else {
        return;
    };
    let dir = TempDir::new().unwrap();
    portrait(dir.path());
    let spec = write_spec(
        dir.path(),
        &format!(
            r##"{{"canvas":{{"width":200,"height":200,"background":"#ffffff"}},
                 "font":{{"family":"{family}"}},
                 "layers":[
                   {{"id":"subject","type":"image","role":"subject",
                     "source":"product.png","rect":[-16000,-16000,100,200]}},
                   {{"id":"t","type":"text","role":"body",
                     "lines":["AB"],"rect":[20,40,160,60],
                     "size":40,"color":"#000000"}}]}}"##
        ),
    );
    let out = dir.path().join("out.png");
    let report = json_stdout(&compose(&spec, &out, &["--dry-run"]));
    assert_eq!(
        layer(&report, "t")["layer_overlap"],
        Value::Null,
        "商品が 1 画素も載っていないのに「重なり 0.0」という測れた合格を返した"
    );
}

/// 後の層に覆われた文字を見逃さない（§10.11 の H2）。
///
/// 背後だけを見ると「白地に黒、コントラスト 21」と報告した絵の中で、文字が
/// 1 画素も見えていないことが起こりうる。
#[test]
fn text_hidden_under_a_later_layer_is_named() {
    let Some(family) = any_font() else {
        return;
    };
    let dir = TempDir::new().unwrap();
    let plate = image::RgbaImage::from_pixel(200, 200, image::Rgba([10, 10, 10, 255]));
    write_png(dir.path(), "plate.png", &plate);

    let spec = write_spec(
        dir.path(),
        &format!(
            r##"{{"canvas":{{"width":200,"height":200,"background":"#ffffff"}},
                 "font":{{"family":"{family}"}},
                 "layers":[
                   {{"id":"t","type":"text","role":"heading",
                     "lines":["AB"],"rect":[20,40,160,60],
                     "size":40,"color":"#000000"}},
                   {{"id":"cover","type":"image","role":"decoration",
                     "source":"plate.png","rect":[0,0,200,200],"fit":"exact"}}]}}"##
        ),
    );
    let out = dir.path().join("out.png");
    let result = compose(&spec, &out, &["--fail-on", "default"]);
    let report = json_stdout(&result);

    let t = layer(&report, "t");
    assert!(
        t["text_contrast"].as_f64().unwrap() > 20.0,
        "背後は白のままなので、コントラストそのものは高く出る"
    );
    assert!(
        t["text_obscured"].as_f64().unwrap() > 0.99,
        "全面を塗り潰されているのに覆いが数えられていない"
    );
    assert!(codes(&report).contains(&"TEXT_OBSCURED".to_string()));
    assert_eq!(
        result.status.code(),
        Some(5),
        "見えない文字が --fail-on default を素通りした"
    );
}

/// 字体にその文字が無ければ、**描く前に**名指しする（§10.11 の H3）。
///
/// 無い文字は豆腐（.notdef）として描けてしまうので、「1 画素も描かれなかった」
/// という網には引っかからない。
#[test]
fn glyphs_missing_from_the_font_are_named_before_drawing() {
    let mut db = resvg::usvg::fontdb::Database::new();
    db.load_system_fonts();
    // 日本語を**持たない**字体を 1 つ探す。見つからない機械では飛ばす
    let latin_only = db.faces().find_map(|face| {
        let family = face.families.first()?.0.clone();
        let id = face.id;
        let has_kana = db
            .with_face_data(id, |data, index| {
                ttf_parser::Face::parse(data, index)
                    .map(|f| f.glyph_index('あ').is_some())
                    .unwrap_or(true)
            })
            .unwrap_or(true);
        (!has_kana).then_some(family)
    });
    let Some(family) = latin_only else {
        return;
    };

    let dir = TempDir::new().unwrap();
    let spec = write_spec(
        dir.path(),
        &format!(
            r##"{{"canvas":{{"width":400,"height":120,"background":"#ffffff"}},
                 "font":{{"family":"{family}"}},
                 "layers":[{{"id":"t","type":"text","role":"body",
                            "lines":["あいうえお"],"rect":[20,20,360,60],
                            "size":28,"color":"#000000"}}]}}"##
        ),
    );
    let out = dir.path().join("out.png");
    let report = json_stdout(&compose(&spec, &out, &["--dry-run"]));
    assert!(
        codes(&report).contains(&"FONT_GLYPHS_MISSING".to_string()),
        "'{family}' に仮名が無いのに黙って組んだ: {:?}",
        codes(&report)
    );
    let warning = report["warnings"]
        .as_array()
        .unwrap()
        .iter()
        .find(|w| w["code"] == "FONT_GLYPHS_MISSING")
        .unwrap();
    assert!(
        warning["data"]["missing"].as_str().unwrap().contains('あ'),
        "欠けている文字が名指しされていない"
    );
}

/// 透過のまま書く実行では、**透明な下地の上のコントラストを測らない**
/// （§10.11 の H5）。
///
/// RGB だけを見ると透明が黒として測られ、白い文字が「読めない」、黒い文字が
/// 「読める」と、どちらも実物とは逆の答えになる。
#[test]
fn contrast_over_a_transparent_backdrop_is_not_guessed() {
    let Some(family) = any_font() else {
        return;
    };
    let dir = TempDir::new().unwrap();
    let body = format!(
        r##"{{"canvas":{{"width":200,"height":120}},
             "font":{{"family":"{family}"}},
             "layers":[{{"id":"t","type":"text","role":"body",
                        "lines":["AAA"],"rect":[10,20,180,60],
                        "size":40,"color":"#000000"}}]}}"##
    );
    let spec = write_spec(dir.path(), &body);

    // PNG は透過を保持する。最後に何色になるかを kiri は知らない
    let png = dir.path().join("out.png");
    let report = json_stdout(&compose(&spec, &png, &["--dry-run"]));
    assert_eq!(
        layer(&report, "t")["text_contrast"],
        Value::Null,
        "見る人の背景を知らないのに数を作っている"
    );

    // JPEG は潰す色が決まっている。そこへ重ねてから測れる
    let jpg = dir.path().join("out.jpg");
    let report = json_stdout(&compose(&spec, &jpg, &["--dry-run"]));
    let measured = layer(&report, "t")["text_contrast"]
        .as_f64()
        .expect("潰す色が決まっていれば測れる");
    assert!(
        measured > 20.0,
        "白へ潰す実行の黒文字が {measured:.2}（21 に近いはず）"
    );
    assert!(!codes(&report).contains(&"TEXT_CONTRAST_LOW".to_string()));
}

/// `default` と明示の条件を併記できる（§10.11 の H6）。
///
/// **`--help` が例として挙げている綴りそのものである。** `default` を 4 つの
/// 条件へ展開してから重複を見ると、ここが「2 つの条件」として断られる。
#[test]
fn the_default_token_combines_with_an_explicit_rule() {
    let Some(family) = any_font() else {
        return;
    };
    let dir = TempDir::new().unwrap();
    let spec = write_spec(
        dir.path(),
        &format!(
            r##"{{"canvas":{{"width":300,"height":120,"background":"#ffffff"}},
                 "font":{{"family":"{family}"}},
                 "layers":[{{"id":"t","type":"text","role":"body",
                            "lines":["AA"],"rect":[10,20,280,60],
                            "size":30,"color":"#000000"}}]}}"##
        ),
    );
    let out = dir.path().join("out.png");
    let result = compose(&spec, &out, &["--fail-on", "default,text_contrast<4.5"]);
    assert_ne!(
        result.status.code(),
        Some(2),
        "--help が挙げている綴りが書式として断られた: {}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert!(
        result.status.success(),
        "収まっている組版が落ちた: exit={:?} {}",
        result.status.code(),
        String::from_utf8_lossy(&result.stdout)
    );

    // **明示が default を上書きする。** 同じ指標に両方が当たるなら、
    // 書いたほうが勝つ（compliance の --fail-on と同じ規約）
    let strict = compose(
        &spec,
        &out,
        &["--force", "--fail-on", "default,text_contrast<99"],
    );
    assert_eq!(
        strict.status.code(),
        Some(5),
        "明示した 99 が default の 4.5 に負けている"
    );
}

/// 真偽の指標は `checks[].actual` でも真偽で返る（§10.11 の M2）。
#[test]
fn a_flag_metric_reports_a_boolean_not_a_number() {
    let dir = TempDir::new().unwrap();
    portrait(dir.path());
    let spec = write_spec(
        dir.path(),
        r##"{"canvas":{"width":300,"height":300,"background":"#ffffff"},
             "safe_area":[100,100,100,100],
             "layers":[{"id":"a","type":"image","role":"subject",
                        "source":"product.png","rect":[0,0,300,300]}]}"##,
    );
    let out = dir.path().join("out.png");
    let result = compose(&spec, &out, &["--fail-on", "outside_safe_area"]);
    assert_eq!(result.status.code(), Some(5));
    let report = json_stdout(&result);
    let check = report["compliance"]["checks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["metric"] == "outside_safe_area")
        .unwrap();
    assert_eq!(
        check["actual"],
        Value::Bool(true),
        "同じ事実が layers[] では真偽、checks[] では数で配られている"
    );
}

/// 描かれなかった文字は、はみ出しとは別の code で言う（§10.11 の H8）。
#[test]
fn text_that_never_rendered_gets_its_own_code() {
    let Some(family) = any_font() else {
        return;
    };
    let dir = TempDir::new().unwrap();
    let spec = write_spec(
        dir.path(),
        &format!(
            r##"{{"canvas":{{"width":200,"height":120,"background":"#ffffff"}},
                 "font":{{"family":"{family}"}},
                 "layers":[{{"id":"t","type":"text","role":"body",
                            "lines":["AAA"],"rect":[-16000,-16000,180,60],
                            "size":40,"color":"#000000"}}]}}"##
        ),
    );
    let out = dir.path().join("out.png");
    let report = json_stdout(&compose(&spec, &out, &["--dry-run"]));
    assert!(
        codes(&report).contains(&"TEXT_NOT_RENDERED".to_string()),
        "はみ出しと同じ code で言うと、overflow を読む側が null を受け取る: {:?}",
        codes(&report)
    );
}

/// 配ったしきい値のキーは、実際に出る `data` のキーと一致する（§10.11 の H7）。
///
/// エージェントが読むのは `kiri schema` のほうなので、**ここがずれると
/// 書いた分岐が動かない。**
#[test]
fn the_published_warning_summaries_name_the_keys_that_are_emitted() {
    let Some(family) = any_font() else {
        return;
    };
    let dir = TempDir::new().unwrap();
    let spec = write_spec(
        dir.path(),
        &format!(
            r##"{{"canvas":{{"width":200,"height":120,"background":"#ffffff"}},
                 "font":{{"family":"{family}"}},
                 "layers":[{{"id":"t","type":"text","role":"body",
                            "lines":["AAAAAAAAAAAAAAAAAAAA"],"rect":[10,20,30,60],
                            "size":40,"color":"#fbfbfb"}}]}}"##
        ),
    );
    let out = dir.path().join("out.png");
    let report = json_stdout(&compose(&spec, &out, &["--dry-run"]));

    let schema = json_stdout(&kiri().args(["schema", "--json"]).output().unwrap());
    let summary = |code: &str| -> String {
        schema["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .find(|w| w["code"] == code)
            .unwrap()["summary"]
            .as_str()
            .unwrap()
            .to_string()
    };

    for warning in report["warnings"].as_array().unwrap() {
        let code = warning["code"].as_str().unwrap();
        let text = summary(code);
        for key in warning["data"].as_object().unwrap().keys() {
            // 文脈のために添えている rect / placed は契約の対象ではない
            if matches!(key.as_str(), "rect" | "placed" | "maximum" | "minimum") {
                continue;
            }
            assert!(
                text.contains(key.as_str()),
                "{code} が data.{key} を出すのに、配っている説明は '{text}'"
            );
        }
    }
}

// ---------------------------------------------------------------------------
// Phase 31: `--profile note` と縮小表示での読みやすさ（12-phase-31.md §12.5）
// ---------------------------------------------------------------------------

/// 見出し 1 行を `size` で組む spec。**キャンバスは書かない**——note の寸法は
/// `--profile` から来る。
fn heading_spec(dir: &Path, family: &str, size: u32, color: &str, background: &str) -> PathBuf {
    write_spec(
        dir,
        &format!(
            r##"{{"canvas":{{"background":"{background}"}},
                 "font":{{"family":"{family}"}},
                 "layers":[{{"id":"heading","type":"text","role":"heading",
                            "lines":["ABAB"],"rect":[80,200,1100,240],
                            "size":{size},"color":"{color}"}}]}}"##
        ),
    )
}

fn display_of<'a>(layer: &'a Value, name: &str) -> &'a Value {
    layer["display"]
        .as_array()
        .unwrap_or_else(|| panic!("display が配列でない: {layer}"))
        .iter()
        .find(|d| d["name"] == name)
        .unwrap_or_else(|| panic!("表示 '{name}' が無い: {layer}"))
}

fn reasons(display: &Value) -> Vec<String> {
    display["reasons"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r.as_str().unwrap().to_string())
        .collect()
}

fn check_of<'a>(report: &'a Value, metric: &str) -> Option<&'a Value> {
    report["compliance"]["checks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["metric"] == metric)
}

/// `canvas` を省いた spec が `--profile note` で 1280x670 になり、安全領域が
/// 上下中央の 1280x454 になる（§12.5 の 2）。
#[test]
fn a_spec_without_a_canvas_takes_the_note_size_and_safe_area() {
    let dir = TempDir::new().unwrap();
    portrait(dir.path());
    // 安全領域 [0,108,1280,454] の内と外に 1 枚ずつ置く
    let spec = write_spec(
        dir.path(),
        r#"{"layers":[
             {"id":"inside","type":"image","role":"subject",
              "source":"product.png","rect":[100,108,200,454]},
             {"id":"above","type":"image","role":"decoration",
              "source":"product.png","rect":[600,0,50,100]},
             {"id":"edge","type":"image","role":"decoration",
              "source":"product.png","rect":[900,100,200,400]}]}"#,
    );
    let out = dir.path().join("out.png");
    let result = compose(&spec, &out, &["--profile", "note", "--dry-run"]);
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stdout)
    );
    let report = json_stdout(&result);

    assert_eq!(report["canvas"]["width"], 1280);
    assert_eq!(report["canvas"]["height"], 670);
    assert_eq!(report["output"]["width"], 1280);
    assert_eq!(report["output"]["height"], 670);
    assert_eq!(report["profile"]["name"], "note");
    assert_eq!(report["profile"]["revision"], "2026-10");
    assert!(
        report["profile"]["source"]
            .as_str()
            .unwrap()
            .starts_with("https://")
    );

    assert_eq!(layer(&report, "inside")["outside_safe_area"], false);
    assert_eq!(layer(&report, "above")["outside_safe_area"], true);
    // 108 の 8px 上から始まる
    assert_eq!(layer(&report, "edge")["outside_safe_area"], true);
    // 画像の層は縮小表示を測らない
    assert!(layer(&report, "inside").get("display").is_none());
    assert!(!codes(&report).contains(&"PROFILE_ASPECT_MISMATCH".to_string()));
}

/// **`--profile` を渡さなければ、省いた canvas は今までどおり断る。**
#[test]
fn without_a_profile_a_missing_canvas_is_still_refused() {
    let dir = TempDir::new().unwrap();
    portrait(dir.path());
    let spec = write_spec(
        dir.path(),
        r#"{"layers":[{"id":"a","type":"image","role":"subject",
                       "source":"product.png","rect":[0,0,100,100]}]}"#,
    );
    let out = dir.path().join("out.png");
    let result = compose(&spec, &out, &[]);
    assert_eq!(result.status.code(), Some(3), "spec の不正は入力の失敗");
    assert_eq!(json_stdout(&result)["error"]["code"], "SPEC_INVALID");
}

/// 1920x1006 は比が同じなので黙り、1280x720 は比の違いを言う（§12.5 の 3）。
#[test]
fn the_larger_recommendation_passes_and_16_9_is_named() {
    let dir = TempDir::new().unwrap();
    portrait(dir.path());
    let run = |w: u32, h: u32| {
        let spec = write_spec(
            dir.path(),
            &format!(
                r#"{{"canvas":{{"width":{w},"height":{h}}},
                     "layers":[{{"id":"a","type":"image","role":"subject",
                                "source":"product.png","rect":[0,0,100,100]}}]}}"#
            ),
        );
        let out = dir.path().join("out.png");
        let result = compose(&spec, &out, &["--profile", "note", "--dry-run"]);
        assert!(result.status.success());
        json_stdout(&result)
    };

    let large = run(1920, 1006);
    assert!(
        !codes(&large).contains(&"PROFILE_ASPECT_MISMATCH".to_string()),
        "{}",
        large["warnings"]
    );

    let wide = run(1280, 720);
    let warning = wide["warnings"]
        .as_array()
        .unwrap()
        .iter()
        .find(|w| w["code"] == "PROFILE_ASPECT_MISMATCH")
        .unwrap_or_else(|| panic!("16:9 が通った: {}", wide["warnings"]));
    assert_eq!(warning["data"]["expected"], serde_json::json!([1280, 670]));
    assert_eq!(warning["data"]["actual"], serde_json::json!([1280, 720]));
    // 書いた寸法が勝つ（明示 > profile）
    assert_eq!(wide["canvas"]["height"], 720);
}

/// compose と cutout の `--profile` は**別の表**を引く。互いの名前を断る。
#[test]
fn the_compose_and_cutout_profiles_do_not_accept_each_other() {
    let dir = TempDir::new().unwrap();
    portrait(dir.path());
    let spec = write_spec(
        dir.path(),
        r#"{"layers":[{"id":"a","type":"image","role":"subject",
                       "source":"product.png","rect":[0,0,100,100]}]}"#,
    );
    let out = dir.path().join("out.png");
    let to_compose = compose(&spec, &out, &["--profile", "amazon"]);
    assert_eq!(
        to_compose.status.code(),
        Some(2),
        "compose が amazon を受けた"
    );

    let input = dir.path().join("product.png");
    let to_cutout = kiri()
        .args(["cutout", input.to_str().unwrap(), "-o"])
        .arg(dir.path().join("cut.png"))
        .args(["--profile", "note"])
        .output()
        .unwrap();
    assert_eq!(to_cutout.status.code(), Some(2), "cutout が note を受けた");
}

/// `--profile` を渡さない結果には `profile` も `display` もキーごと出ない
/// （§12.1 の 3。既存の出力を 1 バイトも動かさない）。
#[test]
fn without_a_profile_no_new_keys_appear() {
    let Some(family) = any_font() else {
        return;
    };
    let dir = TempDir::new().unwrap();
    let spec = write_spec(
        dir.path(),
        &format!(
            r##"{{"canvas":{{"width":400,"height":200,"background":"#ffffff"}},
                 "font":{{"family":"{family}"}},
                 "layers":[{{"id":"t","type":"text","role":"heading",
                            "lines":["AB"],"rect":[10,10,380,180],
                            "size":40,"color":"#000000"}}]}}"##
        ),
    );
    let out = dir.path().join("out.png");
    let report = json_stdout(&compose(&spec, &out, &["--fail-on", "default"]));
    assert!(report.get("profile").is_none(), "{report}");
    assert!(layer(&report, "t").get("display").is_none(), "{report}");
    let metrics: Vec<&str> = report["compliance"]["checks"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["metric"].as_str().unwrap())
        .collect();
    assert!(
        metrics.iter().all(|m| !m.starts_with("display_")),
        "profile の無い default が縮小表示の行を足した: {metrics:?}"
    );
    assert_eq!(report["compliance"]["passed"], true);
}

/// 白地に黒 64px の見出しは原寸では黙り、`mobile_list` では 7.2px で
/// `too_small` になる（§12.5 の 4）。
#[test]
fn a_64px_heading_is_fine_at_full_size_but_too_small_on_mobile() {
    let Some(family) = any_font() else {
        return;
    };
    let dir = TempDir::new().unwrap();
    let spec = heading_spec(dir.path(), &family, 64, "#000000", "#ffffff");
    let out = dir.path().join("out.png");

    // 原寸の測りだけなら何も言わない。canvas を書いていないので、比べるために
    // 寸法を書いた spec を別に作る
    let full = write_spec(
        dir.path(),
        &std::fs::read_to_string(&spec)
            .unwrap()
            .replace(r#""canvas":{"#, r#""canvas":{"width":1280,"height":670,"#),
    );
    let plain = json_stdout(&compose(&full, &out, &["--dry-run"]));
    assert!(codes(&plain).is_empty(), "{}", plain["warnings"]);

    let spec = heading_spec(dir.path(), &family, 64, "#000000", "#ffffff");
    let report = json_stdout(&compose(&spec, &out, &["--profile", "note", "--dry-run"]));
    let heading = layer(&report, "heading");
    let mobile = display_of(heading, "mobile_list");
    assert_eq!(mobile["width"], 144);
    assert_eq!(mobile["size_px"].as_f64().unwrap(), 7.2);
    assert_eq!(mobile["legible"], false);
    assert!(reasons(mobile).contains(&"too_small".to_string()));

    let warning = report["warnings"]
        .as_array()
        .unwrap()
        .iter()
        .find(|w| w["code"] == "TEXT_ILLEGIBLE_AT_DISPLAY" && w["data"]["display"] == "mobile_list")
        .unwrap_or_else(|| panic!("mobile_list の警告が無い: {}", report["warnings"]));
    assert_eq!(warning["data"]["layer"], "heading");
    assert_eq!(warning["data"]["width"], 144);
    assert_eq!(warning["data"]["size_px"].as_f64().unwrap(), 7.2);
    // 直す側が逆算できる数を hint が言う。11 × 1280 / 144 = 97.8 → 98
    assert!(
        warning["hint"].as_str().unwrap().contains("98"),
        "{warning}"
    );
}

/// 白地に黒 110px の見出しは両方の表示で読める（§12.5 の 5）。
#[test]
fn a_110px_heading_is_legible_on_both_displays() {
    let Some(family) = any_font() else {
        return;
    };
    let dir = TempDir::new().unwrap();
    let spec = heading_spec(dir.path(), &family, 110, "#000000", "#ffffff");
    let out = dir.path().join("out.png");
    let report = json_stdout(&compose(&spec, &out, &["--profile", "note", "--dry-run"]));
    let heading = layer(&report, "heading");
    let displays = heading["display"].as_array().unwrap();
    let names: Vec<&str> = displays
        .iter()
        .map(|d| d["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["pc_list", "mobile_list"], "並びは表の順");
    for d in displays {
        assert_eq!(d["legible"], true, "{family}: {d}");
        assert!(d["reasons"].as_array().unwrap().is_empty());
        assert!(d["contrast"].as_f64().unwrap() >= 4.5, "{d}");
    }
    assert!(
        !codes(&report).contains(&"TEXT_ILLEGIBLE_AT_DISPLAY".to_string()),
        "{}",
        report["warnings"]
    );
}

/// **縮小の測りが原寸の測りと違うことの証拠**（§12.5 の 6）。
///
/// 中間色の地（#767676）に白の文字は、原寸のコントラストが 4.54 で 4.5 を
/// わずかに超える。表示幅へ縮めると線が地の色と混ざって前景が薄まり、
/// 大きさは足りていても（100px → 11.25px）`low_contrast` か `strokes_lost` に
/// なる。原寸の `text_contrast` だけを読んだ呼び出し側は「読める」と判断する。
#[test]
fn a_line_that_passes_at_full_size_fails_when_shrunk() {
    let Some(family) = any_font() else {
        return;
    };
    let dir = TempDir::new().unwrap();
    let spec = heading_spec(dir.path(), &family, 100, "#ffffff", "#767676");
    let out = dir.path().join("out.png");
    let report = json_stdout(&compose(&spec, &out, &["--profile", "note", "--dry-run"]));
    let heading = layer(&report, "heading");

    let full = heading["text_contrast"].as_f64().unwrap();
    assert!((4.5..4.6).contains(&full), "原寸 {full}");
    assert!(!codes(&report).contains(&"TEXT_CONTRAST_LOW".to_string()));

    // **字体に依らない余白を取って表明する。** 縮小で前景が地の色と混ざる
    // 量は線の太さで決まり、太い字体ほど 4.5 に近づく。手元の 4 字体
    // （Arial Unicode MS / Hiragino Sans / Helvetica / Arial）は 3.0〜4.0 で、
    // 4.5 を超えるには表示上で線がほぼ 2px 以上ぶ厚く残る必要がある（100px の
    // 文字で幹が 20px 前後）。DejaVu Sans（CI の字体）の幹は 100px でおよそ
    // 13px なので、同じ向きに落ちる。表明は「どれか 1 つの表示が、原寸より
    // 0.5 以上低く 4.5 を下回るか、線を失った」
    let mut caught = false;
    for d in heading["display"].as_array().unwrap() {
        assert!(
            d["size_px"].as_f64().unwrap() >= 11.0,
            "大きさで落ちては証拠にならない: {d}"
        );
        let r = reasons(d);
        let lower = d["contrast"]
            .as_f64()
            .is_some_and(|c| c < 4.5 && c <= full - 0.5);
        if (lower && r.contains(&"low_contrast".to_string()))
            || r.contains(&"strokes_lost".to_string())
        {
            caught = true;
        }
    }
    assert!(
        caught,
        "{family}: 縮めても読めると言った: {}",
        heading["display"]
    );
    assert!(codes(&report).contains(&"TEXT_ILLEGIBLE_AT_DISPLAY".to_string()));
}

/// 同じ入力から同じ結果（§12.5 の 7）。
#[test]
fn the_display_measurement_is_deterministic() {
    let Some(family) = any_font() else {
        return;
    };
    let dir = TempDir::new().unwrap();
    let spec = heading_spec(dir.path(), &family, 100, "#ffffff", "#767676");
    let strip = |mut v: Value| {
        v.as_object_mut().unwrap().remove("elapsed_ms");
        v
    };
    let a_out = dir.path().join("a.png");
    let b_out = dir.path().join("b.png");
    let a = strip(json_stdout(&compose(&spec, &a_out, &["--profile", "note"])));
    let mut b = strip(json_stdout(&compose(&spec, &b_out, &["--profile", "note"])));
    b["output"]["path"] = a["output"]["path"].clone();
    assert_eq!(a, b);
    assert_eq!(
        std::fs::read(&a_out).unwrap(),
        std::fs::read(&b_out).unwrap()
    );
}

/// `--fail-on` の縮小表示の指標（§12.4.6）。
///
/// profile があれば `default` が 3 行を足し、無ければ足さない。**明示して
/// 書いた指標は、測っていないので `unmeasurable`**——合格にはしない。
#[test]
fn the_display_metrics_are_gated_only_when_measured() {
    let Some(family) = any_font() else {
        return;
    };
    let dir = TempDir::new().unwrap();
    let spec = heading_spec(dir.path(), &family, 64, "#000000", "#ffffff");
    let out = dir.path().join("out.png");

    let result = compose(
        &spec,
        &out,
        &["--profile", "note", "--dry-run", "--fail-on", "default"],
    );
    assert_eq!(result.status.code(), Some(5), "7.2px が門を通った");
    let report = json_stdout(&result);
    let size =
        check_of(&report, "display_text_size").expect("default が display_text_size を当てない");
    assert_eq!(size["status"], "fail");
    assert_eq!(size["operator"], "lt");
    assert_eq!(size["threshold"].as_f64().unwrap(), 11.0);
    assert_eq!(size["actual"].as_f64().unwrap(), 7.2, "全表示の最小");
    assert_eq!(
        check_of(&report, "display_contrast").unwrap()["status"],
        "pass"
    );
    let lost = check_of(&report, "display_strokes_lost").unwrap();
    assert_eq!(lost["status"], "pass");
    assert_eq!(lost["actual"], false, "旗は真偽で返る");

    // profile 無しで明示したら測っていない
    let full = write_spec(
        dir.path(),
        &std::fs::read_to_string(&spec)
            .unwrap()
            .replace(r#""canvas":{"#, r#""canvas":{"width":1280,"height":670,"#),
    );
    let result = compose(
        &full,
        &out,
        &["--dry-run", "--fail-on", "display_text_size<11"],
    );
    assert_eq!(
        result.status.code(),
        Some(5),
        "測っていない指標が合格になった"
    );
    let report = json_stdout(&result);
    assert_eq!(
        check_of(&report, "display_text_size").unwrap()["status"],
        "unmeasurable"
    );
}

/// 測る相手（読めることを要求する文字）が居なければ `skipped`。
#[test]
fn display_metrics_without_text_are_skipped() {
    let dir = TempDir::new().unwrap();
    portrait(dir.path());
    let spec = write_spec(
        dir.path(),
        r#"{"layers":[{"id":"a","type":"image","role":"subject",
                       "source":"product.png","rect":[0,0,100,100]}]}"#,
    );
    let out = dir.path().join("out.png");
    let result = compose(
        &spec,
        &out,
        &[
            "--profile",
            "note",
            "--dry-run",
            "--fail-on",
            "display_contrast<4.5",
        ],
    );
    assert!(result.status.success());
    let report = json_stdout(&result);
    assert_eq!(
        check_of(&report, "display_contrast").unwrap()["status"],
        "skipped"
    );
}

/// 書いたバイト数が 10MB を超えたら言う。**`--dry-run` でも言う**——dry-run は
/// エンコードまで通してバイト数を返している。
#[test]
fn bytes_over_the_note_limit_are_named_even_in_a_dry_run() {
    let dir = TempDir::new().unwrap();
    // 圧縮の効かない雑音。2800x1466 の RGB で 12MB を超える（比は 1.910）
    let (w, h) = (2800u32, 1466u32);
    let mut state: u32 = 0x1234_5678;
    let noise = image::RgbaImage::from_fn(w, h, |_, _| {
        let mut next = || {
            state ^= state << 13;
            state ^= state >> 17;
            state ^= state << 5;
            (state & 0xff) as u8
        };
        image::Rgba([next(), next(), next(), 255])
    });
    write_png(dir.path(), "noise.png", &noise);
    let spec = write_spec(
        dir.path(),
        &format!(
            r#"{{"canvas":{{"width":{w},"height":{h}}},
                 "layers":[{{"id":"n","type":"image","role":"decoration",
                            "source":"noise.png","rect":[0,0,{w},{h}]}}]}}"#
        ),
    );
    let out = dir.path().join("out.png");
    let result = compose(&spec, &out, &["--profile", "note", "--dry-run"]);
    assert!(result.status.success());
    let report = json_stdout(&result);
    let bytes = report["output"]["bytes"].as_u64().unwrap();
    assert!(bytes > 10_000_000, "前提が崩れた: {bytes}");
    let warning = report["warnings"]
        .as_array()
        .unwrap()
        .iter()
        .find(|w| w["code"] == "PROFILE_MAX_BYTES_EXCEEDED")
        .unwrap_or_else(|| panic!("{}", report["warnings"]));
    assert_eq!(warning["data"]["bytes"], bytes);
    assert_eq!(warning["data"]["max_bytes"], 10_000_000);

    // profile が無ければ上限そのものが無い
    let plain = json_stdout(&compose(&spec, &out, &["--dry-run"]));
    assert!(!codes(&plain).contains(&"PROFILE_MAX_BYTES_EXCEEDED".to_string()));
}

/// 新しい 3 つの警告も、data のキーを `kiri schema` の summary が名指しする
/// （`the_published_warning_summaries_name_the_keys_that_are_emitted` と同じ契約）。
#[test]
fn the_profile_warnings_name_their_keys_in_the_schema() {
    let schema = json_stdout(&kiri().args(["schema", "--json"]).output().unwrap());
    let summary = |code: &str| -> String {
        schema["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .find(|w| w["code"] == code)
            .unwrap_or_else(|| panic!("{code} が schema に無い"))["summary"]
            .as_str()
            .unwrap()
            .to_string()
    };
    for (code, keys) in [
        (
            "TEXT_ILLEGIBLE_AT_DISPLAY",
            &[
                "layer", "display", "width", "size_px", "contrast", "reasons",
            ][..],
        ),
        ("PROFILE_ASPECT_MISMATCH", &["expected", "actual"][..]),
        ("PROFILE_MAX_BYTES_EXCEEDED", &["bytes", "max_bytes"][..]),
    ] {
        let text = summary(code);
        for key in keys {
            assert!(
                text.contains(key),
                "{code} の説明が {key} を言わない: {text}"
            );
        }
    }
}

/// `kiri schema` が compose の規格の表と `--profile` を配る（§12.4.7）。
#[test]
fn the_schema_publishes_the_compose_profiles() {
    let check = |schema: &Value| {
        let note = schema["compose_profiles"]
            .as_array()
            .expect("compose_profiles が無い")
            .iter()
            .find(|p| p["name"] == "note")
            .expect("note が無い");
        assert_eq!(note["canvas"], serde_json::json!([1280, 670]));
        assert_eq!(
            note["safe_area"],
            serde_json::json!([0.0, 108.0, 1280.0, 454.0])
        );
        assert_eq!(note["max_bytes"], 10_000_000);
        assert_eq!(
            note["displays"],
            serde_json::json!([{"name":"pc_list","width":192},{"name":"mobile_list","width":144}])
        );
        for key in ["revision", "summary", "source"] {
            assert!(note[key].is_string(), "{key}");
        }
        // cutout / lint の表には混ざらない
        assert!(
            schema["profiles"]
                .as_array()
                .unwrap()
                .iter()
                .all(|p| p["name"] != "note")
        );
    };
    let whole = json_stdout(&kiri().args(["schema", "--json"]).output().unwrap());
    check(&whole);
    check(&json_stdout(
        &kiri()
            .args(["schema", "--summary", "--json"])
            .output()
            .unwrap(),
    ));
    check(&json_stdout(
        &kiri()
            .args(["schema", "compose", "--brief", "--json"])
            .output()
            .unwrap(),
    ));

    let compose = whole["commands"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["name"] == "compose")
        .unwrap();
    let option = |name: &str| {
        compose["options"]
            .as_array()
            .unwrap()
            .iter()
            .find(|o| o["name"] == name)
            .unwrap_or_else(|| panic!("{name} が compose に無い"))
            .clone()
    };
    assert_eq!(option("--profile")["accepts"], serde_json::json!(["note"]));
    let fail_on = option("--fail-on")["accepts"].clone();
    for metric in [
        "display_text_size",
        "display_contrast",
        "display_strokes_lost",
    ] {
        assert!(
            fail_on
                .as_array()
                .is_some_and(|a| a.iter().any(|m| m == metric))
                || option("--fail-on").to_string().contains(metric),
            "{metric} が --fail-on に配られていない: {fail_on}"
        );
    }
    for code in [
        "TEXT_ILLEGIBLE_AT_DISPLAY",
        "PROFILE_ASPECT_MISMATCH",
        "PROFILE_MAX_BYTES_EXCEEDED",
    ] {
        assert!(
            whole["warnings"]
                .as_array()
                .unwrap()
                .iter()
                .any(|w| w["code"] == code),
            "{code}"
        );
    }
}

// ---------------------------------------------------------------------------
// Phase 31 のレビューで出た欠陥（12-phase-31.md §12.9）
// ---------------------------------------------------------------------------

/// **原寸で 1 画素も描かれなかった文字は、縮小表示で測らない**（§12.9 の R1）。
///
/// 測ると前景が 0 画素なので `strokes_lost` が立ち、`TEXT_NOT_RENDERED` と並んで
/// 「weight を上げる」と勧める——**原因について嘘をつく。** `text_contrast` が
/// null になるのと同じく `display` を持たず、門も `text_contrast` と同じく
/// 「測れた層が無い」として `unmeasurable` にする。
#[test]
fn text_drawn_off_canvas_is_not_measured_at_display_size() {
    let Some(family) = any_font() else {
        return;
    };
    let dir = TempDir::new().unwrap();
    let spec = write_spec(
        dir.path(),
        &format!(
            r##"{{"canvas":{{"background":"#ffffff"}},
                 "font":{{"family":"{family}"}},
                 "layers":[{{"id":"gone","type":"text","role":"heading",
                            "lines":["ABAB"],"rect":[2000,200,600,240],
                            "size":110,"color":"#000000"}}]}}"##
        ),
    );
    let out = dir.path().join("out.png");
    let result = compose(
        &spec,
        &out,
        &[
            "--profile",
            "note",
            "--dry-run",
            "--fail-on",
            "display_strokes_lost",
        ],
    );
    let report = json_stdout(&result);
    let found = codes(&report);
    assert!(
        found.contains(&"TEXT_NOT_RENDERED".to_string()),
        "{found:?}"
    );
    assert!(
        !found.contains(&"TEXT_ILLEGIBLE_AT_DISPLAY".to_string()),
        "描かれていない文字の線が潰れたと言った: {}",
        report["warnings"]
    );
    let gone = layer(&report, "gone");
    assert_eq!(gone["text_contrast"], Value::Null);
    assert!(gone.get("display").is_none(), "{gone}");

    assert_eq!(
        result.status.code(),
        Some(5),
        "測れていないものが合格になった"
    );
    assert_eq!(
        check_of(&report, "display_strokes_lost").unwrap()["status"],
        "unmeasurable"
    );
}

/// **表示幅がキャンバスの幅以上なら、その表示は測らない**（§12.9 の R3）。
///
/// 縮小ではなく拡大になり、高さが際限なく伸びる（16x16384 を 192 幅へ
/// 「縮める」と 192x196608 になる）。測った表示が 1 つも無ければ `display` は
/// 空の配列で、**測ったように見せない。**
#[test]
fn a_canvas_narrower_than_the_display_is_not_measured() {
    let Some(family) = any_font() else {
        return;
    };
    let dir = TempDir::new().unwrap();
    for (w, h) in [(100u32, 52u32), (16, 16384), (160, 84)] {
        let spec = write_spec(
            dir.path(),
            &format!(
                r##"{{"canvas":{{"width":{w},"height":{h},"background":"#ffffff"}},
                     "font":{{"family":"{family}"}},
                     "layers":[{{"id":"t","type":"text","role":"heading",
                                "lines":["A"],"rect":[0,0,{w},40],
                                "size":30,"color":"#000000"}}]}}"##
            ),
        );
        let out = dir.path().join("out.png");
        let result = compose(&spec, &out, &["--profile", "note", "--dry-run"]);
        assert!(result.status.success(), "{w}x{h}");
        let report = json_stdout(&result);
        let names: Vec<&str> = layer(&report, "t")["display"]
            .as_array()
            .unwrap_or_else(|| panic!("{w}x{h}: display が配列でない: {report}"))
            .iter()
            .map(|d| d["name"].as_str().unwrap())
            .collect();
        let expected: Vec<&str> = if w > 144 { vec!["mobile_list"] } else { vec![] };
        assert_eq!(names, expected, "{w}x{h}");
    }
}

/// 透過の下地は**白**として縮め、透過を保てない出力では潰す色として縮める
/// （§12.6 の 2）。
///
/// 白い文字は、白の上では読めず（`low_contrast`）、黒へ潰されれば読める。
#[test]
fn a_transparent_backdrop_is_shrunk_over_white_or_the_flatten_colour() {
    let Some(family) = any_font() else {
        return;
    };
    let dir = TempDir::new().unwrap();
    let spec = write_spec(
        dir.path(),
        &format!(
            r##"{{"font":{{"family":"{family}"}},
                 "layers":[{{"id":"t","type":"text","role":"heading",
                            "lines":["ABAB"],"rect":[80,200,1100,240],
                            "size":110,"color":"#ffffff"}}]}}"##
        ),
    );
    let run = |name: &str, extra: &[&str]| {
        let out = dir.path().join(name);
        let mut args = vec!["--profile", "note", "--dry-run"];
        args.extend_from_slice(extra);
        let report = json_stdout(&compose(&spec, &out, &args));
        layer(&report, "t")["display"].clone()
    };

    for d in run("clear.png", &[]).as_array().unwrap() {
        assert!(reasons(d).contains(&"low_contrast".to_string()), "{d}");
    }
    for (name, extra) in [
        ("dark.jpg", &["--background", "#000000"][..]),
        ("dark.png", &["--flatten", "--background", "#000000"][..]),
    ] {
        for d in run(name, extra).as_array().unwrap() {
            assert_eq!(d["legible"], true, "{name}: {d}");
        }
    }
}

/// `decoration` の文字は読めることを要求しないので、縮小表示でも測らない。
#[test]
fn a_decoration_has_no_display() {
    let Some(family) = any_font() else {
        return;
    };
    let dir = TempDir::new().unwrap();
    let spec = write_spec(
        dir.path(),
        &format!(
            r##"{{"canvas":{{"background":"#ffffff"}},
                 "font":{{"family":"{family}"}},
                 "layers":[{{"id":"d","type":"text","role":"decoration",
                            "lines":["AB"],"rect":[80,200,600,240],
                            "size":20,"color":"#eeeeee"}}]}}"##
        ),
    );
    let out = dir.path().join("out.png");
    let report = json_stdout(&compose(&spec, &out, &["--profile", "note", "--dry-run"]));
    assert!(layer(&report, "d").get("display").is_none(), "{report}");
    assert!(!codes(&report).contains(&"TEXT_ILLEGIBLE_AT_DISPLAY".to_string()));
}

/// **`--profile` を渡さない出力を main の build に釘で留める**（§12.5 の 1）。
///
/// 字体の要らない spec（画像 1 枚、枠と素材が同じ寸法なので縮小も走らない）を
/// 組み、書いたバイト列の SHA-256 と結果 JSON のキーの集合を、Phase 31 の前の
/// build（6c380ec）で取った値と比べる。**手で並べて確かめた一致を、次の変更で
/// 黙って失わないため**である。
///
/// 素材は透過の余白を持つので、`"trim": "none"` を書いて**`trim` を足す前の
/// 振る舞い**で組む。既定（`alpha`）では余白を落として拡大するのでバイト列が
/// 変わる——それは意図した互換の変化で、`none` で前と同じバイトに戻ることを
/// ここで留める。結果 JSON の層には `opaque_rect` が 1 つ増えた。
#[test]
fn without_a_profile_the_output_matches_the_build_before_phase_31() {
    let dir = TempDir::new().unwrap();
    // **素材は整数の算術だけで作る。** `common::product_image` は `hypot` を使い、
    // libm の実装（macOS と glibc）で末位がずれうる——素材が 1 ビット違えば
    // 釘の値が機械ごとに変わる。縁に半透明を持たせ、合成の算術も通す
    let material = image::RgbaImage::from_fn(200, 400, |x, y| {
        let inside = (30..170).contains(&x) && (40..360).contains(&y);
        let edge = (20..180).contains(&x) && (30..370).contains(&y);
        let alpha = if inside {
            255
        } else if edge {
            128
        } else {
            0
        };
        image::Rgba([
            (x % 256) as u8,
            ((x * 3 + y) % 256) as u8,
            (y % 256) as u8,
            alpha,
        ])
    });
    write_png(dir.path(), "product.png", &material);
    let spec = write_spec(
        dir.path(),
        r##"{"canvas":{"width":400,"height":400,"background":"#f5f2ec"},
             "safe_area":[0,0,400,400],
             "layers":[{"id":"subject","type":"image","role":"subject",
                        "source":"product.png","rect":[100,0,200,400],
                        "trim":"none"}]}"##,
    );
    let out = dir.path().join("out.png");
    let result = compose(&spec, &out, &["--fail-on", "default"]);
    assert!(result.status.success());
    let report = json_stdout(&result);

    let mut hasher = kiri::segment::sha256::Sha256::new();
    hasher.update(&std::fs::read(&out).unwrap());
    assert_eq!(
        hasher.finish(),
        PRE_PHASE_31_SHA256,
        "書いたバイト列が変わった"
    );
    assert_eq!(report["output"]["bytes"], PRE_PHASE_31_BYTES);

    let keys = |v: &Value| -> Vec<String> {
        let mut k: Vec<String> = v.as_object().unwrap().keys().cloned().collect();
        k.sort();
        k
    };
    assert_eq!(
        keys(&report),
        [
            "canvas",
            "compliance",
            "dry_run",
            "elapsed_ms",
            "font",
            "layers",
            "output",
            "schema_version",
            "spec",
            "warnings"
        ]
    );
    assert_eq!(
        keys(&report["layers"][0]),
        [
            "align",
            "id",
            "kind",
            "layer_overlap",
            "line_widths",
            "opaque_rect",
            "outside_safe_area",
            "placed",
            "rect",
            "role",
            "text_contrast",
            "text_obscured",
            "text_overflow"
        ]
    );
    let metrics: Vec<&str> = report["compliance"]["checks"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["metric"].as_str().unwrap())
        .collect();
    assert_eq!(
        metrics,
        [
            "text_overflow",
            "text_contrast",
            "text_obscured",
            "layer_overlap",
            "outside_safe_area"
        ]
    );
}

/// 6c380ec の build で `without_a_profile_the_output_matches_the_build_before_phase_31`
/// の spec を組んだときの値。
const PRE_PHASE_31_SHA256: &str =
    "e71520e62be951f84aff8decc1ed4d704d2946d9273b2f8f1549546bd178268e";
const PRE_PHASE_31_BYTES: u64 = 63235;

// ---------------------------------------------------------------------------
// 透過の余白: image 層の `trim` と `opaque_rect`
// ---------------------------------------------------------------------------

/// 透明なキャンバスの `[x, y, w, h]` だけを不透明にした素材を置く。
///
/// **`cutout --canvas` / `--rotate` の出力の形である。** 商品は素材の一部しか
/// 占めず、残りは透過の余白になる。
fn padded(dir: &Path, name: &str, size: (u32, u32), opaque: [u32; 4]) -> PathBuf {
    let [ox, oy, ow, oh] = opaque;
    let image = image::RgbaImage::from_fn(size.0, size.1, |x, y| {
        if x >= ox && x < ox + ow && y >= oy && y < oy + oh {
            image::Rgba([190, 70, 55, 255])
        } else {
            image::Rgba([0, 0, 0, 0])
        }
    });
    write_png(dir, name, &image)
}

fn assert_close(actual: [f64; 4], expected: [f64; 4], tolerance: f64, what: &str) {
    for i in 0..4 {
        assert!(
            (actual[i] - expected[i]).abs() <= tolerance,
            "{what}: {actual:?} が {expected:?} から ±{tolerance} を外れた"
        );
    }
}

/// **既定では透過の余白を切り詰めてから枠へ当てはめる。**
///
/// 余白ごと内接させると、横幅の 24% しか占めない商品は枠の中で豆粒になる
/// （2026-10-14 のサムネ検証で再現した事故）。
#[test]
fn a_transparent_margin_is_trimmed_before_fitting_by_default() {
    let dir = TempDir::new().unwrap();
    padded(dir.path(), "cut.png", (400, 400), [100, 50, 100, 300]);
    let spec = write_spec(
        dir.path(),
        r##"{"canvas":{"width":400,"height":400,"background":"#ffffff"},
             "layers":[{"id":"p","type":"image","role":"subject",
                        "source":"cut.png","rect":[0,0,200,200]}]}"##,
    );
    let out = dir.path().join("out.png");
    let report = json_stdout(&compose(&spec, &out, &["--dry-run"]));
    let p = layer(&report, "p");

    // 100x300 の商品が 200x200 の枠へ内接して 67x200 になり、中央へ寄る
    assert_close(
        rect_of(&p["placed"]),
        [66.0, 0.0, 67.0, 200.0],
        1.0,
        "placed は切り詰めた素材の矩形",
    );
    assert_close(
        rect_of(&p["opaque_rect"]),
        rect_of(&p["placed"]),
        1.0,
        "切り詰めた素材では、不透明画素の矩形は placed とほぼ一致する",
    );
}

/// `"trim": "none"` は余白ごと内接させる——今までの振る舞いである。
#[test]
fn trim_none_keeps_the_transparent_margin() {
    let dir = TempDir::new().unwrap();
    padded(dir.path(), "cut.png", (400, 400), [100, 50, 100, 300]);
    let spec = write_spec(
        dir.path(),
        r##"{"canvas":{"width":400,"height":400,"background":"#ffffff"},
             "layers":[{"id":"p","type":"image","role":"subject",
                        "source":"cut.png","rect":[0,0,200,200],"trim":"none"}]}"##,
    );
    let out = dir.path().join("out.png");
    let report = json_stdout(&compose(&spec, &out, &["--dry-run"]));
    let p = layer(&report, "p");

    assert_eq!(rect_of(&p["placed"]), [0.0, 0.0, 200.0, 200.0]);
    assert_close(
        rect_of(&p["opaque_rect"]),
        [50.0, 25.0, 50.0, 150.0],
        1.0,
        "余白を残しても、不透明画素が占める矩形は別に返る",
    );
}

/// `safe_area` は**不透明画素**で判定する。透過の余白がはみ出しても言わない。
///
/// `cutout --canvas 1400 --fill-ratio 0.9` の素材を note のヘッダーへ置くと、
/// 余白込みの矩形だけが safe_area の上へ出て、偽の警告になっていた。
#[test]
fn the_safe_area_is_judged_by_the_opaque_pixels_not_the_margin() {
    let dir = TempDir::new().unwrap();
    padded(dir.path(), "cut.png", (100, 100), [25, 25, 50, 50]);
    let spec = write_spec(
        dir.path(),
        r##"{"canvas":{"width":100,"height":100,"background":"#ffffff"},
             "safe_area":[20,20,60,60],
             "layers":[{"id":"p","type":"image","role":"subject",
                        "source":"cut.png","rect":[0,0,100,100],"trim":"none"}]}"##,
    );
    let out = dir.path().join("out.png");
    let report = json_stdout(&compose(&spec, &out, &["--dry-run"]));
    let p = layer(&report, "p");

    assert_eq!(rect_of(&p["placed"]), [0.0, 0.0, 100.0, 100.0]);
    assert_eq!(p["outside_safe_area"], Value::Bool(false));
    assert!(
        !codes(&report).contains(&"OUTSIDE_SAFE_AREA".to_string()),
        "見えない余白だけが外へ出ているのに警告した: {report}"
    );
}

/// 不透明画素が safe_area の外へ出れば、今までどおり言う。
#[test]
fn opaque_pixels_outside_the_safe_area_are_still_named() {
    let dir = TempDir::new().unwrap();
    padded(dir.path(), "cut.png", (100, 100), [25, 25, 50, 50]);
    let spec = write_spec(
        dir.path(),
        r##"{"canvas":{"width":100,"height":100,"background":"#ffffff"},
             "safe_area":[30,30,40,40],
             "layers":[{"id":"p","type":"image","role":"subject",
                        "source":"cut.png","rect":[0,0,100,100],"trim":"none"}]}"##,
    );
    let out = dir.path().join("out.png");
    let report = json_stdout(&compose(&spec, &out, &["--dry-run"]));

    assert_eq!(layer(&report, "p")["outside_safe_area"], Value::Bool(true));
    let warning = report["warnings"]
        .as_array()
        .unwrap()
        .iter()
        .find(|w| w["code"] == "OUTSIDE_SAFE_AREA")
        .expect("OUTSIDE_SAFE_AREA が出ていない");
    assert!(
        warning["data"].get("opaque_rect").is_some(),
        "判定に使った矩形を data に添えていない: {warning}"
    );
}

/// 1 画素も不透明でない素材は、切り詰めず、`opaque_rect` は null。
/// **描く画素が無いので safe_area の外へは出ようがない。**
#[test]
fn a_fully_transparent_image_has_no_opaque_rect() {
    let dir = TempDir::new().unwrap();
    padded(dir.path(), "empty.png", (50, 50), [0, 0, 0, 0]);
    let spec = write_spec(
        dir.path(),
        r##"{"canvas":{"width":100,"height":100},
             "safe_area":[40,40,20,20],
             "layers":[{"id":"p","type":"image","role":"decoration",
                        "source":"empty.png","rect":[0,0,100,100]}]}"##,
    );
    let out = dir.path().join("out.png");
    let result = compose(&spec, &out, &["--dry-run"]);
    assert!(result.status.success());
    let report = json_stdout(&result);
    let p = layer(&report, "p");

    assert_eq!(rect_of(&p["placed"]), [0.0, 0.0, 100.0, 100.0]);
    assert_eq!(p["opaque_rect"], Value::Null);
    assert_eq!(p["outside_safe_area"], Value::Bool(false));
}

/// `trim` に知らない値を書けば断る。
#[test]
fn an_unknown_trim_value_is_refused() {
    let dir = TempDir::new().unwrap();
    portrait(dir.path());
    let spec = write_spec(
        dir.path(),
        r##"{"canvas":{"width":100,"height":100},
             "layers":[{"id":"p","type":"image","role":"subject",
                        "source":"product.png","rect":[0,0,100,100],"trim":"alfa"}]}"##,
    );
    let out = dir.path().join("out.png");
    let result = compose(&spec, &out, &["--dry-run"]);
    assert!(!result.status.success());
    assert_eq!(json_stdout(&result)["error"]["code"], "SPEC_INVALID");
}

/// `kiri schema` が `trim` と `opaque_rect` を配る。
#[test]
fn the_schema_publishes_trim_and_opaque_rect() {
    let schema = json_stdout(&kiri().args(["schema", "--json"]).output().unwrap());
    let image_keys = schema["compose_spec"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["at"] == "layers[type=image]")
        .unwrap()["keys"]
        .clone();
    assert!(
        image_keys.as_array().unwrap().iter().any(|k| k == "trim"),
        "{image_keys}"
    );
    assert!(
        schema["fields"]
            .as_array()
            .unwrap()
            .iter()
            .any(|f| f["path"] == "layers[].opaque_rect"),
        "fields に layers[].opaque_rect が無い"
    );
}

/// `cover` では、切り詰めた後に枠からはみ出す分が切り落とされる。**落ちるのは
/// 余白ではなく商品である。** 余白のほうを落としたければ `"trim": "none"`。
#[test]
fn cover_after_trimming_crops_the_product_itself() {
    let dir = TempDir::new().unwrap();
    // 100x300 の商品を 400x400 の余白の中に置いた素材
    padded(dir.path(), "cut.png", (400, 400), [100, 50, 100, 300]);
    let spec = |trim: &str| {
        write_spec(
            dir.path(),
            &format!(
                r##"{{"canvas":{{"width":200,"height":200}},
                     "layers":[{{"id":"p","type":"image","role":"subject",
                                "source":"cut.png","rect":[0,0,200,200],
                                "fit":"cover","trim":"{trim}"}}]}}"##
            ),
        )
    };
    let out = dir.path().join("out.png");

    // 切り詰めると 100x300 が 200x600 に広がり、中央の 200x200 が残る——商品で埋まる
    let report = json_stdout(&compose(&spec("alpha"), &out, &["--dry-run"]));
    let p = layer(&report, "p");
    assert_eq!(rect_of(&p["placed"]), [0.0, 0.0, 200.0, 200.0]);
    assert_eq!(rect_of(&p["opaque_rect"]), [0.0, 0.0, 200.0, 200.0]);

    // 余白ごとなら 400x400 が 200x200 に縮むだけで、商品は [50, 25, 50, 150]
    let report = json_stdout(&compose(&spec("none"), &out, &["--dry-run"]));
    assert_eq!(
        rect_of(&layer(&report, "p")["opaque_rect"]),
        [50.0, 25.0, 50.0, 150.0]
    );
}
