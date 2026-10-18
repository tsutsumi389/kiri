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

use common::{
    ProductSpec, compose, json_stdout, kiri, layer, product_image, write_png, write_spec,
};
use serde_json::Value;
use tempfile::TempDir;

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
             {"id":"above","type":"image","role":"subject",
              "source":"product.png","rect":[600,0,50,100]},
             {"id":"edge","type":"image","role":"subject",
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
    // 借りた字体に weight 400 の face が無い機械では FONT_WEIGHT_UNAVAILABLE が
    // 出る。**それは字体の事情で、ここで見ている原寸の測りとは関係が無い**
    let measured: Vec<String> = codes(&plain)
        .into_iter()
        .filter(|c| c != "FONT_WEIGHT_UNAVAILABLE")
        .collect();
    assert!(measured.is_empty(), "{}", plain["warnings"]);

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
            // 層ごとの字体（画像の層では null）。**足した鍵で、消した鍵は無い**
            "font",
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

/// **decoration の画像は safe_area を測らない。** 全面に `cover` で敷いた背景の
/// 写真は外へ出るのが当然で、測ると普通の構図が毎回 `OUTSIDE_SAFE_AREA` を出し、
/// `--fail-on default` を落としていた（13-phase-32.md §13.8）。
#[test]
fn a_full_bleed_decoration_image_is_not_judged_against_the_safe_area() {
    let dir = TempDir::new().unwrap();
    padded(dir.path(), "bg.png", (100, 100), [0, 0, 100, 100]);
    let spec = write_spec(
        dir.path(),
        r##"{"canvas":{"width":100,"height":100,"background":"#ffffff"},
             "safe_area":[20,20,60,60],
             "layers":[{"id":"bg","type":"image","role":"decoration",
                        "source":"bg.png","rect":[0,0,100,100],"fit":"cover"}]}"##,
    );
    let out = dir.path().join("out.png");
    let result = compose(&spec, &out, &["--fail-on", "default"]);
    let report = json_stdout(&result);
    let bg = layer(&report, "bg");

    assert_eq!(rect_of(&bg["opaque_rect"]), [0.0, 0.0, 100.0, 100.0]);
    assert_eq!(bg["outside_safe_area"], Value::Null, "{bg}");
    assert!(
        !codes(&report).contains(&"OUTSIDE_SAFE_AREA".to_string()),
        "全面に敷いた背景を外と言った: {report}"
    );
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stdout)
    );
}

/// 同じ spec で背景の上に置いた subject がはみ出せば、今までどおり言う。
/// decoration を測らないのは role で分けた振る舞いで、画像の層全体ではない。
#[test]
fn a_subject_image_beside_a_decoration_background_is_still_judged() {
    let dir = TempDir::new().unwrap();
    padded(dir.path(), "bg.png", (100, 100), [0, 0, 100, 100]);
    padded(dir.path(), "cut.png", (100, 100), [25, 25, 50, 50]);
    let spec = write_spec(
        dir.path(),
        r##"{"canvas":{"width":100,"height":100,"background":"#ffffff"},
             "safe_area":[30,30,40,40],
             "layers":[{"id":"bg","type":"image","role":"decoration",
                        "source":"bg.png","rect":[0,0,100,100],"fit":"cover"},
                       {"id":"p","type":"image","role":"subject",
                        "source":"cut.png","rect":[0,0,100,100],"trim":"none"}]}"##,
    );
    let out = dir.path().join("out.png");
    let result = compose(&spec, &out, &["--fail-on", "default"]);
    let report = json_stdout(&result);

    assert_eq!(layer(&report, "bg")["outside_safe_area"], Value::Null);
    assert_eq!(layer(&report, "p")["outside_safe_area"], Value::Bool(true));
    let named: Vec<&Value> = report["warnings"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|w| w["code"] == "OUTSIDE_SAFE_AREA")
        .collect();
    assert_eq!(named.len(), 1, "subject だけを言うはず: {report}");
    assert_eq!(result.status.code(), Some(5));
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
             "layers":[{"id":"p","type":"image","role":"subject",
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

// ---------------------------------------------------------------------------
// Phase 32: 縁取り・図形の層・縮小表示のプレビュー（13-phase-32.md §13.4）
// ---------------------------------------------------------------------------

/// Phase 31 の条件 6 の見出し（#767676 の地に白 100px）に、`stroke` を足す。
/// `stroke` が `None` なら Phase 31 と同じ spec である。
fn stroked_heading_spec(dir: &Path, family: &str, stroke: Option<u32>) -> PathBuf {
    let stroke = stroke
        .map(|w| format!(r##","stroke":{{"color":"#000000","width":{w}}}"##))
        .unwrap_or_default();
    write_spec(
        dir,
        &format!(
            r##"{{"canvas":{{"background":"#767676"}},
                 "font":{{"family":"{family}"}},
                 "layers":[{{"id":"heading","type":"text","role":"heading",
                            "lines":["ABAB"],"rect":[80,200,1100,240],
                            "size":100,"color":"#ffffff"{stroke}}}]}}"##
        ),
    )
}

/// 幅 8 の黒い縁は `pc_list` へ縮めても残り、縁と比べて読める（§13.4 の 2）。
/// 原寸の `text_contrast` は塗りと縁の比で 21。
#[test]
fn a_thick_stroke_survives_the_shrink_and_makes_the_heading_legible() {
    let Some(family) = any_font() else {
        return;
    };
    let dir = TempDir::new().unwrap();
    let out = dir.path().join("out.png");

    // 縁が無ければ low_contrast（Phase 31 の条件 6 のまま）
    let bare = stroked_heading_spec(dir.path(), &family, None);
    let report = json_stdout(&compose(&bare, &out, &["--profile", "note", "--dry-run"]));
    let pc = display_of(layer(&report, "heading"), "pc_list");
    assert!(reasons(pc).contains(&"low_contrast".to_string()), "{pc}");
    assert!(
        pc.get("against").is_none(),
        "縁の無い層に against が出た: {pc}"
    );

    let spec = stroked_heading_spec(dir.path(), &family, Some(8));
    let report = json_stdout(&compose(&spec, &out, &["--profile", "note", "--dry-run"]));
    let heading = layer(&report, "heading");
    let full = heading["text_contrast"].as_f64().unwrap();
    assert!((full - 21.0).abs() < 1e-9, "原寸 {full}");

    let pc = display_of(heading, "pc_list");
    assert_eq!(pc["against"], "stroke", "{family}: {pc}");
    assert!(pc["contrast"].as_f64().unwrap() >= 4.5, "{family}: {pc}");
    assert_eq!(pc["legible"], true, "{family}: {pc}");
}

/// 幅 1 の縁は `mobile_list` で地に溶け、**縁が効いたとは言わない**（§13.4 の 3）。
#[test]
fn a_hairline_stroke_lost_in_the_shrink_is_compared_with_the_ground() {
    let Some(family) = any_font() else {
        return;
    };
    let dir = TempDir::new().unwrap();
    let spec = stroked_heading_spec(dir.path(), &family, Some(1));
    let out = dir.path().join("out.png");
    let report = json_stdout(&compose(&spec, &out, &["--profile", "note", "--dry-run"]));
    let mobile = display_of(layer(&report, "heading"), "mobile_list");
    assert_eq!(mobile["against"], "ground", "{family}: {mobile}");
    assert!(
        reasons(mobile).contains(&"low_contrast".to_string()),
        "{family}: {mobile}"
    );
    let warning = report["warnings"]
        .as_array()
        .unwrap()
        .iter()
        .find(|w| w["code"] == "TEXT_ILLEGIBLE_AT_DISPLAY" && w["data"]["display"] == "mobile_list")
        .expect("mobile_list の警告が無い");
    let hint = warning["hint"].as_str().unwrap();
    assert!(
        hint.contains("stroke の width を 9 以上") && hint.contains("shape"),
        "low_contrast の手に縁を太らせる手と帯が無い: {hint}"
    );
    // 既に縁を持つ層に「縁取る」とは言わない
    assert!(!hint.contains("縁取る"), "{hint}");
}

/// 縁は見える画素なので、`placed` も `text_overflow` も縁を含む（§13.2.1）。
///
/// 縁のある層は縁の幅だけ内側へ寄せて組む（§13.7）。`placed` は縁の無い層の
/// 矩形を縁の幅だけ動かしたものを含み、その 4 辺を縁の幅だけ広げた範囲に収まる。
#[test]
fn the_placed_rect_includes_the_stroke() {
    let Some(family) = any_font() else {
        return;
    };
    let dir = TempDir::new().unwrap();
    let out = dir.path().join("out.png");
    let bare = stroked_heading_spec(dir.path(), &family, None);
    // キャンバスの寸法は profile から来る（spec が書いていない）
    let args = ["--profile", "note", "--dry-run"];
    let bare = rect_of(&layer(&json_stdout(&compose(&bare, &out, &args)), "heading")["placed"]);
    let spec = stroked_heading_spec(dir.path(), &family, Some(8));
    let report = json_stdout(&compose(&spec, &out, &args));
    let heading = layer(&report, "heading");
    let placed = rect_of(&heading["placed"]);

    // 縁の無い矩形を右と下へ 8 動かしたもの（字の送りの矩形）
    let [bx, by, bw, bh] = [bare[0] + 8.0, bare[1] + 8.0, bare[2], bare[3]];
    let eps = 1e-3;
    assert!(
        placed[0] <= bx + eps
            && placed[1] <= by + eps
            && placed[0] + placed[2] >= bx + bw - eps
            && placed[1] + placed[3] >= by + bh - eps,
        "縁を付けて矩形が縮んだ: {placed:?} / {bare:?}"
    );
    assert!(
        placed[0] >= bx - 8.0 - eps
            && placed[1] >= by - 8.0 - eps
            && placed[0] + placed[2] <= bx + bw + 8.0 + eps
            && placed[1] + placed[3] <= by + bh + 8.0 + eps,
        "縁の幅より外まで広がった: {placed:?} / {bare:?}"
    );
    assert!(
        placed[0] < bx - 5.0,
        "左へ縁が出ていない: {placed:?} / {bare:?}"
    );
    let widths = heading["line_widths"].as_array().unwrap();
    assert!(
        (widths[0].as_f64().unwrap() - placed[2]).abs() < 0.01,
        "1 行のときは行の幅と placed の幅が一致する"
    );

    // **本当に狭い枠では、縁の分だけはみ出す。** 縁の無い字がちょうど収まる枠に
    // 縁を付けると、字は縁の幅だけ右と下へ寄るので、少なくとも縁の幅だけ出る
    let tight = write_spec(
        dir.path(),
        &format!(
            r##"{{"canvas":{{"width":1280,"height":670,"background":"#ffffff"}},
                 "font":{{"family":"{family}"}},
                 "layers":[{{"id":"t","type":"text","role":"heading","lines":["ABAB"],
                            "rect":[{x},{y},{w},{h}],"size":100,"color":"#000000",
                            "stroke":{{"color":"#ff0000","width":8}}}}]}}"##,
            x = bare[0],
            y = bare[1],
            w = bare[2],
            h = bare[3],
        ),
    );
    let report = json_stdout(&compose(&tight, &out, &["--dry-run"]));
    let t = layer(&report, "t");
    let over = t["text_overflow"].as_f64().unwrap();
    assert!(
        over >= 8.0 - eps,
        "縁が枠から出たのにはみ出しと言わない: {t}"
    );
    assert!(codes(&report).contains(&"TEXT_OVERFLOW".to_string()));
}

/// **縁のある見出しも、余裕のある枠なら はみ出さない**（§13.7）。縁を内側へ
/// 寄せずに組むと、`start` 揃えの字頭と最初の行の天で縁が必ず枠の外へ出て、
/// どう `rect` を書いても `--fail-on default` を通らなかった。
#[test]
fn a_stroked_heading_with_room_does_not_overflow() {
    let Some(family) = any_font() else {
        return;
    };
    let dir = TempDir::new().unwrap();
    let out = dir.path().join("out.png");
    for align in ["start", "center", "end"] {
        let spec = write_spec(
            dir.path(),
            &format!(
                r##"{{"canvas":{{"background":"#767676"}},
                     "font":{{"family":"{family}"}},
                     "layers":[{{"id":"heading","type":"text","role":"heading",
                                "lines":["ABAB"],"rect":[80,200,1100,240],"align":"{align}",
                                "size":100,"color":"#ffffff",
                                "stroke":{{"color":"#000000","width":8}}}}]}}"##
            ),
        );
        let report = json_stdout(&compose(
            &spec,
            &out,
            &["--profile", "note", "--dry-run", "--fail-on", "default"],
        ));
        let heading = layer(&report, "heading");
        assert_eq!(heading["text_overflow"], 0.0, "{family} {align}: {heading}");
        assert!(
            rect_of(&heading["placed"])[0] >= 80.0,
            "{family} {align}: {heading}"
        );
        // 縮小表示の読みやすさは字体の太さに依るので、門は text_overflow の行だけを見る
        let check = check_of(&report, "text_overflow").expect("text_overflow の行");
        assert_eq!(check["status"], "pass", "{family} {align}: {check}");
        assert!(!codes(&report).contains(&"TEXT_OVERFLOW".to_string()));
    }
}

/// **塗りと同じ色の縁は、字を地から分けていない。** 黒地に白の字と白の縁は
/// 縁との比が 1 だが、地と比べれば読める（§13.7）。原寸も縮小も地と比べる。
#[test]
fn a_stroke_the_colour_of_the_fill_is_not_called_low_contrast() {
    let Some(family) = any_font() else {
        return;
    };
    let dir = TempDir::new().unwrap();
    let spec = write_spec(
        dir.path(),
        &format!(
            r##"{{"canvas":{{"background":"#000000"}},
                 "font":{{"family":"{family}"}},
                 "layers":[{{"id":"heading","type":"text","role":"heading",
                            "lines":["ABAB"],"rect":[80,200,1100,240],
                            "size":100,"color":"#ffffff",
                            "stroke":{{"color":"#ffffff","width":8}}}}]}}"##
        ),
    );
    let out = dir.path().join("out.png");
    let report = json_stdout(&compose(&spec, &out, &["--profile", "note", "--dry-run"]));
    let heading = layer(&report, "heading");
    assert!(
        heading["text_contrast"].as_f64().unwrap() > 20.0,
        "{heading}"
    );
    assert!(!codes(&report).contains(&"TEXT_CONTRAST_LOW".to_string()));
    let pc = display_of(heading, "pc_list");
    assert_eq!(pc["against"], "ground", "{pc}");
    assert_eq!(pc["legible"], true, "{pc}");
}

/// 白地に白に近い文字の下へ黒い帯（不透明度 0.8）を敷くと、`TEXT_CONTRAST_LOW` が
/// 消える（§13.4 の 4）。
#[test]
fn a_band_beneath_pale_text_clears_the_low_contrast() {
    let Some(family) = any_font() else {
        return;
    };
    let dir = TempDir::new().unwrap();
    let make = |band: bool| {
        let band = if band {
            r##"{"id":"band","type":"shape","rect":[0,40,400,120],
                 "color":"#000000","opacity":0.8},"##
        } else {
            ""
        };
        write_spec(
            dir.path(),
            &format!(
                r##"{{"canvas":{{"width":400,"height":200,"background":"#ffffff"}},
                     "font":{{"family":"{family}"}},
                     "layers":[{band}
                       {{"id":"t","type":"text","role":"heading","lines":["AB"],
                        "rect":[20,50,360,100],"size":60,"color":"#eeeeee"}}]}}"##
            ),
        )
    };
    let out = dir.path().join("out.png");
    let report = json_stdout(&compose(&make(false), &out, &["--dry-run"]));
    assert!(codes(&report).contains(&"TEXT_CONTRAST_LOW".to_string()));
    let hint = report["warnings"]
        .as_array()
        .unwrap()
        .iter()
        .find(|w| w["code"] == "TEXT_CONTRAST_LOW")
        .unwrap()["hint"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(hint.contains("stroke") && hint.contains("shape"), "{hint}");

    let report = json_stdout(&compose(&make(true), &out, &["--dry-run"]));
    assert!(
        !codes(&report).contains(&"TEXT_CONTRAST_LOW".to_string()),
        "帯を敷いたのにまだ低い: {}",
        layer(&report, "t")
    );
    assert!(layer(&report, "t")["text_contrast"].as_f64().unwrap() >= 4.5);
}

/// 文字の上に不透明な図形を重ねると `TEXT_OBSCURED`（§13.4 の 5）。
#[test]
fn an_opaque_shape_over_text_obscures_it() {
    let Some(family) = any_font() else {
        return;
    };
    let dir = TempDir::new().unwrap();
    let spec = write_spec(
        dir.path(),
        &format!(
            r##"{{"canvas":{{"width":400,"height":200,"background":"#ffffff"}},
                 "font":{{"family":"{family}"}},
                 "layers":[
                   {{"id":"t","type":"text","role":"heading","lines":["AB"],
                    "rect":[20,40,360,120],"size":60,"color":"#000000"}},
                   {{"id":"plate","type":"shape","rect":[0,0,400,200],
                    "color":"#202020","radius":12}}]}}"##
        ),
    );
    let out = dir.path().join("out.png");
    let result = compose(&spec, &out, &["--fail-on", "default"]);
    let report = json_stdout(&result);
    assert!(layer(&report, "t")["text_obscured"].as_f64().unwrap() > 0.99);
    assert!(codes(&report).contains(&"TEXT_OBSCURED".to_string()));
    assert_eq!(result.status.code(), Some(5));
}

/// 図形の層は `decoration`・`outside_safe_area: null`・`display` 無し
/// （§13.4 の 6）。`subject` ではないので、文字が重なっても `layer_overlap` は数えない。
#[test]
fn a_shape_layer_reports_as_an_unmeasured_decoration() {
    let Some(family) = any_font() else {
        return;
    };
    let dir = TempDir::new().unwrap();
    let spec = write_spec(
        dir.path(),
        &format!(
            r##"{{"font":{{"family":"{family}"}},
                 "layers":[
                   {{"id":"band","type":"shape","rect":[0,0,1280,670],"color":"#000000"}},
                   {{"id":"heading","type":"text","role":"heading","lines":["AB"],
                    "rect":[80,200,1100,240],"size":120,"color":"#ffffff"}}]}}"##
        ),
    );
    let out = dir.path().join("out.png");
    let report = json_stdout(&compose(&spec, &out, &["--profile", "note", "--dry-run"]));
    let band = layer(&report, "band");
    assert_eq!(band["kind"], "shape");
    assert_eq!(band["role"], "decoration");
    assert_eq!(
        band["outside_safe_area"],
        Value::Null,
        "端まで敷いた帯を外と言った"
    );
    assert!(band.get("display").is_none(), "{band}");
    assert_eq!(rect_of(&band["placed"]), [0.0, 0.0, 1280.0, 670.0]);
    for key in [
        "opaque_rect",
        "line_widths",
        "text_overflow",
        "text_contrast",
        "text_obscured",
        "layer_overlap",
        "align",
        "font",
    ] {
        assert_eq!(band[key], Value::Null, "{key}: {band}");
    }
    assert!(!codes(&report).contains(&"OUTSIDE_SAFE_AREA".to_string()));
    let heading = layer(&report, "heading");
    assert_eq!(
        heading["layer_overlap"],
        Value::Null,
        "図形は subject ではない"
    );
    assert!(
        heading["text_contrast"].as_f64().unwrap() > 20.0,
        "黒い帯の上の白"
    );
}

/// PNG を読み、不透明の RGB を並べる。
fn rgb_of(path: &Path) -> (u32, u32, Vec<u8>) {
    let image = image::open(path).unwrap().to_rgba8();
    let rgb = image.pixels().flat_map(|p| [p[0], p[1], p[2]]).collect();
    (image.width(), image.height(), rgb)
}

/// `--preview` は 2 枚を書き、寸法は表示の CSS px で、**画素は測りの絵そのもの**
/// である（§13.4 の 7）。
///
/// 書いた出力（PNG なので可逆）を、測りと同じ `shrink_canvas` で縮め直して
/// 突き合わせる。輝度は RGB から出す（`display::Shrunk` の doc）ので、RGB が
/// 一致すれば輝度も一致する。
#[test]
fn the_preview_is_the_picture_the_display_measure_saw() {
    let dir = TempDir::new().unwrap();
    // 字体の要らない絵で確かめる。中間色の地に半透明の帯と丸い札
    let spec = write_spec(
        dir.path(),
        r##"{"canvas":{"background":"#767676"},
             "layers":[
               {"id":"band","type":"shape","rect":[0,300.5,1280,97.3],
                "color":"#102030","opacity":0.6},
               {"id":"tag","type":"shape","rect":[900,100,300,120],
                "color":"#ffffff","radius":60}]}"##,
    );
    let out = dir.path().join("header.png");
    let result = compose(&spec, &out, &["--profile", "note", "--preview"]);
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let report = json_stdout(&result);

    let previews = report["previews"].as_array().expect("previews[] が無い");
    let listed: Vec<(String, u64, u64)> = previews
        .iter()
        .map(|p| {
            (
                p["display"].as_str().unwrap().to_string(),
                p["width"].as_u64().unwrap(),
                p["height"].as_u64().unwrap(),
            )
        })
        .collect();
    assert_eq!(
        listed,
        [
            ("pc_list".to_string(), 192, 101),
            ("mobile_list".to_string(), 144, 75)
        ]
    );

    let canvas = image::open(&out).unwrap().to_rgba8();
    for p in previews {
        let path = PathBuf::from(p["path"].as_str().unwrap());
        assert_eq!(
            path,
            dir.path()
                .join(format!("header.{}.png", p["display"].as_str().unwrap()))
        );
        let (w, h, rgb) = rgb_of(&path);
        assert_eq!(
            (u64::from(w), u64::from(h)),
            (p["width"].as_u64().unwrap(), p["height"].as_u64().unwrap())
        );
        let measured = kiri::compose::display::shrink_canvas(&canvas, [255, 255, 255], w, h);
        assert_eq!(
            rgb,
            measured.rgb().as_raw().as_slice(),
            "{}: 書いた絵と測った絵が違う",
            path.display()
        );
    }
}

/// 文字の測りとプレビューの輝度が一致する（§13.4 の 7）。縮めた絵の画素から
/// 前景と背景の中央値を取り直し、`display[].contrast` と同じ数になることを見る。
#[test]
fn the_preview_reproduces_the_measured_contrast() {
    let Some(family) = any_font() else {
        return;
    };
    let dir = TempDir::new().unwrap();
    let spec = stroked_heading_spec(dir.path(), &family, None);
    let out = dir.path().join("h.png");
    let report = json_stdout(&compose(&spec, &out, &["--profile", "note", "--preview"]));
    let heading = layer(&report, "heading");

    // 前景の画素を、文字だけを描いた絵から同じ縮小で取る
    let glyphs_spec = write_spec(
        dir.path(),
        &format!(
            r##"{{"canvas":{{"width":1280,"height":670}},
                 "font":{{"family":"{family}"}},
                 "layers":[{{"id":"heading","type":"text","role":"heading",
                            "lines":["ABAB"],"rect":[80,200,1100,240],
                            "size":100,"color":"#ffffff"}}]}}"##
        ),
    );
    let glyphs_out = dir.path().join("glyphs.png");
    assert!(compose(&glyphs_spec, &glyphs_out, &[]).status.success());
    let glyphs = image::open(&glyphs_out).unwrap().to_rgba8();
    let placed = rect_of(&heading["placed"]);

    for p in report["previews"].as_array().unwrap() {
        let name = p["display"].as_str().unwrap();
        let (w, h, rgb) = rgb_of(Path::new(p["path"].as_str().unwrap()));
        let coverage = kiri::compose::display::shrink_coverage(&glyphs, w, h);
        let lum = |i: usize| {
            let f = |v: u8| {
                let c = f64::from(v) / 255.0;
                if c <= 0.03928 {
                    c / 12.92
                } else {
                    ((c + 0.055) / 1.055).powf(2.4)
                }
            };
            0.2126 * f(rgb[i * 3]) + 0.7152 * f(rgb[i * 3 + 1]) + 0.0722 * f(rgb[i * 3 + 2])
        };
        let (sx, sy) = (f64::from(w) / 1280.0, f64::from(h) / 670.0);
        let x0 = (placed[0] * sx).floor() as usize;
        let x1 = (((placed[0] + placed[2]) * sx).ceil() as usize).min(w as usize);
        let y0 = (placed[1] * sy).floor() as usize;
        let y1 = (((placed[1] + placed[3]) * sy).ceil() as usize).min(h as usize);
        let mut front: Vec<f64> = (0..coverage.len())
            .filter(|&i| coverage[i] == kiri::compose::display::FOREGROUND)
            .map(lum)
            .collect();
        let mut back: Vec<f64> = (y0..y1)
            .flat_map(|y| (x0..x1).map(move |x| y * w as usize + x))
            .filter(|&i| coverage[i] == kiri::compose::display::BACKGROUND)
            .map(lum)
            .collect();
        front.sort_by(f64::total_cmp);
        back.sort_by(f64::total_cmp);
        let (f, b) = (front[front.len() / 2], back[back.len() / 2]);
        let (hi, lo) = if f > b { (f, b) } else { (b, f) };
        let expected = (hi + 0.05) / (lo + 0.05);
        let measured = display_of(heading, name)["contrast"].as_f64().unwrap();
        assert_eq!(measured, expected, "{name}");
    }
}

/// `--preview` は `--profile` が要り、`--dry-run` と併用できない（§13.4 の 8）。
#[test]
fn the_preview_needs_a_profile_and_refuses_a_dry_run() {
    let dir = TempDir::new().unwrap();
    let spec = write_spec(
        dir.path(),
        r##"{"canvas":{"width":1280,"height":670,"background":"#ffffff"},
             "layers":[{"id":"s","type":"shape","rect":[0,0,10,10],"color":"#000000"}]}"##,
    );
    let out = dir.path().join("out.png");
    assert_eq!(compose(&spec, &out, &["--preview"]).status.code(), Some(2));
    assert_eq!(
        compose(
            &spec,
            &out,
            &["--profile", "note", "--preview", "--dry-run"]
        )
        .status
        .code(),
        Some(2)
    );
    assert!(!out.exists());
    assert!(!dir.path().join("out.pc_list.png").exists());
}

/// プレビューの先に既存のファイルがあれば `--force` 無しで断り、**出力も書かない**
/// （§13.4 の 8）。`--force` があれば全部書く。
#[test]
fn an_existing_preview_blocks_every_write_without_force() {
    let dir = TempDir::new().unwrap();
    let spec = write_spec(
        dir.path(),
        r##"{"canvas":{"background":"#ffffff"},
             "layers":[{"id":"s","type":"shape","rect":[0,0,10,10],"color":"#000000"}]}"##,
    );
    let out = dir.path().join("out.png");
    let existing = dir.path().join("out.mobile_list.png");
    std::fs::write(&existing, b"keep").unwrap();

    let result = compose(&spec, &out, &["--profile", "note", "--preview"]);
    assert!(!result.status.success());
    assert_eq!(json_stdout(&result)["error"]["code"], "OUTPUT_EXISTS");
    assert!(!out.exists(), "プレビューで断ったのに出力を書いた");
    assert!(!dir.path().join("out.pc_list.png").exists());
    assert_eq!(std::fs::read(&existing).unwrap(), b"keep");

    let result = compose(&spec, &out, &["--profile", "note", "--preview", "--force"]);
    assert!(result.status.success());
    assert!(out.exists());
    assert_eq!(rgb_of(&existing).0, 144);
}

/// プレビューの先がディレクトリなら、`--force` があっても**描く前に**断り、出力も
/// 書かない・変えない（§13.7）。
#[test]
fn a_directory_at_a_preview_path_blocks_every_write_even_with_force() {
    let dir = TempDir::new().unwrap();
    let spec = write_spec(
        dir.path(),
        r##"{"canvas":{"background":"#ffffff"},
             "layers":[{"id":"s","type":"shape","rect":[0,0,10,10],"color":"#000000"}]}"##,
    );
    let out = dir.path().join("out.png");
    std::fs::write(&out, b"old").unwrap();
    std::fs::create_dir(dir.path().join("out.pc_list.png")).unwrap();

    let result = compose(&spec, &out, &["--profile", "note", "--preview", "--force"]);
    assert!(!result.status.success());
    assert_eq!(json_stdout(&result)["error"]["code"], "OUTPUT_WRITE_FAILED");
    assert_eq!(std::fs::read(&out).unwrap(), b"old", "出力を書き換えた");
    assert!(!dir.path().join("out.mobile_list.png").exists());

    // 出力の先がディレクトリでも同じく描く前に断る
    let other = dir.path().join("other.png");
    std::fs::create_dir(&other).unwrap();
    let result = compose(
        &spec,
        &other,
        &["--profile", "note", "--preview", "--force"],
    );
    assert_eq!(json_stdout(&result)["error"]["code"], "OUTPUT_WRITE_FAILED");
    assert!(!dir.path().join("other.pc_list.png").exists());
}

/// `--preview` を渡さなければ `previews` はキーごと出ず、縁の無い層の
/// `display[]` に `against` は出ない（§13.4 の 1）。
#[test]
fn without_the_new_options_no_new_keys_appear() {
    let Some(family) = any_font() else {
        return;
    };
    let dir = TempDir::new().unwrap();
    let spec = stroked_heading_spec(dir.path(), &family, None);
    let out = dir.path().join("out.png");
    let report = json_stdout(&compose(&spec, &out, &["--profile", "note"]));
    assert!(report.get("previews").is_none(), "{report}");
    for d in layer(&report, "heading")["display"].as_array().unwrap() {
        assert!(d.get("against").is_none(), "{d}");
    }
    assert!(!dir.path().join("out.pc_list.png").exists());
}

/// 不正な `stroke` / `shape` の値とキーを断る（§13.4 の 9）。
#[test]
fn bad_stroke_and_shape_values_are_refused() {
    let dir = TempDir::new().unwrap();
    let out = dir.path().join("out.png");
    for (layer, code) in [
        (
            r##"{"id":"t","type":"text","role":"heading","lines":["A"],"rect":[0,0,10,10],
                "size":20,"color":"#000000","stroke":{"color":"#ffffff","width":21}}"##,
            "SPEC_INVALID",
        ),
        (
            r##"{"id":"t","type":"text","role":"heading","lines":["A"],"rect":[0,0,10,10],
                "size":20,"color":"#000000","stroke":{"color":"#ffffff","width":0}}"##,
            "SPEC_INVALID",
        ),
        (
            r##"{"id":"t","type":"text","role":"heading","lines":["A"],"rect":[0,0,10,10],
                "size":20,"color":"#000000","stroke":{"colour":"#ffffff","width":2}}"##,
            "SPEC_UNKNOWN_FIELD",
        ),
        (
            r##"{"id":"s","type":"shape","rect":[0,0,10,10],"color":"#000000","opacity":0}"##,
            "SPEC_INVALID",
        ),
        (
            r##"{"id":"s","type":"shape","rect":[0,0,10,10],"color":"#000000","opacity":1.5}"##,
            "SPEC_INVALID",
        ),
        (
            r##"{"id":"s","type":"shape","rect":[0,0,10,10],"color":"#000000","radius":-2}"##,
            "SPEC_INVALID",
        ),
        (
            r##"{"id":"s","type":"shape","role":"heading","rect":[0,0,10,10],"color":"#000000"}"##,
            "SPEC_UNKNOWN_FIELD",
        ),
        (
            r##"{"id":"s","type":"shape","rect":[0,0,10,10],"color":"#000"}"##,
            "INVALID_COLOR",
        ),
    ] {
        let spec = write_spec(
            dir.path(),
            &format!(
                r##"{{"canvas":{{"width":100,"height":100}},"font":{{"family":"X"}},
                     "layers":[{layer}]}}"##
            ),
        );
        let result = compose(&spec, &out, &[]);
        assert_eq!(json_stdout(&result)["error"]["code"], code, "{layer}");
        assert!(!out.exists());
    }
}

/// 縁取り・図形・プレビューを使っても、同じ入力から同じ結果（§13.4 の 10）。
#[test]
fn strokes_shapes_and_previews_are_deterministic() {
    let Some(family) = any_font() else {
        return;
    };
    let run = |name: &str| {
        let dir = TempDir::new().unwrap();
        let spec = write_spec(
            dir.path(),
            &format!(
                r##"{{"canvas":{{"background":"#767676"}},
                     "font":{{"family":"{family}"}},
                     "layers":[
                       {{"id":"band","type":"shape","rect":[40,180,1200,280],
                        "color":"#000000","opacity":0.5,"radius":24}},
                       {{"id":"heading","type":"text","role":"heading","lines":["ABAB"],
                        "rect":[80,200,1100,240],"size":100,"color":"#ffffff",
                        "stroke":{{"color":"#000000","width":8}}}}]}}"##
            ),
        );
        let out = dir.path().join(name);
        let mut report = json_stdout(&compose(&spec, &out, &["--profile", "note", "--preview"]));
        let o = report.as_object_mut().unwrap();
        o.remove("elapsed_ms");
        o.remove("spec");
        o.remove("output");
        let mut bytes = vec![std::fs::read(&out).unwrap()];
        for p in o.remove("previews").unwrap().as_array().unwrap() {
            bytes.push(std::fs::read(p["path"].as_str().unwrap()).unwrap());
        }
        (report, bytes)
    };
    let (a, a_bytes) = run("a.png");
    let (b, b_bytes) = run("a.png");
    assert_eq!(a, b);
    assert_eq!(a_bytes, b_bytes);
}

/// `kiri schema` が縁取り・図形・`against`・`previews[]`・`--preview` を配る
/// （§13.4 の 12）。
#[test]
fn the_schema_publishes_strokes_shapes_and_previews() {
    let out = kiri()
        .args(["schema", "compose", "--json"])
        .output()
        .unwrap();
    let schema = json_stdout(&out);
    let at = |name: &str| -> Vec<String> {
        schema["compose_spec"]
            .as_array()
            .unwrap()
            .iter()
            .find(|e| e["at"] == name)
            .unwrap_or_else(|| panic!("compose_spec に '{name}' が無い"))["keys"]
            .as_array()
            .unwrap()
            .iter()
            .map(|k| k.as_str().unwrap().to_string())
            .collect()
    };
    assert!(at("layers[type=text]").contains(&"stroke".to_string()));
    assert_eq!(at("layers[type=text].stroke"), ["color", "width"]);
    assert_eq!(
        at("layers[type=shape]"),
        ["id", "type", "rect", "color", "opacity", "radius"]
    );
    let paths: Vec<&str> = schema["fields"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| f["path"].as_str().unwrap())
        .collect();
    assert!(paths.contains(&"layers[].display[].against"), "{paths:?}");
    for key in ["display", "path", "width", "height"] {
        let path = format!("previews[].{key}");
        assert!(paths.contains(&path.as_str()), "{path}: {paths:?}");
    }
    let options = schema["commands"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["name"] == "compose")
        .unwrap()["options"]
        .to_string();
    assert!(options.contains("--preview"), "{options}");
}

// ---------------------------------------------------------------------------
// Phase 33: `--free-regions`（14-phase-33.md）
// ---------------------------------------------------------------------------

/// `free_regions` を取り出す。無ければ落とす。
fn free_regions(report: &Value) -> &Value {
    report
        .get("free_regions")
        .unwrap_or_else(|| panic!("free_regions が無い: {report}"))
}

/// 候補の矩形を整数で並べる。
fn region_rects(free: &Value) -> Vec<[i64; 4]> {
    free["regions"]
        .as_array()
        .expect("regions[] が無い")
        .iter()
        .map(|r| {
            let a = r["rect"].as_array().expect("rect は配列");
            [0, 1, 2, 3].map(|i| a[i].as_i64().expect("rect は整数"))
        })
        .collect()
}

/// 2 つの矩形 `[x, y, 幅, 高さ]` が、間に `gap` 画素以上を空けて離れているか。
fn apart(a: [f64; 4], b: [f64; 4], gap: f64) -> bool {
    a[0] + a[2] + gap <= b[0]
        || b[0] + b[2] + gap <= a[0]
        || a[1] + a[3] + gap <= b[1]
        || b[1] + b[3] + gap <= a[1]
}

fn as_f64(rect: [i64; 4]) -> [f64; 4] {
    rect.map(|v| v as f64)
}

/// 不透明な画素で `opaque(x, y)` を描いた `w`x`h` の画像。
fn mask_image(w: u32, h: u32, opaque: impl Fn(u32, u32) -> bool) -> image::RgbaImage {
    image::RgbaImage::from_fn(w, h, |x, y| {
        if opaque(x, y) {
            image::Rgba([40, 40, 40, 255])
        } else {
            image::Rgba([0, 0, 0, 0])
        }
    })
}

/// 外接矩形の右上が透明な L 字。縦棒は左の 60px、横棒は下の 60px。
fn l_shape() -> image::RgbaImage {
    mask_image(200, 200, |x, y| x < 60 || y >= 140)
}

/// 渡さなければ `free_regions` はキーごと出ない（§14.4 の 1）。
#[test]
fn free_regions_are_absent_without_the_flag() {
    let dir = TempDir::new().unwrap();
    write_png(dir.path(), "l.png", &l_shape());
    let spec = write_spec(
        dir.path(),
        r##"{"canvas":{"width":400,"height":400,"background":"#ffffff"},
             "layers":[{"id":"subject","type":"image","role":"subject",
                        "source":"l.png","rect":[100,100,200,200]}]}"##,
    );
    let out = dir.path().join("out.png");
    let report = json_stdout(&compose(&spec, &out, &["--dry-run"]));
    assert!(report.get("free_regions").is_none(), "{report}");
}

/// 避ける相手が無ければ `within` そのものが 1 つだけ返る（§14.4 の 2）。
/// **図形と `decoration` の画像は避けない**ので、その上に候補が出る（§14.4 の 5）。
///
/// 層が 1 つも無い spec は `SPEC_EMPTY` で断られるので、ここでは避けない層だけを
/// 置く。`safe_area` の高さ 454 は 8 の倍数でないが、端は削らない。
#[test]
fn shapes_and_decoration_leave_the_whole_range_free() {
    let dir = TempDir::new().unwrap();
    write_png(dir.path(), "bg.png", &mask_image(1280, 670, |_, _| true));
    let spec = write_spec(
        dir.path(),
        r##"{"canvas":{"width":1280,"height":670,"background":"#ffffff"},
             "safe_area":[0,108,1280,454],
             "layers":[{"id":"photo","type":"image","role":"decoration",
                        "source":"bg.png","rect":[0,0,1280,670]},
                       {"id":"band","type":"shape","rect":[0,300,1280,120],
                        "color":"#000000","opacity":0.5}]}"##,
    );
    let out = dir.path().join("out.png");
    let report = json_stdout(&compose(&spec, &out, &["--dry-run", "--free-regions"]));
    let free = free_regions(&report);
    assert_eq!(free["within"], serde_json::json!([0, 108, 1280, 454]));
    assert_eq!(free["margin"], 24, "1280 幅の既定は 24px");
    assert_eq!(free["cell"], 8);
    assert_eq!(region_rects(free), [[0, 108, 1280, 454]]);
    assert_eq!(free["regions"][0]["area_ratio"], 1.0);
}

/// 中央の正方形の `subject` の周りに、左右と上下の帯の 4 つが返り、どれも
/// `opaque_rect` から `margin` 以上離れている（§14.4 の 3）。
#[test]
fn a_square_subject_leaves_four_bands_clear_of_it() {
    let dir = TempDir::new().unwrap();
    write_png(dir.path(), "square.png", &mask_image(80, 80, |_, _| true));
    let spec = write_spec(
        dir.path(),
        r##"{"canvas":{"width":400,"height":400,"background":"#ffffff"},
             "layers":[{"id":"subject","type":"image","role":"subject",
                        "source":"square.png","rect":[160,160,80,80]}]}"##,
    );
    let out = dir.path().join("out.png");
    let report = json_stdout(&compose(&spec, &out, &["--dry-run", "--free-regions"]));
    let free = free_regions(&report);
    let margin = free["margin"].as_f64().unwrap();
    assert_eq!(margin, 8.0, "round(400 × 24 / 1280) = round(7.5) = 8");
    let opaque = rect_of(&layer(&report, "subject")["opaque_rect"]);
    assert_eq!(opaque, [160.0, 160.0, 80.0, 80.0]);

    let rects = region_rects(free);
    assert_eq!(rects.len(), 4, "{free}");
    for r in &rects {
        assert!(
            apart(as_f64(*r), opaque, margin),
            "{r:?} が opaque_rect から {margin}px 離れていない"
        );
    }
    // 上・下・左・右の帯がそれぞれ 1 つずつある
    let [ox, oy, ow, oh] = opaque;
    let above = |r: &[i64; 4]| (r[1] + r[3]) as f64 <= oy;
    let below = |r: &[i64; 4]| r[1] as f64 >= oy + oh;
    let left = |r: &[i64; 4]| (r[0] + r[2]) as f64 <= ox;
    let right = |r: &[i64; 4]| r[0] as f64 >= ox + ow;
    for (name, side) in [
        ("上", &above as &dyn Fn(&[i64; 4]) -> bool),
        ("下", &below),
        ("左", &left),
        ("右", &right),
    ] {
        assert_eq!(
            rects.iter().filter(|r| side(r)).count(),
            1,
            "{name}の帯がちょうど 1 つではない: {rects:?}"
        );
    }
}

/// L 字の `subject` では、**外接矩形の内側の空き**にかかる候補が返る
/// （§14.4 の 4）。同じ外接矩形を塗り潰した `subject` では返らない——
/// `opaque_rect` だけで計算したなら出ない候補である。
///
/// 併せて、どの候補の全画素も L 字の不透明な画素から `margin` を超えて離れて
/// いることを、素材の画素から総当たりで確かめる（§14.4 の 6）。
#[test]
fn an_l_shaped_subject_leaves_room_inside_its_bounding_box() {
    let dir = TempDir::new().unwrap();
    let shape = l_shape();
    write_png(dir.path(), "l.png", &shape);
    write_png(dir.path(), "box.png", &mask_image(200, 200, |_, _| true));
    let spec_for = |source: &str| {
        format!(
            r##"{{"canvas":{{"width":400,"height":400,"background":"#ffffff"}},
                 "layers":[{{"id":"subject","type":"image","role":"subject",
                            "source":"{source}","rect":[100,100,200,200]}}]}}"##
        )
    };
    let out = dir.path().join("out.png");

    let spec = write_spec(dir.path(), &spec_for("l.png"));
    let report = json_stdout(&compose(&spec, &out, &["--dry-run", "--free-regions"]));
    let opaque = rect_of(&layer(&report, "subject")["opaque_rect"]);
    assert_eq!(opaque, [100.0, 100.0, 200.0, 200.0], "外接矩形は L 字全体");
    let free = free_regions(&report);
    let margin = free["margin"].as_i64().unwrap();
    let shaped = region_rects(free);
    let inside: Vec<_> = shaped
        .iter()
        .filter(|r| !apart(as_f64(**r), opaque, 0.0))
        .collect();
    assert!(
        !inside.is_empty(),
        "外接矩形の内側にかかる候補が無い: {shaped:?}"
    );

    // 全画素を、L 字の不透明な画素から総当たりで離す（チェビシェフ距離 > margin）
    let opaque_at = |x: i64, y: i64| {
        let (sx, sy) = (x - 100, y - 100);
        (0..200).contains(&sx)
            && (0..200).contains(&sy)
            && shape.get_pixel(sx as u32, sy as u32)[3] > 0
    };
    for [x, y, w, h] in &shaped {
        for py in *y..y + h {
            for px in *x..x + w {
                for oy in py - margin..=py + margin {
                    for ox in px - margin..=px + margin {
                        assert!(
                            !opaque_at(ox, oy),
                            "({px}, {py}) が不透明な ({ox}, {oy}) から {margin}px 以内"
                        );
                    }
                }
            }
        }
    }

    // 外接矩形を塗り潰すと、内側にかかる候補は消える
    let spec = write_spec(dir.path(), &spec_for("box.png"));
    let report = json_stdout(&compose(&spec, &out, &["--dry-run", "--free-regions"]));
    let boxed = region_rects(free_regions(&report));
    assert!(
        boxed.iter().all(|r| apart(as_f64(*r), opaque, 0.0)),
        "{boxed:?}"
    );
    for r in inside {
        assert!(!boxed.contains(r), "{r:?} は外接矩形でも出てしまう");
    }
}

/// `subject` 以外の画像（ロゴの `heading` など）も避ける。`decoration` と書いた
/// 画像だけは避けない（§14.2.2）。
#[test]
fn every_image_but_decoration_is_avoided() {
    let dir = TempDir::new().unwrap();
    write_png(dir.path(), "logo.png", &mask_image(100, 100, |_, _| true));
    let spec_for = |role: &str| {
        format!(
            r##"{{"canvas":{{"width":400,"height":400,"background":"#ffffff"}},
                 "layers":[{{"id":"logo","type":"image","role":"{role}",
                            "source":"logo.png","rect":[150,150,100,100]}}]}}"##
        )
    };
    let out = dir.path().join("out.png");
    for role in ["heading", "subject", "body"] {
        let spec = write_spec(dir.path(), &spec_for(role));
        let report = json_stdout(&compose(&spec, &out, &["--dry-run", "--free-regions"]));
        let rects = region_rects(free_regions(&report));
        assert!(rects.len() > 1, "{role}: {rects:?}");
        let logo = [150.0, 150.0, 100.0, 100.0];
        assert!(
            rects.iter().all(|r| apart(as_f64(*r), logo, 8.0)),
            "{role}: {rects:?}"
        );
    }
    let spec = write_spec(dir.path(), &spec_for("decoration"));
    let report = json_stdout(&compose(&spec, &out, &["--dry-run", "--free-regions"]));
    assert_eq!(region_rects(free_regions(&report)), [[0, 0, 400, 400]]);
}

/// 既存の文字の `placed` と交わる候補は無い（§14.4 の 5）。`margin` も空ける。
#[test]
fn free_regions_keep_clear_of_placed_text() {
    let Some(family) = any_font() else {
        return;
    };
    let dir = TempDir::new().unwrap();
    let spec = write_spec(
        dir.path(),
        &format!(
            r##"{{"canvas":{{"width":640,"height":400,"background":"#ffffff"}},
                 "font":{{"family":"{family}"}},
                 "layers":[{{"id":"heading","type":"text","role":"heading",
                            "lines":["AB","BA"],"rect":[200,120,300,160],
                            "size":48,"color":"#000000"}},
                           {{"id":"note","type":"text","role":"decoration",
                            "lines":["A"],"rect":[20,320,100,60],
                            "size":24,"color":"#999999"}}]}}"##
        ),
    );
    let out = dir.path().join("out.png");
    let report = json_stdout(&compose(&spec, &out, &["--dry-run", "--free-regions"]));
    let free = free_regions(&report);
    let margin = free["margin"].as_f64().unwrap();
    let rects = region_rects(free);
    assert!(!rects.is_empty(), "{free}");
    for id in ["heading", "note"] {
        let placed = rect_of(&layer(&report, id)["placed"]);
        for r in &rects {
            // `placed` は外向きに画素の格子へ丸めて避ける。1/64px までの端数は
            // 見逃す（`measure::SUBPIXEL_TOLERANCE`。usvg の f32 の誤差である）
            let tol = 1.0 / 64.0;
            let (x0, y0) = ((placed[0] + tol).floor(), (placed[1] + tol).floor());
            let x1 = (placed[0] + placed[2] - tol).ceil();
            let y1 = (placed[1] + placed[3] - tol).ceil();
            let grown = [x0, y0, x1 - x0, y1 - y0];
            assert!(
                apart(as_f64(*r), grown, margin),
                "{r:?} が {id} の placed {placed:?} から {margin}px 離れていない"
            );
        }
    }
}

/// `min_size` の高さは `--profile note` で 98、無ければ短辺の 8%（切り上げ）
/// （§14.4 の 7）。幅は高さの 2 倍。
#[test]
fn the_minimum_size_follows_the_profile() {
    let dir = TempDir::new().unwrap();
    let spec = write_spec(
        dir.path(),
        r##"{"canvas":{"width":1280,"height":670,"background":"#ffffff"},
             "layers":[{"id":"band","type":"shape","rect":[0,0,1280,40],"color":"#000000"}]}"##,
    );
    let out = dir.path().join("out.png");
    let report = json_stdout(&compose(
        &spec,
        &out,
        &["--dry-run", "--free-regions", "--profile", "note"],
    ));
    assert_eq!(
        free_regions(&report)["min_size"],
        serde_json::json!([196, 98])
    );
    // --profile note は safe_area を埋める
    assert_eq!(
        free_regions(&report)["within"],
        serde_json::json!([0, 108, 1280, 454])
    );

    let report = json_stdout(&compose(&spec, &out, &["--dry-run", "--free-regions"]));
    // 670 × 0.08 = 53.6 → 54
    assert_eq!(
        free_regions(&report)["min_size"],
        serde_json::json!([108, 54])
    );
    assert_eq!(
        free_regions(&report)["within"],
        serde_json::json!([0, 0, 1280, 670])
    );
}

/// `--free-margin` は `--free-regions` が要り、負・有限でない値は断る
/// （§14.4 の 8）。渡した値は切り上げて効いた値を返す。
#[test]
fn the_free_margin_is_checked_and_reported() {
    let dir = TempDir::new().unwrap();
    let spec = write_spec(
        dir.path(),
        r##"{"canvas":{"width":400,"height":400,"background":"#ffffff"},
             "layers":[{"id":"band","type":"shape","rect":[0,0,400,40],"color":"#000000"}]}"##,
    );
    let out = dir.path().join("out.png");
    for extra in [
        &["--free-margin", "10"][..],
        &["--free-regions", "--free-margin=-1"],
        &["--free-regions", "--free-margin", "nan"],
        &["--free-regions", "--free-margin", "inf"],
        // キャンバスの辺の上限 16384 を超える値は、効いた値を返せないので断る
        &["--free-regions", "--free-margin", "16384.5"],
    ] {
        let mut args = vec!["--dry-run"];
        args.extend_from_slice(extra);
        assert_eq!(
            compose(&spec, &out, &args).status.code(),
            Some(2),
            "{extra:?}"
        );
    }
    let report = json_stdout(&compose(
        &spec,
        &out,
        &["--dry-run", "--free-regions", "--free-margin", "12.5"],
    ));
    assert_eq!(free_regions(&report)["margin"], 13);

    // キャンバスより広い距離は切り詰めずに空け、渡した値のまま返す。避ける相手が
    // 無ければ within そのものが残る
    let report = json_stdout(&compose(
        &spec,
        &out,
        &["--dry-run", "--free-regions", "--free-margin", "5000"],
    ));
    assert_eq!(free_regions(&report)["margin"], 5000);
    assert_eq!(region_rects(free_regions(&report)), [[0, 0, 400, 400]]);
}

/// 同じ入力から同じ候補が、同じ並びで返る（§14.4 の 9）。
#[test]
fn free_regions_are_deterministic() {
    let dir = TempDir::new().unwrap();
    write_png(dir.path(), "l.png", &l_shape());
    let spec = write_spec(
        dir.path(),
        r##"{"canvas":{"width":1280,"height":670,"background":"#ffffff"},
             "layers":[{"id":"a","type":"image","role":"subject",
                        "source":"l.png","rect":[700,200,300,300]},
                       {"id":"b","type":"image","role":"subject",
                        "source":"l.png","rect":[100,380,200,200]}]}"##,
    );
    let out = dir.path().join("out.png");
    let run = || {
        let report = json_stdout(&compose(
            &spec,
            &out,
            &["--dry-run", "--free-regions", "--profile", "note"],
        ));
        free_regions(&report).clone()
    };
    let first = run();
    assert!(region_rects(&first).len() > 1, "{first}");
    for _ in 0..3 {
        assert_eq!(run(), first);
    }
}

/// `kiri schema` が `--free-regions`・`--free-margin` と `free_regions` の形を
/// 配る（§14.4 の 12）。
#[test]
fn the_schema_publishes_free_regions() {
    let out = kiri()
        .args(["schema", "compose", "--json"])
        .output()
        .unwrap();
    let schema = json_stdout(&out);
    let paths: Vec<&str> = schema["fields"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| f["path"].as_str().unwrap())
        .collect();
    for key in [
        "free_regions.within",
        "free_regions.margin",
        "free_regions.min_size",
        "free_regions.cell",
        "free_regions.regions[].rect",
        "free_regions.regions[].area_ratio",
    ] {
        assert!(paths.contains(&key), "{key}: {paths:?}");
    }
    let options = schema["commands"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["name"] == "compose")
        .unwrap()["options"]
        .to_string();
    assert!(options.contains("--free-regions"), "{options}");
    assert!(options.contains("--free-margin"), "{options}");
}
