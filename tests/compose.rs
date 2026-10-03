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
    let mut db = resvg::usvg::fontdb::Database::new();
    db.load_system_fonts();
    let ids: Vec<_> = db
        .faces()
        .map(|f| (f.id, f.families.first().map(|(n, _)| n.clone())))
        .collect();
    ids.into_iter().find_map(|(id, family)| {
        let family = family?;
        let draws = db.with_face_data(id, |data, index| {
            ttf_parser::Face::parse(data, index)
                .map(|f| TEST_CHARS.iter().all(|&c| f.glyph_index(c).is_some()))
                .unwrap_or(false)
        })?;
        draws.then_some(family)
    })
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
