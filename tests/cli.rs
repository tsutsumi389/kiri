//! CLI の統合テスト。
//!
//! AI エージェントから使われる前提のため、「stdout が常に valid JSON であること」と
//! 「exit code が仕様どおりであること」を最重要の検証項目とする。

mod common;

use std::path::{Path, PathBuf};

use assert_cmd::Command;
use common::{
    ProductSpec, bleeding_product_scene, product_image, shadow_band_scene, split_background_scene,
    transparent_product, woven_background_image, woven_poisoned_scene, write_jpeg, write_png,
};
use serde_json::Value;
use tempfile::TempDir;

fn kiri() -> Command {
    Command::new(env!("CARGO_BIN_EXE_kiri"))
}

/// stdout 全体を JSON として解釈する。ログが混入していれば失敗する。
fn json_stdout(output: &std::process::Output) -> Value {
    let stdout = String::from_utf8_lossy(&output.stdout);
    serde_json::from_str(&stdout)
        .unwrap_or_else(|e| panic!("stdout が JSON として解釈できません: {e}\n---\n{stdout}"))
}

fn fixture_dir() -> TempDir {
    TempDir::new().unwrap()
}

/// 警告に指定の `code` が含まれるか。
///
/// 文言ではなく code で照合する。`warnings` は機械可読な契約であり、
/// テストが散文を掴んでいると、推敲のたびに壊れる（そして推敲を諦めさせる）。
fn has_warning(v: &Value, code: &str) -> bool {
    warning_codes(v).iter().any(|c| c == code)
}

fn warning_codes(v: &Value) -> Vec<String> {
    v["warnings"]
        .as_array()
        .unwrap_or_else(|| panic!("warnings が配列ではない: {v}"))
        .iter()
        .map(|w| {
            w["code"]
                .as_str()
                .unwrap_or_else(|| panic!("警告に code がない: {w}"))
                .to_string()
        })
        .collect()
}

// --- info ---

#[test]
fn info_reports_dimensions_and_background_for_a_studio_shot() {
    let dir = fixture_dir();
    let img = product_image(&ProductSpec {
        width: 160,
        height: 240,
        ..Default::default()
    });
    let input = write_jpeg(dir.path(), "product.jpg", &img);

    let out = kiri()
        .args(["info", input.to_str().unwrap(), "--json"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let v = json_stdout(&out);

    assert_eq!(v["width"], 160);
    assert_eq!(v["height"], 240);
    assert_eq!(v["format"], "jpeg");
    assert_eq!(v["has_alpha"], false);

    // 単色背景として検出されること
    let uniformity = v["background"]["uniformity"].as_f64().unwrap();
    assert!(
        uniformity >= 0.9,
        "uniformity={uniformity} (JPEG のノイズ込みでも単色と判定されるべき)"
    );

    let rgb: Vec<u64> = v["background"]["rgb"]
        .as_array()
        .unwrap()
        .iter()
        .map(|x| x.as_u64().unwrap())
        .collect();
    for c in rgb {
        assert!((240..=255).contains(&c), "背景色が白付近でない: {c}");
    }
    assert!(v["warnings"].as_array().unwrap().is_empty());
}

#[test]
fn info_warns_when_the_background_is_not_uniform() {
    let dir = fixture_dir();
    // 左半分が黒、右半分が白の背景。単色背景ではないので警告が出るべき
    let mut img = product_image(&ProductSpec {
        width: 120,
        height: 120,
        ..Default::default()
    });
    for y in 0..120 {
        for x in 0..60 {
            img.put_pixel(x, y, image::Rgba([20, 20, 22, 255]));
        }
    }
    let input = write_png(dir.path(), "split.png", &img);

    let out = kiri()
        .args(["info", input.to_str().unwrap(), "--json"])
        .output()
        .unwrap();
    assert!(out.status.success());
    let v = json_stdout(&out);

    assert!(v["background"]["uniformity"].as_f64().unwrap() < 0.9);
    let warnings = v["warnings"].as_array().unwrap();
    assert!(!warnings.is_empty(), "均一度が低いのに警告が出ていない");
    assert!(has_warning(&v, "LOW_UNIFORMITY"), "{warnings:?}");
    // 判断に使った数値も返す。エージェントが message をパースせずに検算できる
    assert!(warnings[0]["data"]["uniformity"].as_f64().unwrap() < 0.9);
}

/// 警告は機械可読でなければならない。
///
/// `error.rs` は当初から `code` を持たせているのに、警告だけが日本語の散文だった。
/// エージェントは文字列マッチで分岐するしかなく、文言を推敲するたびに壊れる。
/// **同じ道具の中で契約の形が違うほうが不自然である。**
#[test]
fn every_warning_carries_a_machine_readable_code() {
    let dir = fixture_dir();
    // 左半分だけ暗い背景。均一度が下がり、必ず 1 件以上の警告が出る
    let mut img = product_image(&ProductSpec {
        width: 120,
        height: 120,
        ..Default::default()
    });
    for y in 0..120 {
        for x in 0..60 {
            img.put_pixel(x, y, image::Rgba([20, 20, 22, 255]));
        }
    }
    let input = write_png(dir.path(), "split.png", &img);
    let output = dir.path().join("cut.png");

    let runs: Vec<Vec<String>> = vec![
        vec![
            "info".into(),
            input.to_str().unwrap().into(),
            "--json".into(),
        ],
        vec![
            "cutout".into(),
            input.to_str().unwrap().into(),
            "-o".into(),
            output.to_str().unwrap().into(),
            "--json".into(),
        ],
    ];

    for args in runs {
        let out = kiri().args(&args).output().unwrap();
        let v = json_stdout(&out);
        let warnings = v["warnings"].as_array().unwrap();
        assert!(!warnings.is_empty(), "{args:?} で警告が出ていない");

        for w in warnings {
            let obj = w
                .as_object()
                .unwrap_or_else(|| panic!("{args:?}: 警告が文字列のまま: {w}"));
            let code = obj["code"].as_str().unwrap();
            assert!(
                code.chars()
                    .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_'),
                "code は SCREAMING_SNAKE_CASE であるべき: {code}"
            );
            assert!(!obj["message"].as_str().unwrap().is_empty(), "{w}");
            // 空の hint / data はキーごと消える。null を出すと
            // 「中身の無い手がかりがある」と読まれる
            assert!(
                obj.get("hint").map(|h| h.is_string()).unwrap_or(true),
                "{w}"
            );
            assert!(
                obj.get("data").map(|d| d.is_object()).unwrap_or(true),
                "{w}"
            );
        }
    }
}

/// `subject` は `info` と `cutout` の両方に、検出できなくても必ず出る。
///
/// `separability` / `halo_ratio` と同じ規約。キーごと消すと「主体が無い」と
/// 「このコマンドは報告しない」を区別できず、エージェントは分岐を書けない。
#[test]
fn the_subject_key_is_always_present_in_both_commands() {
    let dir = fixture_dir();
    let img = product_image(&ProductSpec {
        width: 200,
        height: 200,
        ..Default::default()
    });
    let input = write_png(dir.path(), "in.png", &img);
    let output = dir.path().join("cut.png");

    let runs: Vec<Vec<String>> = vec![
        vec![
            "info".into(),
            input.to_str().unwrap().into(),
            "--json".into(),
        ],
        vec![
            "cutout".into(),
            input.to_str().unwrap().into(),
            "-o".into(),
            output.to_str().unwrap().into(),
            "--json".into(),
        ],
    ];

    for args in runs {
        let out = kiri().args(&args).output().unwrap();
        assert!(
            out.status.success(),
            "{args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let v = json_stdout(&out);
        let subject = v
            .get("subject")
            .unwrap_or_else(|| panic!("{args:?} に subject キーが無い: {v}"));
        // 合成の商品画像なので検出できるはず。ここが null なら仕組みが死んでいる
        let s = subject
            .as_object()
            .unwrap_or_else(|| panic!("{args:?}: 主体を見つけられていない: {subject}"));
        assert_eq!(s["confidence"], "high", "{subject}");

        // そのまま --bbox --normalized へ渡せる形であること
        let bbox = s["normalized_bbox"].as_array().unwrap();
        assert_eq!(bbox.len(), 4);
        for c in bbox {
            let c = c.as_f64().unwrap();
            assert!((0.0..=1.0).contains(&c), "正規化されていない: {c}");
        }
        assert!(bbox[0].as_f64().unwrap() < bbox[2].as_f64().unwrap());
        assert!(bbox[1].as_f64().unwrap() < bbox[3].as_f64().unwrap());
    }
}

/// テキストの「主体候補」行と hint は、同じ矩形を同じ丸めで出す。
///
/// **貼り付け可能と謳う行が、貼り付けたときに違う結果になってはいけない。**
/// 見栄えのために小数第 2 位へ落とすと、`bbox_argument` 自身のコメントどおり
/// 20MP で 28px 内側に入る。bbox の外は色によらず背景と確定されるので、
/// その差はそのまま商品の欠けになる。
#[test]
fn the_subject_line_and_the_hint_round_the_box_the_same_way() {
    let dir = fixture_dir();
    let input = write_png(dir.path(), "split.png", &split_background_scene(300, 300));

    let v = json_stdout(
        &kiri()
            .args(["info", input.to_str().unwrap(), "--json"])
            .output()
            .unwrap(),
    );
    let expected = v["subject"]["normalized_bbox"]
        .as_array()
        .unwrap_or_else(|| panic!("主体が見つかっていない: {v}"))
        .iter()
        .map(|c| c.as_f64().unwrap().to_string())
        .collect::<Vec<_>>()
        .join(",");

    // hint 側（既に `bbox_argument` を使っている）
    let hint = v["warnings"]
        .as_array()
        .unwrap()
        .iter()
        .find(|w| w["code"] == "LOW_UNIFORMITY")
        .unwrap_or_else(|| panic!("{v}"))["hint"]
        .as_str()
        .unwrap()
        .to_string();
    // **この行が先に要る。** hint に `--bbox` が載るのは信頼度が high のときだけで、
    // 主体の規則を触ると、丸めとは無関係な理由でこのテストが落ちる。
    // 「丸めが違う」という失敗文言のまま、原因が別の場所にある状態を避ける
    assert!(
        hint.contains("--bbox"),
        "主体が high でなくなっている。丸めではなく主体の判定を疑うこと: {hint}\n{v}"
    );
    assert!(
        hint.contains(&format!("--bbox {expected} ")),
        "hint が JSON と違う丸めで出ている: {hint}"
    );

    // テキスト出力側
    let out = kiri()
        .args(["info", input.to_str().unwrap()])
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&out.stdout).to_string();
    assert!(
        text.contains(&format!("主体候補  {expected}  ")),
        "テキストが JSON と違う丸めで出ている: 期待 {expected}\n---\n{text}"
    );
}

/// 主体を検出できなければ `null` を返す。0 や空配列で埋めない。
#[test]
fn a_background_only_image_reports_a_null_subject() {
    let dir = fixture_dir();
    let img = image::RgbaImage::from_pixel(120, 120, image::Rgba([250, 250, 248, 255]));
    let input = write_png(dir.path(), "empty.png", &img);

    let out = kiri()
        .args(["info", input.to_str().unwrap(), "--json"])
        .output()
        .unwrap();
    let v = json_stdout(&out);
    assert!(
        v["subject"].is_null(),
        "主体が無いなら null: {}",
        v["subject"]
    );
}

/// 主体の検出が切り抜き本体へ影響していないこと。
///
/// `subject` は報告と警告のためだけの情報で、マスクの生成には一切関与しない。
/// **ここが崩れると「診断を足したら結果が変わった」という最悪の壊れ方をする。**
///
/// 同じバイナリを二度走らせて比べるだけでは足りない。**主体検出が本体へ副作用を
/// 持ち込めば、二度とも同じように壊れて通り続ける。** 二度走らせるのは決定性の
/// 検査であって、副作用が無いことの検査ではない。だから期待値を数値で固定する。
///
/// 数値は `product_image(200x200, 既定)` の実測。ここが動いたら、切り抜きの
/// 挙動そのものが変わったということなので、**期待値を書き換える前に何が
/// 変わったのかを説明できること。**
#[test]
fn detecting_the_subject_does_not_change_the_cutout() {
    let dir = fixture_dir();
    let img = product_image(&ProductSpec {
        width: 200,
        height: 200,
        ..Default::default()
    });
    let input = write_png(dir.path(), "in.png", &img);

    let run = |name: &str| -> (Vec<u8>, Value) {
        let output = dir.path().join(name);
        let out = kiri()
            .args([
                "cutout",
                input.to_str().unwrap(),
                "-o",
                output.to_str().unwrap(),
                "--json",
            ])
            .output()
            .unwrap();
        assert!(out.status.success());
        (std::fs::read(&output).unwrap(), json_stdout(&out))
    };
    let (a, va) = run("a.png");
    let (b, vb) = run("b.png");
    assert_eq!(a, b, "同じ入力で出力が揺れている");
    assert_eq!(va["mask"], vb["mask"]);

    let mask = &va["mask"];
    assert_eq!(
        mask["bbox"],
        serde_json::json!([44, 32, 155, 167]),
        "{mask}"
    );
    assert_eq!(mask["touches_edge"], false, "{mask}");
    // 比率と色差は浮動小数のまま比べる。値が動いたら切り抜きが変わっている
    assert_eq!(mask["foreground_ratio"].as_f64().unwrap(), 0.3753, "{mask}");
    assert_eq!(mask["separability"].as_f64().unwrap(), 77.6231, "{mask}");
    assert_eq!(mask["halo_ratio"].as_f64().unwrap(), 0.0, "{mask}");
}

/// 均一な背景では `LOW_UNIFORMITY` を出さない。偽陽性の回帰防止。
///
/// 警告が常に出る道具は、警告が無いのと同じである。
#[test]
fn a_uniform_background_produces_no_uniformity_warning() {
    let dir = fixture_dir();
    let img = product_image(&ProductSpec {
        width: 160,
        height: 160,
        ..Default::default()
    });
    let input = write_png(dir.path(), "studio.png", &img);

    let out = kiri()
        .args(["info", input.to_str().unwrap(), "--json"])
        .output()
        .unwrap();
    let v = json_stdout(&out);
    assert!(
        !has_warning(&v, "LOW_UNIFORMITY"),
        "単色背景で誤警告している: {:?}",
        warning_codes(&v)
    );
}

#[test]
fn info_detects_existing_transparency() {
    let dir = fixture_dir();
    let input = write_png(dir.path(), "cut.png", &transparent_product(120, 120));

    let out = kiri()
        .args(["info", input.to_str().unwrap(), "--json"])
        .output()
        .unwrap();
    let v = json_stdout(&out);
    assert_eq!(v["has_alpha"], true);
}

// --- 色空間 ---

/// ICC を持たない画像は sRGB として報告し、何も変換しない。
#[test]
fn info_reports_the_color_space_and_whether_it_was_converted() {
    let dir = fixture_dir();
    let img = product_image(&ProductSpec {
        width: 60,
        height: 60,
        ..Default::default()
    });
    let input = write_png(dir.path(), "in.png", &img);

    let out = kiri()
        .args(["info", input.to_str().unwrap(), "--json"])
        .output()
        .unwrap();
    let v = json_stdout(&out);
    assert_eq!(v["color_space"], "sRGB");
    assert_eq!(v["color_converted"], false);
    assert_eq!(v["icc_profile"], false);
    assert!(v["warnings"].as_array().unwrap().is_empty());
}

/// 色空間はどのコマンドの結果からも読めること。
///
/// エージェントは cutout の JSON しか見ないことがある。そこに色の扱いが
/// 出ていなければ、色がずれていても気づく手立てが無い。
#[test]
fn every_command_reports_the_color_space() {
    let dir = fixture_dir();
    let img = product_image(&ProductSpec {
        width: 80,
        height: 80,
        ..Default::default()
    });
    let input = write_png(dir.path(), "in.png", &img);
    let path = |name: &str| dir.path().join(name).to_str().unwrap().to_string();

    let runs: Vec<Vec<String>> = vec![
        vec![
            "convert".into(),
            input.to_str().unwrap().into(),
            "-o".into(),
            path("c.png"),
        ],
        vec![
            "resize".into(),
            input.to_str().unwrap().into(),
            "--width".into(),
            "40".into(),
            "-o".into(),
            path("r.png"),
        ],
        vec![
            "cutout".into(),
            input.to_str().unwrap().into(),
            "-o".into(),
            path("k.png"),
        ],
    ];

    for args in runs {
        let out = kiri().args(&args).arg("--json").output().unwrap();
        assert!(
            out.status.success(),
            "{args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let v = json_stdout(&out);
        assert_eq!(v["color_space"], "sRGB", "{args:?}");
        assert_eq!(v["color_converted"], false, "{args:?}");
        assert!(
            v.get("color_profile").is_none(),
            "ICC が無いのに名乗りが出ている: {args:?}"
        );
    }
}

/// `--no-color-convert` はどのコマンドでも受け付けること。
#[test]
fn no_color_convert_is_accepted_by_every_command() {
    let dir = fixture_dir();
    let img = product_image(&ProductSpec {
        width: 60,
        height: 60,
        ..Default::default()
    });
    let input = write_png(dir.path(), "in.png", &img);
    let path = |name: &str| dir.path().join(name).to_str().unwrap().to_string();

    let runs: Vec<Vec<String>> = vec![
        vec!["info".into(), input.to_str().unwrap().into()],
        vec![
            "convert".into(),
            input.to_str().unwrap().into(),
            "-o".into(),
            path("c.png"),
        ],
        vec![
            "resize".into(),
            input.to_str().unwrap().into(),
            "--width".into(),
            "30".into(),
            "-o".into(),
            path("r.png"),
        ],
        vec![
            "cutout".into(),
            input.to_str().unwrap().into(),
            "-o".into(),
            path("k.png"),
        ],
    ];

    for args in runs {
        let out = kiri()
            .args(&args)
            .args(["--no-color-convert", "--json"])
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        json_stdout(&out);
    }
}

// --- convert ---

#[test]
fn convert_writes_each_supported_format() {
    let dir = fixture_dir();
    let img = product_image(&ProductSpec {
        width: 120,
        height: 120,
        ..Default::default()
    });
    let input = write_png(dir.path(), "in.png", &img);

    for (name, format) in [
        ("out.avif", "avif"),
        ("out.png", "png"),
        ("out.jpg", "jpeg"),
    ] {
        let output = dir.path().join(name);
        let out = kiri()
            .args([
                "convert",
                input.to_str().unwrap(),
                "-o",
                output.to_str().unwrap(),
                "--json",
            ])
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{name}: {}",
            String::from_utf8_lossy(&out.stderr)
        );

        let v = json_stdout(&out);
        assert_eq!(v["outputs"][0]["format"], format);
        assert_eq!(v["outputs"][0]["width"], 120);
        assert!(v["outputs"][0]["bytes"].as_u64().unwrap() > 0);
        assert!(output.exists(), "{name} が作られていない");
    }
}

#[test]
fn convert_refuses_to_overwrite_without_force() {
    let dir = fixture_dir();
    let img = product_image(&ProductSpec {
        width: 60,
        height: 60,
        ..Default::default()
    });
    let input = write_png(dir.path(), "in.png", &img);
    let output = dir.path().join("out.png");
    std::fs::write(&output, b"existing").unwrap();

    let out = kiri()
        .args([
            "convert",
            input.to_str().unwrap(),
            "-o",
            output.to_str().unwrap(),
            "--json",
        ])
        .output()
        .unwrap();

    assert_eq!(out.status.code(), Some(2));
    let v = json_stdout(&out);
    assert_eq!(v["error"]["code"], "OUTPUT_EXISTS");
    assert!(v["error"]["hint"].as_str().unwrap().contains("--force"));
    // 既存ファイルが壊されていないこと
    assert_eq!(std::fs::read(&output).unwrap(), b"existing");
}

#[test]
fn convert_overwrites_with_force() {
    let dir = fixture_dir();
    let img = product_image(&ProductSpec {
        width: 60,
        height: 60,
        ..Default::default()
    });
    let input = write_png(dir.path(), "in.png", &img);
    let output = dir.path().join("out.png");
    std::fs::write(&output, b"existing").unwrap();

    let out = kiri()
        .args([
            "convert",
            input.to_str().unwrap(),
            "-o",
            output.to_str().unwrap(),
            "--force",
            "--json",
        ])
        .output()
        .unwrap();

    assert!(out.status.success());
    assert_ne!(std::fs::read(&output).unwrap(), b"existing");
}

#[test]
fn converting_transparency_to_jpeg_warns_about_compositing() {
    let dir = fixture_dir();
    let input = write_png(dir.path(), "cut.png", &transparent_product(100, 100));
    let output = dir.path().join("flat.jpg");

    let out = kiri()
        .args([
            "convert",
            input.to_str().unwrap(),
            "-o",
            output.to_str().unwrap(),
            "--json",
        ])
        .output()
        .unwrap();

    assert!(out.status.success());
    let v = json_stdout(&out);
    assert!(
        has_warning(&v, "ALPHA_FLATTENED"),
        "透過が失われる旨の警告がない: {:?}",
        warning_codes(&v)
    );
}

#[test]
fn convert_is_deterministic() {
    let dir = fixture_dir();
    let img = product_image(&ProductSpec {
        width: 100,
        height: 100,
        ..Default::default()
    });
    let input = write_png(dir.path(), "in.png", &img);

    let encode = |name: &str| -> Vec<u8> {
        let output = dir.path().join(name);
        let out = kiri()
            .args([
                "convert",
                input.to_str().unwrap(),
                "-o",
                output.to_str().unwrap(),
                "--json",
            ])
            .output()
            .unwrap();
        assert!(out.status.success());
        std::fs::read(&output).unwrap()
    };

    assert_eq!(
        encode("a.avif"),
        encode("b.avif"),
        "同じ入力から同じ AVIF が出ていない"
    );
}

// --- エラー処理 ---

#[test]
fn missing_input_exits_with_input_error() {
    let out = kiri()
        .args(["info", "/nonexistent/nope.jpg", "--json"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(3));
    let v = json_stdout(&out);
    assert_eq!(v["error"]["code"], "INPUT_UNREADABLE");
}

#[test]
fn unsupported_input_format_exits_with_input_error() {
    let dir = fixture_dir();
    let input = dir.path().join("notanimage.jpg");
    std::fs::write(&input, b"this is definitely not a jpeg").unwrap();

    let out = kiri()
        .args(["info", input.to_str().unwrap(), "--json"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(3));
    assert!(
        json_stdout(&out)["error"]["code"]
            .as_str()
            .unwrap()
            .starts_with("UNSUPPORTED")
    );
}

/// HEIC は読めないが、読めないことと次の一手はきちんと返す。
///
/// 実ファイルはリポジトリに置かない（数 MB になる上、pure Rust では
/// デコードできないのでフィクスチャとしての用が無い）。判別は先頭の
/// `ftyp` ボックスだけで決まるので、そこだけ持つダミーで固定できる。
#[test]
fn a_heic_input_is_refused_with_a_conversion_hint() {
    let dir = fixture_dir();
    let input = dir.path().join("IMG_0251.HEIC");
    let mut heic = 24u32.to_be_bytes().to_vec();
    heic.extend_from_slice(b"ftypheic");
    heic.extend_from_slice(&0u32.to_be_bytes());
    heic.extend_from_slice(b"mif1heic");
    std::fs::write(&input, heic).unwrap();

    let out = kiri()
        .args(["info", input.to_str().unwrap(), "--json"])
        .output()
        .unwrap();

    assert_eq!(out.status.code(), Some(3));
    let v = json_stdout(&out);
    assert_eq!(v["error"]["code"], "UNSUPPORTED_FORMAT");
    let message = v["error"]["message"].as_str().unwrap();
    assert!(
        message.contains("HEIC"),
        "何が未対応かを言うべき: {message}"
    );
    let hint = v["error"]["hint"].as_str().unwrap();
    assert!(hint.contains("sips"), "macOS での手順が要る: {hint}");
    assert!(
        hint.contains("magick") || hint.contains("heif-convert"),
        "macOS 以外での手順が要る: {hint}"
    );
}

#[test]
fn unknown_output_extension_exits_with_argument_error() {
    let dir = fixture_dir();
    let img = product_image(&ProductSpec {
        width: 60,
        height: 60,
        ..Default::default()
    });
    let input = write_png(dir.path(), "in.png", &img);
    let output = dir.path().join("out.webp");

    let out = kiri()
        .args([
            "convert",
            input.to_str().unwrap(),
            "-o",
            output.to_str().unwrap(),
            "--json",
        ])
        .output()
        .unwrap();

    assert_eq!(out.status.code(), Some(2));
    let v = json_stdout(&out);
    assert_eq!(v["error"]["code"], "UNKNOWN_OUTPUT_FORMAT");
}

#[test]
fn invalid_quality_exits_with_argument_error() {
    let dir = fixture_dir();
    let img = product_image(&ProductSpec {
        width: 60,
        height: 60,
        ..Default::default()
    });
    let input = write_png(dir.path(), "in.png", &img);
    let output = dir.path().join("out.avif");

    let out = kiri()
        .args([
            "convert",
            input.to_str().unwrap(),
            "-o",
            output.to_str().unwrap(),
            "--quality",
            "150",
            "--json",
        ])
        .output()
        .unwrap();

    assert_eq!(out.status.code(), Some(2));
    assert_eq!(json_stdout(&out)["error"]["code"], "INVALID_QUALITY");
}

// --- 出力規約 ---

#[test]
fn logs_never_contaminate_stdout_in_json_mode() {
    let dir = fixture_dir();
    // 警告が必ず出るケース（非単色背景）で stdout の純度を確かめる
    let mut img = product_image(&ProductSpec {
        width: 100,
        height: 100,
        ..Default::default()
    });
    for y in 0..100 {
        for x in 0..50 {
            img.put_pixel(x, y, image::Rgba([20, 20, 22, 255]));
        }
    }
    let input = write_png(dir.path(), "split.png", &img);

    let out = kiri()
        .args(["info", input.to_str().unwrap(), "--json"])
        .output()
        .unwrap();
    let v = json_stdout(&out);
    assert!(!v["warnings"].as_array().unwrap().is_empty());
    // 警告は JSON の中にだけ現れ、stdout に生テキストとしては出ない
    assert!(
        String::from_utf8_lossy(&out.stdout)
            .trim_start()
            .starts_with('{')
    );
}

#[test]
fn human_output_goes_to_stdout_and_warnings_to_stderr() {
    let dir = fixture_dir();
    let mut img = product_image(&ProductSpec {
        width: 100,
        height: 100,
        ..Default::default()
    });
    for y in 0..100 {
        for x in 0..50 {
            img.put_pixel(x, y, image::Rgba([20, 20, 22, 255]));
        }
    }
    let input = write_png(dir.path(), "split.png", &img);

    let out = kiri()
        .args(["info", input.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stdout.contains("寸法"), "stdout に要約がない: {stdout}");
    assert!(stderr.contains("警告:"), "stderr に警告がない: {stderr}");
}

#[test]
fn avif_is_smaller_than_png_for_product_photos() {
    let dir = fixture_dir();
    let img = product_image(&ProductSpec {
        width: 300,
        height: 300,
        ..Default::default()
    });
    let input = write_png(dir.path(), "in.png", &img);

    let size = |name: &str| -> u64 {
        let output = dir.path().join(name);
        let out = kiri()
            .args([
                "convert",
                input.to_str().unwrap(),
                "-o",
                output.to_str().unwrap(),
                "--json",
            ])
            .output()
            .unwrap();
        assert!(out.status.success());
        json_stdout(&out)["outputs"][0]["bytes"].as_u64().unwrap()
    };

    let avif = size("out.avif");
    let png = size("out.png");
    assert!(avif < png, "AVIF({avif}) が PNG({png}) より小さくない");
}

#[test]
fn the_output_path_parent_is_created_if_missing() {
    let dir = fixture_dir();
    let img = product_image(&ProductSpec {
        width: 60,
        height: 60,
        ..Default::default()
    });
    let input = write_png(dir.path(), "in.png", &img);
    let output = dir.path().join("nested").join("deep").join("out.avif");

    let out = kiri()
        .args([
            "convert",
            input.to_str().unwrap(),
            "-o",
            output.to_str().unwrap(),
            "--json",
        ])
        .output()
        .unwrap();

    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(Path::new(&output).exists());
}

// --- resize ---

#[test]
fn resize_by_width_preserves_the_aspect_ratio() {
    let dir = fixture_dir();
    let img = product_image(&ProductSpec {
        width: 400,
        height: 500,
        ..Default::default()
    });
    let input = write_png(dir.path(), "in.png", &img);
    let output = dir.path().join("out.png");

    let out = kiri()
        .args([
            "resize",
            input.to_str().unwrap(),
            "-o",
            output.to_str().unwrap(),
            "--width",
            "200",
            "--json",
        ])
        .output()
        .unwrap();

    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let v = json_stdout(&out);
    assert_eq!(v["source"]["width"], 400);
    assert_eq!(v["outputs"][0]["width"], 200);
    assert_eq!(v["outputs"][0]["height"], 250);
}

#[test]
fn resize_fit_modes_produce_the_documented_shapes() {
    let dir = fixture_dir();
    let img = product_image(&ProductSpec {
        width: 400,
        height: 500,
        ..Default::default()
    });
    let input = write_png(dir.path(), "in.png", &img);

    // 400x500 を 200x200 の枠へ
    let cases = [
        ("contain", 160, 200), // 枠に収まる
        ("cover", 200, 200),   // 枠ちょうど（はみ出しは切る）
        ("exact", 200, 200),   // 枠ちょうど（比率無視）
    ];
    for (fit, w, h) in cases {
        let output = dir.path().join(format!("{fit}.png"));
        let out = kiri()
            .args([
                "resize",
                input.to_str().unwrap(),
                "-o",
                output.to_str().unwrap(),
                "--width",
                "200",
                "--height",
                "200",
                "--fit",
                fit,
                "--json",
            ])
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{fit}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let v = json_stdout(&out);
        assert_eq!(v["outputs"][0]["width"], w, "fit={fit}");
        assert_eq!(v["outputs"][0]["height"], h, "fit={fit}");
    }
}

#[test]
fn resize_rejects_upscaling_by_default() {
    let dir = fixture_dir();
    let img = product_image(&ProductSpec {
        width: 200,
        height: 200,
        ..Default::default()
    });
    let input = write_png(dir.path(), "in.png", &img);
    let output = dir.path().join("out.png");

    let out = kiri()
        .args([
            "resize",
            input.to_str().unwrap(),
            "-o",
            output.to_str().unwrap(),
            "--width",
            "800",
            "--json",
        ])
        .output()
        .unwrap();

    assert_eq!(out.status.code(), Some(2));
    let v = json_stdout(&out);
    assert_eq!(v["error"]["code"], "UPSCALE_NOT_ALLOWED");
    assert!(!output.exists(), "拒否したのにファイルを作っている");
}

#[test]
fn resize_upscales_with_an_explicit_flag_and_warns() {
    let dir = fixture_dir();
    let img = product_image(&ProductSpec {
        width: 200,
        height: 200,
        ..Default::default()
    });
    let input = write_png(dir.path(), "in.png", &img);
    let output = dir.path().join("out.png");

    let out = kiri()
        .args([
            "resize",
            input.to_str().unwrap(),
            "-o",
            output.to_str().unwrap(),
            "--width",
            "400",
            "--allow-upscale",
            "--json",
        ])
        .output()
        .unwrap();

    assert!(out.status.success());
    let v = json_stdout(&out);
    assert_eq!(v["outputs"][0]["width"], 400);
    assert!(
        has_warning(&v, "UPSCALED"),
        "拡大した旨の警告がない: {:?}",
        warning_codes(&v)
    );
}

#[test]
fn resize_without_any_dimension_is_an_argument_error() {
    let dir = fixture_dir();
    let img = product_image(&ProductSpec {
        width: 100,
        height: 100,
        ..Default::default()
    });
    let input = write_png(dir.path(), "in.png", &img);
    let output = dir.path().join("out.png");

    let out = kiri()
        .args([
            "resize",
            input.to_str().unwrap(),
            "-o",
            output.to_str().unwrap(),
            "--json",
        ])
        .output()
        .unwrap();

    assert_eq!(out.status.code(), Some(2));
    assert_eq!(json_stdout(&out)["error"]["code"], "MISSING_DIMENSION");
}

#[test]
fn resize_can_change_the_format_at_the_same_time() {
    let dir = fixture_dir();
    let img = product_image(&ProductSpec {
        width: 400,
        height: 400,
        ..Default::default()
    });
    let input = write_png(dir.path(), "in.png", &img);
    let output = dir.path().join("out.avif");

    let out = kiri()
        .args([
            "resize",
            input.to_str().unwrap(),
            "-o",
            output.to_str().unwrap(),
            "--width",
            "200",
            "--json",
        ])
        .output()
        .unwrap();

    assert!(out.status.success());
    let v = json_stdout(&out);
    assert_eq!(v["outputs"][0]["format"], "avif");
    assert_eq!(v["outputs"][0]["width"], 200);
}

#[test]
fn resize_preserves_transparency() {
    let dir = fixture_dir();
    let input = write_png(dir.path(), "cut.png", &transparent_product(200, 200));
    let output = dir.path().join("small.png");

    let out = kiri()
        .args([
            "resize",
            input.to_str().unwrap(),
            "-o",
            output.to_str().unwrap(),
            "--width",
            "100",
            "--json",
        ])
        .output()
        .unwrap();
    assert!(out.status.success());

    // 出力を読み直して、透過が残っていることを確かめる
    let check = kiri()
        .args(["info", output.to_str().unwrap(), "--json"])
        .output()
        .unwrap();
    assert_eq!(json_stdout(&check)["has_alpha"], true);
}

#[test]
fn resize_respects_the_overwrite_guard() {
    let dir = fixture_dir();
    let img = product_image(&ProductSpec {
        width: 100,
        height: 100,
        ..Default::default()
    });
    let input = write_png(dir.path(), "in.png", &img);
    let output = dir.path().join("out.png");
    std::fs::write(&output, b"existing").unwrap();

    let out = kiri()
        .args([
            "resize",
            input.to_str().unwrap(),
            "-o",
            output.to_str().unwrap(),
            "--width",
            "50",
            "--json",
        ])
        .output()
        .unwrap();

    assert_eq!(out.status.code(), Some(2));
    assert_eq!(json_stdout(&out)["error"]["code"], "OUTPUT_EXISTS");
    assert_eq!(std::fs::read(&output).unwrap(), b"existing");
}

// --- rotate ---

/// 回転結果の JSON を取る。角度以外は常に同じ呼び方をする。
fn rotate_json(input: &Path, output: &Path, angle: &str, extra: &[&str]) -> Value {
    let mut args: Vec<String> = vec![
        "rotate".into(),
        input.to_str().unwrap().into(),
        "-o".into(),
        output.to_str().unwrap().into(),
        "--angle".into(),
        angle.into(),
        "--json".into(),
    ];
    args.extend(extra.iter().map(|s| (*s).to_string()));

    let out = kiri().args(&args).output().unwrap();
    assert!(
        out.status.success(),
        "angle={angle}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    json_stdout(&out)
}

fn rotate_fixture(dir: &TempDir) -> PathBuf {
    let img = product_image(&ProductSpec {
        width: 400,
        height: 500,
        ..Default::default()
    });
    write_png(dir.path(), "in.png", &img)
}

#[test]
fn rotate_by_a_quarter_turn_swaps_the_dimensions_without_resampling() {
    let dir = fixture_dir();
    let input = rotate_fixture(&dir);

    let v = rotate_json(&input, &dir.path().join("out.png"), "90", &[]);
    assert_eq!(v["source"]["width"], 400);
    assert_eq!(v["outputs"][0]["width"], 500);
    assert_eq!(v["outputs"][0]["height"], 400);
    assert_eq!(v["rotate"]["angle"], 90.0);
    assert_eq!(
        v["rotate"]["resampled"], false,
        "90 度単位は補間せずに回すこと"
    );
}

#[test]
fn rotate_normalizes_a_negative_angle_to_a_clockwise_one() {
    let dir = fixture_dir();
    let input = rotate_fixture(&dir);

    // 反時計回りに 90 は、時計回りに 270 と同じ操作
    let v = rotate_json(&input, &dir.path().join("out.png"), "-90", &[]);
    assert_eq!(v["rotate"]["angle"], 270.0);
    assert_eq!(v["rotate"]["resampled"], false);
    assert_eq!(v["outputs"][0]["width"], 500);
}

#[test]
fn rotate_by_a_full_turn_returns_the_original_bytes() {
    let dir = fixture_dir();
    let input = rotate_fixture(&dir);
    let output = dir.path().join("out.png");

    let v = rotate_json(&input, &output, "360", &[]);
    assert_eq!(v["rotate"]["angle"], 0.0);
    assert_eq!(
        std::fs::read(&input).unwrap(),
        std::fs::read(&output).unwrap(),
        "1 周は何もしないのと同じでなければならない"
    );
}

#[test]
fn rotate_by_an_arbitrary_angle_expands_to_hold_the_corners() {
    let dir = fixture_dir();
    let input = rotate_fixture(&dir);

    let v = rotate_json(&input, &dir.path().join("out.png"), "45", &[]);
    assert_eq!(v["rotate"]["resampled"], true);
    // 400x500 を 45 度回した外接矩形は一辺 (400+500)/sqrt(2) = 636.4
    assert_eq!(v["outputs"][0]["width"], 637);
    assert_eq!(v["outputs"][0]["height"], 637);
}

#[test]
fn rotate_leaves_the_new_corners_transparent() {
    let dir = fixture_dir();
    let input = rotate_fixture(&dir);
    let output = dir.path().join("out.png");
    rotate_json(&input, &output, "30", &[]);

    let img = image::open(&output).unwrap().to_rgba8();
    for (x, y) in [
        (0, 0),
        (img.width() - 1, 0),
        (0, img.height() - 1),
        (img.width() - 1, img.height() - 1),
    ] {
        assert_eq!(
            img.get_pixel(x, y)[3],
            0,
            "({x},{y}) は回転で生じた余白なので透過であるべき"
        );
    }
}

#[test]
fn rotate_into_a_format_without_alpha_reports_the_flattening() {
    let dir = fixture_dir();
    let input = rotate_fixture(&dir);

    // 余白は透過で作るので、JPEG へ出すなら塗り潰すしかない。
    // 黙って塗ると「背景色の枠が付いた」に見えるため、必ず知らせる
    let v = rotate_json(&input, &dir.path().join("out.jpg"), "30", &[]);
    assert!(
        has_warning(&v, "ALPHA_FLATTENED"),
        "警告: {:?}",
        warning_codes(&v)
    );
}

#[test]
fn rotate_rejects_an_angle_that_is_not_a_number() {
    let dir = fixture_dir();
    let input = rotate_fixture(&dir);

    for angle in ["nan", "inf", "sideways"] {
        let out = kiri()
            .args([
                "rotate",
                input.to_str().unwrap(),
                "-o",
                dir.path().join("out.png").to_str().unwrap(),
                "--angle",
                angle,
            ])
            .output()
            .unwrap();
        assert_eq!(out.status.code(), Some(2), "angle={angle} は拒否すること");
    }
}

#[test]
fn rotate_respects_the_overwrite_guard() {
    let dir = fixture_dir();
    let input = rotate_fixture(&dir);
    let output = dir.path().join("out.png");
    std::fs::write(&output, b"existing").unwrap();

    let out = kiri()
        .args([
            "rotate",
            input.to_str().unwrap(),
            "-o",
            output.to_str().unwrap(),
            "--angle",
            "90",
            "--json",
        ])
        .output()
        .unwrap();

    assert_eq!(out.status.code(), Some(2));
    assert_eq!(json_stdout(&out)["error"]["code"], "OUTPUT_EXISTS");
    assert_eq!(std::fs::read(&output).unwrap(), b"existing");
}

#[test]
fn other_commands_do_not_report_a_rotation() {
    // rotate だけが持つキーであることを固定する。convert / resize が
    // `"rotate": null` を返し始めると、「回さなかった」と「回せない」が混ざる
    let dir = fixture_dir();
    let input = rotate_fixture(&dir);

    let out = kiri()
        .args([
            "convert",
            input.to_str().unwrap(),
            "-o",
            dir.path().join("out.png").to_str().unwrap(),
            "--json",
        ])
        .output()
        .unwrap();
    assert!(json_stdout(&out).get("rotate").is_none());
}

// --- cutout ---

use common::light_product_image;

#[test]
fn cutout_makes_the_background_transparent() {
    let dir = fixture_dir();
    let img = product_image(&ProductSpec {
        width: 200,
        height: 200,
        ..Default::default()
    });
    let input = write_jpeg(dir.path(), "in.jpg", &img);
    let output = dir.path().join("cut.png");

    let out = kiri()
        .args([
            "cutout",
            input.to_str().unwrap(),
            "-o",
            output.to_str().unwrap(),
            "--json",
        ])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );

    let v = json_stdout(&out);
    let ratio = v["mask"]["foreground_ratio"].as_f64().unwrap();
    assert!((0.05..0.6).contains(&ratio), "前景比率が不自然: {ratio}");
    assert_eq!(v["mask"]["touches_edge"], false);
    assert!(v["mask"]["bbox"].is_array());

    // 出力を読み直して透過が生まれていることを確認
    let check = kiri()
        .args(["info", output.to_str().unwrap(), "--json"])
        .output()
        .unwrap();
    assert_eq!(json_stdout(&check)["has_alpha"], true);
}

#[test]
fn cutout_reports_the_background_it_used() {
    let dir = fixture_dir();
    let img = product_image(&ProductSpec {
        width: 150,
        height: 150,
        ..Default::default()
    });
    let input = write_png(dir.path(), "in.png", &img);
    let output = dir.path().join("cut.png");

    let out = kiri()
        .args([
            "cutout",
            input.to_str().unwrap(),
            "-o",
            output.to_str().unwrap(),
            "--tolerance",
            "9",
            "--json",
        ])
        .output()
        .unwrap();

    let v = json_stdout(&out);
    assert_eq!(v["settings"]["tolerance"], 9.0);
    let rgb: Vec<u64> = v["background"]["rgb"]
        .as_array()
        .unwrap()
        .iter()
        .map(|x| x.as_u64().unwrap())
        .collect();
    for c in rgb {
        assert!((240..=255).contains(&c), "背景色が白付近でない: {c}");
    }
}

/// 設計の核心。淡い商品が背景ごと消えないこと。
///
/// かつては勾配の堤防だけがこれを支えており、`--edge-threshold 0` にすると
/// 商品が消えることをこのテストで固定していた。段差の検査（第2段のフィル）が
/// 入ってからは、堤防を切っても輪郭で止まる。輪郭のコントラストは ΔE 4.9 あり、
/// 1px あたりの色差 2.2 という基準を超えているためである。
/// 「堤防が無いと壊れる」という期待は、堤防以外に守りが無かった時代の仕様なので
/// もう固定しない。代わりに「どちらの設定でも商品が残る」ことを固定する。
#[test]
fn a_light_product_on_a_light_background_survives() {
    let dir = fixture_dir();
    let input = write_png(dir.path(), "light.png", &light_product_image(200, 200));

    let ratio = |extra: &[&str]| -> f64 {
        let output = dir.path().join(format!("out{}.png", extra.join("")));
        let mut args = vec![
            "cutout",
            input.to_str().unwrap(),
            "-o",
            output.to_str().unwrap(),
            "--json",
        ];
        args.extend_from_slice(extra);
        let out = kiri().args(&args).output().unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        json_stdout(&out)["mask"]["foreground_ratio"]
            .as_f64()
            .unwrap()
    };

    // 商品は画像の 1/4 〜 3/4 を占めるので、前景比率は 0.25 前後になるはず
    let with_dam = ratio(&[]);
    assert!(
        (0.20..0.32).contains(&with_dam),
        "淡い商品が背景ごと消えている: {with_dam}"
    );

    let without_dam = ratio(&["--edge-threshold", "0"]);
    assert!(
        (0.20..0.32).contains(&without_dam),
        "堤防を切ると段差の検査だけになるが、それでも商品は残るべき: {without_dam}"
    );

    // 段差の検査まで切ると、淡い商品は色だけで判定されて消える。
    // 「連結性と色だけでは解けない」という前提そのものの確認
    let bare = ratio(&["--edge-threshold", "0", "--step-tolerance", "0"]);
    assert!(
        bare < with_dam / 2.0,
        "守りを全部外しても商品が残る＝この画像は難しくない: {bare} vs {with_dam}"
    );
}

/// 堤防が実際に効いていることを CLI から固定する。
///
/// 上のテストは「堤防を切っても結果が変わらない」ことを固定しているので、
/// `edge_ridges` が全ゼロを返すようになっても気づけない。堤防だけが
/// 結論を変える場面を 1 つ押さえておく。
///
/// 幅 1px のスリットがそれにあたる。色も連結性も「外周から届く背景」と
/// 言うが、堤防は 1px の通路の入口で止める。`--seal` は同じ隙間を別の
/// 理由（細すぎる通路）で塞ぐので、堤防だけを見るために切ってある。
#[test]
fn the_edge_dam_alone_stops_a_one_pixel_slit() {
    let dir = fixture_dir();
    // 32x32 の白地に濃色のブロック。上辺から幅 1px のスリットを彫る
    let mut img = image::RgbaImage::from_pixel(32, 32, image::Rgba([250, 250, 250, 255]));
    for y in 8..24 {
        for x in 8..24 {
            img.put_pixel(x, y, image::Rgba([40, 40, 40, 255]));
        }
    }
    for y in 8..20 {
        img.put_pixel(16, y, image::Rgba([250, 250, 250, 255]));
    }
    let input = write_png(dir.path(), "slit.png", &img);

    let slit_is_background = |extra: &[&str]| -> bool {
        let output = dir.path().join(format!("out{}.png", extra.join("")));
        let mask = dir.path().join(format!("mask{}.png", extra.join("")));
        let mut args = vec![
            "cutout",
            input.to_str().unwrap(),
            "-o",
            output.to_str().unwrap(),
            "--debug-mask",
            mask.to_str().unwrap(),
            "--json",
        ];
        args.extend_from_slice(extra);
        let out = kiri().args(&args).output().unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        let written = image::open(&mask).unwrap().to_luma8();
        written.get_pixel(16, 14)[0] < 128
    };

    assert!(
        slit_is_background(&["--edge-threshold", "0", "--seal", "0"]),
        "守りを外しても 1px のスリットが背景として抜けない＝前提が崩れている"
    );
    assert!(
        !slit_is_background(&["--seal", "0"]),
        "堤防が効いていない: 1px のスリットが素通りしている"
    );
}

/// 織り目のある背景では堤防を自動で引き上げ、その旨を結果に書くこと。
///
/// 布・段ボールのような素材は、背景そのものが 1px あたり十数の変化を持つ。
/// 既定の堤防（8）はそれに反応して**背景の中で**壁になり、フィルが商品まで
/// 届かない。`--edge-threshold 0` を知っていれば救えるが、知識を前提にした
/// 道具は AI エージェントには使えない。
///
/// 黙って値を変えるのはもっと悪い。効いた値は `settings` に、変えた理由は
/// `warnings` に出す。
#[test]
fn a_woven_background_raises_the_dam_and_the_report_says_so() {
    let dir = fixture_dir();
    let input = write_png(dir.path(), "woven.png", &woven_background_image(240, 240));

    let run = |name: &str, extra: &[&str]| -> Value {
        let output = dir.path().join(format!("{name}.png"));
        let mut args = vec![
            "cutout",
            input.to_str().unwrap(),
            "-o",
            output.to_str().unwrap(),
            "--json",
        ];
        args.extend_from_slice(extra);
        let out = kiri().args(&args).output().unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        json_stdout(&out)
    };
    let adjusted = |v: &Value| -> bool { has_warning(v, "EDGE_THRESHOLD_RAISED") };

    let auto = run("auto", &[]);
    // 商品は画像の 1/4 〜 3/4 を占めるので、前景比率は 0.25 前後になるはず
    let ratio = auto["mask"]["foreground_ratio"].as_f64().unwrap();
    assert!(
        (0.20..0.32).contains(&ratio),
        "織り目が堤防を発火させて背景が残っている: {ratio}"
    );
    let raised = auto["settings"]["edge_threshold"].as_f64().unwrap();
    assert!(raised > 8.0, "堤防が引き上げられていない: {raised}");
    assert!(
        adjusted(&auto),
        "黙って設定を変えている: {}",
        auto["warnings"]
    );
    // 判断の根拠も返す。エージェントが自分で確かめられなければ意味がない
    let p90 = auto["background"]["texture"]["p90"].as_f64().unwrap();
    assert!(p90 > 8.0, "外周の勾配が報告されていない: {p90}");

    // 明示指定には従う。従わないと「指定したのに効かない」になる
    let pinned = run("pinned", &["--edge-threshold", "8"]);
    assert_eq!(pinned["settings"]["edge_threshold"], 8.0);
    assert!(!adjusted(&pinned), "明示指定に割り込んでいる");
    let pinned_ratio = pinned["mask"]["foreground_ratio"].as_f64().unwrap();
    assert!(
        pinned_ratio > ratio + 0.1,
        "対照が成立していない（堤防を明示しても背景が消える）: {pinned_ratio} vs {ratio}"
    );
}

/// `kiri info` でも同じ値を返すこと。
///
/// 切り抜く前に「この背景は堤防を張れる素材か」を知るための値なので、
/// cutout でしか出ないのでは遅い。
#[test]
fn info_reports_the_texture_of_the_border() {
    let dir = fixture_dir();
    let texture = |name: &str, img: &image::RgbaImage| -> (f64, f64) {
        let input = write_png(dir.path(), name, img);
        let out = kiri()
            .args(["info", input.to_str().unwrap(), "--json"])
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        let t = json_stdout(&out)["background"]["texture"].clone();
        (
            t["p50"].as_f64().expect("p50 が無い"),
            t["p90"].as_f64().expect("p90 が無い"),
        )
    };

    let studio = product_image(&ProductSpec {
        width: 240,
        height: 240,
        ..Default::default()
    });
    let (_, studio_p90) = texture("studio.png", &studio);
    assert!(
        studio_p90 < 5.0,
        "スタジオ背景でテクスチャが立っている: {studio_p90}"
    );

    let (_, woven_p90) = texture("woven_info.png", &woven_background_image(240, 240));
    assert!(
        woven_p90 > 8.0,
        "織り目を検出できていない: {woven_p90}（スタジオ背景は {studio_p90}）"
    );
}

#[test]
fn cutout_restricts_the_result_to_the_given_bbox() {
    let dir = fixture_dir();
    let img = product_image(&ProductSpec {
        width: 200,
        height: 200,
        ..Default::default()
    });
    let input = write_png(dir.path(), "in.png", &img);
    let output = dir.path().join("cut.png");

    let out = kiri()
        .args([
            "cutout",
            input.to_str().unwrap(),
            "-o",
            output.to_str().unwrap(),
            "--bbox",
            "60,60,120,120",
            "--json",
        ])
        .output()
        .unwrap();

    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let v = json_stdout(&out);
    assert_eq!(v["applied_bbox"], serde_json::json!([60, 60, 120, 120]));

    let bbox: Vec<u64> = v["mask"]["bbox"]
        .as_array()
        .unwrap()
        .iter()
        .map(|x| x.as_u64().unwrap())
        .collect();
    assert!(
        bbox[0] >= 60 && bbox[1] >= 60,
        "bbox の外が残っている: {bbox:?}"
    );
    assert!(
        bbox[2] <= 120 && bbox[3] <= 120,
        "bbox の外が残っている: {bbox:?}"
    );
}

#[test]
fn cutout_accepts_normalized_coordinates() {
    let dir = fixture_dir();
    let img = product_image(&ProductSpec {
        width: 200,
        height: 400,
        ..Default::default()
    });
    let input = write_png(dir.path(), "in.png", &img);
    let output = dir.path().join("cut.png");

    let out = kiri()
        .args([
            "cutout",
            input.to_str().unwrap(),
            "-o",
            output.to_str().unwrap(),
            "--bbox",
            "0.25,0.25,0.75,0.75",
            "--normalized",
            "--json",
        ])
        .output()
        .unwrap();

    assert!(out.status.success());
    assert_eq!(
        json_stdout(&out)["applied_bbox"],
        serde_json::json!([50, 100, 150, 300])
    );
}

#[test]
fn pixel_coordinates_passed_as_normalized_are_rejected() {
    let dir = fixture_dir();
    let img = product_image(&ProductSpec {
        width: 200,
        height: 200,
        ..Default::default()
    });
    let input = write_png(dir.path(), "in.png", &img);
    let output = dir.path().join("cut.png");

    let out = kiri()
        .args([
            "cutout",
            input.to_str().unwrap(),
            "-o",
            output.to_str().unwrap(),
            "--bbox",
            "20,20,180,180",
            "--normalized",
            "--json",
        ])
        .output()
        .unwrap();

    assert_eq!(out.status.code(), Some(2));
    assert_eq!(json_stdout(&out)["error"]["code"], "INVALID_BBOX");
}

#[test]
fn debug_mask_is_written_and_reported() {
    let dir = fixture_dir();
    let img = product_image(&ProductSpec {
        width: 150,
        height: 150,
        ..Default::default()
    });
    let input = write_png(dir.path(), "in.png", &img);
    let output = dir.path().join("cut.png");
    let mask = dir.path().join("mask.png");

    let out = kiri()
        .args([
            "cutout",
            input.to_str().unwrap(),
            "-o",
            output.to_str().unwrap(),
            "--debug-mask",
            mask.to_str().unwrap(),
            "--json",
        ])
        .output()
        .unwrap();

    assert!(out.status.success());
    assert!(mask.exists(), "マスクが書き出されていない");
    assert_eq!(
        json_stdout(&out)["mask"]["debug_mask"],
        mask.to_str().unwrap()
    );
}

#[test]
fn cutout_warns_when_almost_nothing_is_removed() {
    let dir = fixture_dir();
    let img = product_image(&ProductSpec {
        width: 150,
        height: 150,
        ..Default::default()
    });
    let input = write_png(dir.path(), "in.png", &img);
    let output = dir.path().join("cut.png");

    // 許容量 0 なら背景がほぼ残る。AI がこれを検出できること
    let out = kiri()
        .args([
            "cutout",
            input.to_str().unwrap(),
            "-o",
            output.to_str().unwrap(),
            "--tolerance",
            "0",
            "--json",
        ])
        .output()
        .unwrap();

    assert!(out.status.success());
    let v = json_stdout(&out);
    assert!(
        has_warning(&v, "FOREGROUND_TOO_LARGE"),
        "失敗が検出できていない: {:?}",
        warning_codes(&v)
    );
}

/// 商品が画面外へ抜けて外周を汚染したら、主体の数値を信用してはいけない。
///
/// **縮小が走る 600px で回すこと。** 主体は長辺 250px へ縮小してから測るので、
/// 200px の画像ではこの経路を踏まず、下の `cutout_flags_a_product_running_off_the_frame`
/// では検出できない。縮小が入ると、閾値から追い出された商品の輪郭に
/// Lanczos3 のリンギングが 1px の帯として残り、それが `far` のほぼ全部になる。
/// `capture_ratio` が 1.0 近くへ張り付き、**誤検出を弾くはずの捕捉率が誤検出を
/// 後押しする向きに反転する。**
///
/// 返る矩形は画面の 3 分の 1 を占める物体を完全に外している。従えばそれが
/// 丸ごと消える。**誤った助言は助言が無いより悪い。**
#[test]
fn a_frame_filling_object_never_earns_high_confidence() {
    let dir = fixture_dir();
    let input = write_png(dir.path(), "bleed.png", &bleeding_product_scene(600, 600));
    let output = dir.path().join("cut.png");

    let out = kiri()
        .args(["info", input.to_str().unwrap(), "--json"])
        .output()
        .unwrap();
    let v = json_stdout(&out);

    // 前提：面積も捕捉率もこの構図を通してしまう。**弾けるのは
    // 「矩形の外に何が残ったか」だけである**
    let s = &v["subject"];
    assert!(
        s["capture_ratio"].as_f64().unwrap() > 0.70,
        "前提が崩れている: {s}"
    );
    assert!(
        s["leftover_ratio"].as_f64().unwrap() >= 0.15,
        "矩形の外に残った塊を見落としている: {s}"
    );

    // 数値は返してよい。信用してよいかだけが問題である。
    // **`assert_ne!` では `subject` が null でも通ってしまう**ので、
    // 「low が出ている」ことを直接押さえる
    assert_eq!(
        s["confidence"], "low",
        "主体を取りこぼした矩形を信用している: {s}"
    );

    // 信頼度が high でない以上、bbox を勧めてはならない
    let out = kiri()
        .args([
            "cutout",
            input.to_str().unwrap(),
            "-o",
            output.to_str().unwrap(),
            "--json",
        ])
        .output()
        .unwrap();
    assert!(out.status.success());
    let v = json_stdout(&out);
    assert!(
        !has_warning(&v, "BBOX_RECOMMENDED"),
        "誤った矩形へ誘導している: {:?}",
        warning_codes(&v)
    );
}

/// **同じ汚染構図を、リポジトリ自身の織り目テクスチャの上で固定する。**
///
/// 外周統計から汚染を当てる規則は、無地の背景でしか成立しなかった。織り目が
/// あるだけで外周 ΔE の p50 が 6.3 まで上がり、判定は素通しして、画面の 35% を
/// 占める物体を丸ごと外した矩形を `confidence: high` で勧めた。
/// **上の無地版だけを押さえていても、この穴は開いたままになる。**
#[test]
fn the_same_poison_on_the_repositorys_own_texture_is_caught_too() {
    let dir = fixture_dir();
    let input = write_png(dir.path(), "woven.png", &woven_poisoned_scene(600, 600));
    let output = dir.path().join("cut.png");

    let v = json_stdout(
        &kiri()
            .args(["info", input.to_str().unwrap(), "--json"])
            .output()
            .unwrap(),
    );

    // 前提：**旧規則の指紋（p50 < 5）が出ない。** 織り目そのものが p50 を押し上げる
    let d = &v["background"]["perimeter_delta_e"];
    assert!(
        d["p50"].as_f64().unwrap() > 5.0,
        "織り目が効いていない。この回帰テストの意味が失われている: {d}"
    );
    // 前提：面積も捕捉率も通ってしまう
    let s = &v["subject"];
    assert!(
        s["area_ratio"].as_f64().unwrap() > 0.05 && s["capture_ratio"].as_f64().unwrap() > 0.70,
        "前提が崩れている: {s}"
    );

    assert_eq!(
        s["confidence"], "low",
        "織り目の上では汚染を見逃している: {s}"
    );

    let v = json_stdout(
        &kiri()
            .args([
                "cutout",
                input.to_str().unwrap(),
                "-o",
                output.to_str().unwrap(),
                "--json",
            ])
            .output()
            .unwrap(),
    );
    assert!(
        !has_warning(&v, "BBOX_RECOMMENDED"),
        "誤った矩形へ誘導している: {:?}",
        warning_codes(&v)
    );
}

/// 対照：外周に帯が掛かっていても、主体を捉えられていれば `high` のまま。
///
/// **上の 2 本と対で意味を持つ。** 疑わしきを片端から Low に落とせば汚染の
/// テストは通り続けるが、機能そのものが死ぬ。旧規則はまさにこれを踏んで、
/// 主体を完璧に検出（area 14.1% / capture 98.1%）している画像を Low へ落とし、
/// `cutout` の警告を `SUBJECT_TOUCHES_EDGE`——C-1 で誤診として潰したもの——へ
/// 戻していた。
#[test]
fn a_band_on_the_edge_does_not_cost_the_subject_its_confidence() {
    let dir = fixture_dir();
    let input = write_png(dir.path(), "band.png", &shadow_band_scene(800, 800, 200));
    let output = dir.path().join("cut.png");

    let v = json_stdout(
        &kiri()
            .args(["info", input.to_str().unwrap(), "--json"])
            .output()
            .unwrap(),
    );
    let s = &v["subject"];
    // 前提：帯は外周 ΔE を跳ね上げている（旧規則ならここで Low に落ちた）
    let d = &v["background"]["perimeter_delta_e"];
    assert!(
        d["p90"].as_f64().unwrap() > 15.0,
        "帯が効いていない。対照の意味が失われている: {d}"
    );
    assert_eq!(s["confidence"], "high", "帯を汚染と読んでいる: {s}");
    assert!(
        s["leftover_ratio"].as_f64().unwrap() < 0.15,
        "帯だけが残っているはず: {s}"
    );

    // **`SUBJECT_TOUCHES_EDGE` へ戻っていないこと。** 商品は中央にあり、
    // 見切れてはいない
    let v = json_stdout(
        &kiri()
            .args([
                "cutout",
                input.to_str().unwrap(),
                "-o",
                output.to_str().unwrap(),
                "--json",
            ])
            .output()
            .unwrap(),
    );
    assert_eq!(v["mask"]["touches_edge"], true, "前提が崩れている: {v}");
    assert!(
        !has_warning(&v, "SUBJECT_TOUCHES_EDGE"),
        "見切れの誤診が復活している: {:?}",
        warning_codes(&v)
    );
    assert!(
        has_warning(&v, "BBOX_RECOMMENDED"),
        "解ける画像で助言が出ていない: {:?}",
        warning_codes(&v)
    );
}

#[test]
fn cutout_flags_a_product_running_off_the_frame() {
    let dir = fixture_dir();
    // 商品が下端まで伸びて見切れている状態を作る。
    // 全面を商品にすると外周サンプルまで商品色になり、背景推定自体が成立しない
    let mut img = image::RgbaImage::from_pixel(120, 120, image::Rgba([250, 250, 249, 255]));
    for y in 70..120 {
        for x in 40..80 {
            img.put_pixel(x, y, image::Rgba([40, 40, 40, 255]));
        }
    }
    let input = write_png(dir.path(), "cropped.png", &img);
    let output = dir.path().join("cut.png");

    let out = kiri()
        .args([
            "cutout",
            input.to_str().unwrap(),
            "-o",
            output.to_str().unwrap(),
            "--json",
        ])
        .output()
        .unwrap();

    assert!(out.status.success());
    let v = json_stdout(&out);
    assert_eq!(v["mask"]["touches_edge"], true);
    assert!(
        has_warning(&v, "SUBJECT_TOUCHES_EDGE"),
        "{:?}",
        warning_codes(&v)
    );
    // 対照。均一な背景での外周接触は正真正銘の見切れなので、bbox を勧めては
    // ならない（bbox は見切れを直さない）。
    // `a_non_uniform_background_recommends_a_bbox_instead_of_crying_crop` と対
    assert!(
        !has_warning(&v, "BBOX_RECOMMENDED"),
        "見切れに bbox を勧めている: {:?}",
        warning_codes(&v)
    );
}

/// **「外周に接している」を「見切れている」と読むのは、bbox が無く背景が
/// 不均一なときには誤診である。**
///
/// その状態で外周に接しているのは商品ではなく、前景として取り残された背景側で
/// ある。実写（不織布の上のリモコン）では bbox を与えれば `touches_edge` が
/// false になり、商品は見切れていなかった。見切れは撮り直すしかないが、
/// こちらは bbox 一つで解ける。同じ文言で報せると、エージェントは解ける問題を
/// 諦めてしまう。
///
/// **対照として `cutout_flags_a_product_running_off_the_frame`（均一背景で
/// 本当に見切れているシーン）を必ず併せて見ること。** 片側だけを固定すると、
/// 仕組みが死んで全件が同じ警告になっても、どちらか一方は通り続ける。
#[test]
fn a_non_uniform_background_recommends_a_bbox_instead_of_crying_crop() {
    let dir = fixture_dir();
    let input = write_png(dir.path(), "split.png", &split_background_scene(300, 300));
    let output = dir.path().join("cut.png");

    let out = kiri()
        .args([
            "cutout",
            input.to_str().unwrap(),
            "-o",
            output.to_str().unwrap(),
            "--json",
        ])
        .output()
        .unwrap();
    assert!(out.status.success());
    let v = json_stdout(&out);

    // 前提：背景が不均一で、背景側が前景として残り、それが端に達している
    assert!(v["background"]["uniformity"].as_f64().unwrap() < 0.9, "{v}");
    assert_eq!(v["mask"]["touches_edge"], true, "前提が崩れている: {v}");

    let codes = warning_codes(&v);
    assert!(codes.contains(&"BBOX_RECOMMENDED".to_string()), "{codes:?}");
    assert!(
        !codes.contains(&"SUBJECT_TOUCHES_EDGE".to_string()),
        "誤診が残っている: {codes:?}"
    );

    // 勧めた bbox がそのまま実行でき、しかも効くこと。**実行できない助言は
    // 助言ではない。** hint の文字列をそのまま引数へ割って渡す
    let hint = v["warnings"]
        .as_array()
        .unwrap()
        .iter()
        .find(|w| w["code"] == "BBOX_RECOMMENDED")
        .unwrap()["hint"]
        .as_str()
        .unwrap()
        .to_string();
    let bbox = hint
        .split_whitespace()
        .nth(1)
        .expect("hint が --bbox <値> の形になっていない");

    let fixed = dir.path().join("fixed.png");
    let out = kiri()
        .args([
            "cutout",
            input.to_str().unwrap(),
            "-o",
            fixed.to_str().unwrap(),
            "--bbox",
            bbox,
            "--normalized",
            "--json",
        ])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "勧めた bbox が通らない: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let fixed = json_stdout(&out);
    let before = v["mask"]["foreground_ratio"].as_f64().unwrap();
    let after = fixed["mask"]["foreground_ratio"].as_f64().unwrap();
    assert!(
        after < before / 2.0,
        "勧めた bbox が効いていない: {before} -> {after}"
    );
    assert!(
        !warning_codes(&fixed).contains(&"BBOX_RECOMMENDED".to_string()),
        "bbox を指定したのに勧め続けている: {:?}",
        warning_codes(&fixed)
    );
}

#[test]
fn cutout_respects_the_overwrite_guard() {
    let dir = fixture_dir();
    let img = product_image(&ProductSpec {
        width: 100,
        height: 100,
        ..Default::default()
    });
    let input = write_png(dir.path(), "in.png", &img);
    let output = dir.path().join("cut.png");
    std::fs::write(&output, b"existing").unwrap();

    let out = kiri()
        .args([
            "cutout",
            input.to_str().unwrap(),
            "-o",
            output.to_str().unwrap(),
            "--json",
        ])
        .output()
        .unwrap();

    assert_eq!(out.status.code(), Some(2));
    assert_eq!(json_stdout(&out)["error"]["code"], "OUTPUT_EXISTS");
}

#[test]
fn cutout_to_jpeg_composites_onto_the_given_background() {
    let dir = fixture_dir();
    let img = product_image(&ProductSpec {
        width: 120,
        height: 120,
        ..Default::default()
    });
    let input = write_png(dir.path(), "in.png", &img);
    let output = dir.path().join("flat.jpg");

    let out = kiri()
        .args([
            "cutout",
            input.to_str().unwrap(),
            "-o",
            output.to_str().unwrap(),
            "--background",
            "#FFFFFF",
            "--json",
        ])
        .output()
        .unwrap();

    assert!(out.status.success());
    let v = json_stdout(&out);
    assert_eq!(v["outputs"][0]["format"], "jpeg");
    assert!(
        has_warning(&v, "ALPHA_FLATTENED"),
        "{:?}",
        warning_codes(&v)
    );
}

// --- キャンバス配置 (Phase 4) ---

/// 指定サイズ・占有率で切り抜き結果を配置し、JSON から配置情報を取り出す。
fn cutout_on_canvas(dir: &Path, input: &Path, name: &str, extra: &[&str]) -> Value {
    let output = dir.join(name);
    let mut args = vec![
        "cutout",
        input.to_str().unwrap(),
        "-o",
        output.to_str().unwrap(),
        "--json",
    ];
    args.extend_from_slice(extra);
    let out = kiri().args(&args).output().unwrap();
    assert!(
        out.status.success(),
        "{name}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    json_stdout(&out)
}

#[test]
fn canvas_produces_exactly_the_requested_size() {
    let dir = fixture_dir();
    let img = product_image(&ProductSpec {
        width: 300,
        height: 400,
        ..Default::default()
    });
    let input = write_png(dir.path(), "in.png", &img);

    let v = cutout_on_canvas(dir.path(), &input, "out.png", &["--canvas", "500x500"]);
    assert_eq!(v["outputs"][0]["width"], 500);
    assert_eq!(v["outputs"][0]["height"], 500);
    assert_eq!(v["canvas"]["width"], 500);
    assert_eq!(v["canvas"]["height"], 500);
}

#[test]
fn a_single_number_canvas_means_square() {
    let dir = fixture_dir();
    let img = product_image(&ProductSpec {
        width: 200,
        height: 200,
        ..Default::default()
    });
    let input = write_png(dir.path(), "in.png", &img);

    let v = cutout_on_canvas(dir.path(), &input, "out.png", &["--canvas", "600"]);
    assert_eq!(v["outputs"][0]["width"], 600);
    assert_eq!(v["outputs"][0]["height"], 600);
}

#[test]
fn fill_ratio_controls_how_much_of_the_canvas_the_product_takes() {
    let dir = fixture_dir();
    let img = product_image(&ProductSpec {
        width: 300,
        height: 300,
        ..Default::default()
    });
    let input = write_png(dir.path(), "in.png", &img);

    let long_side = |v: &Value| -> u64 {
        let c = v["canvas"]["content"].as_array().unwrap();
        c[0].as_u64().unwrap().max(c[1].as_u64().unwrap())
    };

    let tight = cutout_on_canvas(
        dir.path(),
        &input,
        "a.png",
        &["--canvas", "1000", "--fill-ratio", "0.5"],
    );
    let loose = cutout_on_canvas(
        dir.path(),
        &input,
        "b.png",
        &["--canvas", "1000", "--fill-ratio", "0.9"],
    );

    assert_eq!(
        long_side(&tight),
        500,
        "占有率 0.5 なら長辺は 500 になるはず"
    );
    assert_eq!(
        long_side(&loose),
        900,
        "占有率 0.9 なら長辺は 900 になるはず"
    );
}

/// --fill-ratio の目的。構図の異なる商品でも並べたときの見た目が揃うこと。
#[test]
fn products_of_different_sizes_end_up_the_same_size_on_the_canvas() {
    let dir = fixture_dir();
    // 同じ縦横比で、画像内での占有面積だけが違う2枚を作る
    let small = product_image(&ProductSpec {
        width: 400,
        height: 400,
        ..Default::default()
    });
    let large = product_image(&ProductSpec {
        width: 800,
        height: 800,
        ..Default::default()
    });
    let a = write_png(dir.path(), "small.png", &small);
    let b = write_png(dir.path(), "large.png", &large);

    let opts = ["--canvas", "1000", "--fill-ratio", "0.8"];
    let va = cutout_on_canvas(dir.path(), &a, "a.png", &opts);
    let vb = cutout_on_canvas(dir.path(), &b, "b.png", &opts);

    let content = |v: &Value| -> (u64, u64) {
        let c = v["canvas"]["content"].as_array().unwrap();
        (c[0].as_u64().unwrap(), c[1].as_u64().unwrap())
    };
    let (aw, ah) = content(&va);
    let (bw, bh) = content(&vb);
    assert!(
        aw.abs_diff(bw) <= 2 && ah.abs_diff(bh) <= 2,
        "元の解像度が違っても同じ大きさに揃うべき: {aw}x{ah} と {bw}x{bh}"
    );

    // 中央に置かれていること
    let offset = |v: &Value| -> (u64, u64) {
        let o = v["canvas"]["offset"].as_array().unwrap();
        (o[0].as_u64().unwrap(), o[1].as_u64().unwrap())
    };
    let placed = offset(&va).0 * 2 + aw;
    assert!(
        placed.abs_diff(1000) <= 1,
        "左右の余白が均等でない: {placed} (キャンバス 1000)"
    );
}

#[test]
fn canvas_margins_are_transparent_by_default() {
    let dir = fixture_dir();
    let img = product_image(&ProductSpec {
        width: 200,
        height: 200,
        ..Default::default()
    });
    let input = write_png(dir.path(), "in.png", &img);
    let output = dir.path().join("out.png");

    let out = kiri()
        .args([
            "cutout",
            input.to_str().unwrap(),
            "-o",
            output.to_str().unwrap(),
            "--canvas",
            "400",
            "--fill-ratio",
            "0.5",
            "--json",
        ])
        .output()
        .unwrap();
    assert!(out.status.success());

    let check = kiri()
        .args(["info", output.to_str().unwrap(), "--json"])
        .output()
        .unwrap();
    assert_eq!(
        json_stdout(&check)["has_alpha"],
        true,
        "余白が透明になっていない"
    );
}

#[test]
fn flatten_removes_transparency_even_for_png() {
    let dir = fixture_dir();
    let img = product_image(&ProductSpec {
        width: 200,
        height: 200,
        ..Default::default()
    });
    let input = write_png(dir.path(), "in.png", &img);
    let output = dir.path().join("out.png");

    let out = kiri()
        .args([
            "cutout",
            input.to_str().unwrap(),
            "-o",
            output.to_str().unwrap(),
            "--canvas",
            "400",
            "--flatten",
            "--background",
            "#FFFFFF",
            "--json",
        ])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );

    let check = kiri()
        .args(["info", output.to_str().unwrap(), "--json"])
        .output()
        .unwrap();
    let v = json_stdout(&check);
    assert_eq!(
        v["has_alpha"], false,
        "--flatten を指定したのに透過が残っている"
    );
    // 塗り潰し色が背景として検出される
    assert_eq!(v["background"]["rgb"], serde_json::json!([255, 255, 255]));
}

#[test]
fn flatten_works_without_a_canvas_too() {
    let dir = fixture_dir();
    let img = product_image(&ProductSpec {
        width: 150,
        height: 150,
        ..Default::default()
    });
    let input = write_png(dir.path(), "in.png", &img);
    let output = dir.path().join("out.png");

    let out = kiri()
        .args([
            "cutout",
            input.to_str().unwrap(),
            "-o",
            output.to_str().unwrap(),
            "--flatten",
            "--background",
            "#00FF00",
            "--json",
        ])
        .output()
        .unwrap();
    assert!(out.status.success());

    let check = kiri()
        .args(["info", output.to_str().unwrap(), "--json"])
        .output()
        .unwrap();
    let v = json_stdout(&check);
    assert_eq!(v["has_alpha"], false);
    assert_eq!(v["background"]["rgb"], serde_json::json!([0, 255, 0]));
}

#[test]
fn scaling_up_onto_a_canvas_is_reported_and_warned() {
    let dir = fixture_dir();
    let img = product_image(&ProductSpec {
        width: 100,
        height: 100,
        ..Default::default()
    });
    let input = write_png(dir.path(), "in.png", &img);

    let v = cutout_on_canvas(dir.path(), &input, "out.png", &["--canvas", "1000"]);
    let scale = v["canvas"]["scale"].as_f64().unwrap();
    assert!(scale > 1.0, "拡大しているのに倍率が 1 以下: {scale}");
    assert!(
        has_warning(&v, "CANVAS_UPSCALED"),
        "拡大の警告がない: {:?}",
        warning_codes(&v)
    );
}

#[test]
fn an_invalid_fill_ratio_is_an_argument_error() {
    let dir = fixture_dir();
    let img = product_image(&ProductSpec {
        width: 100,
        height: 100,
        ..Default::default()
    });
    let input = write_png(dir.path(), "in.png", &img);
    let output = dir.path().join("out.png");

    for bad in ["0", "1.5"] {
        let out = kiri()
            .args([
                "cutout",
                input.to_str().unwrap(),
                "-o",
                output.to_str().unwrap(),
                "--canvas",
                "500",
                "--fill-ratio",
                bad,
                "--json",
                "--force",
            ])
            .output()
            .unwrap();
        assert_eq!(
            out.status.code(),
            Some(2),
            "fill-ratio={bad} が通ってしまった"
        );
        assert_eq!(json_stdout(&out)["error"]["code"], "INVALID_FILL_RATIO");
    }
}

#[test]
fn a_canvas_without_any_foreground_is_a_processing_error() {
    let dir = fixture_dir();
    // 全面が単色。前景が検出できないので配置しようがない
    let img = image::RgbaImage::from_pixel(120, 120, image::Rgba([250, 250, 249, 255]));
    let input = write_png(dir.path(), "flat.png", &img);
    let output = dir.path().join("out.png");

    let out = kiri()
        .args([
            "cutout",
            input.to_str().unwrap(),
            "-o",
            output.to_str().unwrap(),
            "--canvas",
            "500",
            "--json",
        ])
        .output()
        .unwrap();

    assert_eq!(out.status.code(), Some(4), "処理失敗は exit 4 であるべき");
    let v = json_stdout(&out);
    assert_eq!(v["error"]["code"], "NO_FOREGROUND");
    assert!(v["error"]["hint"].as_str().unwrap().contains("--tolerance"));
}

#[test]
fn cutout_without_a_canvas_reports_no_canvas_field() {
    let dir = fixture_dir();
    let img = product_image(&ProductSpec {
        width: 120,
        height: 120,
        ..Default::default()
    });
    let input = write_png(dir.path(), "in.png", &img);

    let v = cutout_on_canvas(dir.path(), &input, "out.png", &[]);
    assert!(
        v.get("canvas").is_none(),
        "キャンバス未指定なのに canvas が出ている"
    );
    assert_eq!(v["outputs"][0]["width"], 120, "寸法は元のままであるべき");
}

// --- batch (Phase 5) ---

/// 商品画像を n 枚と spec.json を用意する。
fn batch_fixture(dir: &Path, count: usize) -> Vec<String> {
    let mut names = Vec::new();
    for i in 0..count {
        let name = format!("p{i}.png");
        let img = product_image(&ProductSpec {
            width: 120 + (i as u32) * 20,
            height: 120 + (i as u32) * 10,
            ..Default::default()
        });
        write_png(dir, &name, &img);
        names.push(name);
    }
    names
}

fn write_spec(dir: &Path, json: &str) -> PathBuf {
    let path = dir.join("spec.json");
    std::fs::write(&path, json).unwrap();
    path
}

fn run_batch(spec: &Path, extra: &[&str]) -> std::process::Output {
    let mut args = vec!["batch", spec.to_str().unwrap(), "--json"];
    args.extend_from_slice(extra);
    kiri().args(&args).output().unwrap()
}

#[test]
fn batch_processes_every_item() {
    let dir = fixture_dir();
    batch_fixture(dir.path(), 3);
    let spec = write_spec(
        dir.path(),
        r#"{"defaults":{"canvas":"400x400","format":"png"},
            "items":[{"input":"p0.png","output":"out/a.png"},
                     {"input":"p1.png","output":"out/b.png"},
                     {"input":"p2.png","output":"out/c.png"}]}"#,
    );

    let out = run_batch(&spec, &[]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );

    let v = json_stdout(&out);
    assert_eq!(v["total"], 3);
    assert_eq!(v["succeeded"], 3);
    assert_eq!(v["failed"], 0);

    for name in ["a", "b", "c"] {
        assert!(
            dir.path().join(format!("out/{name}.png")).exists(),
            "{name} が無い"
        );
    }
}

#[test]
fn batch_applies_defaults_and_lets_items_override_them() {
    let dir = fixture_dir();
    batch_fixture(dir.path(), 2);
    let spec = write_spec(
        dir.path(),
        r#"{"defaults":{"canvas":"400x400","fill_ratio":0.5,"format":"png"},
            "items":[{"input":"p0.png","output":"a.png"},
                     {"input":"p1.png","output":"b.png","fill_ratio":0.9}]}"#,
    );

    let v = json_stdout(&run_batch(&spec, &[]));
    let long = |i: usize| -> u64 {
        let c = v["results"][i]["result"]["canvas"]["content"]
            .as_array()
            .unwrap();
        c[0].as_u64().unwrap().max(c[1].as_u64().unwrap())
    };
    assert_eq!(long(0), 200, "既定の占有率 0.5 が効いていない");
    assert_eq!(long(1), 360, "項目側の 0.9 が優先されていない");
}

/// 数百点を回す前提では、1 件の失敗で全体が止まっては困る。
#[test]
fn one_failing_item_does_not_stop_the_rest() {
    let dir = fixture_dir();
    batch_fixture(dir.path(), 2);
    std::fs::write(dir.path().join("broken.jpg"), b"not an image").unwrap();
    let spec = write_spec(
        dir.path(),
        r#"{"defaults":{"format":"png"},
            "items":[{"input":"p0.png","output":"a.png"},
                     {"input":"broken.jpg","output":"bad.png"},
                     {"input":"p1.png","output":"c.png"}]}"#,
    );

    let out = run_batch(&spec, &[]);
    assert_eq!(
        out.status.code(),
        Some(4),
        "失敗があれば exit 4 で知らせるべき"
    );

    let v = json_stdout(&out);
    assert_eq!(v["total"], 3);
    assert_eq!(v["succeeded"], 2);
    assert_eq!(v["failed"], 1);

    assert_eq!(v["results"][1]["status"], "error");
    assert!(
        v["results"][1]["error"]["code"]
            .as_str()
            .unwrap()
            .starts_with("UNSUPPORTED")
    );

    // 失敗の前後にある項目はどちらも処理されていること
    assert!(dir.path().join("a.png").exists());
    assert!(dir.path().join("c.png").exists());
}

#[test]
fn batch_results_follow_the_spec_order() {
    let dir = fixture_dir();
    batch_fixture(dir.path(), 4);
    let spec = write_spec(
        dir.path(),
        r#"{"defaults":{"format":"png"},
            "items":[{"input":"p3.png","output":"d.png"},
                     {"input":"p0.png","output":"a.png"},
                     {"input":"p2.png","output":"c.png"},
                     {"input":"p1.png","output":"b.png"}]}"#,
    );

    let v = json_stdout(&run_batch(&spec, &[]));
    let inputs: Vec<String> = v["results"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| {
            r["input"]
                .as_str()
                .unwrap()
                .rsplit('/')
                .next()
                .unwrap()
                .to_string()
        })
        .collect();
    assert_eq!(
        inputs,
        ["p3.png", "p0.png", "p2.png", "p1.png"],
        "並列でも順序が保たれるべき"
    );
}

#[test]
fn serial_and_parallel_runs_agree() {
    let dir = fixture_dir();
    batch_fixture(dir.path(), 3);
    let spec = write_spec(
        dir.path(),
        r#"{"defaults":{"canvas":"300","format":"png"},
            "items":[{"input":"p0.png","output":"s/a.png"},
                     {"input":"p1.png","output":"s/b.png"},
                     {"input":"p2.png","output":"s/c.png"}]}"#,
    );

    let ratios = |extra: &[&str]| -> Vec<f64> {
        let v = json_stdout(&run_batch(&spec, extra));
        v["results"]
            .as_array()
            .unwrap()
            .iter()
            .map(|r| r["result"]["mask"]["foreground_ratio"].as_f64().unwrap())
            .collect()
    };
    let parallel = ratios(&["--force"]);
    let serial = ratios(&["--jobs", "1", "--force"]);
    assert_eq!(parallel, serial, "並列と直列で結果が食い違っている");
}

/// batch のテキスト出力でも hint を捨てない。
///
/// **同じ画像の同じ失敗が、呼び方によって回復できたりできなかったりしては
/// いけない。** 単体実行では次の一手が出るのに、数百点を回す本命の経路でだけ
/// 消えると、そこで詰まった利用者は手がかりを持てない。
#[test]
fn batch_prints_the_hint_alongside_the_warning() {
    let dir = fixture_dir();
    batch_fixture(dir.path(), 1);
    // 許容量 0 なら背景がほぼ残り、FOREGROUND_TOO_LARGE が hint 付きで出る
    let spec = write_spec(
        dir.path(),
        r#"{"defaults":{"format":"png"},
            "items":[{"input":"p0.png","output":"a.png","tolerance":0}]}"#,
    );

    let out = kiri()
        .args(["batch", spec.to_str().unwrap()])
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr).to_string();
    assert!(
        stderr.contains("警告 ["),
        "警告そのものが出ていない: {stderr}"
    );
    assert!(
        stderr.contains("--tolerance を上げてください"),
        "hint が捨てられている: {stderr}"
    );
}

/// AI が生成した仕様の綴り違いを黙って無視しない。
#[test]
fn a_misspelled_key_in_the_spec_is_reported_with_a_suggestion() {
    let dir = fixture_dir();
    batch_fixture(dir.path(), 1);
    let spec = write_spec(
        dir.path(),
        r#"{"items":[{"input":"p0.png","output":"a.png","tolerence":5}]}"#,
    );

    let out = run_batch(&spec, &[]);
    assert_eq!(out.status.code(), Some(3));
    let v = json_stdout(&out);
    assert_eq!(v["error"]["code"], "SPEC_UNKNOWN_FIELD");
    assert!(v["error"]["hint"].as_str().unwrap().contains("tolerance"));
}

/// 色変換の可否も spec から指定できること。
///
/// バッチは数百点を一度に回す。単発だけ `--no-color-convert` を持っていても、
/// 本命の経路で指定できなければ意味がない。
#[test]
fn the_spec_accepts_color_convert() {
    let dir = fixture_dir();
    batch_fixture(dir.path(), 1);
    let spec = write_spec(
        dir.path(),
        r#"{"defaults":{"color_convert":false,"format":"png"},
            "items":[{"input":"p0.png","output":"a.png"}]}"#,
    );

    let out = run_batch(&spec, &[]);
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let v = json_stdout(&out);
    assert_eq!(v["results"][0]["result"]["color_space"], "sRGB");
    assert_eq!(v["results"][0]["result"]["color_converted"], false);
}

#[test]
fn a_malformed_spec_is_rejected() {
    let dir = fixture_dir();
    let spec = write_spec(dir.path(), "{ this is not json");
    let out = run_batch(&spec, &[]);
    assert_eq!(out.status.code(), Some(3));
    assert_eq!(json_stdout(&out)["error"]["code"], "SPEC_INVALID_JSON");
}

#[test]
fn an_empty_item_list_is_an_argument_error() {
    let dir = fixture_dir();
    let spec = write_spec(dir.path(), r#"{"items":[]}"#);
    let out = run_batch(&spec, &[]);
    assert_eq!(out.status.code(), Some(2));
    assert_eq!(json_stdout(&out)["error"]["code"], "SPEC_EMPTY");
}

#[test]
fn a_missing_spec_file_is_an_input_error() {
    let out = run_batch(Path::new("/nonexistent/spec.json"), &[]);
    assert_eq!(out.status.code(), Some(3));
    assert_eq!(json_stdout(&out)["error"]["code"], "SPEC_UNREADABLE");
}

#[test]
fn relative_paths_resolve_against_the_spec_file() {
    let dir = fixture_dir();
    let images = dir.path().join("images");
    std::fs::create_dir_all(&images).unwrap();
    batch_fixture(&images, 1);

    // 仕様ファイルを画像と同じ場所に置き、カレントディレクトリとは無関係に動くこと
    let spec = write_spec(
        &images,
        r#"{"items":[{"input":"p0.png","output":"a.png"}]}"#,
    );
    let out = kiri()
        .args(["batch", spec.to_str().unwrap(), "--json"])
        .current_dir(dir.path())
        .output()
        .unwrap();

    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        images.join("a.png").exists(),
        "仕様ファイルの隣に出力されるべき"
    );
}

#[test]
fn base_dir_overrides_where_relative_paths_point() {
    let dir = fixture_dir();
    let images = dir.path().join("images");
    std::fs::create_dir_all(&images).unwrap();
    batch_fixture(&images, 1);

    // 仕様ファイルは別の場所に置く
    let spec = write_spec(
        dir.path(),
        r#"{"items":[{"input":"p0.png","output":"out.png"}]}"#,
    );
    let out = run_batch(&spec, &["--base-dir", images.to_str().unwrap()]);

    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(images.join("out.png").exists());
}

#[test]
fn batch_respects_the_overwrite_guard() {
    let dir = fixture_dir();
    batch_fixture(dir.path(), 1);
    std::fs::write(dir.path().join("a.png"), b"existing").unwrap();
    let spec = write_spec(
        dir.path(),
        r#"{"defaults":{"format":"png"},"items":[{"input":"p0.png","output":"a.png"}]}"#,
    );

    let out = run_batch(&spec, &[]);
    assert_eq!(out.status.code(), Some(4), "全項目が失敗しても exit 4");
    let v = json_stdout(&out);
    assert_eq!(v["results"][0]["error"]["code"], "OUTPUT_EXISTS");
    assert_eq!(
        std::fs::read(dir.path().join("a.png")).unwrap(),
        b"existing"
    );

    // --force なら通る
    let out = run_batch(&spec, &["--force"]);
    assert!(out.status.success());
    assert_ne!(
        std::fs::read(dir.path().join("a.png")).unwrap(),
        b"existing"
    );
}

#[test]
fn batch_counts_items_that_need_review() {
    let dir = fixture_dir();
    batch_fixture(dir.path(), 2);
    let spec = write_spec(
        dir.path(),
        r#"{"defaults":{"format":"png"},
            "items":[{"input":"p0.png","output":"a.png"},
                     {"input":"p1.png","output":"b.png","tolerance":0}]}"#,
    );

    let v = json_stdout(&run_batch(&spec, &[]));
    assert_eq!(v["succeeded"], 2);
    assert_eq!(v["with_warnings"], 1, "tolerance 0 の項目に警告が付くはず");
}

// --- 検証用プレビューと診断値 ---

#[test]
fn preview_is_written_as_a_three_panel_sheet() {
    let dir = fixture_dir();
    let img = product_image(&ProductSpec {
        width: 300,
        height: 200,
        ..Default::default()
    });
    let input = write_png(dir.path(), "in.png", &img);
    let output = dir.path().join("cut.png");
    let preview = dir.path().join("check.png");

    let out = kiri()
        .args([
            "cutout",
            input.to_str().unwrap(),
            "-o",
            output.to_str().unwrap(),
            "--preview",
            preview.to_str().unwrap(),
            "--json",
        ])
        .output()
        .unwrap();

    assert!(out.status.success());
    assert!(preview.exists(), "プレビューが書き出されていない");
    assert_eq!(json_stdout(&out)["preview"], preview.to_str().unwrap());

    // 元画像より小さいのでパネルは原寸のまま 3 枚並ぶ (8 + 300*3 + 8*2 + 8)
    let sheet = image::open(&preview).unwrap();
    assert_eq!(sheet.width(), 8 * 2 + 300 * 3 + 8 * 2);
    assert_eq!(sheet.height(), 8 * 2 + 200);
}

#[test]
fn preview_shrinks_a_large_image_to_the_panel_size() {
    let dir = fixture_dir();
    let img = product_image(&ProductSpec {
        width: 1200,
        height: 900,
        ..Default::default()
    });
    let input = write_png(dir.path(), "in.png", &img);
    let output = dir.path().join("cut.png");
    let preview = dir.path().join("check.png");

    kiri()
        .args([
            "cutout",
            input.to_str().unwrap(),
            "-o",
            output.to_str().unwrap(),
            "--preview",
            preview.to_str().unwrap(),
            "--preview-size",
            "128",
        ])
        .assert()
        .success();

    // 各パネルの長辺が 128 に収まる。原寸を視覚モデルに渡せないことが
    // --preview の存在理由なので、これが効かなければ意味がない
    let sheet = image::open(&preview).unwrap();
    assert_eq!(sheet.width(), 8 * 2 + 128 * 3 + 8 * 2);
    assert_eq!(sheet.height(), 8 * 2 + 96);
}

#[test]
fn an_absurd_preview_size_is_an_argument_error() {
    let dir = fixture_dir();
    let img = product_image(&ProductSpec::default());
    let input = write_png(dir.path(), "in.png", &img);

    kiri()
        .args([
            "cutout",
            input.to_str().unwrap(),
            "-o",
            dir.path().join("cut.png").to_str().unwrap(),
            "--preview",
            dir.path().join("check.png").to_str().unwrap(),
            "--preview-size",
            "4",
        ])
        .assert()
        .failure();
}

#[test]
fn cutout_reports_separability_alongside_tolerance() {
    let dir = fixture_dir();
    let img = product_image(&ProductSpec {
        width: 200,
        height: 200,
        background: [248, 248, 247],
        product: [40, 40, 45],
        ..Default::default()
    });
    let input = write_png(dir.path(), "in.png", &img);

    let out = kiri()
        .args([
            "cutout",
            input.to_str().unwrap(),
            "-o",
            dir.path().join("cut.png").to_str().unwrap(),
            "--json",
        ])
        .output()
        .unwrap();

    assert!(out.status.success());
    let json = json_stdout(&out);
    let sep = json["mask"]["separability"]
        .as_f64()
        .expect("separability がない");
    assert!(
        sep > 20.0,
        "濃い商品と明るい背景なら大きく離れるはず: {sep}"
    );
}

#[test]
fn info_reports_the_spread_of_the_perimeter() {
    let dir = fixture_dir();
    let img = product_image(&ProductSpec::default());
    let input = write_png(dir.path(), "in.png", &img);

    let out = kiri()
        .args(["info", input.to_str().unwrap(), "--json"])
        .output()
        .unwrap();

    let json = json_stdout(&out);
    let d = &json["background"]["perimeter_delta_e"];
    for key in ["p50", "p90", "max"] {
        assert!(d[key].is_number(), "{key} がない: {d}");
    }
    assert!(
        d["p50"].as_f64().unwrap() <= d["max"].as_f64().unwrap(),
        "p50 は max を超えない"
    );
}

#[test]
fn preview_may_not_overwrite_the_output() {
    // 付随出力は本出力の後に書かれるため、パスが同じだと成果物が壊れる。
    // しかも JSON は上書き前の寸法を報告するので、エージェントには検知できない
    let dir = fixture_dir();
    let img = product_image(&ProductSpec::default());
    let input = write_png(dir.path(), "in.png", &img);
    let same = dir.path().join("same.png");

    let out = kiri()
        .args([
            "cutout",
            input.to_str().unwrap(),
            "-o",
            same.to_str().unwrap(),
            "--preview",
            same.to_str().unwrap(),
            "--json",
        ])
        .output()
        .unwrap();

    assert!(!out.status.success());
    assert_eq!(json_stdout(&out)["error"]["code"], "SIDE_OUTPUT_CONFLICT");
    assert!(!same.exists(), "重い処理に入る前に弾くべき");
}

#[test]
fn preview_respects_the_overwrite_guard() {
    let dir = fixture_dir();
    let img = product_image(&ProductSpec::default());
    let input = write_png(dir.path(), "in.png", &img);
    let taken = dir.path().join("taken.png");
    std::fs::write(&taken, b"do not clobber me").unwrap();

    let out = kiri()
        .args([
            "cutout",
            input.to_str().unwrap(),
            "-o",
            dir.path().join("cut.png").to_str().unwrap(),
            "--preview",
            taken.to_str().unwrap(),
            "--json",
        ])
        .output()
        .unwrap();

    assert!(!out.status.success());
    assert_eq!(json_stdout(&out)["error"]["code"], "OUTPUT_EXISTS");
    assert_eq!(
        std::fs::read(&taken).unwrap(),
        b"do not clobber me",
        "既存ファイルを壊してはいけない"
    );
}

#[test]
fn an_unknown_preview_extension_is_an_argument_error() {
    // --output は同じ状況でエラーにするので、こちらだけ黙って PNG にはしない
    let dir = fixture_dir();
    let img = product_image(&ProductSpec::default());
    let input = write_png(dir.path(), "in.png", &img);

    let out = kiri()
        .args([
            "cutout",
            input.to_str().unwrap(),
            "-o",
            dir.path().join("cut.png").to_str().unwrap(),
            "--preview",
            dir.path().join("p.webp").to_str().unwrap(),
            "--json",
        ])
        .output()
        .unwrap();

    assert!(!out.status.success());
    assert_eq!(json_stdout(&out)["error"]["code"], "UNKNOWN_OUTPUT_FORMAT");
}

#[test]
fn a_failing_preview_does_not_fail_the_command() {
    // プレビューは検証用の付随物。これを理由にエラーを返すと「成果物は
    // 書けているのにエラー」となり、エージェントは再実行して OUTPUT_EXISTS で
    // 二重に詰まる
    let dir = fixture_dir();
    let img = product_image(&ProductSpec::default());
    let input = write_png(dir.path(), "in.png", &img);
    let output = dir.path().join("cut.png");

    // 通常ファイルを親に持つパスは作れない
    let blocker = dir.path().join("blocker");
    std::fs::write(&blocker, b"x").unwrap();
    let preview = blocker.join("p.png");

    let out = kiri()
        .args([
            "cutout",
            input.to_str().unwrap(),
            "-o",
            output.to_str().unwrap(),
            "--preview",
            preview.to_str().unwrap(),
            "--json",
        ])
        .output()
        .unwrap();

    assert!(out.status.success(), "成果物が書けているなら成功で返すべき");
    assert!(output.exists());
    let json = json_stdout(&out);
    assert!(json["preview"].is_null(), "書けなかったなら報告しない");
    assert!(
        has_warning(&json, "PREVIEW_FAILED"),
        "警告として伝えるべき: {}",
        json["warnings"]
    );
}

#[test]
fn separability_is_reported_as_null_rather_than_omitted() {
    // null になるのはエージェントが最も知りたい失敗ケース。キーごと消えると
    // 「値が無い」と「そもそも報告されていない」を区別できない
    let dir = fixture_dir();
    let img = product_image(&ProductSpec::default());
    let input = write_png(dir.path(), "in.png", &img);

    let out = kiri()
        .args([
            "cutout",
            input.to_str().unwrap(),
            "-o",
            dir.path().join("cut.png").to_str().unwrap(),
            "--json",
        ])
        .output()
        .unwrap();

    let json = json_stdout(&out);
    let mask = json["mask"].as_object().unwrap();
    // 同じ理由で halo_ratio と edge_width もキーを消さない。0 と報告すると
    // 「縁が残っていない」「輪郭がギザギザ」に見えてしまい、
    // 「そもそも測れていない」と区別できなくなる
    for key in ["separability", "halo_ratio", "edge_width"] {
        assert!(mask.contains_key(key), "{key} のキーは常に存在すべき");
    }
}

#[test]
fn cutout_reports_the_edge_diagnostics() {
    // separability は境界の内側を測るため、前景の外側に残った背景色の縁を
    // 検出できない。縁そのものを測る値を別に返す
    let dir = fixture_dir();
    let img = product_image(&ProductSpec {
        width: 200,
        height: 200,
        product: [40, 40, 45],
        ..Default::default()
    });
    let input = write_png(dir.path(), "in.png", &img);

    let out = kiri()
        .args([
            "cutout",
            input.to_str().unwrap(),
            "-o",
            dir.path().join("cut.png").to_str().unwrap(),
            "--json",
        ])
        .output()
        .unwrap();

    assert!(out.status.success());
    let json = json_stdout(&out);
    let halo = json["mask"]["halo_ratio"]
        .as_f64()
        .expect("halo_ratio がない");
    let width = json["mask"]["edge_width"]
        .as_f64()
        .expect("edge_width がない");
    assert!(halo < 0.05, "既定の経路で縁が残っている: {halo}");
    assert!(width > 0.0, "境界の遷移幅が測れていない: {width}");
}

#[test]
fn no_refine_falls_back_to_the_old_boundary_handling() {
    // 逃げ道が実際に別の結果を出すこと。旧経路はマスクの形からアルファを作るため、
    // エッジ堤防が残す背景色の縁がそのまま不透明で残る
    let dir = fixture_dir();
    let img = product_image(&ProductSpec {
        width: 200,
        height: 200,
        product: [40, 40, 45],
        ..Default::default()
    });
    let input = write_png(dir.path(), "in.png", &img);

    let halo = |args: &[&str]| -> f64 {
        let mut cmd = kiri();
        cmd.args([
            "cutout",
            input.to_str().unwrap(),
            "-o",
            dir.path().join("cut.png").to_str().unwrap(),
            "--force",
            "--json",
        ]);
        cmd.args(args);
        let out = cmd.output().unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        json_stdout(&out)["mask"]["halo_ratio"].as_f64().unwrap()
    };

    let refined = halo(&[]);
    let legacy = halo(&["--no-refine"]);
    // 差の下限が 0.02 なのは、旧経路が残す縁そのものが薄くなったため。
    // 稜線の細線化で堤防が 1px になり、測地的オープニングも堤防の分を
    // 補うようになったので、背景色のまま不透明で残る画素は実測 3.0% しかない
    // （どちらも入る前は 33.9% だった）。それでも 0 ではない点が肝で、
    // 色から決め直す経路だけが 0 にできる
    assert!(
        legacy > refined + 0.02,
        "--no-refine で旧挙動に戻っていない: {legacy} vs {refined}"
    );
}

#[test]
fn batch_accepts_the_refine_key() {
    let dir = fixture_dir();
    let img = product_image(&ProductSpec {
        width: 120,
        height: 120,
        ..Default::default()
    });
    write_png(dir.path(), "a.png", &img);
    let spec = dir.path().join("spec.json");
    std::fs::write(
        &spec,
        r#"{"defaults":{"refine":false},
             "items":[{"input":"a.png","output":"out.png"}]}"#,
    )
    .unwrap();

    let out = kiri()
        .args(["batch", spec.to_str().unwrap(), "--json"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(json_stdout(&out)["succeeded"], 1);
}

#[test]
fn a_misspelled_refine_key_suggests_the_right_one() {
    let dir = fixture_dir();
    let spec = dir.path().join("spec.json");
    std::fs::write(
        &spec,
        r#"{"items":[{"input":"a.png","output":"b.png","refien":false}]}"#,
    )
    .unwrap();

    let out = kiri()
        .args(["batch", spec.to_str().unwrap(), "--json"])
        .output()
        .unwrap();
    let json = json_stdout(&out);
    assert_eq!(json["error"]["code"], "SPEC_UNKNOWN_FIELD");
    assert!(
        json["error"]["hint"].as_str().unwrap().contains("refine"),
        "候補に refine が出ていない: {}",
        json["error"]["hint"]
    );
}

// --- 既定値の重複 ---

/// CLI の既定値とライブラリの既定値が食い違っていないこと。
///
/// 同じ数字が clap の `default_value_t`、`CutoutOptions::default()`、そして
/// batch の `unwrap_or` の 3 箇所に書かれている。片方だけ動かしても
/// コンパイルは通り、テストも「その値でたまたま通る」ので誰も気づかない。
/// ここでは CLI とライブラリを突き合わせる。batch の `to_cutout_args` は非公開で
/// ここからは触れないので、同じモジュール内のユニットテストで押さえている。
#[test]
fn the_cli_defaults_match_the_library_defaults() {
    use clap::Parser;
    use kiri::cli::{Cli, Command as CliCommand};
    use kiri::cutout::CutoutOptions;

    let cli = Cli::parse_from(["kiri", "cutout", "in.png", "-o", "out.png"]);
    let CliCommand::Cutout(args) = cli.command else {
        panic!("cutout として解釈されていない");
    };
    let defaults = CutoutOptions::default();

    assert_eq!(args.tolerance, defaults.tolerance, "--tolerance の既定値");
    assert_eq!(args.border, defaults.border, "--border の既定値");
    assert_eq!(args.cleanup, defaults.cleanup, "--cleanup の既定値");
    assert_eq!(args.feather, defaults.feather, "--feather の既定値");
    // --edge-threshold だけは「既定値」がここに無い。CLI もライブラリも
    // 未指定を None のまま持ち回り、DEFAULT_EDGE_THRESHOLD を起点にした
    // 自動調整へ渡すためである。両者が None であることは
    // 「未指定と 8 の明示を区別する」という約束そのものなので、ここで固定する
    assert_eq!(
        args.edge_threshold, None,
        "--edge-threshold の未指定が None のまま渡っていない"
    );
    assert_eq!(
        defaults.edge_threshold, None,
        "CutoutOptions の edge_threshold が未指定でなくなっている"
    );
    assert_eq!(
        args.step_tolerance, defaults.step_tolerance,
        "--step-tolerance の既定値"
    );
    assert_eq!(
        args.shadow_tolerance, defaults.shadow_tolerance,
        "--shadow-tolerance の既定値"
    );
    assert_eq!(args.seal, defaults.seal, "--seal の既定値");
    assert_eq!(!args.no_despill, defaults.despill, "デスピルの既定");
    assert_eq!(!args.no_refine, defaults.refine, "アルファ再推定の既定");
    // 色変換だけ既定値の持ち主が CutoutOptions ではなく LoadOptions になる。
    // 読み込み側の設定なので、切り抜きの設定に混ぜるとかえって追えない
    assert_eq!(
        !args.color.no_color_convert,
        kiri::image_io::LoadOptions::default().convert_color,
        "--no-color-convert の既定値"
    );
}

/// `--help` が語る既定値が `DEFAULT_EDGE_THRESHOLD` と食い違っていないこと。
///
/// `--edge-threshold` の既定値は clap の `default_value_t` に無く、ヘルプの
/// 文言としてしか現れない。**AI エージェントは `--help` を読んで判断する**ので、
/// 定数を動かしてヘルプが取り残されると「指定しなくても 8 が効く」という
/// 誤った前提のまま使われる。上のテストが拾えない唯一の抜け道がここにある。
#[test]
fn the_help_text_quotes_the_real_default_edge_threshold() {
    use clap::CommandFactory;
    use kiri::cli::Cli;

    let help = Cli::command()
        .find_subcommand_mut("cutout")
        .expect("cutout サブコマンドが無い")
        .render_long_help()
        .to_string();
    let expected = format!("既定 {:.0}", kiri::cutout::DEFAULT_EDGE_THRESHOLD);
    assert!(
        help.contains(&expected),
        "--help が「{expected}」を語っていない:\n{help}"
    );
}

#[test]
fn batch_accepts_the_new_fill_keys() {
    let dir = fixture_dir();
    let img = product_image(&ProductSpec {
        width: 120,
        height: 120,
        shadow: true,
        ..Default::default()
    });
    write_png(dir.path(), "a.png", &img);
    let spec = dir.path().join("spec.json");
    std::fs::write(
        &spec,
        r#"{"defaults":{"step_tolerance":3.0,"shadow_tolerance":20.0,"seal":2},
             "items":[{"input":"a.png","output":"out.png"}]}"#,
    )
    .unwrap();

    let out = kiri()
        .args(["batch", spec.to_str().unwrap(), "--json"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(json_stdout(&out)["succeeded"], 1);
}

#[test]
fn a_misspelled_shadow_tolerance_key_suggests_the_right_one() {
    let dir = fixture_dir();
    let spec = dir.path().join("spec.json");
    std::fs::write(
        &spec,
        r#"{"items":[{"input":"a.png","output":"b.png","shadow_tolerence":20}]}"#,
    )
    .unwrap();

    let out = kiri()
        .args(["batch", spec.to_str().unwrap(), "--json"])
        .output()
        .unwrap();
    let json = json_stdout(&out);
    assert_eq!(json["error"]["code"], "SPEC_UNKNOWN_FIELD");
    assert!(
        json["error"]["hint"]
            .as_str()
            .unwrap()
            .contains("shadow_tolerance"),
        "候補に shadow_tolerance が出ていない: {}",
        json["error"]["hint"]
    );
}

/// 効いた設定が結果の JSON に載ること。
///
/// 結果が期待と違ったとき、エージェントがまず知りたいのは「自分の指定が
/// 効いたのか、既定のまま走ったのか」である。画像を開いても分からないし、
/// バッチでは defaults と item の継承が絡むので、結果側に答えが要る。
#[test]
fn the_report_states_the_settings_that_took_effect() {
    let dir = fixture_dir();
    let img = product_image(&ProductSpec {
        width: 80,
        height: 80,
        ..Default::default()
    });
    let input = write_png(dir.path(), "in.png", &img);
    let output = dir.path().join("out.png");

    let settings = |extra: &[&str]| -> Value {
        let mut args = vec![
            "cutout",
            input.to_str().unwrap(),
            "-o",
            output.to_str().unwrap(),
            "--force",
            "--json",
        ];
        args.extend_from_slice(extra);
        let out = kiri().args(&args).output().unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        json_stdout(&out)["settings"].clone()
    };

    let defaults = settings(&[]);
    assert_eq!(defaults["tolerance"], 12.0);
    assert_eq!(defaults["edge_threshold"], 8.0);
    assert_eq!(defaults["step_tolerance"], 2.2);
    assert_eq!(defaults["shadow_tolerance"], 35.0);
    assert_eq!(defaults["seal"], 1);
    assert_eq!(defaults["cleanup"], 2);
    assert_eq!(defaults["feather"], 1);
    assert_eq!(defaults["despill"], true);
    assert_eq!(defaults["refine"], true);

    let tuned = settings(&[
        "--step-tolerance",
        "3.5",
        "--shadow-tolerance",
        "0",
        "--seal",
        "2",
        "--no-refine",
    ]);
    assert_eq!(tuned["step_tolerance"], 3.5);
    assert_eq!(tuned["shadow_tolerance"], 0.0);
    assert_eq!(tuned["seal"], 2);
    assert_eq!(tuned["refine"], false);
}

/// バッチの spec でも「未指定」と「8 を明示」が区別されること。
///
/// spec は clap を通らない経路なので、既定値で埋める実装が残っていると
/// 自動調整が働かない。数百点を回した後で気づくのでは遅い。
#[test]
fn a_batch_spec_distinguishes_an_unset_dam_from_an_explicit_one() {
    let dir = fixture_dir();
    write_png(dir.path(), "a.png", &woven_background_image(240, 240));
    let spec = dir.path().join("spec.json");

    let run = |body: &str| -> Value {
        std::fs::write(&spec, body).unwrap();
        let out = kiri()
            .args(["batch", spec.to_str().unwrap(), "--json", "--force"])
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        json_stdout(&out)["results"][0]["result"].clone()
    };

    let auto = run(r#"{"items":[{"input":"a.png","output":"auto.png"}]}"#);
    assert!(
        auto["settings"]["edge_threshold"].as_f64().unwrap() > 8.0,
        "spec で未指定なのに自動調整が働いていない: {}",
        auto["settings"]
    );

    let pinned =
        run(r#"{"defaults":{"edge_threshold":8.0},"items":[{"input":"a.png","output":"p.png"}]}"#);
    assert_eq!(
        pinned["settings"]["edge_threshold"], 8.0,
        "spec の明示指定に割り込んでいる"
    );
}

/// 範囲外の数値は受け取る前に断ること。
///
/// 負値や nan は比較が常に偽になるだけなので、通してしまうと「指定したのに
/// 効かない」という形で黙って無視される。エージェントは結果の JSON を見て
/// 判断するので、無視されたことに気づく手がかりが無い。
#[test]
fn out_of_range_numeric_options_are_rejected() {
    let dir = fixture_dir();
    let img = product_image(&ProductSpec {
        width: 60,
        height: 60,
        ..Default::default()
    });
    let input = write_png(dir.path(), "in.png", &img);

    for bad in [
        "--tolerance=-1",
        "--step-tolerance=-0.5",
        "--shadow-tolerance=nan",
        "--edge-threshold=inf",
        // 半径に比例して走査量が増えるので、二桁の指定は事故しかない
        "--seal=400",
        // 長辺 1000px 換算の半径。上限を超えると商品そのものが
        // 「孤立ノイズ」になり、消す道具ではなく全消しの道具になる
        "--cleanup=65",
        "--cleanup=4294967295",
    ] {
        let out = kiri()
            .args([
                "cutout",
                input.to_str().unwrap(),
                "-o",
                dir.path().join("out.png").to_str().unwrap(),
                "--force",
                bad,
            ])
            .output()
            .unwrap();
        assert!(!out.status.success(), "{bad} が受け付けられてしまった");
    }
}

/// バッチの spec も同じ約束で弾くこと。clap を通らない経路なので別立てで見る。
#[test]
fn an_out_of_range_setting_in_a_batch_spec_is_rejected() {
    let dir = fixture_dir();
    let img = product_image(&ProductSpec {
        width: 60,
        height: 60,
        ..Default::default()
    });
    write_png(dir.path(), "a.png", &img);
    for defaults in [
        r#"{"step_tolerance":-1.0}"#,
        r#"{"seal":400}"#,
        // 上限を超えた cleanup。clap 側と同じ関門を spec にも掛ける
        r#"{"cleanup":65}"#,
        r#"{"cleanup":4294967295}"#,
    ] {
        let spec = dir.path().join("spec.json");
        std::fs::write(
            &spec,
            format!(
                r#"{{"defaults":{defaults},"items":[{{"input":"a.png","output":"out.png"}}]}}"#
            ),
        )
        .unwrap();

        let out = kiri()
            .args(["batch", spec.to_str().unwrap(), "--json", "--force"])
            .output()
            .unwrap();
        let json = json_stdout(&out);
        assert_eq!(
            json["results"][0]["error"]["code"], "INVALID_SETTING",
            "spec の {defaults} が弾かれていない: {json}"
        );
    }
}

/// 落ち影が既定で消えること。CLI から通しで確かめる。
#[test]
fn cutout_removes_a_cast_shadow_by_default() {
    let dir = fixture_dir();
    // 落ち影の裾は商品の大きさに比例するので、小さすぎる画像では
    // 消えても消えなくても前景比率がほとんど動かない
    let img = product_image(&ProductSpec {
        width: 600,
        height: 600,
        shadow: true,
        ..Default::default()
    });
    let input = write_png(dir.path(), "shadow.png", &img);
    let plain = write_png(
        dir.path(),
        "plain.png",
        &product_image(&ProductSpec {
            width: 600,
            height: 600,
            ..Default::default()
        }),
    );

    let ratio = |src: &Path, extra: &[&str]| -> f64 {
        let output = dir.path().join(format!(
            "out-{}{}.png",
            src.file_stem().unwrap().to_str().unwrap(),
            extra.join("")
        ));
        let mut args = vec![
            "cutout",
            src.to_str().unwrap(),
            "-o",
            output.to_str().unwrap(),
            "--json",
        ];
        args.extend_from_slice(extra);
        let out = kiri().args(&args).output().unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        json_stdout(&out)["mask"]["foreground_ratio"]
            .as_f64()
            .unwrap()
    };

    // 影が残ると前景比率がその分だけ膨らむ。判定は絶対値の差ではなく
    // 「影を置かなかった場合」との距離で見る。600px では影の裾が前景比率を
    // 0.005 しか動かさないので、固定のマージンでは実測との差が薄すぎて、
    // 影の消え方が少し変わっただけで落ちたり通ったりしてしまう
    let removed = ratio(&input, &[]);
    let kept = ratio(&input, &["--shadow-tolerance", "0"]);
    let none = ratio(&plain, &[]);
    assert!(
        (removed - none).abs() < (kept - none) * 0.5,
        "既定で影が消えていない: 影あり {removed} / 影判定なし {kept} / 影なし {none}"
    );
}

// --- dry-run ---

/// `--dry-run` は成果物を書かない。書かないが、書いたときと同じ数値を返す。
///
/// 救済フェーズでは同じ画像へ tolerance を何度も振る。そのたびに本番のパスへ
/// 書かせると、失敗した試行が納品物を上書きする。**成果物を守るために
/// `--force` を常用させるのは順序が逆で**、探索そのものが書かなければよい。
#[test]
fn dry_run_reports_the_result_without_writing_the_output() {
    let dir = fixture_dir();
    let img = product_image(&ProductSpec::default());
    let input = write_jpeg(dir.path(), "product.jpg", &img);
    let output = dir.path().join("out.png");

    let out = kiri()
        .args([
            "cutout",
            input.to_str().unwrap(),
            "-o",
            output.to_str().unwrap(),
            "--dry-run",
            "--json",
        ])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let v = json_stdout(&out);

    assert!(!output.exists(), "--dry-run なのに書き出されている");
    assert_eq!(v["dry_run"], true);
    assert_eq!(v["outputs"][0]["path"], output.to_str().unwrap());
    // 書かないだけでエンコードまでは実際に行う。バイト数は見積もりではない
    assert!(
        v["outputs"][0]["bytes"].as_u64().unwrap() > 0,
        "bytes が実測値になっていない: {v}"
    );
    assert!(v["mask"]["foreground_ratio"].as_f64().unwrap() > 0.0);
}

/// 書いた実行では `dry_run` が false で出る。
///
/// キーを省略して「無ければ書いた」にはしない。エージェントから見て
/// 「古いバージョンで走った」と「書いた」が同じ形になり、成果物が無いのに
/// あるものとして次へ進む事故が起きる。
#[test]
fn a_real_run_says_dry_run_is_false() {
    let dir = fixture_dir();
    let img = product_image(&ProductSpec::default());
    let input = write_jpeg(dir.path(), "product.jpg", &img);
    let output = dir.path().join("out.png");

    let v = json_stdout(
        &kiri()
            .args([
                "cutout",
                input.to_str().unwrap(),
                "-o",
                output.to_str().unwrap(),
                "--json",
            ])
            .output()
            .unwrap(),
    );

    assert_eq!(v["dry_run"], false);
    assert!(output.exists());
}

/// 既存の出力があっても `--dry-run` は落ちない。中身も変わらない。
///
/// 上書き検査は成果物を守るためのもので、書かない実行を止める理由が無い。
/// ただし本番実行では `--force` が要る事実は、往復を 1 回減らすために
/// 警告として先に告げる。
#[test]
fn dry_run_leaves_an_existing_output_untouched_and_warns_about_the_real_run() {
    let dir = fixture_dir();
    let img = product_image(&ProductSpec::default());
    let input = write_jpeg(dir.path(), "product.jpg", &img);
    let output = dir.path().join("out.png");
    std::fs::write(&output, b"deliverable").unwrap();

    let out = kiri()
        .args([
            "cutout",
            input.to_str().unwrap(),
            "-o",
            output.to_str().unwrap(),
            "--dry-run",
            "--json",
        ])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "既存ファイルがあるだけで dry-run が落ちている: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let v = json_stdout(&out);
    assert!(has_warning(&v, "DRY_RUN_OUTPUT_EXISTS"), "{v}");
    assert_eq!(
        std::fs::read(&output).unwrap(),
        b"deliverable",
        "dry-run が既存の成果物を壊している"
    );
}

/// `--force` があれば本番実行も通るので、その警告は出さない。
#[test]
fn dry_run_with_force_does_not_warn_about_the_existing_output() {
    let dir = fixture_dir();
    let img = product_image(&ProductSpec::default());
    let input = write_jpeg(dir.path(), "product.jpg", &img);
    let output = dir.path().join("out.png");
    std::fs::write(&output, b"deliverable").unwrap();

    let v = json_stdout(
        &kiri()
            .args([
                "cutout",
                input.to_str().unwrap(),
                "-o",
                output.to_str().unwrap(),
                "--dry-run",
                "--force",
                "--json",
            ])
            .output()
            .unwrap(),
    );

    assert!(!has_warning(&v, "DRY_RUN_OUTPUT_EXISTS"), "{v}");
    assert_eq!(std::fs::read(&output).unwrap(), b"deliverable");
}

/// プレビューは `--dry-run` でも書く。
///
/// 本出力は成果物だが、プレビューは検証用の付随物である。**「本番を壊さずに
/// 目で確かめる」が救済フェーズそのもの**なので、ここで書かないと
/// `--dry-run` と `--preview` が併用できず、探索のたびに納品物を潰すことになる。
#[test]
fn dry_run_still_writes_the_preview() {
    let dir = fixture_dir();
    let img = product_image(&ProductSpec::default());
    let input = write_jpeg(dir.path(), "product.jpg", &img);
    let output = dir.path().join("out.png");
    let preview = dir.path().join("check.png");

    let v = json_stdout(
        &kiri()
            .args([
                "cutout",
                input.to_str().unwrap(),
                "-o",
                output.to_str().unwrap(),
                "--preview",
                preview.to_str().unwrap(),
                "--dry-run",
                "--json",
            ])
            .output()
            .unwrap(),
    );

    assert!(!output.exists(), "本出力が書かれている");
    assert!(preview.exists(), "プレビューが書かれていない");
    assert_eq!(v["preview"], preview.to_str().unwrap());
}

/// `--dry-run` でも付随出力の上書き検査は掛かる。
///
/// **本出力と付随出力で規約が違う。** dry-run が外すのは本出力の検査だけで、
/// `--preview` / `--debug-mask` は実際に書くので既存のファイルを壊しうる。
/// 探索のたびに同じ検証パスへ書きたければ `--force` を添える——**dry-run と
/// 併せた `--force` は本出力を書かないので安全**である。
#[test]
fn dry_run_still_guards_the_side_outputs() {
    let dir = fixture_dir();
    let img = product_image(&ProductSpec::default());
    let input = write_jpeg(dir.path(), "product.jpg", &img);
    let output = dir.path().join("out.png");
    let preview = dir.path().join("check.png");

    let run = |extra: &[&str]| -> std::process::Output {
        let mut args = vec![
            "cutout",
            input.to_str().unwrap(),
            "-o",
            output.to_str().unwrap(),
            "--preview",
            preview.to_str().unwrap(),
            "--dry-run",
            "--json",
        ];
        args.extend_from_slice(extra);
        kiri().args(&args).output().unwrap()
    };

    assert!(run(&[]).status.success(), "1 回目が通らない");

    // 2 回目は検証ファイルが残っているので止まる
    let second = run(&[]);
    assert_eq!(second.status.code(), Some(2));
    assert_eq!(json_stdout(&second)["error"]["code"], "OUTPUT_EXISTS");

    // --force で通る。本出力は dry-run のまま書かれない
    let third = run(&["--force"]);
    assert!(
        third.status.success(),
        "{}",
        String::from_utf8_lossy(&third.stderr)
    );
    let v = json_stdout(&third);
    assert_eq!(v["dry_run"], true);
    assert!(!output.exists(), "--force が本出力まで書いている");
}

/// 出力を伴うコマンドはすべて `--dry-run` を受ける。
///
/// cutout だけに付けると、エージェントは「convert では試せない」を
/// 覚えなければならない。共通オプションは全部で同じ意味を持つべきである。
#[test]
fn every_writing_command_accepts_dry_run() {
    let dir = fixture_dir();
    let img = product_image(&ProductSpec::default());
    let input = write_jpeg(dir.path(), "product.jpg", &img);

    let cases: Vec<(&str, Vec<&str>)> = vec![
        ("convert", vec![]),
        ("resize", vec!["--width", "80"]),
        ("rotate", vec!["--angle", "90"]),
    ];

    for (command, extra) in cases {
        let output = dir.path().join(format!("{command}.png"));
        let mut args = vec![
            command,
            input.to_str().unwrap(),
            "-o",
            output.to_str().unwrap(),
            "--dry-run",
            "--json",
        ];
        args.extend_from_slice(&extra);

        let out = kiri().args(&args).output().unwrap();
        assert!(
            out.status.success(),
            "{command}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let v = json_stdout(&out);

        assert!(!output.exists(), "{command} が書き出している");
        assert_eq!(v["dry_run"], true, "{command}: {v}");
        assert!(
            v["outputs"][0]["bytes"].as_u64().unwrap() > 0,
            "{command}: {v}"
        );
    }
}

/// batch も 1 件も書かずに全件の統計を返す。
///
/// 数百点の spec を本番へ流す前に、警告の出る項目を洗い出せる必要がある。
#[test]
fn batch_dry_run_writes_nothing_but_reports_every_item() {
    let dir = fixture_dir();
    batch_fixture(dir.path(), 3);
    let spec = write_spec(
        dir.path(),
        r#"{"defaults":{"canvas":"400x400","format":"png"},
            "items":[{"input":"p0.png","output":"out/a.png"},
                     {"input":"p1.png","output":"out/b.png"},
                     {"input":"p2.png","output":"out/c.png"}]}"#,
    );

    let out = run_batch(&spec, &["--dry-run"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let v = json_stdout(&out);

    assert_eq!(v["total"], 3);
    assert_eq!(v["succeeded"], 3);
    assert_eq!(v["dry_run"], true);
    for name in ["a", "b", "c"] {
        assert!(
            !dir.path().join(format!("out/{name}.png")).exists(),
            "{name} が書き出されている"
        );
    }
    // 項目ごとの結果も「書いていない」と言う。results[] だけを見て回る
    // エージェントが、成果物があるものとして次へ進まないため
    assert_eq!(v["results"][0]["result"]["dry_run"], true);
}

// --- schema ---

fn schema_json() -> Value {
    let out = kiri().args(["schema", "--json"]).output().unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    json_stdout(&out)
}

fn codes_of(v: &Value, section: &str) -> Vec<String> {
    v[section]
        .as_array()
        .unwrap_or_else(|| panic!("{section} が配列ではない: {v}"))
        .iter()
        .map(|e| e["code"].as_str().unwrap().to_string())
        .collect()
}

/// `kiri schema --json` は契約そのものを返す。
///
/// README は 1000 行ある。**エージェントに読ませられる長さではない**ので、
/// 契約（code / exit code / オプション）だけを機械可読で配る。
#[test]
fn schema_returns_the_whole_contract() {
    let v = schema_json();

    assert!(v["schema_version"].as_u64().unwrap() >= 1, "{v}");
    assert_eq!(v["kiri_version"], env!("CARGO_PKG_VERSION"));

    // 出口の 5 通りが揃っている
    let exits: Vec<u64> = v["exit_codes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["code"].as_u64().unwrap())
        .collect();
    assert_eq!(exits, vec![0, 1, 2, 3, 4], "{v}");

    // 説明のない code を配らない。code だけ渡されても次の一手は決まらない
    for section in ["warnings", "errors"] {
        let entries = v[section].as_array().unwrap();
        assert!(!entries.is_empty(), "{section} が空: {v}");
        for e in entries {
            let code = e["code"].as_str().unwrap();
            assert!(
                !e["summary"].as_str().unwrap().is_empty(),
                "{section} の {code} に説明が無い"
            );
        }
    }

    // 同じ code が 2 度出ない
    for section in ["warnings", "errors"] {
        let mut codes = codes_of(&v, section);
        let total = codes.len();
        codes.sort();
        codes.dedup();
        assert_eq!(codes.len(), total, "{section} に重複した code がある");
    }
}

/// エラーの code は exit code を伴う。
///
/// `errors[]` を引けば「この失敗で何番が返るか」が分かる。実際に起こして
/// 突き合わせ、表と実装が離れていないことを固定する。
#[test]
fn an_error_that_actually_fires_matches_the_schema() {
    let v = schema_json();
    let dir = fixture_dir();

    let out = kiri()
        .args([
            "convert",
            dir.path().join("no-such-file.jpg").to_str().unwrap(),
            "-o",
            dir.path().join("out.png").to_str().unwrap(),
            "--json",
        ])
        .output()
        .unwrap();
    let observed = json_stdout(&out);
    let code = observed["error"]["code"].as_str().unwrap();

    let documented = v["errors"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["code"] == code)
        .unwrap_or_else(|| panic!("{code} が schema に載っていない"));

    assert_eq!(
        documented["exit_code"].as_i64().unwrap(),
        i64::from(out.status.code().unwrap()),
        "{code} の exit code が schema と食い違う"
    );
}

/// 実際に出た警告の code は必ず schema に載っている。
#[test]
fn a_warning_that_actually_fires_is_documented_in_the_schema() {
    let documented = codes_of(&schema_json(), "warnings");
    let dir = fixture_dir();

    // 背景が単色でない素材は LOW_UNIFORMITY を出す
    let img = split_background_scene(200, 200);
    let input = write_png(dir.path(), "split.png", &img);
    let v = json_stdout(
        &kiri()
            .args(["info", input.to_str().unwrap(), "--json"])
            .output()
            .unwrap(),
    );

    let observed = warning_codes(&v);
    assert!(!observed.is_empty(), "警告が 1 つも出ていない: {v}");
    for code in observed {
        assert!(
            documented.contains(&code),
            "{code} が schema に載っていない（載っている: {documented:?}）"
        );
    }
}

/// オプションはパーサから組み立てる。README ではなく実装が真実である。
///
/// 手で書いた表は必ず離れる。**離れた表は、指定したのに効かないという
/// 最も追いにくい失敗をそのまま招く。**
#[test]
fn schema_options_come_from_the_parser() {
    let v = schema_json();
    let cutout = v["commands"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["name"] == "cutout")
        .expect("cutout が無い");

    let option = |name: &str| -> Value {
        cutout["options"]
            .as_array()
            .unwrap()
            .iter()
            .find(|o| o["name"] == name)
            .unwrap_or_else(|| panic!("{name} が無い: {cutout}"))
            .clone()
    };

    assert_eq!(option("--tolerance")["default"], "12");
    assert_eq!(option("--dry-run")["takes_value"], false);
    // 「未指定」と「8 を明示」を区別する項目は既定値を名乗らない。
    // ここに 8 が出ると、指定しなくても 8 が効くという誤った前提を招く
    assert!(
        option("--edge-threshold")["default"].is_null(),
        "--edge-threshold が既定値を名乗っている"
    );

    // 位置引数も出す。エージェントは入力の渡し方から知る必要がある
    assert_eq!(cutout["arguments"][0]["name"], "input");
    assert_eq!(cutout["arguments"][0]["required"], true);
}

/// 契約に載っている code はすべて、実際に返りうる。
///
/// **返らない code を配るのは、誤った助言と同じ害を持つ。** エージェントはそれ用の
/// 分岐を書き、その枝は永久に死んだままになる。実装の側から使われているかを
/// 走査して、カタログに書いたまま使われていない code を落とす。
///
/// 走査はテストモジュールの手前までに限る。**テストの中の 1 回を「使われている」と
/// 数えると、実運用では返らない code が通ってしまう。** 実際にこの検査で
/// `NOT_FOUND` が見つかった（error.rs のテストでしか使われていなかった）。
#[test]
fn every_documented_code_is_actually_reachable() {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut body = String::new();
    collect_source(&src, &mut body);

    let v = schema_json();
    let mut dead = Vec::new();
    for (section, prefix) in [("errors", "ErrorCode::"), ("warnings", "WarningCode::")] {
        for code in codes_of(&v, section) {
            let variant = format!("{prefix}{}", pascal_case(&code));
            if !body.contains(&variant) {
                dead.push(code);
            }
        }
    }
    assert!(
        dead.is_empty(),
        "契約に載っているが実装から返らない code: {dead:?}"
    );
}

/// `src` 配下の Rust ソースを、各ファイルのテストモジュールの手前まで連結する。
fn collect_source(dir: &Path, out: &mut String) {
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            collect_source(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            let text = std::fs::read_to_string(&path).unwrap();
            out.push_str(text.split("#[cfg(test)]").next().unwrap_or(""));
        }
    }
}

fn pascal_case(screaming_snake: &str) -> String {
    screaming_snake
        .split('_')
        .map(|word| {
            let mut chars = word.chars();
            match chars.next() {
                Some(first) => first.to_string() + &chars.as_str().to_lowercase(),
                None => String::new(),
            }
        })
        .collect()
}

/// 引数の書式や値域で落ちたときは JSON が返らない。
///
/// clap の検証は kiri のエラー型を通らないので、`--json` を付けても **stdout は
/// 空のまま exit 2 で終わる。** `errors[]` のどの code にも対応しない唯一の
/// 失敗なので、契約として固定し、`exit_codes` の説明でも言う。
#[test]
fn a_parser_level_failure_returns_no_json() {
    let dir = fixture_dir();
    let img = product_image(&ProductSpec::default());
    let input = write_jpeg(dir.path(), "product.jpg", &img);

    let output = dir.path().join("o.png");
    for args in [vec!["--angle", "nan"], vec!["--angle", "sideways"]] {
        let mut full = vec![
            "rotate",
            input.to_str().unwrap(),
            "-o",
            output.to_str().unwrap(),
            "--json",
        ];
        full.extend_from_slice(&args);
        let out = kiri().args(&full).output().unwrap();

        assert_eq!(out.status.code(), Some(2), "{args:?}");
        assert!(out.stdout.is_empty(), "{args:?} で stdout に何か出ている");
        assert!(!out.stderr.is_empty(), "{args:?} で stderr が空");
    }

    // 値域の検証も同じ経路。schema の exit_codes がこれを述べている
    let meaning = schema_json()["exit_codes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["code"] == 2)
        .unwrap()["meaning"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(
        meaning.contains("code"),
        "exit 2 の説明が code 無しの失敗に触れていない: {meaning}"
    );
}

/// ドキュメントが名指しする code は、実在する code か実在する定数のどちらかである。
///
/// `every_documented_code_is_actually_reachable` はカタログ → 実装の向きしか見ない。
/// **逆向き（文書 → カタログ）が抜けていて、実際に幽霊を 2 つ通した**
/// （README の `NOT_FOUND` と design.md の `BACKGROUND_NOT_UNIFORM`）。
/// 書き写した例は、エージェントが最も写し取りやすい場所にある。
#[test]
fn every_code_named_in_the_docs_exists() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let v = schema_json();
    let mut known: Vec<String> = codes_of(&v, "errors");
    known.extend(codes_of(&v, "warnings"));

    // ドキュメントは Rust の定数名にも触れる（`MAX_CLEANUP` など）。
    // 実装に存在する定数は code ではないので、幽霊と区別して通す
    let mut source = String::new();
    collect_source(&root.join("src"), &mut source);

    for doc in ["README.md", "docs/design.md", "docs/implementation-plan.md"] {
        let text = std::fs::read_to_string(root.join(doc)).unwrap();
        for name in all_caps_words(&text) {
            let is_code = known.contains(&name);
            let is_constant = source.contains(&format!("const {name}"));
            assert!(
                is_code || is_constant,
                "{doc} が実在しない code を名指ししている: {name}"
            );
        }
    }
}

/// 大文字とアンダースコアだけで綴られた語を拾う。
///
/// 引用符の内側だけを見ようとすると、**バッククォートと二重引用符が混ざった
/// 文書で区切りの数え方が崩れる**（実際に `NOT_FOUND` を取りこぼした）。
/// 囲みに頼らず、語の形だけで拾う。数字だけの区画を持つ語は除く——
/// `IMG_0251` のような実写素材のファイル名がそれで、code ではない。
fn all_caps_words(text: &str) -> Vec<String> {
    let mut found = Vec::new();
    for token in text.split(|c: char| !(c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')) {
        let segments: Vec<&str> = token.split('_').collect();
        let looks_like_a_code = segments.len() >= 2
            && segments
                .iter()
                .all(|seg| seg.chars().any(|c| c.is_ascii_uppercase()));
        if looks_like_a_code && !found.contains(&token.to_string()) {
            found.push(token.to_string());
        }
    }
    found
}

/// 値が決まっている項目は、受け付ける値も返す。
///
/// `--format` / `--fit` は選択肢のある項目で、**綴りを外すと clap が
/// code 無しの exit 2 で落ちる。** 返ってきた結果から `errors[]` へ辿れない
/// 失敗なので、呼ぶ前に選択肢を知れることが要る。
#[test]
fn schema_lists_the_accepted_values_for_enum_options() {
    let v = schema_json();
    let find = |command: &str, option: &str| -> Value {
        v["commands"]
            .as_array()
            .unwrap()
            .iter()
            .find(|c| c["name"] == command)
            .unwrap()["options"]
            .as_array()
            .unwrap()
            .iter()
            .find(|o| o["name"] == option)
            .unwrap_or_else(|| panic!("{command} に {option} が無い"))
            .clone()
    };

    let format: Vec<String> = find("cutout", "--format")["accepts"]
        .as_array()
        .unwrap_or_else(|| panic!("--format が accepts を返さない"))
        .iter()
        .map(|x| x.as_str().unwrap().to_string())
        .collect();
    assert_eq!(format, vec!["avif", "png", "jpeg"]);

    assert!(
        find("resize", "--fit")["accepts"].is_array(),
        "--fit が accepts を返さない"
    );
    // 自由な値を取る項目には付けない。空配列は「選択肢が無い」と読めてしまう
    assert!(find("cutout", "--tolerance").get("accepts").is_none());
}

/// 全コマンドで受けるオプションは 1 箇所にまとめて返す。
///
/// `--json` は契約の中心にありながら、clap のサブコマンドの引数一覧には現れない
/// （グローバル引数は実行時に伝播する）。**素直に組むと schema から丸ごと
/// 落ちる。** 各コマンドへ複製するのではなく「全部で受ける」と 1 度言う。
#[test]
fn schema_lists_the_options_that_every_command_accepts() {
    let v = schema_json();
    let globals = v["global_options"].as_array().unwrap();

    let json = globals
        .iter()
        .find(|o| o["name"] == "--json")
        .unwrap_or_else(|| panic!("--json が無い: {v}"));
    assert_eq!(json["global"], true);
    assert_eq!(json["takes_value"], false);

    // コマンド側には重複させない
    let cutout = v["commands"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["name"] == "cutout")
        .unwrap();
    assert!(
        !cutout["options"]
            .as_array()
            .unwrap()
            .iter()
            .any(|o| o["name"] == "--json"),
        "--json がコマンド側にも出ている"
    );
}

/// すべての結果 JSON が schema_version を名乗る。
///
/// 契約が動いたときに、古い読み手が黙って誤読するのを防ぐ。
#[test]
fn every_report_carries_the_schema_version() {
    let expected = schema_json()["schema_version"].clone();
    let dir = fixture_dir();
    let img = product_image(&ProductSpec::default());
    let input = write_jpeg(dir.path(), "product.jpg", &img);
    let spec = write_spec(
        dir.path(),
        r#"{"items":[{"input":"product.jpg","output":"out/a.png"}]}"#,
    );

    let runs: Vec<Vec<String>> = vec![
        vec!["info".into(), input.display().to_string()],
        vec![
            "convert".into(),
            input.display().to_string(),
            "-o".into(),
            dir.path().join("c.png").display().to_string(),
        ],
        vec![
            "cutout".into(),
            input.display().to_string(),
            "-o".into(),
            dir.path().join("k.png").display().to_string(),
        ],
        vec!["batch".into(), spec.display().to_string()],
    ];

    for mut args in runs {
        let name = args[0].clone();
        args.push("--json".into());
        let out = kiri().args(&args).output().unwrap();
        assert!(
            out.status.success(),
            "{name}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert_eq!(json_stdout(&out)["schema_version"], expected, "{name}");
    }
}

/// エラーの JSON も版を名乗る。失敗の形だけ契約から外れる理由が無い。
#[test]
fn the_error_json_carries_the_schema_version_too() {
    let expected = schema_json()["schema_version"].clone();
    let dir = fixture_dir();

    let out = kiri()
        .args([
            "info",
            dir.path().join("missing.jpg").to_str().unwrap(),
            "--json",
        ])
        .output()
        .unwrap();
    assert_eq!(json_stdout(&out)["schema_version"], expected);
}

/// `--json` を付けなければ人間向けの要約を返す。既存の規約と同じ。
#[test]
fn schema_without_json_is_a_human_summary() {
    let out = kiri().args(["schema"]).output().unwrap();
    assert!(out.status.success());
    let text = String::from_utf8_lossy(&out.stdout);

    assert!(
        serde_json::from_str::<Value>(&text).is_err(),
        "--json 無しで JSON を返している"
    );
    assert!(text.contains("LOW_UNIFORMITY"), "{text}");
    assert!(text.contains("OUTPUT_EXISTS"), "{text}");
}

// --- 値の読み方（fields） ---

fn fields_of(v: &Value) -> Vec<Value> {
    v["fields"]
        .as_array()
        .expect("fields が配列ではない")
        .clone()
}

/// ドット区切りの path で JSON を辿る。無ければ None。
fn pick<'a>(report: &'a Value, path: &str) -> Option<&'a Value> {
    let mut node = report;
    for segment in path.split('.') {
        node = node.get(segment)?;
    }
    Some(node)
}

/// しきい値は実装の定数と同じ値で配られる。
///
/// **散文では配れない種類の情報である。** 警告はしきい値を越えたときにしか
/// 出ないので、出ていない値が良いのか悪いのかは、しきい値を知らなければ
/// 判断できない。そこを知るために README を読ませるのでは、`kiri schema` が
/// 契約を配る意味が半分しか果たせない。
#[test]
fn schema_publishes_the_thresholds_behind_the_warnings() {
    let v = schema_json();
    let fields = fields_of(&v);
    assert!(!fields.is_empty(), "fields が空: {v}");

    let field = |path: &str| -> Value {
        fields
            .iter()
            .find(|f| f["path"] == path)
            .unwrap_or_else(|| panic!("{path} が fields に無い"))
            .clone()
    };

    let warn = |path: &str, code: &str| -> Value {
        field(path)["warns"]
            .as_array()
            .unwrap_or_else(|| panic!("{path} が warns を持たない"))
            .iter()
            .find(|w| w["code"] == code)
            .unwrap_or_else(|| panic!("{path} に {code} が無い"))
            .clone()
    };

    // 実装の定数をそのまま配る。書き写した数値はここで落ちる
    assert_eq!(
        warn("background.uniformity", "LOW_UNIFORMITY")["threshold"]
            .as_f64()
            .unwrap(),
        kiri::cutout::background::MIN_UNIFORMITY
    );
    assert_eq!(
        warn("mask.halo_ratio", "HALO_REMAINS")["threshold"]
            .as_f64()
            .unwrap(),
        kiri::cutout::diagnostics::HALO_WARN
    );
    assert_eq!(
        warn("mask.foreground_ratio", "FOREGROUND_TOO_SMALL")["threshold"]
            .as_f64()
            .unwrap(),
        kiri::cutout::MIN_FOREGROUND_RATIO
    );
    assert_eq!(
        warn("mask.foreground_ratio", "FOREGROUND_TOO_LARGE")["threshold"]
            .as_f64()
            .unwrap(),
        kiri::cutout::MAX_FOREGROUND_RATIO
    );

    // 信頼度 high の 3 条件も同じ形で配る
    let gate = |path: &str| -> Value { field(path)["gates"].clone() };
    assert_eq!(
        gate("subject.area_ratio")["threshold"].as_f64().unwrap(),
        kiri::cutout::subject::MIN_AREA_RATIO
    );
    assert_eq!(
        gate("subject.capture_ratio")["threshold"].as_f64().unwrap(),
        kiri::cutout::subject::MIN_CAPTURE_RATIO
    );
    assert_eq!(
        gate("subject.leftover_ratio")["threshold"]
            .as_f64()
            .unwrap(),
        kiri::cutout::subject::MAX_LEFTOVER_RATIO
    );
    assert_eq!(gate("subject.area_ratio")["confidence"], "high");

    // 警告の code は契約に載っているものだけ
    let known = codes_of(&v, "warnings");
    for f in &fields {
        for w in f["warns"].as_array().unwrap_or(&vec![]) {
            let code = w["code"].as_str().unwrap().to_string();
            assert!(known.contains(&code), "{} の {code} が未知", f["path"]);
            assert!(
                ["lt", "lte", "gt", "gte"].contains(&w["operator"].as_str().unwrap()),
                "{} の operator が不正: {w}",
                f["path"]
            );
        }
    }
}

/// 配る文面に、日本語どうしの間の半角スペースが混ざっていない。
///
/// Rust の行継続（`\` の前のスペース）と clap の doc コメント連結（改行を
/// スペースへ置き換える）は、どちらも英文を前提にしている。日本語では
/// 「固定の しきい値」のような隙間になって残る。**`--help` では折り返しに
/// 紛れて見えないが、schema は文面をそのまま配る**ので、受け取った側が
/// 人間に見せたときに露出する。
///
/// 複数行で書きたい doc コメントは、空行で段落に分ける（clap が 1 行目を
/// `summary`、以降を `detail` に回す）。文字列リテラルは `\` の前の
/// スペースを落とす。
#[test]
fn the_published_prose_has_no_stray_spaces() {
    let v = schema_json();
    let japanese = |c: char| {
        matches!(c,
            'ぁ'..='ん' | 'ァ'..='ヶ' | '一'..='\u{9fff}'
            | 'ー' | '。' | '、' | '「' | '」' | '（' | '）')
    };

    let mut stray = Vec::new();
    let mut check = |label: String, text: Option<&str>| {
        let Some(text) = text else { return };
        let chars: Vec<char> = text.chars().collect();
        for window in chars.windows(3) {
            if japanese(window[0]) && window[1] == ' ' && japanese(window[2]) {
                stray.push(format!(
                    "{label}: 「{}{}{}」",
                    window[0], window[1], window[2]
                ));
            }
        }
    };

    for f in v["fields"].as_array().unwrap() {
        let path = f["path"].as_str().unwrap();
        check(format!("fields/{path}/summary"), f["summary"].as_str());
        check(format!("fields/{path}/notes"), f["notes"].as_str());
        check(
            format!("fields/{path}/null_means"),
            f["null_means"].as_str(),
        );
    }
    for section in ["warnings", "errors"] {
        for e in v[section].as_array().unwrap() {
            let code = e["code"].as_str().unwrap();
            check(format!("{section}/{code}"), e["summary"].as_str());
        }
    }
    let empty = vec![];
    for c in v["commands"].as_array().unwrap() {
        let name = c["name"].as_str().unwrap();
        for arg in c["options"]
            .as_array()
            .unwrap()
            .iter()
            .chain(c["arguments"].as_array().unwrap_or(&empty))
        {
            let option = arg["name"].as_str().unwrap();
            check(format!("{name} {option}/summary"), arg["summary"].as_str());
            check(format!("{name} {option}/detail"), arg["detail"].as_str());
        }
    }

    assert!(
        stray.is_empty(),
        "配る文面に余分な半角スペースがある（{} 件）:\n{}",
        stray.len(),
        stray.join("\n")
    );
}

/// 値を取らない項目に選択肢は無い。
///
/// clap は bool のフラグにも `true` / `false` を possible_values として持つが、
/// **`--flatten true` と書けるわけではない。** そのまま配ると、受け付けない
/// 書き方を契約が勧めることになる。
#[test]
fn a_flag_does_not_claim_to_accept_values() {
    let v = schema_json();
    for command in v["commands"].as_array().unwrap() {
        let empty = vec![];
        let args = command["options"]
            .as_array()
            .unwrap()
            .iter()
            .chain(command["arguments"].as_array().unwrap_or(&empty));
        for arg in args {
            if arg["takes_value"] == Value::Bool(false) {
                assert!(
                    arg.get("accepts").is_none(),
                    "{} の {} が選択肢を名乗っている: {}",
                    command["name"],
                    arg["name"],
                    arg["accepts"]
                );
            }
        }
    }
    for arg in v["global_options"].as_array().unwrap() {
        if arg["takes_value"] == Value::Bool(false) {
            assert!(
                arg.get("accepts").is_none(),
                "{} が選択肢を名乗っている",
                arg["name"]
            );
        }
    }
}

/// `null` を返しうる項目は、`null` が何を意味するかを言う。
///
/// **0 と `null` を混同させないことが要点である。** 0 と報告すると
/// 「縁が残っていない」という良い結果に見えてしまう。
#[test]
fn a_nullable_field_says_what_null_means() {
    for f in fields_of(&schema_json()) {
        if f["nullable"] == Value::Bool(true) {
            let explained = f["null_means"].as_str().unwrap_or("");
            assert!(
                !explained.is_empty(),
                "{} が null の意味を言わない",
                f["path"]
            );
        } else {
            assert!(
                f.get("null_means").is_none(),
                "{} は null を返さないのに説明がある",
                f["path"]
            );
        }
    }
}

/// 配った path は実際の結果に存在する。
///
/// `appears_in` が嘘をつくと、エージェントは `info` で取れない値を待つ。
#[test]
fn every_published_field_exists_in_the_result() {
    let dir = fixture_dir();
    let img = product_image(&ProductSpec::default());
    let input = write_jpeg(dir.path(), "product.jpg", &img);

    let run = |args: &[&str]| -> Value {
        let out = kiri().args(args).output().unwrap();
        assert!(
            out.status.success(),
            "{args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        json_stdout(&out)
    };

    let output = dir.path().join("out.png");
    let info = run(&["info", input.to_str().unwrap(), "--json"]);
    let cutout = run(&[
        "cutout",
        input.to_str().unwrap(),
        "-o",
        output.to_str().unwrap(),
        "--dry-run",
        "--json",
    ]);

    for f in fields_of(&schema_json()) {
        let path = f["path"].as_str().unwrap();
        let commands: Vec<&str> = f["appears_in"]
            .as_array()
            .unwrap_or_else(|| panic!("{path} が appears_in を持たない"))
            .iter()
            .map(|c| c.as_str().unwrap())
            .collect();
        assert!(!commands.is_empty(), "{path} の appears_in が空");

        for command in commands {
            let report = match command {
                "info" => &info,
                "cutout" => &cutout,
                other => panic!("{path} が未知のコマンド {other} を名指ししている"),
            };
            assert!(
                pick(report, path).is_some(),
                "{command} の結果に {path} が無い"
            );
            if f["nullable"] == Value::Bool(false) {
                assert!(
                    !pick(report, path).unwrap().is_null(),
                    "{command} の {path} が null を返した（nullable: false）"
                );
            }
        }
    }
}

/// 配ったしきい値は、実際に出る警告と一致する。
///
/// ここが見るのは **`operator` の向き**である。`FOREGROUND_TOO_SMALL` を `gt` と
/// 書けば、前景が十分にある画像で「小さすぎる警告が出ていない」として落ちる。
/// 定数を正しく参照していても向きは間違えられるので、実行でしか確かめられない。
///
/// **数値の誤りはここでは捕まらない。** しきい値の近傍の値を持つ素材が無いと、
/// どんな値を書いても整合してしまう（`uniformity` 0.90 に対して手元の素材は
/// 1.00 と 0.50 しかない）。そちらは
/// `schema_publishes_the_thresholds_behind_the_warnings` が定数との照合で守る。
/// 2 つで「書き写しの誤り」と「向きの誤り」を分担している。
#[test]
fn the_published_thresholds_agree_with_the_warnings_that_fire() {
    let v = schema_json();
    let dir = fixture_dir();

    // 警告が出る素材と出ない素材の両方を通す。片側だけでは
    // 「出るべきときに出る」か「出ないべきときに出ない」の一方しか見られない
    let plain = write_jpeg(
        dir.path(),
        "plain.jpg",
        &product_image(&ProductSpec::default()),
    );
    let split = write_png(dir.path(), "split.png", &split_background_scene(200, 200));
    let empty = write_png(
        dir.path(),
        "empty.png",
        &product_image(&ProductSpec {
            product: [248, 248, 247],
            shadow: false,
            noise: false,
            ..Default::default()
        }),
    );

    let mut reports = Vec::new();
    for (name, input) in [("plain", &plain), ("split", &split), ("empty", &empty)] {
        for command in ["info", "cutout"] {
            let output = dir.path().join(format!("{name}-{command}.png"));
            let args: Vec<String> = if command == "info" {
                vec!["info".into(), input.display().to_string(), "--json".into()]
            } else {
                vec![
                    "cutout".into(),
                    input.display().to_string(),
                    "-o".into(),
                    output.display().to_string(),
                    "--dry-run".into(),
                    "--json".into(),
                ]
            };
            let out = kiri().args(&args).output().unwrap();
            // 前景が 1 画素も残らない素材は cutout が失敗しうる。その結果は使わない
            if out.status.success() {
                reports.push((format!("{name}/{command}"), json_stdout(&out)));
            }
        }
    }
    assert!(reports.len() >= 4, "比べられる結果が足りない");

    for f in fields_of(&v) {
        let path = f["path"].as_str().unwrap();
        for w in f["warns"].as_array().unwrap_or(&vec![]) {
            let code = w["code"].as_str().unwrap();
            let threshold = w["threshold"].as_f64().unwrap();
            let operator = w["operator"].as_str().unwrap();

            for (label, report) in &reports {
                let Some(value) = pick(report, path).and_then(Value::as_f64) else {
                    continue;
                };
                let crossed = match operator {
                    "lt" => value < threshold,
                    "lte" => value <= threshold,
                    "gt" => value > threshold,
                    "gte" => value >= threshold,
                    other => panic!("未知の operator: {other}"),
                };
                if crossed {
                    assert!(
                        has_warning(report, code),
                        "{label}: {path}={value} は {operator} {threshold} を満たすのに \
                         {code} が出ていない。配ったしきい値が実際の発火点と違う"
                    );
                }
            }
        }
    }
}
