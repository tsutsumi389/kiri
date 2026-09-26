//! CLI の統合テスト。
//!
//! AI エージェントから使われる前提のため、「stdout が常に valid JSON であること」と
//! 「exit code が仕様どおりであること」を最重要の検証項目とする。

mod common;

use std::path::{Path, PathBuf};

use assert_cmd::Command;
use common::{
    ProductSpec, bleeding_product_scene, dense_key_grid, product_image, shadow_band_scene,
    split_background_scene, transparent_product, woven_background_image, woven_poisoned_scene,
    write_jpeg, write_png, write_srgb_png,
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

/// この build が推論でき、かつモデルのファイルが置いてあるか。
///
/// **モデルはリポジトリにも CI にも置かない。** 176MB あり、`--segment` を
/// 使う利用者だけが取ればよいものである。無ければモデルを要する検査は黙って
/// 飛ばす——`#[ignore]` を付けて回らない検査にすると、置いてある機械でも
/// 走らなくなる。
///
/// 存在だけを見る（ダイジェストは突き合わせない）。176MB を舐めるのは
/// `kiri model list` の仕事で、検査の前段で毎回払う費用ではない。
fn segment_ready() -> bool {
    cfg!(feature = "segment")
        && kiri::segment::model::ISNET
            .expected_path()
            .is_some_and(|p| p.is_file())
}

/// 警告に指定の `code` が含まれるか。
///
/// 文言ではなく code で照合する。`warnings` は機械可読な契約であり、
/// テストが散文を掴んでいると、推敲のたびに壊れる（そして推敲を諦めさせる）。
fn has_warning(v: &Value, code: &str) -> bool {
    warning_codes(v).iter().any(|c| c == code)
}

/// 結果 JSON と同じ丸め方。実装の定数や関数と突き合わせるときに要る。
fn round4(v: f64) -> f64 {
    (v * 10_000.0).round() / 10_000.0
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

/// 主体の傾きが `--rotate` にそのまま渡せる形で返り、渡すと水平になること。
///
/// **`info` → `rotate` の 1 往復で閉じることを確かめる。** 符号が逆でも
/// 「角度は返っている」ので、値の存在だけを見るテストでは捕まらない。
#[test]
fn the_reported_level_rotation_actually_levels_the_subject() {
    let dir = fixture_dir();
    // 5 度傾いた濃色の矩形を白背景に置く
    let (w, h) = (400u32, 400u32);
    let mut scene = image::RgbaImage::from_pixel(w, h, image::Rgba([250, 250, 249, 255]));
    let (sin, cos) = 5.0f64.to_radians().sin_cos();
    for y in 0..h {
        for x in 0..w {
            let (dx, dy) = (f64::from(x) - 200.0, f64::from(y) - 200.0);
            let (u, v) = (dx * cos + dy * sin, -dx * sin + dy * cos);
            if u.abs() <= 130.0 && v.abs() <= 55.0 {
                scene.put_pixel(x, y, image::Rgba([40, 40, 45, 255]));
            }
        }
    }
    let input = write_png(dir.path(), "tilted.png", &scene);

    let v = json_stdout(
        &kiri()
            .args(["info", input.to_str().unwrap(), "--json"])
            .output()
            .unwrap(),
    );
    let deg = v["subject"]["level_rotation"]
        .as_f64()
        .unwrap_or_else(|| panic!("傾きが返っていない: {v}"));
    assert!(
        (deg + 5.0).abs() < 1.0,
        "5 度傾いた主体に対して {deg} を返した（期待は -5 付近）"
    );

    // 返った値をそのまま `rotate` に渡すと、次の `info` は 0 付近を返す
    let rotated = dir.path().join("level.png");
    let out = kiri()
        .args([
            "rotate",
            input.to_str().unwrap(),
            "-o",
            rotated.to_str().unwrap(),
            "--angle",
            &deg.to_string(),
            "--json",
        ])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let after = json_stdout(
        &kiri()
            .args(["info", rotated.to_str().unwrap(), "--json"])
            .output()
            .unwrap(),
    );
    let left = after["subject"]["level_rotation"].as_f64().unwrap();
    assert!(
        left.abs() < 1.0,
        "水平出しした後もまだ {left} 度傾いていると言う"
    );
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

/// JPEG の SOS より前のセグメントを (マーカー, ペイロード) で並べる。
fn jpeg_segments(bytes: &[u8]) -> Vec<(u8, &[u8])> {
    assert_eq!(&bytes[..2], &[0xFF, 0xD8], "JPEG ではない");
    let mut out = Vec::new();
    let mut i = 2;
    loop {
        // 区切りを確かめずに長さで歩くと、1 度読み違えただけで以降のセグメントを
        // 黙って取り違える。数え上げの検査が偽りの緑になる
        assert_eq!(bytes[i], 0xFF, "{i} にマーカーが無い");
        let marker = bytes[i + 1];
        let len = u16::from_be_bytes([bytes[i + 2], bytes[i + 3]]) as usize;
        out.push((marker, &bytes[i + 4..i + 2 + len]));
        if marker == 0xDA {
            return out;
        }
        i += 2 + len;
    }
}

/// ICC を運ぶ APP2 の数。
fn jpeg_icc_segments(bytes: &[u8]) -> usize {
    jpeg_segments(bytes)
        .iter()
        .filter(|(m, p)| *m == 0xE2 && p.starts_with(b"ICC_PROFILE\0"))
        .count()
}

/// 歩く途中で区切りが `0xFF` でなければ止まる。長さを信じて進むだけだと、
/// 壊れた並びでも何かしらのセグメント列を返してしまう
#[test]
#[should_panic(expected = "マーカーが無い")]
fn jpeg_segments_refuses_a_misaligned_marker() {
    // SOI の次が 0x00 で始まる。長さどおりに歩けば SOS まで「読めて」しまう
    let bytes = [
        0xFF, 0xD8, 0x00, 0xE0, 0x00, 0x04, 0x00, 0x00, 0xFF, 0xDA, 0x00, 0x02,
    ];
    jpeg_segments(&bytes);
}

/// 書いた出力を kiri 自身に戻すと sRGB と読める。
///
/// `color_space` だけでは足りない——ICC が無くても "sRGB" と答えるので、
/// 名乗りが効いていることは `color_profile` の名前で確かめる。ここが外れると
/// kiri の出力を kiri へ戻すたびに色変換が走る
#[test]
fn a_written_png_and_jpeg_name_srgb_when_read_back() {
    let dir = fixture_dir();
    let img = product_image(&ProductSpec::default());
    let input = write_jpeg(dir.path(), "product.jpg", &img);

    for ext in ["png", "jpg"] {
        let output = dir.path().join(format!("out.{ext}"));
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
        assert!(out.status.success(), "{ext}");
        assert_eq!(json_stdout(&out)["outputs"][0]["icc"], "embedded", "{ext}");

        let out = kiri()
            .args(["info", output.to_str().unwrap(), "--json"])
            .output()
            .unwrap();
        assert!(out.status.success(), "{ext}");
        let v = json_stdout(&out);
        assert_eq!(v["icc_profile"], true, "{ext}");
        assert_eq!(v["color_space"], "sRGB", "{ext}");
        assert_eq!(v["color_profile"], "sRGB IEC61966-2.1", "{ext}");
        assert_eq!(v["color_converted"], false, "{ext}");
        assert_eq!(v["warnings"], Value::Array(vec![]), "{ext}");
    }
}

/// ICC を埋めても決定性は崩れない。JPEG の決定性はこれまで 1 本も見ていなかった
#[test]
fn png_and_jpeg_outputs_are_deterministic_with_icc() {
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
        assert!(out.status.success(), "{name}");
        std::fs::read(&output).unwrap()
    };

    let png = encode("a.png");
    assert_eq!(png, encode("b.png"), "同じ入力から同じ PNG が出ていない");
    assert_eq!(
        png_chunk_kinds(&png)
            .iter()
            .filter(|k| *k == b"iCCP")
            .count(),
        1
    );

    let jpeg = encode("a.jpg");
    assert_eq!(jpeg, encode("b.jpg"), "同じ入力から同じ JPEG が出ていない");
    assert_eq!(jpeg_icc_segments(&jpeg), 1);
}

/// 形式ごとの名乗りを結果が言う。`--dry-run` でも書いたときと同じ値になる
#[test]
fn outputs_report_how_they_name_the_colour() {
    let dir = fixture_dir();
    let img = product_image(&ProductSpec {
        width: 80,
        height: 80,
        ..Default::default()
    });
    let input = write_png(dir.path(), "in.png", &img);

    for (ext, expected) in [("png", "embedded"), ("jpg", "embedded"), ("avif", "nclx")] {
        for dry_run in [false, true] {
            let output = dir.path().join(format!("out-{dry_run}.{ext}"));
            let mut args = vec![
                "convert",
                input.to_str().unwrap(),
                "-o",
                output.to_str().unwrap(),
                "--json",
            ];
            if dry_run {
                args.push("--dry-run");
            }
            let out = kiri().args(&args).output().unwrap();
            assert!(out.status.success(), "{ext} dry_run={dry_run}");
            let v = json_stdout(&out);
            assert_eq!(v["outputs"][0]["icc"], expected, "{ext} dry_run={dry_run}");
            assert!(
                !has_warning(&v, "ICC_NOT_EMBEDDED"),
                "{ext} dry_run={dry_run}: sRGB の画素で鳴ってはいけない"
            );
            assert_eq!(output.exists(), !dry_run, "{ext}");
        }
    }
}

/// sRGB ではない ICC。kiri 自身の sRGB プロファイルの赤と緑の原色を入れ替えて作る。
///
/// 合成プロファイルの組み立て（`color::synthetic`）はライブラリの `cfg(test)` に
/// しか無く、ここからは呼べない。1 本書き起こすより、外部の検証器を通った自前の
/// sRGB をタグ表だけいじるほうが、壊れた ICC として別の分岐（`Unsupported`）へ
/// 落ちる心配が無い
fn swapped_primaries_icc() -> Vec<u8> {
    let mut icc = kiri::color::srgb_profile::srgb_icc().to_vec();
    let count = u32::from_be_bytes(icc[128..132].try_into().unwrap()) as usize;
    let entry = |icc: &[u8], sig: &[u8; 4]| {
        (0..count)
            .map(|k| 132 + 12 * k)
            .find(|&at| &icc[at..at + 4] == sig)
            .unwrap_or_else(|| panic!("{} のタグが無い", String::from_utf8_lossy(sig)))
    };
    let r = entry(&icc, b"rXYZ");
    let g = entry(&icc, b"gXYZ");
    // 署名はそのままに、指す先（オフセットと長さの 8 バイト）だけを入れ替える
    let r_ptr: [u8; 8] = icc[r + 4..r + 12].try_into().unwrap();
    let g_ptr: [u8; 8] = icc[g + 4..g + 12].try_into().unwrap();
    icc[r + 4..r + 12].copy_from_slice(&g_ptr);
    icc[g + 4..g + 12].copy_from_slice(&r_ptr);

    // 名乗りが sRGB のままだと、警告の文面が「sRGB を sRGB へ変換していない」と読めて
    // 紛らわしい。長さを変えるとタグの長さも直すことになるので、同じ長さで書き換える
    let from = kiri::color::srgb_profile::SRGB_PROFILE_NAME.as_bytes();
    let to = SWAPPED_PROFILE_NAME.as_bytes();
    assert_eq!(from.len(), to.len());
    let at = icc
        .windows(from.len())
        .position(|w| w == from)
        .expect("desc に名前が無い");
    icc[at..at + to.len()].copy_from_slice(to);
    icc
}

const SWAPPED_PROFILE_NAME: &str = "kiri: R/G swapped";

fn write_jpeg_with_icc(dir: &Path, name: &str, img: &image::RgbaImage, icc: Vec<u8>) -> PathBuf {
    use image::ImageEncoder;

    let rgb = image::DynamicImage::ImageRgba8(img.clone()).to_rgb8();
    let mut jpeg = Vec::new();
    let mut encoder = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut jpeg, 90);
    encoder.set_icc_profile(icc).unwrap();
    encoder
        .write_image(
            rgb.as_raw(),
            rgb.width(),
            rgb.height(),
            image::ExtendedColorType::Rgb8,
        )
        .unwrap();
    let path = dir.join(name);
    std::fs::write(&path, jpeg).unwrap();
    path
}

/// 書いたファイルが ICC を運んでいるか。AVIF は ICC を書かないので問わない
fn file_carries_icc(path: &Path) -> Option<bool> {
    let bytes = std::fs::read(path).unwrap();
    match path.extension().and_then(|e| e.to_str()) {
        Some("png") => Some(png_chunk_kinds(&bytes).contains(b"iCCP")),
        Some("jpg") => Some(jpeg_icc_segments(&bytes) > 0),
        _ => None,
    }
}

/// `--no-color-convert` で変換しなかった画素は、書いたファイルでも sRGB を名乗らない。
///
/// 単体テスト（`output.rs`）は dry-run の報告だけを見ている。報告と中身は同じ
/// `Derivation` から作られるが、**`write_image` が派生を組む段で値を取り違えても
/// 報告の側は合ったまま通る**。だからファイルの中身で確かめる。変換した側を対に
/// 置くのは、入力の ICC がそもそも読めていない（`Unsupported` で埋め込みへ倒れる）
/// 取り違えを、ここで分けるため
#[test]
fn unconverted_pixels_are_written_without_the_srgb_icc() {
    let dir = fixture_dir();
    let img = product_image(&ProductSpec {
        width: 80,
        height: 80,
        ..Default::default()
    });
    let input = write_jpeg_with_icc(dir.path(), "wide.jpg", &img, swapped_primaries_icc());

    for (ext, unconverted) in [("png", "none"), ("jpg", "none"), ("avif", "nclx")] {
        for convert in [true, false] {
            let output = dir.path().join(format!("out-{convert}.{ext}"));
            let mut args = vec![
                "convert",
                input.to_str().unwrap(),
                "-o",
                output.to_str().unwrap(),
                "--json",
            ];
            if !convert {
                args.push("--no-color-convert");
            }
            let out = kiri().args(&args).output().unwrap();
            assert!(
                out.status.success(),
                "{ext} convert={convert}: {}",
                String::from_utf8_lossy(&out.stderr)
            );
            let v = json_stdout(&out);
            assert_eq!(v["color_converted"], convert, "{ext} convert={convert}");
            let named: Vec<&Value> = v["warnings"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|w| w["code"] == "ICC_NOT_EMBEDDED")
                .collect();

            if convert {
                let expected = if ext == "avif" { "nclx" } else { "embedded" };
                assert_eq!(v["outputs"][0]["icc"], expected, "{ext}");
                assert!(named.is_empty(), "{ext}: 変換した画素で鳴っている");
                assert_ne!(file_carries_icc(&output), Some(false), "{ext}");
            } else {
                assert_eq!(v["outputs"][0]["icc"], unconverted, "{ext}");
                assert_eq!(named.len(), 1, "{ext}: {:?}", warning_codes(&v));
                assert_eq!(named[0]["data"]["format"], v["outputs"][0]["format"]);
                assert_eq!(named[0]["data"]["icc"], unconverted, "{ext}");
                assert_eq!(named[0]["data"]["profile"], SWAPPED_PROFILE_NAME);
                assert_ne!(
                    file_carries_icc(&output),
                    Some(true),
                    "{ext}: 変換していない画素に sRGB の ICC を付けた"
                );
            }
        }
    }

    // batch は項目ごとの `color_convert` で同じ分かれ道を通る
    let spec = write_spec(
        dir.path(),
        r#"{"defaults":{"format":"png"},
            "items":[{"input":"wide.jpg","output":"b/kept.png","color_convert":false},
                     {"input":"wide.jpg","output":"b/converted.png"}]}"#,
    );
    let out = run_batch(&spec, &[]);
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let v = json_stdout(&out);
    for (i, name, icc, warned) in [
        (0, "kept.png", "none", true),
        (1, "converted.png", "embedded", false),
    ] {
        let result = &v["results"][i]["result"];
        assert_eq!(result["outputs"][0]["icc"], icc, "{name}");
        assert_eq!(has_warning(result, "ICC_NOT_EMBEDDED"), warned, "{name}");
        assert_eq!(
            file_carries_icc(&dir.path().join("b").join(name)),
            Some(!warned),
            "{name}"
        );
    }
}

// --- --max-bytes（Phase 19）---

/// 梯子を回す素材。**圧縮しにくいノイズを載せる。**
///
/// 真っ平らな合成画像は品質 25 でも 75 でも数百バイトに収まってしまい、
/// 「段を降りた」と「最初から収まっていた」の区別が付かない。200x200 に
/// 抑えるのは、AVIF の梯子を大きな画像で回すと CI がそのぶん伸びるためである
fn budget_input(dir: &Path) -> PathBuf {
    let img = product_image(&ProductSpec {
        width: 200,
        height: 200,
        shadow: true,
        ..Default::default()
    });
    write_jpeg(dir, "budget.jpg", &img)
}

/// 品質の梯子。**実装の定数をそのまま引く。** 文面もテストも同じ 1 つの表を
/// 見ていないと、段を動かしたときに片方だけが古いまま緑になる
fn ladder() -> &'static [f32] {
    kiri::image_io::derive::QUALITY_LADDER
}

/// `convert` を 1 回回して結果 JSON を返す。
fn convert_json(input: &Path, output: &Path, extra: &[&str]) -> Value {
    let mut args = vec![
        "convert",
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
        "{args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    json_stdout(&out)
}

/// 要求品質では収まらず、梯子のどこかでは収まる上限を 2 点返す。
///
/// **「基準の半分」のような割合で決め打ちにしない。** 素材は 200x200 と小さく、
/// JPEG には ICC の APP2 が 534 バイト固定で乗る。半分は品質 25 でも届かない
/// ことがあり、そこで固定すると (a) の検査が未達の道を通ったまま緑になる
/// （実際そうなった）。梯子の下限で何バイトになるかを測ってから決める。
///
/// 2 点返すのは、1 点だけだと「どんな上限でも最下段まで降りる」実装が通って
/// しまうためである。**どちらも実測から相対で決める**——絶対値のゆとりを
/// 足すと、素材が変わったときに意味が変わる
fn reachable_budgets(input: &Path, output: &Path, format: &str) -> [u64; 2] {
    let at = |quality: &str| {
        convert_json(input, output, &["--format", format, "--quality", quality])["outputs"][0]
            ["bytes"]
            .as_u64()
            .unwrap()
    };
    // 要求品質は CLI の既定（75）、下限は梯子のいちばん下の段
    let baseline = at("75");
    let floor = at(&ladder().last().unwrap().to_string());
    assert!(
        floor < baseline,
        "{format}: 品質を落としても縮まない素材では梯子を試せない（{floor} / {baseline}）"
    );
    // いちばんきつい達成可能な上限と、そこから基準までの中点
    [floor, floor + (baseline - floor) / 2]
}

/// 達成できる上限を 1 つだけ要るとき用。緩いほうを使う
fn reachable_budget(input: &Path, output: &Path, format: &str) -> u64 {
    reachable_budgets(input, output, format)[1]
}

/// 受け入れ基準 (a)。**達成したら必ず `--max-bytes` 以下**である。
///
/// 報告と実ファイルの両方を見る。`render` が探索した大きさと書いたバイト列が
/// 食い違っても、報告だけを見る検査は素通りする。AVIF を 1 ケースに絞るのは、
/// 梯子 1 段あたりのエンコードが JPEG より桁で重いためである
#[test]
fn a_reachable_budget_always_lands_under_the_limit() {
    let dir = fixture_dir();
    let input = budget_input(dir.path());

    for format in ["jpeg", "avif"] {
        let output = dir.path().join(format!("out.{format}"));
        // 上限は 1 形式につき 1 度だけ測る。同じ値を 2 度測り直す理由が無い
        for max in reachable_budgets(&input, &output, format) {
            let v = convert_json(
                &input,
                &output,
                &["--format", format, "--max-bytes", &max.to_string()],
            );
            let out = &v["outputs"][0];
            let label = format!("{format} max={max}");
            assert!(
                out["bytes"].as_u64().unwrap() <= max,
                "{label}: 報告が上限を超えた: {out}"
            );
            assert_eq!(
                std::fs::metadata(&output).unwrap().len(),
                out["bytes"].as_u64().unwrap(),
                "{label}: 実ファイルと報告がずれた"
            );

            let quality = out["quality_used"].as_f64().unwrap() as f32;
            assert!(
                ladder().contains(&quality) && quality < 75.0,
                "{label}: {quality} は要求品質より下の梯子の段ではない"
            );
            assert!(out["attempts"].as_u64().unwrap() > 1, "{label}: {out}");
            assert!(has_warning(&v, "QUALITY_REDUCED"), "{label}: {v}");
            assert!(
                !has_warning(&v, "MAX_BYTES_UNREACHABLE"),
                "{label}: 収まったのに未達と言っている"
            );
        }
    }
}

/// 受け入れ基準 (i)。`QUALITY_REDUCED` の `data` が揃っていること。
///
/// **エージェントは `message` を読まない。** 落とした事実だけでは次の判断が
/// できず、「いくつからいくつへ」「何バイトになったか」が要る
#[test]
fn the_quality_reduced_warning_carries_every_number() {
    let dir = fixture_dir();
    let input = budget_input(dir.path());
    let output = dir.path().join("reduced.jpg");
    let max = reachable_budget(&input, &output, "jpeg");

    let v = convert_json(&input, &output, &["--max-bytes", &max.to_string()]);
    let warning = v["warnings"]
        .as_array()
        .unwrap()
        .iter()
        .find(|w| w["code"] == "QUALITY_REDUCED")
        .unwrap_or_else(|| panic!("QUALITY_REDUCED が無い: {v}"));
    let data = &warning["data"];
    let out = &v["outputs"][0];

    assert_eq!(data["requested"], 75.0);
    assert_eq!(data["quality_used"], out["quality_used"]);
    assert_eq!(data["max_bytes"], max);
    assert_eq!(data["bytes"], out["bytes"]);
    assert_eq!(data["attempts"], out["attempts"]);
    assert_eq!(data["format"], "jpeg");
}

/// 受け入れ基準 (b)。**未達なら要求品質のものを書く。**
///
/// 書いたファイルが `--max-bytes` 無しの出力と 1 バイトも違わないことで、
/// 「どうせ制約は破れているので画質まで捨てない」という決定を固定する。
/// 成果物は残り、終了コードも変わらない——合否で落とすのは別の仕事である
#[test]
fn an_unreachable_budget_writes_the_requested_quality_file() {
    let dir = fixture_dir();
    let input = budget_input(dir.path());
    let plain_path = dir.path().join("plain.jpg");
    let plain = convert_json(&input, &plain_path, &[]);
    let expected = std::fs::read(&plain_path).unwrap();

    let output = dir.path().join("tiny.jpg");
    let v = convert_json(&input, &output, &["--max-bytes", "64"]);
    assert_eq!(
        std::fs::read(&output).unwrap(),
        expected,
        "未達のときに書くものが --max-bytes 無しの出力と違う"
    );

    let out = &v["outputs"][0];
    assert_eq!(out["bytes"], plain["outputs"][0]["bytes"]);
    assert_eq!(out["quality_used"], 75.0, "要求品質へ戻る");
    // 要求品質の 1 回 + 75 より下の 5 段（65 / 55 / 45 / 35 / 25）
    assert_eq!(out["attempts"], 6);

    let warning = v["warnings"]
        .as_array()
        .unwrap()
        .iter()
        .find(|w| w["code"] == "MAX_BYTES_UNREACHABLE")
        .unwrap_or_else(|| panic!("MAX_BYTES_UNREACHABLE が無い: {v}"));
    let data = &warning["data"];
    assert_eq!(data["max_bytes"], 64);
    assert_eq!(data["bytes"], out["bytes"], "書いたファイルの大きさを言う");
    assert_eq!(data["attempts"], out["attempts"]);
    assert_eq!(data["format"], "jpeg");
    let smallest = data["smallest_bytes"].as_u64().unwrap();
    assert!(
        smallest > 64 && smallest <= out["bytes"].as_u64().unwrap(),
        "あとどれだけ足りないかを言えていない: {data}"
    );
    assert!(data["smallest_quality"].as_f64().unwrap() <= 75.0);
    assert!(warning["hint"].is_string(), "次の一手が要る: {warning}");
}

/// 受け入れ基準 (c)。同じ入力からは毎回同じ着地点になる。
///
/// **決定性は kiri の中核の約束である。** 梯子が時刻やタイムアウトを見た
/// 瞬間にここが割れる
#[test]
fn the_same_input_lands_on_the_same_rung_every_time() {
    let dir = fixture_dir();
    let input = budget_input(dir.path());
    let output = dir.path().join("stable.jpg");
    let max = reachable_budget(&input, &output, "jpeg").to_string();

    let once = || {
        let out = convert_json(&input, &output, &["--max-bytes", &max])["outputs"][0].clone();
        (
            out["quality_used"].clone(),
            out["attempts"].clone(),
            out["bytes"].clone(),
        )
    };
    let first = once();
    assert_eq!(once(), first);
    assert_eq!(once(), first);
}

/// 受け入れ基準 (f)。`--dry-run` でも探索は走る。
///
/// 書かないだけで、`bytes` / `quality_used` / `attempts` は本番と同じ実測値で
/// ある。ここが見積もりになると、dry-run は品質とサイズを決める用に使えない
#[test]
fn a_dry_run_searches_for_the_budget_without_writing() {
    let dir = fixture_dir();
    let input = budget_input(dir.path());
    let output = dir.path().join("real.jpg");
    let max = reachable_budget(&input, &output, "jpeg").to_string();

    let real = convert_json(&input, &output, &["--max-bytes", &max]);
    let dry_path = dir.path().join("dry.jpg");
    let dry = convert_json(&input, &dry_path, &["--max-bytes", &max, "--dry-run"]);

    assert!(!dry_path.exists(), "dry-run が書いている");
    assert!(
        real["outputs"][0]["attempts"].as_u64().unwrap() > 1,
        "探索が走らない上限では dry-run と本番の一致を見たことにならない"
    );
    for key in ["bytes", "quality_used", "attempts"] {
        assert_eq!(
            dry["outputs"][0][key], real["outputs"][0][key],
            "{key} が本番と食い違う"
        );
    }
    assert_eq!(warning_codes(&dry), warning_codes(&real));
}

/// 受け入れ基準 (e)。PNG は無損失なので段を降りない。
///
/// ファイルは `--max-bytes` 無しの PNG と 1 バイトも変わらず、`attempts` は 1、
/// `quality_used` は null になる。**同じバイト列を 8 回作らない**ことが要点で、
/// 「試したが変わらなかった」と「試す意味が無い」は結果が同じでも報告が違う
#[test]
fn png_ignores_the_budget_but_says_why() {
    let dir = fixture_dir();
    let input = budget_input(dir.path());
    let plain_path = dir.path().join("plain.png");
    convert_json(&input, &plain_path, &[]);
    let expected = std::fs::read(&plain_path).unwrap();

    let output = dir.path().join("budget.png");
    let v = convert_json(&input, &output, &["--max-bytes", "1k"]);
    assert_eq!(std::fs::read(&output).unwrap(), expected);

    let out = &v["outputs"][0];
    assert_eq!(out["attempts"], 1, "段を降りてはいけない");
    assert!(out["quality_used"].is_null(), "PNG に品質は無い: {out}");
    let warning = v["warnings"]
        .as_array()
        .unwrap()
        .iter()
        .find(|w| w["code"] == "MAX_BYTES_UNREACHABLE")
        .unwrap_or_else(|| panic!("MAX_BYTES_UNREACHABLE が無い: {v}"));
    assert_eq!(warning["data"]["attempts"], 1);
    assert!(
        warning["data"].get("smallest_bytes").is_none(),
        "段を降りていないのに最小を語っている: {warning}"
    );
    let hint = warning["hint"].as_str().unwrap();
    assert!(hint.contains("jpeg"), "品質で収める道を示すべき: {hint}");
}

/// 同じ品質が、結果と警告の `data` で同じ字面になる。
///
/// **`f32` をそのまま JSON へ出すと綴りが割れる。** serde は `33.3` と書くが、
/// `serde_json::Value` へ入れると `f64` へ広がって `33.29999923706055` になる。
/// `outputs[].quality_used` と `data.quality_used` は同じ値を指しているので、
/// エージェントが突き合わせたときに一致しなければならない。
///
/// **JPEG の報告は「エンコーダが受け取った値」である。** `image` の JPEG
/// エンコーダは `u8` しか受けず、`--quality 33.3` は 33 として効く
#[test]
fn a_fractional_quality_is_spelled_the_same_everywhere() {
    let dir = fixture_dir();
    let input = budget_input(dir.path());

    // AVIF は f32 をそのまま受けるので 33.3 が残る
    let avif = dir.path().join("frac.avif");
    let v = convert_json(&input, &avif, &["--format", "avif", "--quality", "33.3"]);
    assert_eq!(v["outputs"][0]["quality_used"], 33.3, "{v}");

    let jpeg = dir.path().join("frac.jpg");
    let v = convert_json(&input, &jpeg, &["--format", "jpeg", "--quality", "33.3"]);
    assert_eq!(
        v["outputs"][0]["quality_used"], 33.0,
        "JPEG は整数へ丸めて渡している: {v}"
    );

    // 落とした実行では requested も同じ規約で綴られる
    let at = |quality: &str| {
        convert_json(&input, &jpeg, &["--format", "jpeg", "--quality", quality])["outputs"][0]
            ["bytes"]
            .as_u64()
            .unwrap()
    };
    let bottom = ladder().last().unwrap().to_string();
    let (baseline, floor) = (at("33.3"), at(&bottom));
    assert!(floor < baseline, "品質を落としても縮まない素材");
    let max = floor + (baseline - floor) / 2;

    let v = convert_json(
        &input,
        &jpeg,
        &[
            "--format",
            "jpeg",
            "--quality",
            "33.3",
            "--max-bytes",
            &max.to_string(),
        ],
    );
    let warning = v["warnings"]
        .as_array()
        .unwrap()
        .iter()
        .find(|w| w["code"] == "QUALITY_REDUCED")
        .unwrap_or_else(|| panic!("QUALITY_REDUCED が無い: {v}"));
    assert_eq!(
        warning["data"]["requested"], 33.3,
        "要求した値をそのまま言う: {warning}"
    );
    assert_eq!(
        warning["data"]["quality_used"], v["outputs"][0]["quality_used"],
        "同じ品質が 2 通りに綴られている: {v}"
    );
    // 33.3 より下の段は下限だけ
    assert_eq!(
        v["outputs"][0]["quality_used"],
        *ladder().last().unwrap() as f64
    );
}

/// 受け入れ基準 (h)。spec の `max_bytes` は数値でも文字列でも書ける。
///
/// エージェントが書く JSON には両方が現れる。片方を断ると、CLI では通る
/// 書き方が spec でだけ通らない道具になる
#[test]
fn a_spec_takes_the_budget_as_a_number_or_a_string() {
    let dir = fixture_dir();
    let img = product_image(&ProductSpec {
        width: 200,
        height: 200,
        shadow: true,
        ..Default::default()
    });
    write_jpeg(dir.path(), "p.jpg", &img);

    // 要求品質と梯子の下限を先に測って、届く上限を決める（`reachable_budget`
    // と同じ理由。割合で決め打ちにすると未達の道を通ったまま緑になる）
    let spec = write_spec(
        dir.path(),
        r#"{"defaults":{"format":"jpeg"},
            "items":[{"input":"p.jpg","output":"out/plain.jpg"},
                     {"input":"p.jpg","output":"out/floor.jpg","quality":25}]}"#,
    );
    let probe = json_stdout(&run_batch(&spec, &[]));
    let bytes = |i: usize| {
        probe["results"][i]["result"]["outputs"][0]["bytes"]
            .as_u64()
            .unwrap()
    };
    let (baseline, floor) = (bytes(0), bytes(1));
    assert!(floor < baseline, "品質を落としても縮まない素材");
    let max = (baseline + floor) / 2;

    let spec = write_spec(
        dir.path(),
        &format!(
            r#"{{"defaults":{{"format":"jpeg"}},
                 "items":[{{"input":"p.jpg","output":"out/n.jpg","max_bytes":{max}}},
                          {{"input":"p.jpg","output":"out/s.jpg","max_bytes":"{max}"}}]}}"#
        ),
    );
    let v = json_stdout(&run_batch(&spec, &[]));
    assert_eq!(v["failed"], 0, "{v}");
    for i in 0..2 {
        let result = &v["results"][i]["result"];
        let out = &result["outputs"][0];
        assert!(out["bytes"].as_u64().unwrap() <= max, "{i}: {out}");
        assert!(has_warning(result, "QUALITY_REDUCED"), "{i}: {result}");
    }
    assert_eq!(
        v["results"][0]["result"]["outputs"][0]["quality_used"],
        v["results"][1]["result"]["outputs"][0]["quality_used"],
        "数値と文字列で着地点が違う"
    );
}

/// 受け入れ基準 (h)。読めない `max_bytes` はその項目を落とす。
///
/// **黙って無視しない。** 上限が効かないまま数百点が処理されると、気づけるのは
/// 配信の段になる。CLI では clap が code 無しの exit 2 で断るので、
/// `INVALID_MAX_BYTES` が出るのは spec 経由だけである（batch は 1 件の失敗で
/// 全体を止めないので、終了コードは処理失敗の 4 になる）
#[test]
fn a_malformed_budget_in_a_spec_is_refused() {
    let dir = fixture_dir();
    let img = product_image(&ProductSpec {
        width: 60,
        height: 60,
        ..Default::default()
    });
    write_jpeg(dir.path(), "p.jpg", &img);

    for written in ["\"1.5m\"", "\"500g\"", "\"\"", "0", "-1", "1.5", "true"] {
        let spec = write_spec(
            dir.path(),
            &format!(
                r#"{{"items":[{{"input":"p.jpg","output":"out/x.jpg","max_bytes":{written}}}]}}"#
            ),
        );
        let out = run_batch(&spec, &[]);
        assert_eq!(out.status.code(), Some(4), "max_bytes:{written}");
        let v = json_stdout(&out);
        assert_eq!(
            v["results"][0]["error"]["code"], "INVALID_MAX_BYTES",
            "max_bytes:{written}"
        );
    }
}

/// CLI 側の書式違いは clap が断る。**code は伴わない**（`ErrorKind::Argument`
/// の説明がそう述べている唯一の失敗である）ので、stdout は空のままになる
#[test]
fn a_malformed_budget_on_the_command_line_is_refused_by_the_parser() {
    let dir = fixture_dir();
    let input = budget_input(dir.path());
    for written in ["1.5m", "0", "500g", "-1"] {
        let out = kiri()
            .args([
                "convert",
                input.to_str().unwrap(),
                "-o",
                dir.path().join("x.jpg").to_str().unwrap(),
                "--max-bytes",
                written,
                "--json",
            ])
            .output()
            .unwrap();
        assert_eq!(out.status.code(), Some(2), "--max-bytes {written}");
        assert!(
            out.stdout.is_empty(),
            "--max-bytes {written} で stdout が出た"
        );
    }
}

// --- 多派生出力とマニフェスト（Phase 20）---

/// 圧縮しにくいノイズ。単色だと PNG が数 KB まで縮み、リサイズも符号化も
/// 常駐量を語らなくなる（`image_io::derive` の同名の道具と同じ理由）
fn noisy_image(width: u32, height: u32) -> image::RgbaImage {
    let mut state = 0x2545_F491_4F6C_DD1Du64;
    image::RgbaImage::from_fn(width, height, |_, _| {
        let mut next = || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            (state >> 33) as u8
        };
        image::Rgba([next(), next(), next(), 255])
    })
}

/// 受け入れ基準 (a)。**派生が 1 本の実行は Phase 19 と 1 バイトも変わらない。**
///
/// ここが崩れたら設計を間違えている。既存の md5 / 決定性テスト
/// （`convert_is_deterministic` / `png_and_jpeg_outputs_are_deterministic_with_icc`
/// ほか 228 本）を 1 本も書き換えずに通すことが第一の証拠で、この 1 本は
/// **`--derive` を 1 つ渡した実行も同じバイト列・同じパスになる**ことまで固定する。
/// 「派生の仕組みを通ったかどうか」で出力が動かないのは、多派生を後から足すうえで
/// 最も守りたい性質である
#[test]
fn a_single_derivation_writes_the_same_bytes_to_the_same_path() {
    let dir = fixture_dir();
    let input = write_jpeg(dir.path(), "p.jpg", &product_image(&ProductSpec::default()));

    for ext in ["png", "jpg", "avif"] {
        let plain = dir.path().join(format!("plain.{ext}"));
        let derived = dir.path().join(format!("derived.{ext}"));
        let a = convert_json(&input, &plain, &[]);
        // role だけを足した 1 本。寸法も形式も品質も何ひとつ上書きしない
        let b = convert_json(&input, &derived, &["--derive", "role=main"]);

        assert_eq!(
            std::fs::read(&plain).unwrap(),
            std::fs::read(&derived).unwrap(),
            "{ext}: 派生を 1 本渡しただけでバイト列が動いた"
        );
        assert_eq!(a["outputs"].as_array().unwrap().len(), 1, "{a}");
        assert_eq!(
            a["outputs"][0]["path"],
            plain.to_str().unwrap(),
            "--output がそのまま出力パスでない: {a}"
        );
        assert_eq!(a["outputs"][0]["role"], Value::Null, "{a}");
        assert_eq!(b["outputs"][0]["role"], "main", "{b}");
        for key in ["format", "width", "height", "bytes", "icc", "attempts"] {
            assert_eq!(a["outputs"][0][key], b["outputs"][0][key], "{ext}/{key}");
        }
    }
}

/// 派生を増やしても、1 本目のバイト列は 1 本だけ書いたときと同じである。
///
/// **同じ最終画像から作る以上、隣に何本あるかで符号化が変わってはいけない。**
/// 派生ごとにリサイズ済み画像を作り直す実装では、借りるか複製するかの分岐が
/// ここに現れうる
#[test]
fn adding_derivations_does_not_disturb_the_ones_already_there() {
    let dir = fixture_dir();
    let input = write_jpeg(dir.path(), "p.jpg", &product_image(&ProductSpec::default()));
    let alone = dir.path().join("alone.png");
    let many = dir.path().join("many.png");

    // 片方は幅 100 の 1 本だけ、もう片方は同じ幅を含む 3 本
    let one = convert_json(&input, &alone, &["--sizes", "100"]);
    let five = convert_json(&input, &many, &["--sizes", "100,80,60"]);
    assert_eq!(five["outputs"].as_array().unwrap().len(), 3, "{five}");

    let bytes =
        |v: &Value, i: usize| std::fs::read(v["outputs"][i]["path"].as_str().unwrap()).unwrap();
    assert_eq!(
        bytes(&one, 0),
        bytes(&five, 0),
        "隣に何本あるかで 1 本目の符号化が変わっている"
    );
    assert_eq!(one["outputs"][0]["bytes"], five["outputs"][0]["bytes"]);
}

/// 直積の並びは size が外・format が内で、`{index}` は `outputs[]` の添字と一致する。
#[test]
fn the_product_of_sizes_and_formats_keeps_size_outside() {
    let dir = fixture_dir();
    let input = write_jpeg(dir.path(), "p.jpg", &product_image(&ProductSpec::default()));
    let v = convert_json(
        &input,
        &dir.path().join("p.png"),
        &[
            "--sizes",
            "100,50",
            "--formats",
            "png,jpeg",
            "--naming",
            "{index}-{width}.{ext}",
        ],
    );
    let names: Vec<String> = v["outputs"]
        .as_array()
        .unwrap()
        .iter()
        .map(|o| {
            Path::new(o["path"].as_str().unwrap())
                .file_name()
                .unwrap()
                .to_str()
                .unwrap()
                .to_string()
        })
        .collect();
    assert_eq!(
        names,
        ["0-100.png", "1-100.jpg", "2-50.png", "3-50.jpg"],
        "{v}"
    );
    for name in &names {
        assert!(dir.path().join(name).is_file(), "{name} が書かれていない");
    }
}

/// 受け入れ基準 (b)。マニフェストのゴールデンと、2 回走らせたときの同一性。
///
/// **キーの並びまで丸ごと突き合わせる。** マニフェストは成果物と並べて版管理
/// されうるものなので、時刻や所要時間が混ざれば毎回差分が出る。決定性を
/// 「同じバイト列」で固定しておかないと、混ざったことに気づけない
#[test]
fn the_manifest_is_a_byte_for_byte_golden() {
    let dir = fixture_dir();
    let input = write_png(dir.path(), "p.png", &noisy_image(40, 30));
    let output = dir.path().join("out.png");
    let manifest = dir.path().join("m.json");

    let run = || {
        convert_json(
            &input,
            &output,
            &[
                "--sizes",
                "20",
                "--formats",
                "png,jpeg",
                "--manifest",
                manifest.to_str().unwrap(),
            ],
        )
    };
    let report = run();
    let first = std::fs::read(&manifest).unwrap();
    run();
    let second = std::fs::read(&manifest).unwrap();
    assert_eq!(first, second, "2 回走らせて同じバイト列にならない");

    let v: Value = serde_json::from_slice(&first).unwrap();
    assert_eq!(v["schema_version"], 2);
    assert_eq!(v["kiri_version"], env!("CARGO_PKG_VERSION"));
    let items = v["items"].as_array().unwrap();
    assert_eq!(items.len(), 1, "convert の items は必ず 1 要素: {v}");
    assert_eq!(items[0]["input"], input.to_str().unwrap());
    // **結果 JSON の outputs[] とまったく同じ要素である。** 2 つの綴りを
    // 持たせると、受け手はどちらを信じるかを決めなければならなくなる
    assert_eq!(items[0]["outputs"], report["outputs"]);

    // **キーの並びも契約である。** 書いたバイト列そのものを見る——
    // `serde_json::Value` へ読み直すと `Map` が綴りで並べ替えてしまい、
    // ファイルの中で何番目に出ているかは分からなくなる
    let text = String::from_utf8(first).unwrap();
    let mut at = 0;
    for key in [
        "\"path\"",
        "\"format\"",
        "\"width\"",
        "\"height\"",
        "\"bytes\"",
        "\"icc\"",
        "\"quality_used\"",
        "\"attempts\"",
        "\"role\"",
    ] {
        let found = text[at..]
            .find(key)
            .unwrap_or_else(|| panic!("{key} が順番どおりに出てこない:\n{text}"));
        at += found + key.len();
    }
    // 時刻の類が 1 つも混ざっていない
    for banned in ["elapsed", "time", "date", "generated"] {
        assert!(!text.contains(banned), "{banned} がマニフェストに混ざった");
    }
}

/// 受け入れ基準 (c)。**命名の衝突は書き始める前に捕まえる。**
///
/// 1 枚でも書いた後に落ちると半端な成果物が残り、しかも結果 JSON は返らないので
/// 何が書けたのかを追う手段が無い。出力ディレクトリに 1 ファイルも増えていない
/// ことまで見る
#[test]
fn a_name_collision_is_caught_before_anything_is_written() {
    let dir = fixture_dir();
    let input = write_jpeg(dir.path(), "p.jpg", &product_image(&ProductSpec::default()));
    let out = TempDir::new().unwrap();
    let output = out.path().join("p.jpg");

    // 幅が同じ 2 本は既定のテンプレート（{stem}_{width}.{ext}）で同じ名前になる
    for extra in [
        vec!["--derive", "width=100", "--derive", "width=100,quality=50"],
        // --naming が寸法も番号も持たなければ、どんな派生でも必ず潰れる
        vec!["--sizes", "100,50", "--naming", "{stem}.{ext}"],
    ] {
        let mut args = vec![
            "convert",
            input.to_str().unwrap(),
            "-o",
            output.to_str().unwrap(),
            "--json",
        ];
        args.extend_from_slice(&extra);
        let result = kiri().args(&args).output().unwrap();
        assert_eq!(result.status.code(), Some(2), "{extra:?}");
        let v = json_stdout(&result);
        assert_eq!(v["error"]["code"], "OUTPUT_NAME_COLLISION", "{v}");
        assert_eq!(
            std::fs::read_dir(out.path()).unwrap().count(),
            0,
            "{extra:?}: 断る前にファイルを書いている"
        );
    }
}

/// `--force` でも衝突は許さない。
///
/// 上書きの可否は「利用者の既存のファイルを壊してよいか」の話で、こちらは
/// **1 回の実行が自分の成果物を自分で潰す**指定である。通せば結果 JSON は
/// 2 本とも書いたと報告し、実際には後の 1 本しか残らない
#[test]
fn force_does_not_excuse_two_derivations_sharing_a_path() {
    let dir = fixture_dir();
    let input = write_jpeg(dir.path(), "p.jpg", &product_image(&ProductSpec::default()));
    let out = kiri()
        .args([
            "convert",
            input.to_str().unwrap(),
            "-o",
            dir.path().join("q.jpg").to_str().unwrap(),
            "--force",
            "--json",
            "--derive",
            "width=100",
            "--derive",
            "width=100,format=jpeg",
        ])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
    assert_eq!(json_stdout(&out)["error"]["code"], "OUTPUT_NAME_COLLISION");
}

/// 子プロセスのピーク RSS(KB)。`tests/edge_quality.rs` の `resident_kb()` と
/// 同じ手法（`ps -o rss=`）を、走っている子へ向けたもの。
///
/// **同一プロセス内では測れない。** アロケータは解放したページを OS へ返さない
/// ので、N=1 の後に N=5 を回すと「確保済みの空き」を使い回して差が 0 としか
/// 出ない（edge_quality.rs の同じ注意書き）。新しいプロセスを毎回立てる
fn peak_child_kb(args: &[&str]) -> i64 {
    let mut child = std::process::Command::new(env!("CARGO_BIN_EXE_kiri"))
        .args(args)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let pid = child.id().to_string();
    let mut peak = 0i64;
    loop {
        let sample = std::process::Command::new("ps")
            .args(["-o", "rss=", "-p", &pid])
            .output()
            .ok()
            .and_then(|o| String::from_utf8(o.stdout).ok())
            .and_then(|s| s.trim().parse::<i64>().ok())
            .unwrap_or(0);
        peak = peak.max(sample);
        if let Some(status) = child.try_wait().unwrap() {
            assert!(status.success(), "{args:?} が失敗した");
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
    assert!(peak > 0, "{args:?} のピークを 1 度も採れなかった");
    peak
}

/// 受け入れ基準 (d)。**ピークメモリが派生の数に比例しない。**
///
/// 1 本ずつ「リサイズ → エンコード → 書き出し → 解放」を回しているかを、
/// 外から見える唯一の形で固定する。比例する実装なら N=5 は 1 本ぶんの作業領域を
/// 4 つ余分に抱えるので、増分は 1 本ぶんの実行全体と同じ桁になる。
///
/// **絶対値は書かない。** 機械によって基礎の常駐量が何倍も違ううえ、画像の
/// 大きさを変えれば数値も動く。見るのは「増分が N=1 のピークに対して十分小さい」
/// ことだけで、比例していれば桁で外れる。
///
/// # なぜ JPEG で測るか
///
/// **PNG の符号化はアロケータが 1 回あたり十数 MB を抱え込む。** 1400x1400 の
/// ノイズを PNG で 5 本書くと RSS は 48MB から 95MB へ伸びるが、これは派生の
/// 実装とは無関係で（同じ寸法の 5 本でも同じだけ伸びる）、`kiri batch` で同じ
/// 画像を 5 件並べても同じように伸びる。ここで確かめたいのは「リサイズ済み画像と
/// 符号化バッファを N 本ぶん同時に抱えていないか」なので、アロケータの癖が
/// 乗らない JPEG で測る。同じ条件の JPEG では 5 本と 1 本の差が 1MB を切る
#[test]
fn five_derivations_do_not_cost_five_times_the_peak_memory() {
    let dir = fixture_dir();
    // 1 本あたりの作業領域（リサイズ済み RGBA 7.8MB + 符号化バッファ）が
    // 基礎の常駐量に対して無視できない大きさになるようにしてある
    let input = write_png(dir.path(), "noise.png", &noisy_image(1400, 1400));
    let out = dir.path().join("out.jpg");

    let run = |widths: &[u32]| {
        let mut args: Vec<String> = vec![
            "convert".into(),
            input.display().to_string(),
            "-o".into(),
            out.display().to_string(),
            "--force".into(),
            "--naming".into(),
            "{index}.jpg".into(),
        ];
        for width in widths {
            args.push("--derive".into());
            args.push(format!("width={width},format=jpeg"));
        }
        let borrowed: Vec<&str> = args.iter().map(String::as_str).collect();
        peak_child_kb(&borrowed)
    };
    let one = run(&[1399]);
    let five = run(&[1399, 1398, 1397, 1396, 1395]);

    assert!(
        five - one < one / 2,
        "派生 5 本のピーク {five}KB が 1 本 {one}KB に対して増えすぎている\
         （比例していれば 1 本ぶんの作業領域が 4 つ積む）"
    );
}

/// 派生に紐づく警告は、必ず `data.output` で「どの出力の話か」を名乗る。
///
/// **1 実行で同じ code が複数回出るようになった。** どの派生の話かが分からない
/// 警告は分岐の材料にならないので、6 つそれぞれを実際に鳴らして確かめる。
/// 値は `outputs[].path` と同じ文字列でなければならない——別の綴りだと、
/// 受け手は突き合わせのために正規化を書くことになる
#[test]
fn every_derivation_bound_warning_names_its_output() {
    let dir = fixture_dir();
    let input = write_png(dir.path(), "p.png", &transparent_product(120, 120));
    let noisy = write_png(dir.path(), "n.png", &noisy_image(80, 80));
    let out = dir.path().join("o.png");

    let bound = |v: &Value, code: &str| {
        let paths: Vec<String> = v["outputs"]
            .as_array()
            .unwrap()
            .iter()
            .map(|o| o["path"].as_str().unwrap().to_string())
            .collect();
        let found: Vec<&Value> = v["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|w| w["code"] == code)
            .collect();
        assert!(!found.is_empty(), "{code} が出ていない: {v}");
        for w in found {
            let named = w["data"]["output"]
                .as_str()
                .unwrap_or_else(|| panic!("{code} が data.output を持たない: {w}"));
            assert!(
                paths.iter().any(|p| p == named),
                "{code} の data.output '{named}' が outputs[].path のどれとも一致しない: {v}"
            );
        }
    };

    // ALPHA_FLATTENED — 透過を JPEG へ書く
    bound(
        &convert_json(&input, &out, &["--sizes", "60", "--formats", "jpeg"]),
        "ALPHA_FLATTENED",
    );
    // QUALITY_REDUCED / MAX_BYTES_UNREACHABLE — 届く上限と届かない上限
    bound(
        &convert_json(
            &noisy,
            &out,
            &[
                "--derive",
                "format=jpeg,max_bytes=6k",
                "--naming",
                "{stem}_a.{ext}",
            ],
        ),
        "QUALITY_REDUCED",
    );
    bound(
        &convert_json(
            &noisy,
            &out,
            &[
                "--derive",
                "format=jpeg,max_bytes=64",
                "--naming",
                "{stem}_b.{ext}",
            ],
        ),
        "MAX_BYTES_UNREACHABLE",
    );
    // UPSCALED — 派生のリサイズで拡大した
    bound(
        &convert_json(
            &input,
            &out,
            &[
                "--derive",
                "width=200,allow_upscale=true",
                "--naming",
                "{stem}_c.{ext}",
            ],
        ),
        "UPSCALED",
    );
    // ICC_NOT_EMBEDDED — sRGB でない画素を書いた
    let wide = write_jpeg_with_icc(
        dir.path(),
        "wide.jpg",
        &product_image(&ProductSpec {
            width: 80,
            height: 80,
            ..Default::default()
        }),
        swapped_primaries_icc(),
    );
    bound(
        &convert_json(
            &wide,
            &out,
            &["--no-color-convert", "--sizes", "40,20", "--formats", "png"],
        ),
        "ICC_NOT_EMBEDDED",
    );
    // DRY_RUN_OUTPUT_EXISTS — 派生ごとに 1 回ずつ出る。**--force は付けない**
    // （付けると知らせるものが無くなる）
    let existing = convert_json(&input, &out, &["--sizes", "40,20", "--formats", "png"]);
    let dry = json_stdout(
        &kiri()
            .args([
                "convert",
                input.to_str().unwrap(),
                "-o",
                out.to_str().unwrap(),
                "--json",
                "--sizes",
                "40,20",
                "--formats",
                "png",
                "--dry-run",
            ])
            .output()
            .unwrap(),
    );
    bound(&dry, "DRY_RUN_OUTPUT_EXISTS");
    assert_eq!(
        warning_codes(&dry)
            .iter()
            .filter(|c| *c == "DRY_RUN_OUTPUT_EXISTS")
            .count(),
        existing["outputs"].as_array().unwrap().len(),
        "派生ごとに 1 回ずつ出ていない: {dry}"
    );

    // **`UPSCALED` だけは `data.output` を持たない枝がある。** `kiri resize` 自身の
    // 拡大は**最終画像そのもの**に起きたことで、派生ごとの事象ではない。無い帰属を
    // でっち上げて `output` を付けるほうが嘘になるので、持たないことを固定する
    // （`data.output` の有無がそのまま出どころの区別になる、と契約が言っている）
    let resized = kiri()
        .args([
            "resize",
            input.to_str().unwrap(),
            "-o",
            out.to_str().unwrap(),
            "--json",
            "--force",
            "--width",
            "200",
            "--allow-upscale",
            "--sizes",
            "160,200",
        ])
        .output()
        .unwrap();
    assert!(
        resized.status.success(),
        "{}",
        String::from_utf8_lossy(&resized.stderr)
    );
    let v = json_stdout(&resized);
    let upscaled: Vec<&Value> = v["warnings"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|w| w["code"] == "UPSCALED")
        .collect();
    assert_eq!(
        upscaled.len(),
        1,
        "resize 段の UPSCALED が 1 本だけ出る: {v}"
    );
    assert!(
        upscaled[0]["data"].get("output").is_none(),
        "resize 自身の拡大に出力を帰属させている: {v}"
    );
}

/// `--derive` と `--sizes` は構造として混ぜられない。
///
/// 2 つの組み立て方が同時に効くと「どちらが勝つか」という覚える規則が増える。
/// **clap が弾くので code 無しの exit 2 になる**（`--max-bytes` の書式違いと
/// 同じ前例で、stdout は空のまま）
#[test]
fn derive_and_the_product_flags_cannot_be_mixed() {
    let dir = fixture_dir();
    let input = write_jpeg(dir.path(), "p.jpg", &product_image(&ProductSpec::default()));
    for other in [["--sizes", "100"], ["--formats", "png"]] {
        let out = kiri()
            .args([
                "convert",
                input.to_str().unwrap(),
                "-o",
                dir.path().join("q.png").to_str().unwrap(),
                "--json",
                "--derive",
                "width=100",
                other[0],
                other[1],
            ])
            .output()
            .unwrap();
        assert_eq!(out.status.code(), Some(2), "{other:?}");
        assert!(out.stdout.is_empty(), "{other:?} で stdout が出た");
    }
}

/// `--derive` の綴り違いと読めない値は、clap が code 無しの exit 2 で断る。
#[test]
fn a_malformed_derivation_on_the_command_line_is_refused_by_the_parser() {
    let dir = fixture_dir();
    let input = write_jpeg(dir.path(), "p.jpg", &product_image(&ProductSpec::default()));
    for spec in [
        "widht=100",
        "width=0",
        "width",
        "quality=200",
        "effort=0",
        "fit=exact",
        "format=webp",
        "max_bytes=1.5m",
        "",
    ] {
        let out = kiri()
            .args([
                "convert",
                input.to_str().unwrap(),
                "-o",
                dir.path().join("q.png").to_str().unwrap(),
                "--json",
                "--derive",
                spec,
            ])
            .output()
            .unwrap();
        assert_eq!(out.status.code(), Some(2), "--derive '{spec}' が通った");
        assert!(out.stdout.is_empty(), "--derive '{spec}' で stdout が出た");
    }
}

/// `--naming` の綴り違いは、重い処理の前に `INVALID_NAMING_TEMPLATE` で断る。
#[test]
fn a_malformed_naming_template_is_refused_with_a_code() {
    let dir = fixture_dir();
    let input = write_jpeg(dir.path(), "p.jpg", &product_image(&ProductSpec::default()));
    let out_dir = TempDir::new().unwrap();
    for template in ["{stem}_{wdith}.{ext}", "{stem}.{ext", "{stem}-{role}.{ext}"] {
        let out = kiri()
            .args([
                "convert",
                input.to_str().unwrap(),
                "-o",
                out_dir.path().join("q.png").to_str().unwrap(),
                "--json",
                "--naming",
                template,
            ])
            .output()
            .unwrap();
        assert_eq!(out.status.code(), Some(2), "{template}");
        let v = json_stdout(&out);
        assert_eq!(v["error"]["code"], "INVALID_NAMING_TEMPLATE", "{v}");
        assert_eq!(
            std::fs::read_dir(out_dir.path()).unwrap().count(),
            0,
            "{template}: 断る前にファイルを書いている"
        );
    }
}

/// 派生のパスが付随出力と重なったら `SIDE_OUTPUT_CONFLICT` で断る。
#[test]
fn a_derivation_that_lands_on_a_side_output_is_refused() {
    let dir = fixture_dir();
    let input = write_jpeg(dir.path(), "p.jpg", &product_image(&ProductSpec::default()));
    let out_dir = TempDir::new().unwrap();
    let out = kiri()
        .args([
            "cutout",
            input.to_str().unwrap(),
            "-o",
            out_dir.path().join("p.png").to_str().unwrap(),
            "--json",
            "--naming",
            "{stem}.{ext}",
            "--preview",
            out_dir.path().join("p.png").to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
    assert_eq!(
        json_stdout(&out)["error"]["code"],
        "SIDE_OUTPUT_CONFLICT",
        "{}",
        String::from_utf8_lossy(&out.stdout)
    );
}

/// `--manifest` は本出力と同じ上書きの規約に従い、`--dry-run` では 1 バイトも書かない。
#[test]
fn the_manifest_obeys_the_overwrite_rules_and_dry_run() {
    let dir = fixture_dir();
    let input = write_jpeg(dir.path(), "p.jpg", &product_image(&ProductSpec::default()));
    let output = dir.path().join("out.png");
    let manifest = dir.path().join("m.json");

    // **--force を足さずに回す。** 上書きの規約そのものを見る検査なので、
    // 検査を黙らせる指定を付けたままでは何も確かめられない
    let run = |extra: &[&str]| -> std::process::Output {
        let mut args = vec![
            "convert",
            input.to_str().unwrap(),
            "-o",
            output.to_str().unwrap(),
            "--json",
            "--manifest",
            manifest.to_str().unwrap(),
        ];
        args.extend_from_slice(extra);
        kiri().args(&args).output().unwrap()
    };

    // dry-run では書かない。まだ無いのだから警告も出ない
    let out = run(&["--dry-run"]);
    assert!(out.status.success());
    assert!(!manifest.exists(), "dry-run でマニフェストを書いた");
    assert!(!has_warning(&json_stdout(&out), "DRY_RUN_OUTPUT_EXISTS"));

    let out = run(&[]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(manifest.is_file());

    // 既にあるなら dry-run は先に知らせる
    let v = json_stdout(&run(&["--dry-run", "--force"]));
    assert!(
        !has_warning(&v, "DRY_RUN_OUTPUT_EXISTS"),
        "--force なら知らせるものが無い: {v}"
    );
    std::fs::remove_file(&output).unwrap();
    let v = json_stdout(&run(&["--dry-run"]));
    let named: Vec<&Value> = v["warnings"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|w| w["code"] == "DRY_RUN_OUTPUT_EXISTS")
        .collect();
    assert_eq!(named.len(), 1, "本出力は消したので残るのは目録の 1 本: {v}");
    assert_eq!(
        named[0]["data"]["output"],
        manifest.to_str().unwrap(),
        "{v}"
    );

    // 本番実行は --force が無ければ断る（本出力と同じ規約）
    let out = run(&[]);
    assert_eq!(out.status.code(), Some(2));
    assert_eq!(json_stdout(&out)["error"]["code"], "OUTPUT_EXISTS");
    assert!(run(&["--force"]).status.success(), "--force なら上書きする");
}

/// batch は失敗した項目を目録へ載せず、`MANIFEST_PARTIAL` で欠けを言う。
///
/// **目録だけを見て「これで全部だ」と読まれるのが最も高くつく。** 件数まで
/// 添えて、受け手が分岐を書ける形にしてある
#[test]
fn a_partial_batch_says_so_in_the_manifest_warning() {
    let dir = fixture_dir();
    let good = write_jpeg(
        dir.path(),
        "good.jpg",
        &product_image(&ProductSpec::default()),
    );
    let spec = dir.path().join("spec.json");
    let manifest = dir.path().join("m.json");
    std::fs::write(
        &spec,
        format!(
            r#"{{"items":[
                {{"input":"{}","output":"out/a.png"}},
                {{"input":"missing.jpg","output":"out/b.png"}}
            ]}}"#,
            good.file_name().unwrap().to_str().unwrap()
        ),
    )
    .unwrap();

    let out = kiri()
        .args([
            "batch",
            spec.to_str().unwrap(),
            "--json",
            "--manifest",
            manifest.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(4), "1 件失敗したら exit 4");
    let v = json_stdout(&out);
    assert_eq!(v["failed"], 1, "{v}");
    let partial: Vec<&Value> = v["warnings"]
        .as_array()
        .unwrap_or_else(|| panic!("batch が warnings を持たない: {v}"))
        .iter()
        .filter(|w| w["code"] == "MANIFEST_PARTIAL")
        .collect();
    assert_eq!(partial.len(), 1, "{v}");
    assert_eq!(partial[0]["data"]["failed"], 1, "{v}");
    assert_eq!(partial[0]["data"]["manifest"], manifest.to_str().unwrap());

    let written: Value = serde_json::from_slice(&std::fs::read(&manifest).unwrap()).unwrap();
    assert_eq!(
        written["items"].as_array().unwrap().len(),
        1,
        "失敗した項目が目録に載っている: {written}"
    );
}

/// batch は**1 件も処理する前に**目録の上書き可否を問う。
///
/// 書き終えてから断ると、`OUTPUT_EXISTS` が `Err` として返って `BatchReport` が
/// 丸ごと捨てられる。利用者に残るのはエラー 1 行だけで、**数百枚が書かれた事実も、
/// どれが成功しどれが失敗したかも一切返らない**。成果物が 1 つも増えていないこと
/// まで見る
#[test]
fn the_batch_manifest_overwrite_check_runs_before_any_item_is_written() {
    let dir = fixture_dir();
    let input = write_jpeg(dir.path(), "p.jpg", &product_image(&ProductSpec::default()));
    let name = input.file_name().unwrap().to_str().unwrap().to_string();
    let spec = dir.path().join("spec.json");
    std::fs::write(
        &spec,
        format!(
            r#"{{"items":[
                {{"input":"{name}","output":"out/o1.png"}},
                {{"input":"{name}","output":"out/o2.png"}}
            ]}}"#
        ),
    )
    .unwrap();
    let manifest = dir.path().join("m.json");
    std::fs::write(&manifest, b"{\"stale\":true}").unwrap();

    let out = kiri()
        .args([
            "batch",
            spec.to_str().unwrap(),
            "--json",
            "--manifest",
            manifest.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
    assert_eq!(json_stdout(&out)["error"]["code"], "OUTPUT_EXISTS");

    let written = dir.path().join("out");
    assert!(
        !written.exists() || std::fs::read_dir(&written).unwrap().count() == 0,
        "断る前に項目を書いている"
    );
    assert_eq!(
        std::fs::read(&manifest).unwrap(),
        b"{\"stale\":true}",
        "既存の目録を壊している"
    );
}

/// `--debug-mask` は命名の検査より後に書かれる。
///
/// README の「どれで落ちてもファイルは 1 つも書かれない」は cutout でも成り立つ。
/// **綴り違いは切り抜き本体より前に捕まえる**——テンプレートの解析も `{role}` の
/// 検査も寸法に一切依存しないのに、書き出しの直前でやると `--optimize` 込みで
/// 数秒〜十数秒を捨てることになる
#[test]
fn a_cutout_that_fails_the_naming_check_writes_no_debug_mask() {
    let dir = fixture_dir();
    let input = write_jpeg(dir.path(), "p.jpg", &product_image(&ProductSpec::default()));
    let out_dir = TempDir::new().unwrap();
    let result = kiri()
        .args([
            "cutout",
            input.to_str().unwrap(),
            "-o",
            out_dir.path().join("out.png").to_str().unwrap(),
            "--json",
            "--debug-mask",
            out_dir.path().join("mask.png").to_str().unwrap(),
            "--sizes",
            "200,400",
            "--naming",
            "{stem}_{dpi}.{ext}",
        ])
        .output()
        .unwrap();
    assert_eq!(result.status.code(), Some(2));
    assert_eq!(
        json_stdout(&result)["error"]["code"],
        "INVALID_NAMING_TEMPLATE"
    );
    assert_eq!(
        std::fs::read_dir(out_dir.path()).unwrap().count(),
        0,
        "断る前にマスクを書いている"
    );
}

/// 綴った名前が `--output` の親から出ていく指定は断る。
///
/// `Path::join` は引数が絶対パスなら基底を捨てるので、素通しにすると `{role}` や
/// `--naming` の 1 語で任意の場所へ書ける。**spec はエージェントや他人が生成しうる
/// データファイル**なので、`--base-dir` の境界をこの 1 行で越えられてはならない
#[test]
fn a_name_that_leaves_the_output_directory_is_refused() {
    let dir = fixture_dir();
    let input = write_jpeg(dir.path(), "p.jpg", &product_image(&ProductSpec::default()));
    let out_dir = TempDir::new().unwrap();
    let escape = std::env::temp_dir().join("kiri_escape_test.png");
    let _ = std::fs::remove_file(&escape);

    for template in [
        escape.with_extension("{ext}").to_str().unwrap().to_string(),
        "../escaped.{ext}".to_string(),
        "sub/{stem}.{ext}".to_string(),
    ] {
        let out = kiri()
            .args([
                "convert",
                input.to_str().unwrap(),
                "-o",
                out_dir.path().join("q.png").to_str().unwrap(),
                "--json",
                "--naming",
                &template,
            ])
            .output()
            .unwrap();
        assert_eq!(out.status.code(), Some(2), "{template}");
        assert_eq!(
            json_stdout(&out)["error"]["code"],
            "INVALID_NAMING_TEMPLATE",
            "{template}"
        );
    }
    assert!(!escape.exists(), "--output の親の外へ書いた");
    assert_eq!(
        std::fs::read_dir(out_dir.path()).unwrap().count(),
        0,
        "断る前にファイルを書いている"
    );

    // `role` は `outputs[].role` にも出る札なので、もっと早い段で断る
    // （clap の value_parser が読むので code 無しの exit 2 になる）
    for spec in ["width=60,role=/tmp/kiri_escape_test", "width=60,role=../x"] {
        let out = kiri()
            .args([
                "convert",
                input.to_str().unwrap(),
                "-o",
                out_dir.path().join("q.png").to_str().unwrap(),
                "--json",
                "--derive",
                spec,
                "--naming",
                "{role}.{ext}",
            ])
            .output()
            .unwrap();
        assert_eq!(out.status.code(), Some(2), "{spec}");
        assert!(out.stdout.is_empty(), "{spec} で stdout が出た");
    }
}

/// 大文字小文字だけが違う 2 本も衝突として断る。
///
/// macOS（APFS の既定）や Windows では区別されないので同じ 1 ファイルへ落ちる。
/// 通せば **JSON は 2 本書いたと報告し、ディスクには 1 本しか無い**——機械可読な
/// レポートが嘘をつくのはこのコードベースで最も重い失敗である
#[test]
fn two_derivations_differing_only_in_case_are_a_collision() {
    let dir = fixture_dir();
    let input = write_jpeg(dir.path(), "p.jpg", &product_image(&ProductSpec::default()));
    let out_dir = TempDir::new().unwrap();
    let out = kiri()
        .args([
            "convert",
            input.to_str().unwrap(),
            "-o",
            out_dir.path().join("x.jpg").to_str().unwrap(),
            "--json",
            "--derive",
            "width=60,role=Hero",
            "--derive",
            "width=60,role=hero",
            "--naming",
            "{stem}_{role}.{ext}",
        ])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
    let v = json_stdout(&out);
    assert_eq!(v["error"]["code"], "OUTPUT_NAME_COLLISION", "{v}");
    assert!(
        v["error"]["message"]
            .as_str()
            .unwrap()
            .contains("大文字小文字"),
        "なぜ衝突なのかを言っていない: {v}"
    );
    assert_eq!(
        std::fs::read_dir(out_dir.path()).unwrap().count(),
        0,
        "断る前にファイルを書いている"
    );
}

/// 対応する `{` の無い `}` は、テンプレートのどこにあっても断る。
///
/// 「`{` を探してその後ろの `}` を探す」形の走査では、**最後の置換子より前に
/// ある** `}` を一度も見ない。`stem}_{width}.{ext}` が「`stem}_` で始まる名前」
/// として書き出されるのは、まさに実装コメントが挙げていた反例である
#[test]
fn an_unmatched_closing_brace_is_refused_anywhere_in_the_template() {
    let dir = fixture_dir();
    let input = write_jpeg(dir.path(), "p.jpg", &product_image(&ProductSpec::default()));
    let out_dir = TempDir::new().unwrap();
    for template in ["stem}_{width}.{ext}", "}{stem}.{ext}"] {
        let out = kiri()
            .args([
                "convert",
                input.to_str().unwrap(),
                "-o",
                out_dir.path().join("q.png").to_str().unwrap(),
                "--json",
                "--sizes",
                "60,80",
                "--naming",
                template,
            ])
            .output()
            .unwrap();
        assert_eq!(out.status.code(), Some(2), "{template}");
        let v = json_stdout(&out);
        assert_eq!(v["error"]["code"], "INVALID_NAMING_TEMPLATE", "{v}");
        assert_eq!(
            std::fs::read_dir(out_dir.path()).unwrap().count(),
            0,
            "{template}: 断る前にファイルを書いている"
        );
    }
}

/// `--derive` の同じキーを 2 回書いたら断る。
///
/// 黙って後勝ちにすると「書いたのに効かない」指定がここにだけ残る。未知のキーを
/// きちんと断っているのだから、緩める理由が無い。**clap が弾くので code 無しの
/// exit 2 になる**（`--max-bytes` の書式違いと同じ前例）
#[test]
fn a_derivation_key_written_twice_is_refused() {
    let dir = fixture_dir();
    let input = write_jpeg(dir.path(), "p.jpg", &product_image(&ProductSpec::default()));
    for spec in [
        "width=100,width=250",
        "format=png,format=jpeg",
        "role=a,role=b",
    ] {
        let out = kiri()
            .args([
                "convert",
                input.to_str().unwrap(),
                "-o",
                dir.path().join("q.png").to_str().unwrap(),
                "--json",
                "--derive",
                spec,
            ])
            .output()
            .unwrap();
        assert_eq!(out.status.code(), Some(2), "--derive '{spec}' が通った");
        assert!(out.stdout.is_empty(), "--derive '{spec}' で stdout が出た");
    }
}

/// spec の `derive` / `sizes` / `formats` / `naming` が CLI と同じ関門を通る。
#[test]
fn a_spec_builds_derivations_through_the_same_gate() {
    let dir = fixture_dir();
    let input = write_jpeg(dir.path(), "p.jpg", &product_image(&ProductSpec::default()));
    let name = input.file_name().unwrap().to_str().unwrap().to_string();
    let spec = dir.path().join("spec.json");

    let run = |body: &str| -> std::process::Output {
        std::fs::write(&spec, body).unwrap();
        kiri()
            .args(["batch", spec.to_str().unwrap(), "--json", "--force"])
            .output()
            .unwrap()
    };

    let out = run(&format!(
        r#"{{"items":[{{"input":"{name}","output":"out/p.png",
             "derive":[{{"width":60,"format":"jpeg","quality":82,"max_bytes":"500k","role":"hero"}},
                       {{"width":30,"role":"thumb"}}],
             "naming":"{{stem}}-{{role}}.{{ext}}"}}]}}"#
    ));
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let v = json_stdout(&out);
    let outputs = v["results"][0]["result"]["outputs"].as_array().unwrap();
    assert_eq!(outputs.len(), 2, "{v}");
    assert_eq!(outputs[0]["role"], "hero");
    assert_eq!(outputs[0]["format"], "jpeg");
    assert_eq!(outputs[0]["quality_used"], 82.0);
    assert!(outputs[0]["path"].as_str().unwrap().ends_with("p-hero.jpg"));
    assert_eq!(outputs[1]["role"], "thumb");
    assert!(
        outputs[1]["path"]
            .as_str()
            .unwrap()
            .ends_with("p-thumb.png")
    );

    // 未知のキーと同時指定は INVALID_DERIVATION でその項目を落とす
    for body in [
        format!(
            r#"{{"items":[{{"input":"{name}","output":"out/q.png","derive":[{{"widht":60}}]}}]}}"#
        ),
        format!(
            r#"{{"items":[{{"input":"{name}","output":"out/q.png","derive":[{{"width":60}}],"sizes":[20]}}]}}"#
        ),
    ] {
        let out = run(&body);
        let v = json_stdout(&out);
        assert_eq!(
            v["results"][0]["error"]["code"], "INVALID_DERIVATION",
            "{v}"
        );
    }
}

/// 成功と失敗の両方の JSON が `schema_version` 2 を名乗る。
///
/// **`outputs[]` が常に 1 要素という前提が崩れた版である。** 古い読み手が
/// `outputs[0]` だけを読んで 2 本目以降を捨てるのを、版で断つ
#[test]
fn the_schema_version_is_two_everywhere() {
    let dir = fixture_dir();
    let input = write_jpeg(dir.path(), "p.jpg", &product_image(&ProductSpec::default()));
    let ok = convert_json(&input, &dir.path().join("o.png"), &[]);
    assert_eq!(ok["schema_version"], 2, "{ok}");
    assert_eq!(schema_json()["schema_version"], 2);

    let bad = kiri()
        .args(["convert", "/nonexistent/nope.jpg", "-o", "x.png", "--json"])
        .output()
        .unwrap();
    assert_eq!(json_stdout(&bad)["schema_version"], 2);
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

/// PNG のチャンクの型を並べる。
fn png_chunk_kinds(bytes: &[u8]) -> Vec<[u8; 4]> {
    assert_eq!(&bytes[..8], b"\x89PNG\r\n\x1a\n", "PNG ではない");
    let mut kinds = Vec::new();
    let mut i = 8;
    while i < bytes.len() {
        let len = u32::from_be_bytes(bytes[i..i + 4].try_into().unwrap()) as usize;
        kinds.push(bytes[i + 4..i + 8].try_into().unwrap());
        i += 12 + len;
    }
    kinds
}

/// 指定した型のチャンクを落とす。CRC はチャンクごと持ち運ぶので計算し直さない
fn png_without_chunk(bytes: &[u8], kind: &[u8; 4]) -> Vec<u8> {
    let mut out = bytes[..8].to_vec();
    let mut i = 8;
    while i < bytes.len() {
        let len = u32::from_be_bytes(bytes[i..i + 4].try_into().unwrap()) as usize;
        if &bytes[i + 4..i + 8] != kind {
            out.extend_from_slice(&bytes[i..i + 12 + len]);
        }
        i += 12 + len;
    }
    out
}

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
    assert_eq!(v["outputs"][0]["icc"], "embedded");
    let written = std::fs::read(&output).unwrap();
    assert_eq!(
        png_chunk_kinds(&written)
            .iter()
            .filter(|k| *k == b"iCCP")
            .count(),
        1
    );
    assert_eq!(
        std::fs::read(&input).unwrap(),
        png_without_chunk(&written, b"iCCP"),
        "1 周は何もしないのと同じでなければならない（sRGB の名乗りを除く）"
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
/// 理由（細すぎる通路）で塞ぐので、堤防だけを見る周では切ってある。
///
/// **既定の経路でも同じ問いを立てる。** 利用者が受け取るのは既定の経路の
/// 出力であり、そこでスリットが透明になるなら「幅 2N px 以下の隙間を前景へ
/// 戻す」という `--seal` の約束は破れている。
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
    // **既定の経路で問う。** 利用者が受け取るのはこれである。
    //
    // 既定では帯の中のアルファを色から解き直すので、背景色そのもので彫られた
    // スリットには (b) が「背景」と答える。それを連結性で前景へ戻すのが
    // `close_new_gaps` で、戻した画素は帯から外すため二値のまま不透明で残る
    // （`refine::close_new_gaps`）。ここが崩れると、README の「幅 2N px 以下の
    // 隙間を前景へ戻す」が境界処理の中で黙って取り消される
    assert!(
        !slit_is_background(&[]),
        "既定の経路で 1px のスリットが透明になっている（--seal の約束が破れている）"
    );
    // **`--seal 0` は「塞がない」である。** 上が通ったのが seal のおかげだと
    // 言うには、切ったときに通らないことまで見なければならない
    assert!(
        slit_is_background(&["--seal", "0"]),
        "--seal 0 なのにスリットが塞がっている"
    );
    // **堤防だけを見る。** 堤防が守るのはフィルであって境界のアルファでは
    // ないので、色で決め直す 3 段と `--seal` を切ったうえで問う。ここが
    // 二値マスクの上での堤防の働きそのものである
    assert!(
        !slit_is_background(&[
            "--seal",
            "0",
            "--no-reclassify",
            "--smooth-contour",
            "0",
            "--matting",
            "projection",
        ]),
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

// --- 空間的な指示（トライマップ / マスク画像 / ポリゴン） ---

/// 指示つきの実験に使う素材。200x200 の中央に角丸の商品が載っている。
///
/// 商品はおおよそ x 44-156 / y 32-168 を占める。指示の座標はこれを基準に置く。
fn constraint_fixture(dir: &Path) -> PathBuf {
    let img = product_image(&ProductSpec {
        width: 200,
        height: 200,
        ..Default::default()
    });
    write_png(dir, "in.png", &img)
}

/// グレー画像を書き出す。トライマップとマスクはこれで作る。
fn write_gray(dir: &Path, name: &str, width: u32, height: u32, value: u8) -> PathBuf {
    let img = image::RgbaImage::from_pixel(width, height, image::Rgba([value, value, value, 255]));
    write_png(dir, name, &img)
}

/// EXIF Orientation を持つ JPEG を書く。
///
/// APP1 セグメントを SOI の直後へ差し込むだけ。**画素は回さない**ので、
/// 「向きの申告だけがある画像」になる——`--trimap` / `--fg-mask` が寸法の
/// 検査を素通りしてしまう状況そのものである。
fn write_jpeg_with_orientation(
    dir: &Path,
    name: &str,
    img: &image::RgbaImage,
    orientation: u16,
) -> PathBuf {
    use image::ImageEncoder;

    let rgb = image::DynamicImage::ImageRgba8(img.clone()).to_rgb8();
    let mut jpeg = Vec::new();
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut jpeg, 90)
        .write_image(
            rgb.as_raw(),
            rgb.width(),
            rgb.height(),
            image::ExtendedColorType::Rgb8,
        )
        .unwrap();

    // TIFF ヘッダ（リトルエンディアン）+ IFD0 に Orientation ただ 1 つ
    let [lo, hi] = orientation.to_le_bytes();
    let mut tiff: Vec<u8> = vec![0x49, 0x49, 0x2A, 0x00, 0x08, 0x00, 0x00, 0x00];
    tiff.extend_from_slice(&[0x01, 0x00]); // エントリ数
    tiff.extend_from_slice(&[0x12, 0x01]); // タグ 0x0112 = Orientation
    tiff.extend_from_slice(&[0x03, 0x00]); // 型 3 = SHORT
    tiff.extend_from_slice(&[0x01, 0x00, 0x00, 0x00]); // 個数 1
    tiff.extend_from_slice(&[lo, hi, 0x00, 0x00]); // 値（4 バイト枠の先頭 2 バイト）
    tiff.extend_from_slice(&[0x00, 0x00, 0x00, 0x00]); // 次の IFD は無し

    let mut app1: Vec<u8> = vec![0xFF, 0xE1];
    let length = u16::try_from(tiff.len() + 8).unwrap();
    app1.extend_from_slice(&length.to_be_bytes());
    app1.extend_from_slice(b"Exif\0\0");
    app1.extend_from_slice(&tiff);

    let mut out = Vec::with_capacity(jpeg.len() + app1.len());
    out.extend_from_slice(&jpeg[..2]); // SOI
    out.extend_from_slice(&app1);
    out.extend_from_slice(&jpeg[2..]);

    let path = dir.join(name);
    std::fs::write(&path, out).unwrap();
    path
}

fn paint(img: &mut image::RgbaImage, rect: (u32, u32, u32, u32), value: u8) {
    let (x1, y1, x2, y2) = rect;
    for y in y1..=y2 {
        for x in x1..=x2 {
            img.put_pixel(x, y, image::Rgba([value, value, value, 255]));
        }
    }
}

fn run_cutout(args: &[&str]) -> std::process::Output {
    let mut full = vec!["cutout"];
    full.extend_from_slice(args);
    full.push("--json");
    kiri().args(&full).output().unwrap()
}

/// トライマップで切り抜けること。
///
/// 確定前景は不透明のまま残り、確定背景は透明になる。**その 2 つが同時に
/// 成り立たなければ、指示は届いていない。** 片方だけなら、たまたま色で
/// そうなっただけということがありうる。
#[test]
fn a_trimap_decides_both_sides() {
    let dir = fixture_dir();
    let input = constraint_fixture(dir.path());
    let output = dir.path().join("cut.png");

    // 中央の 60x60 を確定前景、外周 20px を確定背景、あいだは不明
    let mut trimap = image::RgbaImage::from_pixel(200, 200, image::Rgba([128, 128, 128, 255]));
    paint(&mut trimap, (0, 0, 199, 19), 0);
    paint(&mut trimap, (0, 180, 199, 199), 0);
    paint(&mut trimap, (70, 70, 129, 129), 255);
    let path = write_png(dir.path(), "trimap.png", &trimap);

    let out = run_cutout(&[
        input.to_str().unwrap(),
        "-o",
        output.to_str().unwrap(),
        "--trimap",
        path.to_str().unwrap(),
    ]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let v = json_stdout(&out);
    assert_eq!(v["constraints"]["sources"][0], "trimap");
    assert!(v["constraints"]["fg_ratio"].as_f64().unwrap() > 0.0);
    assert!(v["constraints"]["bg_ratio"].as_f64().unwrap() > 0.0);
    assert!(
        v["constraints"]["unknown_ratio"].as_f64().unwrap() > 0.0,
        "不明の帯が消えている: {v}"
    );

    let cut = image::open(&output).unwrap().to_rgba8();
    assert_eq!(cut.get_pixel(100, 100)[3], 255, "確定前景が透けている");
    assert_eq!(cut.get_pixel(5, 5)[3], 0, "確定背景が残っている");
}

/// 切り抜き済みのアルファを指示として読めること（`--alpha-trimap`）。
///
/// **`--trimap` と同じ約束を、輝度ではなくアルファで確かめる。** 不透明は
/// 確定前景、透明は確定背景、半透明は不明の 3 つが同時に成り立たなければ、
/// 入口は届いていない。
#[test]
fn an_alpha_trimap_decides_both_sides() {
    let dir = fixture_dir();
    let input = constraint_fixture(dir.path());
    let output = dir.path().join("cut.png");

    // 中央の 60x60 を不透明（確定前景）、外周 20px を透明（確定背景）、
    // あいだは半透明（不明）。**色はすべて同じにする**——輝度で読まれていたら
    // 1 画素も塗られず、`CONSTRAINT_EMPTY` が出て落ちる
    let mut alpha = image::RgbaImage::from_pixel(200, 200, image::Rgba([90, 90, 90, 128]));
    for (rect, a) in [
        ((0u32, 0u32, 199u32, 19u32), 0u8),
        ((0, 180, 199, 199), 0),
        ((70, 70, 129, 129), 255),
    ] {
        for y in rect.1..=rect.3 {
            for x in rect.0..=rect.2 {
                alpha.get_pixel_mut(x, y).0[3] = a;
            }
        }
    }
    let path = write_png(dir.path(), "cutout.png", &alpha);

    let out = run_cutout(&[
        input.to_str().unwrap(),
        "-o",
        output.to_str().unwrap(),
        "--alpha-trimap",
        path.to_str().unwrap(),
    ]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let v = json_stdout(&out);
    assert_eq!(v["constraints"]["sources"][0], "alpha_trimap");
    assert!(v["constraints"]["fg_ratio"].as_f64().unwrap() > 0.0);
    assert!(v["constraints"]["bg_ratio"].as_f64().unwrap() > 0.0);
    assert!(
        v["constraints"]["unknown_ratio"].as_f64().unwrap() > 0.0,
        "不明の帯が消えている: {v}"
    );

    let cut = image::open(&output).unwrap().to_rgba8();
    assert_eq!(cut.get_pixel(100, 100)[3], 255, "確定前景が透けている");
    assert_eq!(cut.get_pixel(5, 5)[3], 0, "確定背景が残っている");
}

/// **全画素が不透明な画像は断る。** アルファを持たない JPEG を
/// `--alpha-trimap` へ渡すと、そのまま読めば画像全体が確定前景になる。
/// 「指示が効かない」ではなく「何も切り抜かれない」が起きるので黙って進めない。
#[test]
fn an_opaque_image_is_refused_as_an_alpha_trimap() {
    let dir = fixture_dir();
    let input = constraint_fixture(dir.path());
    let output = dir.path().join("cut.png");
    let opaque = image::RgbaImage::from_pixel(200, 200, image::Rgba([10, 200, 10, 255]));
    let path = write_png(dir.path(), "opaque.png", &opaque);

    let out = run_cutout(&[
        input.to_str().unwrap(),
        "-o",
        output.to_str().unwrap(),
        "--alpha-trimap",
        path.to_str().unwrap(),
    ]);
    assert!(!out.status.success());
    let v = json_stdout(&out);
    assert_eq!(v["error"]["code"], "CONSTRAINT_ALL_OPAQUE");
    assert!(
        v["error"]["hint"].as_str().unwrap().contains("--trimap"),
        "輝度で読む入口への案内が無い: {v}"
    );
    assert!(!output.exists(), "断ったのに書き出している");
}

/// **同じファイルを 2 通りに読まない。** 切り抜き済み PNG を `--trimap` へ
/// 渡すと輝度で読まれ、黒い商品が確定背景になって指示が裏返る。入口が
/// 分かれていること自体を、出力の違いとして固定する。
#[test]
fn the_two_trimap_entries_read_the_same_file_differently() {
    let dir = fixture_dir();
    let input = constraint_fixture(dir.path());

    // 中央 60x60 だけが不透明な「暗い商品の切り抜き」。輝度で読めば全面が
    // 確定背景（30 <= 63）、アルファで読めば中央だけが確定前景になる
    let mut cut = image::RgbaImage::from_pixel(200, 200, image::Rgba([30, 30, 30, 0]));
    for y in 70..=129 {
        for x in 70..=129 {
            cut.get_pixel_mut(x, y).0[3] = 255;
        }
    }
    let path = write_png(dir.path(), "dark_cut.png", &cut);

    let by_alpha = run_cutout(&[
        input.to_str().unwrap(),
        "-o",
        dir.path().join("a.png").to_str().unwrap(),
        "--alpha-trimap",
        path.to_str().unwrap(),
    ]);
    let by_luma = run_cutout(&[
        input.to_str().unwrap(),
        "-o",
        dir.path().join("b.png").to_str().unwrap(),
        "--trimap",
        path.to_str().unwrap(),
    ]);
    assert!(by_alpha.status.success() && by_luma.status.success());
    let a = json_stdout(&by_alpha);
    let b = json_stdout(&by_luma);
    assert_eq!(a["constraints"]["sources"][0], "alpha_trimap");
    assert_eq!(b["constraints"]["sources"][0], "trimap");
    // アルファで読めば中央が確定前景、輝度で読めば確定前景は 1 画素も無い
    assert!(a["constraints"]["fg_ratio"].as_f64().unwrap() > 0.0);
    assert_eq!(b["constraints"]["fg_ratio"].as_f64().unwrap(), 0.0);
    assert!(b["constraints"]["bg_ratio"].as_f64().unwrap() > 0.99);
}

/// **一度切ったものを戻しても、確定した画素は動かない。**
///
/// `--alpha-trimap` の用途は「別の道具（あるいは前回の kiri）が出した切り抜きを、
/// 境界だけ解き直す」である。戻したときに不透明だった画素が透けたり、透明だった
/// 画素が戻ったりすれば、その用途は成り立たない。
#[test]
fn a_cutout_fed_back_as_an_alpha_trimap_keeps_what_it_decided() {
    let dir = fixture_dir();
    let input = constraint_fixture(dir.path());
    let first = dir.path().join("first.png");
    let second = dir.path().join("second.png");

    let out = run_cutout(&[input.to_str().unwrap(), "-o", first.to_str().unwrap()]);
    assert!(out.status.success());

    let out = run_cutout(&[
        input.to_str().unwrap(),
        "-o",
        second.to_str().unwrap(),
        "--alpha-trimap",
        first.to_str().unwrap(),
    ]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        json_stdout(&out)["constraints"]["sources"][0],
        "alpha_trimap"
    );

    let a = image::open(&first).unwrap().to_rgba8();
    let b = image::open(&second).unwrap().to_rgba8();
    assert_eq!(a.dimensions(), b.dimensions());
    let (mut kept_fg, mut kept_bg) = (0u32, 0u32);
    for (p, q) in a.pixels().zip(b.pixels()) {
        if p.0[3] >= 250 {
            assert_eq!(q.0[3], 255, "確定前景が透けた");
            kept_fg += 1;
        } else if p.0[3] <= 5 {
            assert_eq!(q.0[3], 0, "確定背景が戻った");
            kept_bg += 1;
        }
    }
    assert!(kept_fg > 0 && kept_bg > 0, "確定領域が両側とも無い");
}

/// 商品の色をした画素でも、確定背景と言われれば消えること。
///
/// **色では区別できないものを空間で教えるのが、この入口の存在理由である。**
/// 商品と同じ色の小道具を指しても、そこから商品へは色の規則で進めないので
/// 商品は残る。
#[test]
fn a_background_mask_removes_a_prop_that_matches_the_product() {
    let dir = fixture_dir();
    let mut img = product_image(&ProductSpec {
        width: 200,
        height: 200,
        ..Default::default()
    });
    // 商品と同じ色の小道具を左上に置く（商品からは離れている）
    let product = img.get_pixel(100, 100).0;
    for y in 10..30 {
        for x in 10..30 {
            img.put_pixel(x, y, image::Rgba(product));
        }
    }
    let input = write_png(dir.path(), "in.png", &img);
    let output = dir.path().join("cut.png");

    let plain = run_cutout(&[input.to_str().unwrap(), "-o", output.to_str().unwrap()]);
    assert!(plain.status.success());
    let kept = image::open(&output).unwrap().to_rgba8();
    assert_eq!(kept.get_pixel(20, 20)[3], 255, "対照：小道具は前景で残る");

    let mut mask = image::RgbaImage::from_pixel(200, 200, image::Rgba([0, 0, 0, 255]));
    paint(&mut mask, (5, 5, 35, 35), 255);
    let path = write_png(dir.path(), "bg.png", &mask);

    let out = run_cutout(&[
        input.to_str().unwrap(),
        "-o",
        output.to_str().unwrap(),
        "--force",
        "--bg-mask",
        path.to_str().unwrap(),
    ]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let v = json_stdout(&out);
    assert_eq!(v["constraints"]["sources"][0], "bg_mask");

    let cut = image::open(&output).unwrap().to_rgba8();
    assert_eq!(cut.get_pixel(20, 20)[3], 0, "指した小道具が消えていない");
    assert_eq!(cut.get_pixel(100, 100)[3], 255, "商品まで巻き込んでいる");
}

/// 多角形を正規化座標で受けること。ビジョンモデルは 0.0-1.0 で返す。
#[test]
fn a_polygon_is_accepted_in_normalized_coordinates() {
    let dir = fixture_dir();
    let input = constraint_fixture(dir.path());
    let output = dir.path().join("cut.png");

    // 画像の左上 1/4 を確定前景にする（商品の外なので、色では前景にならない）
    let out = run_cutout(&[
        input.to_str().unwrap(),
        "-o",
        output.to_str().unwrap(),
        "--normalized",
        "--fg-polygon",
        "0.05,0.05,0.25,0.05,0.25,0.25,0.05,0.25",
    ]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let v = json_stdout(&out);
    assert_eq!(v["constraints"]["sources"][0], "fg_polygon");
    // 0.2 x 0.2 = 全体の 4%
    let fg = v["constraints"]["fg_ratio"].as_f64().unwrap();
    assert!((fg - 0.04).abs() < 0.005, "面の大きさが合わない: {fg}");

    let cut = image::open(&output).unwrap().to_rgba8();
    assert_eq!(cut.get_pixel(30, 30)[3], 255, "指した面が守られていない");
    assert_eq!(cut.get_pixel(60, 10)[3], 0, "面の外まで守っている");
}

/// **小さい確定前景が面積フィルタに黙って消えないこと。**
///
/// `--cleanup` の下限は解像度に比例するので、高解像度ほど大きな指示が消える
/// （5712x4284 では 20x20 の `--fg-polygon` が丸ごと落ちた）。しかも
/// `constraints.sources` には入口の名前が出たままなので、結果の JSON からは
/// 「指示は効いた」としか読めない。ここでは 200x200 と `--cleanup 12`
/// （下限 625px²）で同じ大きさ関係を作る。
///
/// **同じ実行の中に対照を置く。** 指示の無い 20x20 の塊は消えるので、
/// しきい値が本当に指示より大きいことがその場で確かめられる。対照を商品から
/// 離して置くのは、堤防が残す 1px の縁どうしが 8 近傍でつながると、
/// 成分が商品と合体して面積フィルタに掛からなくなるためである。
#[test]
fn a_small_forced_foreground_survives_the_speck_filter() {
    let dir = fixture_dir();
    let mut img = product_image(&ProductSpec {
        width: 200,
        height: 200,
        ..Default::default()
    });
    // 対照：指示を伴わない 20x20 の濃い塊（商品は x 44-156 / y 32-168）
    paint(&mut img, (10, 160, 29, 179), 30);
    let input = write_png(dir.path(), "speck.png", &img);
    let output = dir.path().join("cut.png");

    // 20x20 = 400px² は下限 625px² を下回る
    let out = run_cutout(&[
        input.to_str().unwrap(),
        "-o",
        output.to_str().unwrap(),
        "--cleanup",
        "12",
        "--fg-polygon",
        "10,10,30,10,30,30,10,30",
    ]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(json_stdout(&out)["constraints"]["sources"][0], "fg_polygon");

    let cut = image::open(&output).unwrap().to_rgba8();
    assert_eq!(
        cut.get_pixel(20, 170)[3],
        0,
        "対照が消えていない。--cleanup 12 の下限が 400px² を超えていない"
    );
    assert_eq!(
        cut.get_pixel(20, 20)[3],
        255,
        "確定前景が面積フィルタに消された"
    );

    // 種の円（半径 5px = 81px²）も同じ約束の下にある
    let seeded = run_cutout(&[
        input.to_str().unwrap(),
        "-o",
        output.to_str().unwrap(),
        "--force",
        "--cleanup",
        "12",
        "--fg-seed",
        "20,20",
    ]);
    assert!(
        seeded.status.success(),
        "{}",
        String::from_utf8_lossy(&seeded.stderr)
    );
    let cut = image::open(&output).unwrap().to_rgba8();
    assert_eq!(
        cut.get_pixel(20, 20)[3],
        255,
        "--fg-seed の円が面積フィルタに消された"
    );
}

/// 指示が決めた境界しか無ければ `separability` は `null` になること。
///
/// **契約が禁じているのは 0.0 のほうである。** `null_means` は「測れる境界が
/// 無かった。0（色差が無い）ではない」と言っている。確定前景と確定背景を
/// 隙間なく接して置けば、そこに色の判断は 1 つも入っていないので、
/// 返すべきものは「測れなかった」でしかない。
///
/// 除外を「前景側が確定前景 **かつ** 背景側が確定背景」の厳密一致で問うと、
/// 境界が 1px でもずれた瞬間に素通りして 0.0 が出る。refine と feather が
/// 動かす場合もあれば、ここのように**不明の帯を挟んだだけ**でもそうなる
/// （フィルが指示にぶつかって止まった線は、やはり色の判断ではない）。
#[test]
fn a_boundary_drawn_entirely_by_the_instructions_is_not_measurable() {
    let dir = fixture_dir();
    // 一様な背景。色の手がかりはどこにも無い
    let img = image::RgbaImage::from_pixel(200, 200, image::Rgba([248, 248, 247, 255]));
    let input = write_png(dir.path(), "flat.png", &img);
    let output = dir.path().join("cut.png");

    // 中央の 40x40 を確定前景、その外側を 4 枚の帯で確定背景にする
    // （多角形 1 つでは「矩形の外」を表せない）。帯と確定前景の隙間 `gap` を
    // 広げて、隙間なく接する場合と 2px の不明帯を挟む場合の両方を見る
    let run = |name: &str, gap: u32| -> Value {
        let (a, b) = (80 - gap, 120 + gap);
        let out = run_cutout(&[
            input.to_str().unwrap(),
            "-o",
            output.to_str().unwrap(),
            "--force",
            "--fg-polygon",
            "80,80,120,80,120,120,80,120",
            &format!("--bg-polygon=40,40,160,40,160,{a},40,{a}"),
            &format!("--bg-polygon=40,{b},160,{b},160,160,40,160"),
            &format!("--bg-polygon=40,{a},{a},{a},{a},{b},40,{b}"),
            &format!("--bg-polygon={b},{a},160,{a},160,{b},{b},{b}"),
        ]);
        assert!(
            out.status.success(),
            "{name}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        json_stdout(&out)
    };

    for (name, gap) in [("隙間なく接する", 0), ("2px の不明帯を挟む", 2)] {
        let v = run(name, gap);
        assert!(
            v["constraints"]["fg_ratio"].as_f64().unwrap() > 0.0
                && v["constraints"]["bg_ratio"].as_f64().unwrap() > 0.0,
            "{name}: 指示が置かれていない: {v}"
        );
        assert!(
            v["mask"]["separability"].is_null(),
            "{name}: 指示が引いた線を色の境界として数えている: {}",
            v["mask"]["separability"]
        );
    }
}

/// 画素座標を `--normalized` で渡す取り違えを断ること。
#[test]
fn a_pixel_polygon_passed_as_normalized_is_rejected() {
    let dir = fixture_dir();
    let input = constraint_fixture(dir.path());
    let output = dir.path().join("cut.png");

    let out = run_cutout(&[
        input.to_str().unwrap(),
        "-o",
        output.to_str().unwrap(),
        "--normalized",
        "--fg-polygon",
        "10,10,50,10,50,50",
    ]);
    assert_eq!(out.status.code(), Some(2));
    assert_eq!(json_stdout(&out)["error"]["code"], "INVALID_POLYGON");
}

/// 点数が足りない多角形は clap が断る。**code を伴わない exit 2 になる。**
#[test]
fn a_two_point_polygon_never_reaches_the_command() {
    let dir = fixture_dir();
    let input = constraint_fixture(dir.path());
    let output = dir.path().join("cut.png");

    let out = run_cutout(&[
        input.to_str().unwrap(),
        "-o",
        output.to_str().unwrap(),
        "--fg-polygon",
        "10,10,50,50",
    ]);
    assert_eq!(out.status.code(), Some(2));
    assert!(out.stdout.is_empty(), "パーサの失敗で JSON が出ている");
    assert!(String::from_utf8_lossy(&out.stderr).contains("3"));
}

/// 寸法の違うマスクは拡縮せずに断ること。
///
/// **黙って伸ばすと、指示した境界が商品の輪郭から半画素ずつずれたまま、
/// 結果だけがそれらしく返る。**
#[test]
fn a_mask_of_the_wrong_size_is_refused() {
    let dir = fixture_dir();
    let input = constraint_fixture(dir.path());
    let output = dir.path().join("cut.png");
    let mask = write_gray(dir.path(), "mask.png", 100, 200, 255);

    let out = run_cutout(&[
        input.to_str().unwrap(),
        "-o",
        output.to_str().unwrap(),
        "--fg-mask",
        mask.to_str().unwrap(),
    ]);
    assert_eq!(out.status.code(), Some(2));
    let v = json_stdout(&out);
    assert_eq!(v["error"]["code"], "MASK_SIZE_MISMATCH");
    let message = v["error"]["message"].as_str().unwrap();
    assert!(
        message.contains("100x200") && message.contains("200x200"),
        "両方の寸法を出すべき: {message}"
    );
    assert!(
        v["error"]["hint"].as_str().unwrap().contains("kiri info"),
        "合わせ方を言うべき: {v}"
    );
}

/// 読めないマスクは、どの指示のどのファイルかまで言うこと。
#[test]
fn an_unreadable_mask_names_the_flag_and_the_path() {
    let dir = fixture_dir();
    let input = constraint_fixture(dir.path());
    let output = dir.path().join("cut.png");
    let missing = dir.path().join("nope.png");

    let out = run_cutout(&[
        input.to_str().unwrap(),
        "-o",
        output.to_str().unwrap(),
        "--trimap",
        missing.to_str().unwrap(),
    ]);
    assert_eq!(out.status.code(), Some(3), "入力の失敗と同じ分類になる");
    let v = json_stdout(&out);
    assert_eq!(v["error"]["code"], "INPUT_UNREADABLE");
    let message = v["error"]["message"].as_str().unwrap();
    assert!(
        message.contains("--trimap"),
        "どの指示かが分からない: {message}"
    );
    assert!(
        message.contains("nope.png"),
        "どのファイルかが分からない: {message}"
    );
}

/// JPEG で渡したマスクでも、指示された面積が膨らまないこと。
///
/// **「輝度が 0 でない」ではリンギングを拾う。** 黒く塗ったはずの周囲に
/// 1 桁の値が散り、実測で指示面積が 2.4 倍になっていた。中点（128）で
/// 切れば、可逆でない形式を経由しても指示は動かない。
#[test]
fn a_lossy_mask_does_not_inflate_the_instructed_area() {
    let dir = fixture_dir();
    let input = constraint_fixture(dir.path());
    let output = dir.path().join("cut.png");

    // 黒地に 60x60 の白い矩形。境目の周りに JPEG のリンギングが出る
    let mut mask = image::RgbaImage::from_pixel(200, 200, image::Rgba([0, 0, 0, 255]));
    paint(&mut mask, (70, 70, 129, 129), 255);
    let path = write_jpeg(dir.path(), "mask.jpg", &mask);

    let out = run_cutout(&[
        input.to_str().unwrap(),
        "-o",
        output.to_str().unwrap(),
        "--bg-mask",
        path.to_str().unwrap(),
    ]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let v = json_stdout(&out);
    let pixels = v["constraints"]["bg_pixels"].as_u64().unwrap();
    assert!(
        (3400..=3800).contains(&pixels),
        "指示された面積が 60x60=3600 から離れている: {pixels}"
    );
}

/// EXIF Orientation を持つ指示画像は、黙って向き違いのまま使わないこと。
///
/// **180 度（3）や鏡像（2/4）は寸法が変わらない。** `MASK_SIZE_MISMATCH` を
/// 素通りして、上下逆さまの指示がそのまま効く。結果の数値からは
/// 「切り抜きが下手」としか読めない失敗である。
#[test]
fn a_mask_carrying_an_exif_orientation_is_reported() {
    let dir = fixture_dir();
    let input = constraint_fixture(dir.path());
    let output = dir.path().join("cut.png");

    let mut mask = image::RgbaImage::from_pixel(200, 200, image::Rgba([0, 0, 0, 255]));
    paint(&mut mask, (10, 10, 60, 60), 255);
    let path = write_jpeg_with_orientation(dir.path(), "rotated.jpg", &mask, 3);

    let out = run_cutout(&[
        input.to_str().unwrap(),
        "-o",
        output.to_str().unwrap(),
        "--bg-mask",
        path.to_str().unwrap(),
    ]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let v = json_stdout(&out);
    assert!(
        has_warning(&v, "MASK_ORIENTATION_IGNORED"),
        "向きを無視したことを報せていない: {:?}",
        warning_codes(&v)
    );
    let w = v["warnings"]
        .as_array()
        .unwrap()
        .iter()
        .find(|w| w["code"] == "MASK_ORIENTATION_IGNORED")
        .unwrap();
    assert_eq!(w["data"]["orientation"], 3);
    assert!(
        w["message"].as_str().unwrap().contains("rotated.jpg"),
        "どのファイルかが分からない: {w}"
    );
    assert!(w["hint"].as_str().unwrap().contains("向き"), "{w}");
}

/// 渡したのに 1 画素も塗らなかった指示は、黙って無かったことにしないこと。
///
/// **`sources` に無いことを「空だった」と読ませるのは無理がある。**
/// 渡し忘れと空振りは打つ手が違うので、code で分ける。ブロックそのものは
/// 出す（比率 0、`sources` は空配列）。
#[test]
fn an_instruction_that_marked_nothing_is_reported() {
    let dir = fixture_dir();
    let input = constraint_fixture(dir.path());
    let output = dir.path().join("cut.png");

    let out = run_cutout(&[
        input.to_str().unwrap(),
        "-o",
        output.to_str().unwrap(),
        "--fg-polygon",
        "500,500,600,500,600,600",
    ]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let v = json_stdout(&out);
    assert!(
        has_warning(&v, "CONSTRAINT_EMPTY"),
        "空振りを報せていない: {:?}",
        warning_codes(&v)
    );
    let w = v["warnings"]
        .as_array()
        .unwrap()
        .iter()
        .find(|w| w["code"] == "CONSTRAINT_EMPTY")
        .unwrap();
    assert_eq!(w["data"]["source"], "fg_polygon");

    let c = &v["constraints"];
    assert!(!c.is_null(), "constraints ごと消えている: {v}");
    assert_eq!(c["sources"].as_array().unwrap().len(), 0);
    assert_eq!(c["fg_ratio"], 0.0);
    assert_eq!(c["fg_pixels"], 0);
}

/// 確定前景と確定背景が重なったら断ること。
///
/// **黙ってどちらかを選ぶと「指定が効いていない」という最も追いにくい失敗に
/// なる。** 重なった画素数と外接矩形まで返して、どこを直せばよいかを示す。
#[test]
fn overlapping_instructions_are_refused() {
    let dir = fixture_dir();
    let input = constraint_fixture(dir.path());
    let output = dir.path().join("cut.png");

    let out = run_cutout(&[
        input.to_str().unwrap(),
        "-o",
        output.to_str().unwrap(),
        "--fg-polygon",
        "10,10,60,10,60,60,10,60",
        "--bg-polygon",
        "50,50,120,50,120,120,50,120",
    ]);
    assert_eq!(out.status.code(), Some(2));
    let v = json_stdout(&out);
    assert_eq!(v["error"]["code"], "CONSTRAINT_CONFLICT");
    let message = v["error"]["message"].as_str().unwrap();
    assert!(
        message.contains("100") && message.contains("50,50"),
        "重なりの量と場所を言うべき: {message}"
    );
    assert!(
        v["error"]["hint"].as_str().unwrap().contains("片方"),
        "直し方を言うべき: {v}"
    );
    assert!(!output.exists(), "断ったのに書き出している");
}

/// 指示を渡さなければ `constraints` はキーごと現れないこと。
///
/// **`null` も出さない。** 「指示していない」と「指示したが空だった」を
/// 同じ形にすると、読み手は自分の指示が届いたかを判断できない。
#[test]
fn the_constraints_block_appears_only_when_something_was_given() {
    let dir = fixture_dir();
    let input = constraint_fixture(dir.path());
    let output = dir.path().join("cut.png");

    let plain = run_cutout(&[
        input.to_str().unwrap(),
        "-o",
        output.to_str().unwrap(),
        "--dry-run",
    ]);
    assert!(plain.status.success());
    assert!(
        json_stdout(&plain).get("constraints").is_none(),
        "指示が無いのに constraints が出ている"
    );

    // --fg-seed も空間的な指示の 1 つ。半径 5px の円が確定前景になる
    let seeded = run_cutout(&[
        input.to_str().unwrap(),
        "-o",
        output.to_str().unwrap(),
        "--dry-run",
        "--fg-seed",
        "100,100",
    ]);
    assert!(seeded.status.success());
    let v = json_stdout(&seeded);
    assert_eq!(v["constraints"]["sources"][0], "fg_seed");
    assert!(v["constraints"]["fg_ratio"].as_f64().unwrap() > 0.0);
    assert_eq!(v["constraints"]["bg_ratio"], 0.0);
}

/// 指示を重ねると、効いた入口が並ぶこと。
#[test]
fn every_source_that_marked_a_pixel_is_listed() {
    let dir = fixture_dir();
    let input = constraint_fixture(dir.path());
    let output = dir.path().join("cut.png");
    let mask = write_gray(dir.path(), "empty.png", 200, 200, 0);

    let out = run_cutout(&[
        input.to_str().unwrap(),
        "-o",
        output.to_str().unwrap(),
        "--dry-run",
        "--fg-polygon",
        "80,80,120,80,120,120,80,120",
        "--bg-polygon",
        "0,0,20,0,20,20,0,20",
        // 1 画素も塗らないマスク。**渡しても sources には出ない**
        "--fg-mask",
        mask.to_str().unwrap(),
    ]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let sources = json_stdout(&out)["constraints"]["sources"].clone();
    assert_eq!(sources[0], "fg_polygon");
    assert_eq!(sources[1], "bg_polygon");
    assert_eq!(
        sources.as_array().unwrap().len(),
        2,
        "空の入口が出ている: {sources}"
    );
}

/// プレビューの重ね描きは、指示があるときにだけ現れること。
///
/// **指示が無い実行のバイト列は 1 バイトも動かない。** 重ね描きは指示の
/// 置き場所を確かめるためのもので、既存の検証画像を変える理由が無い。
#[test]
fn the_preview_overlay_appears_only_with_instructions() {
    let dir = fixture_dir();
    let input = constraint_fixture(dir.path());
    let output = dir.path().join("cut.png");

    let preview = |name: &str, extra: &[&str]| -> Vec<u8> {
        let path = dir.path().join(name);
        let mut args = vec![
            input.to_str().unwrap(),
            "-o",
            output.to_str().unwrap(),
            "--force",
            "--preview",
            path.to_str().unwrap(),
        ];
        args.extend_from_slice(extra);
        let out = run_cutout(&args);
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        std::fs::read(&path).unwrap()
    };

    let plain = preview("a.png", &[]);
    let again = preview("b.png", &[]);
    assert_eq!(plain, again, "指示なしのプレビューが決定的でない");

    let marked = preview("c.png", &["--fg-polygon", "80,80,120,80,120,120,80,120"]);
    assert_ne!(plain, marked, "指示が重ね描きされていない");
}

/// spec からも同じ指示を渡せること。**相対パスは spec の場所を基準に解く。**
#[test]
fn the_batch_spec_accepts_spatial_instructions() {
    let dir = fixture_dir();
    let img = product_image(&ProductSpec {
        width: 200,
        height: 200,
        ..Default::default()
    });
    write_png(dir.path(), "p0.png", &img);
    let mut mask = image::RgbaImage::from_pixel(200, 200, image::Rgba([0, 0, 0, 255]));
    paint(&mut mask, (10, 10, 40, 40), 255);
    write_png(dir.path(), "fg.png", &mask);

    let spec = write_spec(
        dir.path(),
        r#"{"defaults":{"format":"png"},
            "items":[{"input":"p0.png","output":"out/a.png",
                      "fg_mask":"fg.png",
                      "bg_polygons":[[100,10,140,10,140,40,100,40]]}]}"#,
    );
    let out = run_batch(&spec, &[]);
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let v = json_stdout(&out);
    let constraints = &v["results"][0]["result"]["constraints"];
    assert_eq!(constraints["sources"][0], "fg_mask");
    assert_eq!(constraints["sources"][1], "bg_polygon");
    assert!(constraints["fg_ratio"].as_f64().unwrap() > 0.0);

    let cut = image::open(dir.path().join("out/a.png"))
        .unwrap()
        .to_rgba8();
    assert_eq!(cut.get_pixel(25, 25)[3], 255, "マスクが効いていない");
}

/// 新しいキーの綴り違いも候補を示して断ること。
#[test]
fn a_misspelled_spatial_key_suggests_the_right_one() {
    let dir = fixture_dir();
    batch_fixture(dir.path(), 1);
    let spec = write_spec(
        dir.path(),
        r#"{"items":[{"input":"p0.png","output":"a.png","fg_polygon":[[0,0,1,0,1,1]]}]}"#,
    );

    let out = run_batch(&spec, &[]);
    assert_eq!(out.status.code(), Some(3));
    let v = json_stdout(&out);
    assert_eq!(v["error"]["code"], "SPEC_UNKNOWN_FIELD");
    assert!(
        v["error"]["hint"].as_str().unwrap().contains("fg_polygons"),
        "綴り違いの候補を示すべき: {v}"
    );
}

/// spec の多角形も CLI と同じ関門を通ること。
#[test]
fn a_two_point_polygon_in_a_spec_is_rejected() {
    let dir = fixture_dir();
    batch_fixture(dir.path(), 1);
    let spec = write_spec(
        dir.path(),
        r#"{"items":[{"input":"p0.png","output":"a.png","fg_polygons":[[0,0,1,1]]}]}"#,
    );

    let out = run_batch(&spec, &[]);
    // 項目の失敗は全体を止めない。exit 4 で results[] に理由が入る
    assert_eq!(out.status.code(), Some(4));
    let v = json_stdout(&out);
    assert_eq!(v["results"][0]["error"]["code"], "INVALID_POLYGON");
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

    // **1 色モデルで固定する。** この構図が危ないのは「外周 ΔE の p90 が
    // 汚染されて、面積も捕捉率も通ってしまう」からで、その状態を作れるのは
    // 1 色で測ったときだけである。照明場は外周に掛かった灰色を背景として
    // 吸うので、同じ穴がそもそも開かない
    let out = kiri()
        .args([
            "info",
            input.to_str().unwrap(),
            "--background-model",
            "flat",
            "--json",
        ])
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

    // 既定（照明場）でも信用しない。**弾く仕組みは変わってよいが、
    // 結論が裏返ってはいけない**——ここでは捕捉率が落ちて Low になる
    let auto = json_stdout(
        &kiri()
            .args(["info", input.to_str().unwrap(), "--json"])
            .output()
            .unwrap(),
    );
    assert_eq!(
        auto["subject"]["confidence"], "low",
        "照明場で信頼度が裏返っている: {}",
        auto["subject"]
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
    // **「助言が出ない」だけでは、最悪の結果で通ってしまう。** 場が外周の
    // 灰色を背景として学べば物体はまるごと消え、消えた結果として
    // `BBOX_RECOMMENDED` も出なくなる。物体が残っていることまで言う
    // （灰色 35% + 右下の四角で正解は 0.45 前後）
    assert!(
        v["mask"]["foreground_ratio"].as_f64().unwrap() > 0.40,
        "物体が消えた結果として警告が消えている: {}",
        v["mask"]
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

    // 上と同じ理由で 1 色モデルで固定する（この穴は 1 色でしか開かない）
    let v = json_stdout(
        &kiri()
            .args([
                "info",
                input.to_str().unwrap(),
                "--background-model",
                "flat",
                "--json",
            ])
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

    // 既定（照明場）でも結論は裏返らない
    let auto = json_stdout(
        &kiri()
            .args(["info", input.to_str().unwrap(), "--json"])
            .output()
            .unwrap(),
    );
    assert_eq!(
        auto["subject"]["confidence"], "low",
        "照明場で信頼度が裏返っている: {}",
        auto["subject"]
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
    // 上と同じ理由で、物体が残っていることまで言う。**物体が消えれば
    // 警告も消えるので、警告の不在だけを見ていると最悪の結果で通る**
    assert!(
        v["mask"]["foreground_ratio"].as_f64().unwrap() > 0.40,
        "物体が消えた結果として警告が消えている: {}",
        v["mask"]
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
    // 見切れてはいない。
    //
    // 1 色モデルで問う。帯は背景色から離れているので 1 色では前景として残り、
    // それが端に達する——「外周接触をどう読むか」の分岐が働くのはこの状態で
    // ある。照明場は帯ごと背景として吸ってしまい、分岐そのものに届かない
    let v = json_stdout(
        &kiri()
            .args([
                "cutout",
                input.to_str().unwrap(),
                "-o",
                output.to_str().unwrap(),
                "--background-model",
                "flat",
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

    // **既定（照明場）でも同じ答えになる。帯は場の材料から外れる。**
    //
    // 場の材料には 1 色の中央値からの色の門が掛かっており、この帯（ΔE 17.67）は
    // 門（既定 15）の外側にある。吸わせようと門を広げると、**同じ ΔE の物体まで
    // 背景として学ぶ**——`woven_poisoned_scene` の灰色の物体は ΔE 17.74 で、
    // この帯と 0.07 しか違わない。色差だけでは「背景に落ちた影」と「背景に
    // 置かれた物体」を分けられないので、**物体を消さない側へ倒してある**
    // （門を 18 まで広げると、この帯は吸われる代わりに画面の 35% を占める
    // 灰色の物体が消え、前景比率は 0.4461 → 0.1068 になる）。
    //
    // 残るのは 1 色モデルと同じ「bbox を勧める」で、これは**実行できる助言**
    // である。帯が前景に残ること自体は矩形ひとつで解ける
    let auto = json_stdout(
        &kiri()
            .args([
                "cutout",
                input.to_str().unwrap(),
                "-o",
                dir.path().join("auto.png").to_str().unwrap(),
                "--json",
            ])
            .output()
            .unwrap(),
    );
    assert_eq!(
        auto["settings"]["background_model"], "field",
        "既定で照明場が効いていない: {auto}"
    );
    let codes = warning_codes(&auto);
    assert!(
        codes.contains(&"BBOX_RECOMMENDED".to_string()),
        "解ける画像で助言が出ていない: {codes:?}"
    );
    assert!(
        !codes.contains(&"SUBJECT_TOUCHES_EDGE".to_string()),
        "見切れの誤診が復活している: {codes:?}"
    );
    // **1 色と同じ画素になっていること。** 場が効いていながら帯を吸わない以上、
    // 結果は 1 色モデルと変わらないはずで、そこがずれていたら場が別の何かを
    // 学んでいる
    assert!(
        (auto["mask"]["foreground_ratio"].as_f64().unwrap()
            - v["mask"]["foreground_ratio"].as_f64().unwrap())
        .abs()
            < 0.01,
        "1 色と場で結果が食い違っている: {} と {}",
        auto["mask"],
        v["mask"]
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

    // **1 色モデルで問う。** 「背景側が前景として残って端に達する」状態を
    // 作れるのは 1 色で測ったときだけで、照明場は上下 2 色の段差ごと吸って
    // しまう（吸えることは下で対にして押さえる）。分岐そのものを測るには、
    // 分岐が働く状態を作らなければならない
    let out = kiri()
        .args([
            "cutout",
            input.to_str().unwrap(),
            "-o",
            output.to_str().unwrap(),
            "--background-model",
            "flat",
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
            "--background-model",
            "flat",
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

    // **既定（照明場）は bbox なしでここを解く。** 上下 2 色の背景は
    // 照明場が持てる形そのもので、1 色で測ったときに残っていた背景側が
    // 丸ごと消える。助言が要らなくなることまでを対で押さえる
    let auto = json_stdout(
        &kiri()
            .args([
                "cutout",
                input.to_str().unwrap(),
                "-o",
                dir.path().join("auto.png").to_str().unwrap(),
                "--json",
            ])
            .output()
            .unwrap(),
    );
    assert_eq!(
        auto["settings"]["background_model"], "field",
        "既定で照明場が効いていない: {auto}"
    );
    assert_eq!(
        auto["mask"]["touches_edge"], false,
        "背景側が前景として残っている: {auto}"
    );
    let after = auto["mask"]["foreground_ratio"].as_f64().unwrap();
    assert!(
        (0.15..0.25).contains(&after),
        "商品だけが残っていない: {after}"
    );
    let codes = warning_codes(&auto);
    assert!(
        !codes.contains(&"BBOX_RECOMMENDED".to_string())
            && !codes.contains(&"SUBJECT_TOUCHES_EDGE".to_string()),
        "何も残っていないのに残っていると言っている: {codes:?}"
    );
    assert!(
        codes.contains(&"BACKGROUND_FIELD_USED".to_string()),
        "場を使ったことを黙っている: {codes:?}"
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

// --- 落ち影の合成 (Phase 15) ---

/// 影の合成を頼まない実行では、結果に `shadow` ブロックが現れない。
///
/// `null` も出さない（`constraints` と同じ規約）。走らなかった処理の
/// 痕跡が残ると、エージェントは「合成したが影が出なかった」と読む。
#[test]
fn a_run_without_a_shadow_reports_no_shadow_block() {
    let dir = fixture_dir();
    let img = product_image(&ProductSpec {
        width: 160,
        height: 160,
        ..Default::default()
    });
    let input = write_png(dir.path(), "in.png", &img);

    let v = cutout_on_canvas(dir.path(), &input, "out.png", &[]);
    assert!(
        v.get("shadow").is_none(),
        "影を頼んでいないのに shadow が出た"
    );
    assert_eq!(
        v["settings"]["shadow"], "off",
        "settings.shadow は常に出すべき"
    );
}

/// `--shadow off` は、`--shadow` を渡さない実行と 1 バイトも変わらない。
#[test]
fn an_explicit_shadow_off_writes_the_same_bytes_as_no_shadow_at_all() {
    let dir = fixture_dir();
    let img = product_image(&ProductSpec {
        width: 200,
        height: 200,
        ..Default::default()
    });
    let input = write_png(dir.path(), "in.png", &img);

    cutout_on_canvas(dir.path(), &input, "plain.png", &["--canvas", "400"]);
    cutout_on_canvas(
        dir.path(),
        &input,
        "off.png",
        &["--canvas", "400", "--shadow", "off"],
    );
    assert_eq!(
        std::fs::read(dir.path().join("plain.png")).unwrap(),
        std::fs::read(dir.path().join("off.png")).unwrap(),
        "--shadow off が成果物を変えている"
    );
}

/// 効いた値は px@1000 から実寸へ掛け戻したものが出る。
///
/// 指定値をそのまま返すと、長辺が違う素材のあいだで同じ数字が違う見た目を
/// 指すことになる。`settings.smooth_radius_px` と同じ理由で実寸を出す。
#[test]
fn a_synthetic_shadow_reports_the_pixels_that_actually_took_effect() {
    let dir = fixture_dir();
    let img = product_image(&ProductSpec {
        width: 400,
        height: 400,
        ..Default::default()
    });
    let input = write_png(dir.path(), "in.png", &img);

    let v = cutout_on_canvas(dir.path(), &input, "out.png", &["--shadow", "synth"]);
    assert_eq!(v["settings"]["shadow"], "synth");

    let shadow = &v["shadow"];
    // 長辺 400px なので、px@1000 の既定値 0,12 と σ 10 は 0.4 倍で効く
    assert_eq!(
        shadow["offset"],
        serde_json::json!([0, 5]),
        "オフセットが実寸に換算されていない: {shadow}"
    );
    // **報告される σ は箱型の幅が実現する値**で、要求値そのものではない。
    // 幅は奇数の整数しか取れないので、4.0 を頼むと 3.83 になる
    assert_eq!(
        shadow["blur"].as_f64().unwrap(),
        round4(kiri::transform::shadow::effective_sigma(4.0)),
        "σ が実寸に換算されていないか、要求値をそのまま返している: {shadow}"
    );
    assert!(
        (shadow["blur"].as_f64().unwrap() - 4.0).abs() < 0.2,
        "実現した σ が要求から離れすぎている: {shadow}"
    );
    assert_eq!(shadow["opacity"], 0.25);
    assert_eq!(shadow["color"], "#000000");
    assert!(
        shadow["bounds"].as_array().is_some_and(|b| b.len() == 4),
        "影の矩形が出ていない: {shadow}"
    );
    assert!(
        shadow["clipped"].is_boolean(),
        "clipped が真偽で出ていない: {shadow}"
    );
}

/// キャンバスの長辺が px@1000 の基準になる。
#[test]
fn the_canvas_long_side_is_what_px_at_1000_is_measured_against() {
    let dir = fixture_dir();
    let img = product_image(&ProductSpec {
        width: 200,
        height: 200,
        ..Default::default()
    });
    let input = write_png(dir.path(), "in.png", &img);

    let v = cutout_on_canvas(
        dir.path(),
        &input,
        "out.png",
        &["--canvas", "2000", "--shadow", "synth"],
    );
    assert_eq!(
        v["shadow"]["offset"],
        serde_json::json!([0, 24]),
        "元画像 200px ではなくキャンバス 2000px を基準にすべき"
    );
    assert_eq!(
        v["shadow"]["blur"].as_f64().unwrap(),
        round4(kiri::transform::shadow::effective_sigma(20.0))
    );
}

/// ぼかしが小さすぎて箱型が恒等に落ちるときは、σ を名乗らない。
///
/// **要求値をそのまま返すと嘘になる。** 幅が 3 回とも 1 なら画素は 1 つも
/// 動いておらず、縁は 0→255 の段差のままである。schema の「0 ならぼかして
/// いない」という注記もそのときだけ成り立つ。
#[test]
fn a_blur_too_small_to_take_effect_is_reported_as_zero() {
    let dir = fixture_dir();
    let img = product_image(&ProductSpec {
        width: 200,
        height: 200,
        ..Default::default()
    });
    let input = write_png(dir.path(), "in.png", &img);

    // 長辺 200px なので px@1000 の 2 は実寸 0.4 にしかならない
    let v = cutout_on_canvas(
        dir.path(),
        &input,
        "out.png",
        &["--shadow", "synth", "--shadow-blur", "2"],
    );
    assert_eq!(
        v["shadow"]["blur"], 0.0,
        "ぼかしていないのに σ を名乗った: {}",
        v["shadow"]
    );
}

/// 影を足しても商品の配置は動かず、切り抜きの診断値も動かない。
///
/// `fill_ratio` は商品だけで決める。影のぶん商品を小さくすると、同じ設定を
/// 通した素材群で占有率が影の有無によって変わってしまう。
#[test]
fn a_shadow_moves_neither_the_product_nor_the_mask_statistics() {
    let dir = fixture_dir();
    let img = product_image(&ProductSpec {
        width: 300,
        height: 300,
        ..Default::default()
    });
    let input = write_png(dir.path(), "in.png", &img);

    let off = cutout_on_canvas(dir.path(), &input, "off.png", &["--canvas", "1000"]);
    let on = cutout_on_canvas(
        dir.path(),
        &input,
        "on.png",
        &["--canvas", "1000", "--shadow", "synth"],
    );

    assert_eq!(off["canvas"], on["canvas"], "影で配置が変わっている");
    assert_eq!(
        off["mask"], on["mask"],
        "影が切り抜きの診断値を動かしている（影は測る前に足してはいけない）"
    );
    assert_eq!(off["outputs"][0]["width"], on["outputs"][0]["width"]);
    assert_eq!(off["outputs"][0]["height"], on["outputs"][0]["height"]);
}

/// 下地を塗るときの順序は「下地 → 影 → 商品」になる。
#[test]
fn a_flattened_canvas_shows_the_shadow_under_the_product_but_not_in_the_far_corner() {
    let dir = fixture_dir();
    let img = product_image(&ProductSpec {
        width: 300,
        height: 300,
        ..Default::default()
    });
    let input = write_png(dir.path(), "in.png", &img);

    let v = cutout_on_canvas(
        dir.path(),
        &input,
        "out.png",
        &[
            "--canvas",
            "1000",
            "--fill-ratio",
            "0.6",
            "--flatten",
            "--background",
            "#FFFFFF",
            "--shadow",
            "synth",
            "--shadow-offset",
            "0,40",
            "--shadow-blur",
            "8",
            "--shadow-opacity",
            "0.5",
        ],
    );
    let out = image::open(dir.path().join("out.png")).unwrap().to_rgba8();

    let offset = v["canvas"]["offset"].as_array().unwrap();
    let content = v["canvas"]["content"].as_array().unwrap();
    let cx = (offset[0].as_u64().unwrap() + content[0].as_u64().unwrap() / 2) as u32;
    let below = (offset[1].as_u64().unwrap() + content[1].as_u64().unwrap() + 10) as u32;

    let under = out.get_pixel(cx, below).0;
    assert_eq!(under[3], 255, "--flatten なのに透過が残っている");
    assert!(
        under[0] < 250,
        "商品の下の余白が白のまま（影が落ちていない）: {under:?}"
    );
    assert_eq!(
        out.get_pixel(3, 3).0,
        [255, 255, 255, 255],
        "商品から遠い角が純白でない"
    );
}

/// 透過を保てる形式では、影は半透明のアルファとして残る。
#[test]
fn a_shadow_stays_translucent_when_the_format_keeps_alpha() {
    let dir = fixture_dir();
    let img = product_image(&ProductSpec {
        width: 300,
        height: 300,
        ..Default::default()
    });
    let input = write_png(dir.path(), "in.png", &img);

    let v = cutout_on_canvas(
        dir.path(),
        &input,
        "out.png",
        &[
            "--canvas",
            "1000",
            "--fill-ratio",
            "0.6",
            "--shadow",
            "synth",
            "--shadow-offset",
            "0,40",
            "--shadow-blur",
            "8",
        ],
    );
    let out = image::open(dir.path().join("out.png")).unwrap().to_rgba8();

    let offset = v["canvas"]["offset"].as_array().unwrap();
    let content = v["canvas"]["content"].as_array().unwrap();
    let cx = (offset[0].as_u64().unwrap() + content[0].as_u64().unwrap() / 2) as u32;
    let below = (offset[1].as_u64().unwrap() + content[1].as_u64().unwrap() + 10) as u32;

    let a = out.get_pixel(cx, below).0[3];
    assert!(
        a > 0 && a < 255,
        "影が半透明のアルファとして残っていない: alpha={a}"
    );
    assert_eq!(out.get_pixel(3, 3).0[3], 0, "遠い角の余白が透明でない");
}

/// 影がキャンバスからはみ出しても寸法は変わらず、切れたことが報告される。
#[test]
fn a_shadow_running_off_the_canvas_is_clipped_and_says_so() {
    let dir = fixture_dir();
    let img = product_image(&ProductSpec {
        width: 200,
        height: 200,
        ..Default::default()
    });
    let input = write_png(dir.path(), "in.png", &img);

    let v = cutout_on_canvas(
        dir.path(),
        &input,
        "out.png",
        &[
            "--canvas",
            "500",
            "--shadow",
            "synth",
            "--shadow-offset",
            "0,900",
        ],
    );
    assert_eq!(v["outputs"][0]["height"], 500, "寸法が変わっている");
    assert_eq!(v["shadow"]["clipped"], true);
    assert_eq!(v["canvas"]["content"], {
        let plain = cutout_on_canvas(dir.path(), &input, "plain.png", &["--canvas", "500"]);
        plain["canvas"]["content"].clone()
    });
}

/// 負のオフセットで影を上・左へ出せる。
#[test]
fn a_negative_offset_throws_the_shadow_the_other_way() {
    let dir = fixture_dir();
    let img = product_image(&ProductSpec {
        width: 500,
        height: 500,
        ..Default::default()
    });
    let input = write_png(dir.path(), "in.png", &img);

    let v = cutout_on_canvas(
        dir.path(),
        &input,
        "out.png",
        &["--shadow", "synth", "--shadow-offset", "-20,-30"],
    );
    assert_eq!(v["shadow"]["offset"], serde_json::json!([-10, -15]));
}

/// 範囲外の指定は code を伴わない exit 2 で断る（clap の関門）。
#[test]
fn shadow_settings_outside_their_range_are_refused() {
    let dir = fixture_dir();
    let img = product_image(&ProductSpec {
        width: 120,
        height: 120,
        ..Default::default()
    });
    let input = write_png(dir.path(), "in.png", &img);
    let output = dir.path().join("out.png");

    for bad in [
        vec!["--shadow-opacity", "1.5"],
        vec!["--shadow-opacity", "-0.2"],
        vec!["--shadow-blur", "-1"],
        // **上限が無いと箱型の幅が u32 を溢れる。** release では panic せず、
        // ぼかしていないのに σ を報告する嘘の結果になっていた
        vec!["--shadow-blur", "1001"],
        vec!["--shadow-blur", "8e9"],
        vec!["--shadow-blur", "1e30"],
        vec!["--shadow-color", "chartreuse"],
        vec!["--shadow-offset", "1,2,3"],
        vec!["--shadow", "drop"],
    ] {
        let mut args = vec![
            "cutout",
            input.to_str().unwrap(),
            "-o",
            output.to_str().unwrap(),
            "--json",
            "--force",
        ];
        args.extend_from_slice(&bad);
        let out = kiri().args(&args).output().unwrap();
        assert_eq!(
            out.status.code(),
            Some(2),
            "{bad:?} が exit 2 で断られていない"
        );
    }
}

/// 上限ちょうどは通る。
///
/// 上限を置いた側の検査で、`1000` まで断ってしまうと「σ を上げる」という
/// 正当な指定が使えない幅で切られる。
#[test]
fn a_blur_at_exactly_the_limit_is_accepted() {
    let dir = fixture_dir();
    let img = product_image(&ProductSpec {
        width: 200,
        height: 200,
        ..Default::default()
    });
    let input = write_png(dir.path(), "in.png", &img);

    let limit = kiri::cli::SHADOW_BLUR_MAX.to_string();
    let v = cutout_on_canvas(
        dir.path(),
        &input,
        "out.png",
        &["--shadow", "synth", "--shadow-blur", &limit],
    );
    // σ が画像より広いので影は丸めで消える。**落ちないこと**がここの主題
    assert_eq!(v["settings"]["shadow"], "synth");
    assert!(v["shadow"]["clipped"].is_boolean());
}

/// 上限は spec 経由でも効く。**spec は clap を通らない。**
#[test]
fn an_out_of_range_shadow_blur_is_refused_in_a_spec_too() {
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
        r#"{"items":[{"input":"a.png","output":"out.png","shadow":"synth","shadow_blur":8e9}]}"#,
    )
    .unwrap();

    let out = kiri()
        .args(["batch", spec.to_str().unwrap(), "--json"])
        .output()
        .unwrap();
    let json = json_stdout(&out);
    assert_eq!(json["results"][0]["error"]["code"], "INVALID_SETTING");
}

/// `--shadow-opacity 0` は、ずらし量が大きくても切れたとは言わない。
///
/// `bounds` が null になる 2 つの理由——影を置かなかった／全部はみ出した——を
/// `clipped` が分ける、という契約そのものの検査である。
#[test]
fn a_zero_opacity_is_never_reported_as_clipped() {
    let dir = fixture_dir();
    let img = product_image(&ProductSpec {
        width: 200,
        height: 200,
        ..Default::default()
    });
    let input = write_png(dir.path(), "in.png", &img);

    let empty = cutout_on_canvas(
        dir.path(),
        &input,
        "empty.png",
        &[
            "--canvas",
            "500",
            "--shadow",
            "synth",
            "--shadow-offset",
            "0,2000",
            "--shadow-opacity",
            "0",
        ],
    );
    assert!(empty["shadow"]["bounds"].is_null());
    assert_eq!(
        empty["shadow"]["clipped"], false,
        "影を置いていないのに切れたと報告した: {}",
        empty["shadow"]
    );

    // 同じずらし量でも、影を置いたなら切れたと言う
    let pushed = cutout_on_canvas(
        dir.path(),
        &input,
        "pushed.png",
        &[
            "--canvas",
            "500",
            "--shadow",
            "synth",
            "--shadow-offset",
            "0,2000",
        ],
    );
    assert!(pushed["shadow"]["bounds"].is_null());
    assert_eq!(pushed["shadow"]["clipped"], true);
}

/// 既定値で影がキャンバスに収まっていれば `clipped` は偽。
///
/// **箱型の台（約 3σ）が縁を跨いだかどうかで決めていた頃は、既定値でも
/// 真になっていた。** 偽陽性が既定で出る旗は、読む側が無視するようになる。
#[test]
fn a_default_shadow_that_fits_the_canvas_is_not_reported_as_clipped() {
    let dir = fixture_dir();
    let img = product_image(&ProductSpec {
        width: 300,
        height: 300,
        ..Default::default()
    });
    let input = write_png(dir.path(), "in.png", &img);

    let v = cutout_on_canvas(
        dir.path(),
        &input,
        "out.png",
        &[
            "--canvas",
            "1000",
            "--fill-ratio",
            "0.7",
            "--shadow",
            "synth",
        ],
    );
    let shadow = &v["shadow"];
    assert_eq!(
        shadow["clipped"], false,
        "余白に収まっている影が切れたと報告された: {shadow}"
    );
    let bounds = shadow["bounds"].as_array().expect("影があるはず");
    assert!(
        bounds[0].as_u64().unwrap() > 0 && bounds[3].as_u64().unwrap() < 999,
        "矩形が縁に接していないのに clipped の判定が縁を見ている: {shadow}"
    );
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
    // 同じ理由で halo_ratio / edge_width / contour_roughness /
    // rim_contamination もキーを消さない。0 と報告すると「縁が残っていない」
    // 「輪郭は滑らか」「縁は汚れていない」に見えてしまい、「そもそも測れて
    // いない」と区別できなくなる
    for key in [
        "separability",
        "halo_ratio",
        "edge_width",
        "contour_roughness",
        "rim_contamination",
    ] {
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

/// matting の 3 つのスイッチが `settings` に出て、**実際に効いた**帯幅の
/// 下限も出ること。
///
/// `band_min_radius` は指定値からは読めない——輪郭が粗ければ粗さぶんだけ
/// 持ち上がる。`edge_threshold` の自動調整と同じで、**黙って変えた値は
/// 結果に出す**という約束の側にある。
#[test]
fn the_report_states_which_matting_took_effect() {
    let dir = fixture_dir();
    let img = product_image(&ProductSpec {
        width: 120,
        height: 120,
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
    assert_eq!(defaults["matting"], "guided");
    assert_eq!(defaults["smooth_contour"], 2.0);
    assert_eq!(defaults["reclassify"], true);
    assert!(
        defaults["band_min_radius"].as_u64().is_some(),
        "実際に効いた帯幅の下限が出ていない: {defaults}"
    );
    // **要求値と実効値を並べて出す。** `--smooth-contour` は長辺 1000px 換算
    // なので、指定値だけでは「実寸で何 px 均したか」を語らない
    assert!(
        defaults["smooth_radius_px"].as_u64().is_some(),
        "実際に効いた平滑化の半径が出ていない: {defaults}"
    );

    let plain = settings(&[
        "--matting",
        "projection",
        "--smooth-contour",
        "0",
        "--no-reclassify",
    ]);
    assert_eq!(plain["matting"], "projection");
    assert_eq!(plain["smooth_contour"], 0.0);
    assert_eq!(plain["reclassify"], false);

    // 帯そのものが無い経路では、帯幅の下限はキーごと出さない。
    // **0 と「帯が無い」を同じ形にしない**
    let legacy = settings(&["--no-refine"]);
    assert!(
        legacy.get("band_min_radius").is_none(),
        "--no-refine なのに帯幅の下限を名乗っている: {legacy}"
    );
    assert!(
        legacy.get("smooth_radius_px").is_none(),
        "--no-refine なのに平滑化の半径を名乗っている: {legacy}"
    );
}

/// `--smooth-contour` の上限が CLI と spec の両方で効くこと。
///
/// **要求値をそのまま結果に書き写していた。** 実効の半径は `RADIUS_CEILING`
/// （48px）で頭打ちになるので、100 と指定しても効くのはそこまでなのに
/// `settings.smooth_contour` には 100 が出ていた。「指定したのに効かない」が
/// 数値の上では見分けられない。入口で断り、実効値は別のキーで出す。
#[test]
fn an_out_of_range_smooth_contour_is_refused_everywhere() {
    let dir = fixture_dir();
    let img = product_image(&ProductSpec {
        width: 120,
        height: 120,
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
            "--force",
            "--json",
            "--smooth-contour",
            "100",
        ])
        .output()
        .unwrap();
    assert!(!out.status.success(), "上限を越えた指定が通っている");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("16"), "上限を文面に出していない: {stderr}");

    // spec も同じ関門を通ること。**CLI だけに置くと batch が素通りする**
    let spec = dir.path().join("spec.json");
    std::fs::write(
        &spec,
        serde_json::json!({
            "items": [{
                "input": input.to_str().unwrap(),
                "output": dir.path().join("b.png").to_str().unwrap(),
                "smooth_contour": 100.0,
            }]
        })
        .to_string(),
    )
    .unwrap();
    let out = kiri()
        .args(["batch", spec.to_str().unwrap(), "--json", "--force"])
        .output()
        .unwrap();
    let report = json_stdout(&out);
    let item = &report["results"][0];
    assert_eq!(
        item["status"], "error",
        "spec の上限越えが通っている: {report}"
    );
    assert!(
        item["error"]["message"]
            .as_str()
            .unwrap_or_default()
            .contains("16"),
        "上限を文面に出していない: {item}"
    );
}

/// 綴りを外した `matting` を黙って既定へ落とさないこと。
///
/// 数百点を回した後に仕上がりを見るまで気づけない種類の失敗になる。
#[test]
fn an_unknown_matting_is_refused_everywhere() {
    // CLI は clap が弾く（code の無い exit 2）
    let dir = fixture_dir();
    let img = product_image(&ProductSpec::default());
    let input = write_png(dir.path(), "a.png", &img);
    let out = kiri()
        .args([
            "cutout",
            input.to_str().unwrap(),
            "-o",
            dir.path().join("o.png").to_str().unwrap(),
            "--matting",
            "closed-form",
        ])
        .output()
        .unwrap();
    assert!(!out.status.success(), "未知の matting が通っている");

    // spec は自前の関門を通す
    let spec = dir.path().join("spec.json");
    std::fs::write(
        &spec,
        r#"{"items":[{"input":"a.png","output":"b.png","matting":"closed-form"}]}"#,
    )
    .unwrap();
    let out = kiri()
        .args(["batch", spec.to_str().unwrap(), "--json"])
        .output()
        .unwrap();
    let json = json_stdout(&out);
    assert_eq!(json["results"][0]["error"]["code"], "SPEC_INVALID");
}

/// spec が matting の 3 つを受けること。
#[test]
fn batch_accepts_the_matting_keys() {
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
        r#"{"defaults":{"matting":"projection","smooth_contour":0,"reclassify":false},
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
    let json = json_stdout(&out);
    assert_eq!(json["succeeded"], 1);
    let settings = &json["results"][0]["result"]["settings"];
    assert_eq!(settings["matting"], "projection");
    assert_eq!(settings["smooth_contour"], 0.0);
    assert_eq!(settings["reclassify"], false);
}

/// spec が `segment` と `model_path` を受けること。
///
/// **モデルは要らない。** ここで確かめるのは spec の入口が 2 つのキーを
/// 受け取って `settings` へ流すところまでで、`off` なら推論の経路は
/// 1 行も走らない（走らせる側は tests/segment.rs が受け持つ）。
#[test]
fn batch_accepts_the_segment_keys() {
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
        r#"{"defaults":{"segment":"off"},
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
    let json = json_stdout(&out);
    assert_eq!(json["succeeded"], 1);
    let settings = &json["results"][0]["result"]["settings"];
    assert_eq!(settings["segment"], "off");
    assert_eq!(settings["segment_ran"], false);
}

/// spec の `segment` は綴りを検査する。**未知の値を既定へ落とさない。**
///
/// 落とすと、その項目だけ黙ってモデル無しで処理され、数百点を回した後に
/// 仕上がりを見るまで気づけない（`matting` と同じ理由）。
#[test]
fn an_unknown_segment_value_is_refused() {
    let dir = fixture_dir();
    let spec = dir.path().join("spec.json");
    std::fs::write(
        &spec,
        r#"{"items":[{"input":"a.png","output":"b.png","segment":"isnet2"}]}"#,
    )
    .unwrap();
    let out = kiri()
        .args(["batch", spec.to_str().unwrap(), "--json"])
        .output()
        .unwrap();
    let json = json_stdout(&out);
    assert_eq!(json["results"][0]["error"]["code"], "SPEC_INVALID");
}

/// spec の綴り違いに候補を返すこと（`refine` と同じ関門）。
#[test]
fn a_misspelled_matting_key_suggests_the_right_one() {
    let dir = fixture_dir();
    let spec = dir.path().join("spec.json");
    std::fs::write(
        &spec,
        r#"{"items":[{"input":"a.png","output":"b.png","smooth_contur":1}]}"#,
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
            .contains("smooth_contour"),
        "候補に smooth_contour が出ていない: {}",
        json["error"]["hint"]
    );
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

/// spec が影の 5 つのキーを受けること。
#[test]
fn batch_accepts_the_shadow_keys() {
    let dir = fixture_dir();
    let img = product_image(&ProductSpec {
        width: 200,
        height: 200,
        ..Default::default()
    });
    write_png(dir.path(), "a.png", &img);
    let spec = dir.path().join("spec.json");
    std::fs::write(
        &spec,
        r##"{"defaults":{"canvas":"1000","shadow":"synth","shadow_offset":[6,20],
                         "shadow_blur":4,"shadow_color":"#204060","shadow_opacity":0.4},
             "items":[{"input":"a.png","output":"out.png"}]}"##,
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
    let json = json_stdout(&out);
    assert_eq!(json["succeeded"], 1);
    let result = &json["results"][0]["result"];
    assert_eq!(result["settings"]["shadow"], "synth");
    assert_eq!(result["shadow"]["offset"], serde_json::json!([6, 20]));
    assert_eq!(
        result["shadow"]["blur"].as_f64().unwrap(),
        round4(kiri::transform::shadow::effective_sigma(4.0))
    );
    assert_eq!(result["shadow"]["color"], "#204060");
    assert_eq!(result["shadow"]["opacity"], 0.4);
}

/// 影のキーの綴り違いも候補を返す。
///
/// `shadow_tolerance`（実写の影を消す側）と綴りが近いので、黙って無視されると
/// 「消す側を指定したつもりが足す側だった」の取り違えが残る。
#[test]
fn a_misspelled_shadow_offset_key_suggests_the_right_one() {
    let dir = fixture_dir();
    let spec = dir.path().join("spec.json");
    std::fs::write(
        &spec,
        r#"{"items":[{"input":"a.png","output":"b.png","shadow_ofset":[0,12]}]}"#,
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
            .contains("shadow_offset"),
        "候補に shadow_offset が出ていない: {}",
        json["error"]["hint"]
    );
}

/// spec でも範囲外の影の設定は断る（CLI と同じ関門）。
#[test]
fn an_out_of_range_shadow_opacity_is_refused_in_a_spec_too() {
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
        r#"{"items":[{"input":"a.png","output":"out.png","shadow":"synth","shadow_opacity":1.5}]}"#,
    )
    .unwrap();

    let out = kiri()
        .args(["batch", spec.to_str().unwrap(), "--json"])
        .output()
        .unwrap();
    let json = json_stdout(&out);
    assert_eq!(json["results"][0]["error"]["code"], "INVALID_SETTING");
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

    // 出口が揃っている。**5 は「成果物はあるが人が見るべき」**で、
    // 「0 以外は失敗」と読んでいる呼び出し側にとって新しい意味である
    let exits: Vec<u64> = v["exit_codes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["code"].as_u64().unwrap())
        .collect();
    assert_eq!(exits, vec![0, 1, 2, 3, 4, 5], "{v}");

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

/// 影の合成の 5 つのノブが契約に載ること。
///
/// **`--shadow-tolerance`（消す側）と綴りが並ぶ。** 6 つが同じ一覧に出て、
/// それぞれの summary が「消す」「足す」のどちらなのかを言えていないと、
/// エージェントは逆のノブを回す。
#[test]
fn schema_publishes_the_five_knobs_of_the_synthetic_shadow() {
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
            .unwrap_or_else(|| panic!("{name} が無い"))
            .clone()
    };

    let accepts: Vec<String> = option("--shadow")["accepts"]
        .as_array()
        .unwrap_or_else(|| panic!("--shadow が accepts を返さない"))
        .iter()
        .map(|x| x.as_str().unwrap().to_string())
        .collect();
    assert_eq!(accepts, vec!["off", "synth"]);
    assert_eq!(option("--shadow")["default"], "off");
    assert_eq!(option("--shadow-offset")["default"], "0,12");
    assert_eq!(option("--shadow-blur")["default"], "10");
    assert_eq!(option("--shadow-color")["default"], "#000000");
    assert_eq!(option("--shadow-opacity")["default"], "0.25");

    // 足す側と消す側が、1 行目だけで見分けられること
    let adds = option("--shadow")["summary"].as_str().unwrap().to_string();
    let removes = option("--shadow-tolerance")["summary"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(adds.contains("合成"), "足す側だと分からない: {adds}");
    assert!(removes.contains("消す"), "消す側だと分からない: {removes}");
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

/// `tests` 配下から `pub const X: &str = ...` の宣言だけを拾う。
///
/// 文書が名指しできるのは、テスト側では「契約として公開された綴り」だけで
/// ある（`KIRI_BENCH_DIR`）。それ以外の大文字の語がテストのどこかに現れたか
/// どうかは、文書の正しさと何の関係も無い。
fn collect_public_string_constants(dir: &Path, out: &mut String) {
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            collect_public_string_constants(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            let text = std::fs::read_to_string(&path).unwrap();
            for line in text.lines() {
                let line = line.trim();
                if line.starts_with("pub const ") && line.contains(": &str") {
                    out.push_str(line);
                    out.push('\n');
                }
            }
        }
    }
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

/// 計画書（docs/implementation-plan.md）だけが先に名指ししてよい、未実装の code。
/// **実装したら必ずここから消す**——実装済みのまま残っていると下の検査が落ちる。
/// README / design.md には許さない（エージェントが写し取る場所だから）
const PLANNED_CODES: &[&str] = &[
    // ROTATE_AUTO_SKIPPED と SET_SCALE_CLAMPED は Phase 23 で実装済み
    "WHITE_BALANCE_SKIPPED",
    "REFLECT_CLIPPED",
];

/// ドキュメントが名指しする code は、実在する code か実在する定数のどちらかである。
///
/// `every_documented_code_is_actually_reachable` はカタログ → 実装の向きしか見ない。
/// **逆向き（文書 → カタログ）が抜けていて、実際に幽霊を 2 つ通した**
/// （README の `NOT_FOUND` と design.md の `BACKGROUND_NOT_UNIFORM`）。
/// 書き写した例は、エージェントが最も写し取りやすい場所にある。
///
/// 計画書だけは例外で、これから作る code を先に名指しする。そこを一律に落とすと
/// 計画を書いた時点で赤くなり、検査ごと外したくなる。**予定の code を表で数え上げ、
/// 計画書の中でだけ通す。** 表に載ったまま実装された code と、計画書から消えたのに
/// 表に残った code は別の表明で落とす——表が実態から少しずつずれないようにするため
#[test]
fn every_code_named_in_the_docs_exists() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let v = schema_json();
    let mut known: Vec<String> = codes_of(&v, "errors");
    known.extend(codes_of(&v, "warnings"));

    // ドキュメントは Rust の定数名にも触れる（`MAX_CLEANUP` など）。
    // 実装に存在する定数は code ではないので、幽霊と区別して通す。
    //
    // `tests` も見るのは、**ベンチの入口が環境変数だから**である
    // （`KIRI_BENCH_DIR`）。本体の挙動ではないので `src` には置き場所が無いが、
    // README が名指しする以上、実在することは確かめたい。**ただし拾うのは
    // `pub const X: &str` の宣言だけにする。** テストのソース全文を許すと、
    // 表明の文字列やコメントに大文字の語が 1 度でも出てきた時点で「実在する」
    // ことになってしまい、この検査は何も守らなくなる
    let mut source = String::new();
    collect_source(&root.join("src"), &mut source);
    collect_public_string_constants(&root.join("tests"), &mut source);

    // 逆向きも見る。計画書が名指しをやめた code が表に残ると、次に誰かが同じ名前を
    // 計画書へ書いたとき、検討されないまま素通しになる
    let plan =
        all_caps_words(&std::fs::read_to_string(root.join("docs/implementation-plan.md")).unwrap());
    for p in PLANNED_CODES {
        assert!(
            !known.iter().any(|k| k == p),
            "{p} は実装済みなので PLANNED_CODES から消すこと"
        );
        assert!(
            plan.iter().any(|w| w == p),
            "{p} は計画書が名指ししていないので PLANNED_CODES から消すこと"
        );
    }

    for doc in ["README.md", "docs/design.md", "docs/implementation-plan.md"] {
        let text = std::fs::read_to_string(root.join(doc)).unwrap();
        for name in all_caps_words(&text) {
            let is_code = known.contains(&name);
            let is_constant = source.contains(&format!("const {name}"));
            let is_planned =
                doc == "docs/implementation-plan.md" && PLANNED_CODES.contains(&name.as_str());
            assert!(
                is_code || is_constant || is_planned,
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
///
/// `candidates[]` のように `[]` で終わる区間は配列で、その先は要素の中を指す。
/// **要素は 1 つ目だけを見る。** 契約が言っているのは「どの要素もこの形を
/// 持つ」であって、空の配列はそもそも path を確かめる材料にならない。
fn pick<'a>(report: &'a Value, path: &str) -> Option<&'a Value> {
    let mut node = report;
    for segment in path.split('.') {
        match segment.strip_suffix("[]") {
            Some(name) => node = node.get(name)?.get(0)?,
            None => node = node.get(segment)?,
        }
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
    // **隙間は 1 文字とは限らない。** 3 文字の窓で「日本語・空白・日本語」を
    // 探していた頃は、**行継続を書き忘れた箇所を 1 つも拾えなかった**——`\` の
    // 無い文字列リテラルは改行と次行の字下げをそのまま抱えるので、隙間が
    // 改行 1 つと空白 17 個になり、窓の 3 文字目が空白のままになる。
    // 仕掛けた網が一番大きな獲物だけを通す形で、実際 `schema.rs` の 5 箇所が
    // そうやって漏れていた。**空白の連なりを 1 つの隙間として見る。**
    //
    // 改行も隙間に含めるが、**空白を 1 つも含まない改行は咎めない**——
    // `--fail-on` の長いヘルプのように、段落を分けるために意図して置いた `\n`
    // は正しい書き方である。
    let mut check = |label: String, text: Option<&str>| {
        let Some(text) = text else { return };
        let chars: Vec<char> = text.chars().collect();
        let gap = |c: char| c == ' ' || c == '\n';
        let mut i = 0;
        while i < chars.len() {
            if !gap(chars[i]) {
                i += 1;
                continue;
            }
            let start = i;
            while i < chars.len() && gap(chars[i]) {
                i += 1;
            }
            let (Some(&before), Some(&after)) = (chars.get(start.wrapping_sub(1)), chars.get(i))
            else {
                continue;
            };
            let spaces = chars[start..i].iter().filter(|&&c| c == ' ').count();
            // **日本語の直後に空白が 2 つ以上続いたら、後ろが何であれ書き損じ。**
            // 行継続を書き忘れた箇所の半分は次の行が英数字で始まっており
            // （「回る）。<空白>true なら」）、両隣が日本語であることを求めると
            // そこを見逃す。列を揃えるための 2 連スペース（`off  … `）は
            // 英数字の後ろにしか現れないので、これで取り違えない
            if spaces > 0 && japanese(before) && (japanese(after) || spaces >= 2) {
                stray.push(format!(
                    "{label}: 「{before}{}{after}」（空白 {spaces} 個）",
                    " ".repeat(spaces),
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

/// README の警告の表は、契約の `warnings[]` を 1 つも落としていない。
///
/// 表の直後に「この表は `kiri schema --json` の `warnings[]` が同じものを返す」と
/// 書いてある。ところが `every_code_named_in_the_docs_exists` は**文書 → カタログ**の
/// 向きしか見ないので、**カタログへ足して表へ足し忘れる**と素通りする。実際
/// Phase 19 の 2 つがそうやって抜けた。逆向きをここで塞ぐ。
///
/// 見るのは警告だけである。README は error の全一覧を持たない（持つと名乗っても
/// いない）ので、同じ表明を errors[] へ広げると「文書に無いから落ちる」だけの
/// 検査になる。**同期を守るのは、同期すると書いてある表に対してだけ意味がある。**
#[test]
fn the_readme_warning_table_lists_every_warning_in_the_contract() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let readme = std::fs::read_to_string(root.join("README.md")).unwrap();
    // 表の行は `| `CODE` | 意味 |`。code を名乗る行だけを拾う
    let listed: Vec<&str> = readme
        .lines()
        .filter_map(|line| line.strip_prefix("| `"))
        .filter_map(|rest| rest.split('`').next())
        .filter(|word| {
            !word.is_empty()
                && word
                    .chars()
                    .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
        })
        .collect();

    let missing: Vec<String> = codes_of(&schema_json(), "warnings")
        .into_iter()
        .filter(|code| !listed.contains(&code.as_str()))
        .collect();
    assert!(
        missing.is_empty(),
        "README の警告の表に載っていない code がある（{} 件）: {}",
        missing.len(),
        missing.join(" / ")
    );
}

/// README の exit code の表は、`ErrorKind::meaning()` の文言をそのまま並べる。
///
/// `meaning()` の doc は「README の表と同じ文言をここから配る」と名乗っている。
/// **名乗っただけでは守られない**——0〜4 行目は 1 文字違わず一致していたのに、
/// **その契約を足した当のコミットで 5 行目が破られていた。** 片方だけを直すと
/// ここが落ちる。exit code は「0 以外は失敗」と読んでいる呼び出し側にとって
/// 意味の分かれ目なので、説明が 2 通りあってはならない。
#[test]
fn the_readme_exit_code_table_quotes_the_published_meanings() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let readme = std::fs::read_to_string(root.join("README.md")).unwrap();
    for kind in kiri::error::ErrorKind::ALL {
        let prefix = format!("| {} | ", kind.exit_code());
        let row = readme
            .lines()
            .find(|line| line.starts_with(&prefix))
            .unwrap_or_else(|| panic!("README に exit {} の行が無い", kind.exit_code()));
        assert!(
            row.contains(kind.meaning()),
            "exit {} の行が meaning() と食い違う\n  README : {row}\n  meaning: {}",
            kind.exit_code(),
            kind.meaning()
        );
    }
}

/// 梯子の段は、実装と配る文面で同じ綴りである。
///
/// 同じ 7 つの数が `--max-bytes` の長いヘルプと `outputs[].quality_used` の
/// notes に出る。**手書きが 2 つあれば、段を動かしたとき片方だけが古くなる。**
/// どちらも `QUALITY_LADDER` と突き合わせる
#[test]
fn the_published_prose_spells_the_real_ladder() {
    let spelled = ladder()
        .iter()
        .map(|q| q.to_string())
        .collect::<Vec<_>>()
        .join(" / ");
    let v = schema_json();

    let detail = v["commands"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["name"] == "convert")
        .unwrap()["options"]
        .as_array()
        .unwrap()
        .iter()
        .find(|o| o["name"] == "--max-bytes")
        .unwrap_or_else(|| panic!("--max-bytes が無い: {v}"))["detail"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(
        detail.contains(&spelled),
        "--max-bytes のヘルプが梯子 '{spelled}' を綴っていない: {detail}"
    );

    let notes = fields_of(&v)
        .into_iter()
        .find(|f| f["path"] == "outputs[].quality_used")
        .unwrap()["notes"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(
        notes.contains(&spelled),
        "quality_used の notes が梯子 '{spelled}' を綴っていない: {notes}"
    );
}

/// `fields[].unit` の綴りは決まった語彙の中にある。
///
/// schema は `unit` をそのまま配るので、これも契約である。**一覧を持たずに
/// 増やすと、受け手は `unit` で分岐できなくなる**（`ratio` と `quality` の
/// ように、値域が違うのに同じ綴りへ寄せてしまう誤りも起きる）。
/// 語彙は `FieldEntry::unit` の doc コメントと同じもの
#[test]
fn every_published_unit_is_in_the_known_vocabulary() {
    const KNOWN: &[&str] = &[
        "ratio",
        "delta_e",
        "gradient",
        "px",
        "px_at_1000",
        "deg",
        // 数値か "auto" の union（`settings.rotate`）。`deg` と分けるのは、
        // 受け手が `as_f64()` で読んでよいかがここで変わるため
        "deg_or_auto",
        "ms",
        "count",
        "quality",
        "bool",
        "enum",
        "path",
        // 決まった選択肢を持たない文字列（`compliance.fail_on`）。`enum` と
        // 分けるのは、受け手が値を照合してよいかがここで変わるため
        "text",
        "list",
        "normalized_bbox",
    ];
    for f in fields_of(&schema_json()) {
        let unit = f["unit"]
            .as_str()
            .unwrap_or_else(|| panic!("{f} が unit を持たない"));
        assert!(
            KNOWN.contains(&unit),
            "{} の unit '{unit}' が語彙に無い（FieldEntry::unit の doc も直すこと）",
            f["path"]
        );
    }
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
    // **`constraints` は指示を渡したときだけ現れる。** 指示なしの結果で探すと
    // 「配った path が存在しない」になるので、指示を渡した実行も用意する。
    // `notes` がそう述べていることは `a_nullable_field_says_what_null_means` と
    // 同じ流儀で文面の側が守る
    let constrained = run(&[
        "cutout",
        input.to_str().unwrap(),
        "-o",
        output.to_str().unwrap(),
        "--dry-run",
        "--json",
        "--fg-seed",
        "100,100",
    ]);
    // **`optimize` も探索が走ったときだけ現れる。** 指示と同じ理由で、
    // 走らせた実行を別に 1 つ用意する
    let optimized = run(&[
        "cutout",
        input.to_str().unwrap(),
        "-o",
        output.to_str().unwrap(),
        "--dry-run",
        "--json",
        "--optimize",
    ]);
    // **`rotate` も回したときだけ現れる。** `cutout --rotate` と `kiri rotate` の
    // 両方が同じブロックを返すので、`appears_in` が名指しする 2 つとも用意する
    let rotated = run(&[
        "cutout",
        input.to_str().unwrap(),
        "-o",
        output.to_str().unwrap(),
        "--dry-run",
        "--json",
        "--rotate",
        "90",
    ]);
    let turned = run(&[
        "rotate",
        input.to_str().unwrap(),
        "-o",
        output.to_str().unwrap(),
        "--dry-run",
        "--json",
        "--angle",
        "90",
    ]);
    // `outputs[]` は書き出すコマンドすべてに出る。convert / resize も名指しされる
    let converted = run(&[
        "convert",
        input.to_str().unwrap(),
        "-o",
        output.to_str().unwrap(),
        "--dry-run",
        "--json",
    ]);
    let resized = run(&[
        "resize",
        input.to_str().unwrap(),
        "-o",
        output.to_str().unwrap(),
        "--width",
        "200",
        "--dry-run",
        "--json",
    ]);
    // **`compliance` も `--fail-on` を渡したときだけ現れる。** 条件は必ず通る
    // ものを選ぶ——`run` は成功を要求するので、ここで不合格にすると exit 5 で
    // 落ちる（それ自体は別の検査が固定している）
    let gated = run(&[
        "cutout",
        input.to_str().unwrap(),
        "-o",
        output.to_str().unwrap(),
        "--dry-run",
        "--json",
        "--fail-on",
        "foreground_ratio>0.99",
    ]);
    // **`settings.profile` も `--profile` を渡したときだけ現れる。** 渡さない実行の
    // 結果 JSON は `--profile` を足す前と 1 バイトも変わらない、というのが
    // そのブロックの約束なので、`compliance.` / `optimize.` と同じく専用の実行が要る
    let profiled = run(&[
        "cutout",
        input.to_str().unwrap(),
        "-o",
        output.to_str().unwrap(),
        "--dry-run",
        "--json",
        "--profile",
        "amazon",
    ]);
    // **lint の結果 JSON は `LintReport` そのもの**なので、path に接頭辞が無い
    // （`passed` / `code` / `checks`）。`run` は成功を要求するので、**必ず通る
    // profile を選ぶ**——不合格は exit 5 で落ちる（それ自体は別の検査が固定する）。
    // shopify は寸法とバイト数しか規定しないので、合成した小さい JPEG は素通りする
    let linted = run(&[
        "lint",
        input.to_str().unwrap(),
        "--profile",
        "shopify",
        "--json",
    ]);
    // **`shadow` も合成したときだけ現れる。** 既定の実行で探すと「配った path が
    // 存在しない」になるので、影を足した実行も用意する（`constraints` と同じ扱い）
    let shadowed = run(&[
        "cutout",
        input.to_str().unwrap(),
        "-o",
        output.to_str().unwrap(),
        "--dry-run",
        "--json",
        "--shadow",
        "synth",
    ]);
    // **`segment` もモデルが走ったときだけ現れる。** モデルは 176MB あって
    // リポジトリにも CI にも置かないので、無ければその path だけを飛ばす。
    // 「配ったが確かめられなかった」と「配ったのに無い」は別で、後者だけを
    // 落とす
    // **`set` は spec に書いた batch の実行にしか現れない。** `compliance` /
    // `optimize` と同じ扱いで、専用の実行を 1 つ用意する。`--dry-run` で
    // 回すのは、pass 1（代表寸法の測り）が 1 バイトも書かずに走る規約を
    // ここでも使うためである
    let batched = {
        let spec = write_spec(
            dir.path(),
            r#"{"set":{"align":"height"},
                 "defaults":{"canvas":"500x500","format":"png"},
                 "items":[{"input":"product.jpg","output":"out/a.png"}]}"#,
        );
        let out = run_batch(&spec, &["--dry-run"]);
        assert!(
            out.status.success(),
            "batch: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        json_stdout(&out)
    };
    let segmented = segment_ready().then(|| {
        (
            run(&[
                "info",
                input.to_str().unwrap(),
                "--segment",
                "isnet",
                "--json",
            ]),
            run(&[
                "cutout",
                input.to_str().unwrap(),
                "-o",
                output.to_str().unwrap(),
                "--dry-run",
                "--json",
                "--segment",
                "isnet",
            ]),
        )
    });

    for f in fields_of(&schema_json()) {
        let path = f["path"].as_str().unwrap();
        let commands: Vec<&str> = f["appears_in"]
            .as_array()
            .unwrap_or_else(|| panic!("{path} が appears_in を持たない"))
            .iter()
            .map(|c| c.as_str().unwrap())
            .collect();
        assert!(!commands.is_empty(), "{path} の appears_in が空");
        if path.starts_with("segment.") && segmented.is_none() {
            continue;
        }

        for command in commands {
            let report = match command {
                _ if path.starts_with("segment.") => {
                    let (info, cutout) = segmented.as_ref().unwrap();
                    if command == "info" { info } else { cutout }
                }
                "info" => &info,
                "cutout" if path.starts_with("constraints.") => &constrained,
                "cutout" if path.starts_with("optimize.") => &optimized,
                "cutout" if path.starts_with("shadow.") => &shadowed,
                "cutout" if path.starts_with("compliance.") => &gated,
                "cutout" if path.starts_with("settings.") => &profiled,
                "cutout" if path.starts_with("rotate.") => &rotated,
                "lint" => &linted,
                "batch" => &batched,
                "rotate" => &turned,
                "convert" => &converted,
                "resize" => &resized,
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
///
/// # `--segment` の実行も材料に入れる
///
/// `segment.*` は**走らせないと 1 つも現れない**ので、指示なしの実行だけを
/// 並べていると契約のその一角がまるごと素通りする。実際 `info` は
/// `segment.uncertain_ratio` を返しながら `SEGMENT_UNCERTAIN` を出し忘れて
/// いた——`cutout` だけが出していたので、コマンドを 1 つしか回さない検査では
/// 見えなかった。モデルが無ければその 2 本だけを飛ばす。
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
    // **モデルが迷う材料を 1 枚だけ通す。** 単色背景の合成商品ではモデルが
    // 素直に言い切ってしまい、不明の帯は 5% 程度にしかならない（しきい値は
    // 0.3）。暗いキーの格子なら実写キーボードと同じ形で迷う
    if segment_ready() {
        let keys = write_png(dir.path(), "keys.png", &dense_key_grid(256, 256));
        let output = dir.path().join("keys-cutout.png");
        for args in [
            vec![
                "info".to_string(),
                keys.display().to_string(),
                "--segment".into(),
                "isnet".into(),
                "--json".into(),
            ],
            vec![
                "cutout".to_string(),
                keys.display().to_string(),
                "-o".into(),
                output.display().to_string(),
                "--dry-run".into(),
                "--json".into(),
                "--segment".into(),
                "isnet".into(),
            ],
        ] {
            let out = kiri().args(&args).output().unwrap();
            if out.status.success() {
                reports.push((format!("keys/{}", args[0]), json_stdout(&out)));
            }
        }
    } else {
        eprintln!(
            "モデルが無いので飛ばす: {}",
            kiri::segment::model::ISNET
                .expected_path()
                .map(|p| p.display().to_string())
                .unwrap_or_else(|| "(置き場所を決められません)".to_string())
        );
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

/// **`subject` の較正表。`--ignored` を付けたときだけ走る。**
///
/// README と `subject.rs` の表はここから取り直す。表の数値がどの画像から
/// 出たのかを、コミットの外に置かないためである。
///
/// 1 色と場の 2 列を並べる。**主体は 1 色の背景に対して測る**ので 2 列は
/// 一致するはずで、一致しなくなったら背景のモデルが主体検出へ漏れている。
///
/// ```text
/// cargo test --release --test cli -- --ignored --nocapture print_the_subject_calibration
/// ```
#[test]
#[ignore = "計測用。較正表を出すだけ"]
fn print_the_subject_calibration() {
    println!(
        "\n{:<34} {:>7} {:>8} {:>9}  {:<5} {:>7} {:>8} {:>9}  {:<5}  正解",
        "シーン", "area", "capture", "leftover", "1色", "area", "capture", "leftover", "場"
    );
    for (name, image, want_high) in common::subject_scenes() {
        let dir = fixture_dir();
        let input = write_png(dir.path(), "s.png", &image);
        let mut row = format!("{name:<34}");
        for model in ["flat", "auto"] {
            let v = json_stdout(
                &kiri()
                    .args([
                        "info",
                        input.to_str().unwrap(),
                        "--background-model",
                        model,
                        "--json",
                    ])
                    .output()
                    .unwrap(),
            );
            let s = &v["subject"];
            row.push_str(&format!(
                " {:>7.3} {:>8.3} {:>9.3}  {:<5}",
                s["area_ratio"].as_f64().unwrap_or(f64::NAN),
                s["capture_ratio"].as_f64().unwrap_or(f64::NAN),
                s["leftover_ratio"].as_f64().unwrap_or(f64::NAN),
                s["confidence"].as_str().unwrap_or("null")
            ));
        }
        println!("{row}  {}", if want_high { "high" } else { "low" });
    }
}

/// **較正表の判定が 1 件も裏返らないこと。**
///
/// `high` は「この矩形に従って切り抜いてよい」という助言そのものである。
/// 裏返れば、誤った矩形へ誘導するか、助言を出し損ねる。
///
/// **背景のモデルを変えても判定は動かない。** 主体は 1 色の背景に対して
/// 測るからで（`cutout::analyse_background` の表を参照）、場に対して測る案は
/// この 11 行と実写 2 枚で 2 件を裏返したので採らなかった。2 列を並べて
/// 問うのは、その約束が黙って破れないようにするためである。
#[test]
fn the_subject_verdicts_do_not_flip_between_the_two_background_models() {
    for (name, image, want_high) in common::subject_scenes() {
        let dir = fixture_dir();
        let input = write_png(dir.path(), "s.png", &image);
        for model in ["flat", "auto"] {
            let v = json_stdout(
                &kiri()
                    .args([
                        "info",
                        input.to_str().unwrap(),
                        "--background-model",
                        model,
                        "--json",
                    ])
                    .output()
                    .unwrap(),
            );
            let s = &v["subject"];
            let got = s["confidence"].as_str().unwrap_or("null");
            assert_eq!(
                got,
                if want_high { "high" } else { "low" },
                "{name} / {model}: 判定が裏返った: {s}"
            );
        }
    }
}

// --- 探索を kiri に任せる（--optimize, Phase 15） ---

/// 探索の記録が結果 JSON に載り、**選ばれた候補と `settings` が一致する**こと。
///
/// 一致していなければ、エージェントは「表の 1 位」と「実際に書き出された絵」を
/// 別のものとして読む。`settings` は効いた値を出す規約なので、`chosen` の側が
/// 記録として意味を持つには両者が同じでなければならない。
#[test]
fn optimize_reports_every_candidate_and_agrees_with_the_settings() {
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
            "--optimize",
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

    let optimize = &v["optimize"];
    let candidates = optimize["candidates"]
        .as_array()
        .unwrap_or_else(|| panic!("candidates が配列ではない: {optimize}"));
    assert!(!candidates.is_empty(), "候補が 1 つも無い: {optimize}");
    assert_eq!(
        candidates.iter().filter(|c| c["chosen"] == true).count(),
        1,
        "選ばれた候補がちょうど 1 つでない: {optimize}"
    );
    let chosen = &optimize["chosen"];
    assert_eq!(chosen["stage"], "final", "選ばれた候補は原寸で回すべき");
    assert_eq!(chosen["tolerance"], v["settings"]["tolerance"]);
    assert_eq!(v["settings"]["optimize"], true);
    // 矩形を使わない候補が選ばれたなら applied_bbox ごと無い、という対応も見る
    assert_eq!(
        chosen["bbox"].is_null(),
        v.get("applied_bbox").is_none(),
        "chosen.bbox と applied_bbox が食い違う: {v}"
    );
    assert!(optimize["searched_at"].as_u64().unwrap() > 0, "{optimize}");
}

/// `--optimize` を渡さない実行では `optimize` ブロックごと現れない。
///
/// `null` を出すと「探索したが何も出なかった」と読める。走ったかどうかは
/// `settings.optimize` が真偽で言う（`segment` と同じ規約）。
#[test]
fn a_plain_cutout_has_no_optimize_block() {
    let dir = fixture_dir();
    let img = product_image(&ProductSpec::default());
    let input = write_png(dir.path(), "in.png", &img);
    let output = dir.path().join("out.png");

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
    let v = json_stdout(&out);
    assert!(v.get("optimize").is_none(), "{v}");
    assert_eq!(v["settings"]["optimize"], false);
}

/// **明示した値は探索しない。** `--tolerance 30 --optimize` は
/// 「30 に固定して残りの軸を探す」の意味になる。
///
/// 既定値と同じ 12 を明示した場合も同じでなければならない。`--tolerance` は
/// `default_value_t` を持つので、解いた後の値では区別が付かない——ここが
/// 落ちるなら `ValueSource` を見る経路が切れている。
#[test]
fn an_explicit_tolerance_is_not_searched() {
    let dir = fixture_dir();
    let img = product_image(&ProductSpec {
        width: 200,
        height: 200,
        ..Default::default()
    });
    let input = write_png(dir.path(), "in.png", &img);
    let output = dir.path().join("out.png");

    let tolerances = |value: &str| -> Vec<f64> {
        let out = kiri()
            .args([
                "cutout",
                input.to_str().unwrap(),
                "-o",
                output.to_str().unwrap(),
                "--tolerance",
                value,
                "--optimize",
                "--json",
                "--force",
            ])
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        json_stdout(&out)["optimize"]["candidates"]
            .as_array()
            .unwrap()
            .iter()
            .map(|c| c["tolerance"].as_f64().unwrap())
            .collect()
    };

    for value in ["30", "12"] {
        let got = tolerances(value);
        let want: f64 = value.parse().unwrap();
        assert!(
            got.iter().all(|t| *t == want),
            "--tolerance {value} を明示したのに他の値を試した: {got:?}"
        );
    }
}

/// `--dry-run` と併用できる。成果物は 1 バイトも書かれない。
#[test]
fn optimize_writes_nothing_under_dry_run() {
    let dir = fixture_dir();
    let img = product_image(&ProductSpec::default());
    let input = write_png(dir.path(), "in.png", &img);
    let output = dir.path().join("out.png");

    let out = kiri()
        .args([
            "cutout",
            input.to_str().unwrap(),
            "-o",
            output.to_str().unwrap(),
            "--optimize",
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
    assert_eq!(v["dry_run"], true);
    assert!(v["optimize"]["candidates"].as_array().unwrap().len() > 1);
    assert!(!output.exists(), "dry-run なのに成果物が書かれている");
}

/// spec の `optimize: true` が効き、そこに書いた値は探索の軸から外れる。
///
/// CLI 側は clap の `ValueSource` を見て「明示した」を判断するが、spec では
/// `Some` がそのまま明示である。**同じ問いに 2 つの経路で答えている**ので、
/// 片方だけが効いていないことがありうる。
#[test]
fn batch_accepts_the_optimize_key() {
    let dir = fixture_dir();
    let img = product_image(&ProductSpec {
        width: 200,
        height: 200,
        ..Default::default()
    });
    write_png(dir.path(), "a.png", &img);
    write_png(dir.path(), "b.png", &img);
    let spec = dir.path().join("spec.json");
    std::fs::write(
        &spec,
        r#"{"defaults":{"optimize":true},
             "items":[{"input":"a.png","output":"a.out.png"},
                      {"input":"b.png","output":"b.out.png","tolerance":30}]}"#,
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
    let v = json_stdout(&out);
    assert_eq!(v["succeeded"], 2, "{v}");
    let results = v["results"].as_array().unwrap();

    let free = &results[0]["result"];
    assert_eq!(free["settings"]["optimize"], true);
    assert!(
        free["optimize"]["candidates"].as_array().unwrap().len() > 1,
        "spec の optimize が効いていない: {}",
        free["optimize"]
    );

    let fixed = &results[1]["result"];
    let tolerances: Vec<f64> = fixed["optimize"]["candidates"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["tolerance"].as_f64().unwrap())
        .collect();
    assert!(
        tolerances.iter().all(|t| *t == 30.0),
        "spec に書いた tolerance が探索されている: {tolerances:?}"
    );
}

/// 色では分けられない素材では、全候補を試しても致命的な警告が残る。
///
/// **その事実そのものが報告である。** 20 通り試して駄目だったなら、残る手は
/// 素材を変えるか、色ではない手がかり（モデル）を足すかしかない。
/// 実写のキーボード（暗い机の上の黒いキーボード）がこの形で、合成では
/// 「背景と色がほとんど同じ商品が下端で見切れている」場面が同じ code を返す。
#[test]
fn an_inseparable_scene_says_that_no_candidate_was_clean() {
    let dir = fixture_dir();
    let mut img = image::RgbaImage::from_pixel(200, 200, image::Rgba([120, 118, 112, 255]));
    for y in 90..200 {
        for x in 50..150 {
            img.put_pixel(x, y, image::Rgba([126, 124, 118, 255]));
        }
    }
    let input = write_png(dir.path(), "flat.png", &img);
    let output = dir.path().join("out.png");

    let out = kiri()
        .args([
            "cutout",
            input.to_str().unwrap(),
            "-o",
            output.to_str().unwrap(),
            "--optimize",
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
    assert!(
        has_warning(&v, "OPTIMIZE_NO_CLEAN_CANDIDATE"),
        "{:?}",
        warning_codes(&v)
    );
    assert!(
        v["optimize"]["chosen"]["score"]["fatal"].as_u64().unwrap() > 0,
        "警告は出ているのに fatal が 0: {}",
        v["optimize"]["chosen"]
    );
    let warning = v["warnings"]
        .as_array()
        .unwrap()
        .iter()
        .find(|w| w["code"] == "OPTIMIZE_NO_CLEAN_CANDIDATE")
        .unwrap();
    let remaining = warning["data"]["remaining"].as_array().unwrap();
    assert!(
        !remaining.is_empty(),
        "残った code を載せていない: {warning}"
    );
    // **矩形の勧めは「手詰まり」に数えない。** BBOX_RECOMMENDED は矩形つきで
    // 次の一手を言っているので、ここに混ぜると「その矩形を渡せ」と
    // 「撮り直せ」が同じ結果に並ぶ
    assert!(
        !remaining.iter().any(|c| c == "BBOX_RECOMMENDED"),
        "矩形の勧めを手詰まりの理由に混ぜている: {warning}"
    );
}

/// `kiri schema` が `--optimize` と新しい code を配る。
///
/// **エージェントはまず schema を読む。** 載っていない code が飛んでくると、
/// 受け手は分岐を書きようがない。
#[test]
fn schema_publishes_the_optimize_option_and_its_warning() {
    let v = schema_json();
    let cutout = v["commands"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["name"] == "cutout")
        .expect("cutout がある");
    let option = cutout["options"]
        .as_array()
        .unwrap()
        .iter()
        .find(|o| o["name"] == "--optimize")
        .unwrap_or_else(|| panic!("--optimize が schema に無い: {cutout}"));
    assert_eq!(option["takes_value"], false, "フラグである");
    assert!(
        option["detail"]
            .as_str()
            .unwrap_or("")
            .contains("OPTIMIZE_NO_CLEAN_CANDIDATE"),
        "長いヘルプが code を案内していない: {option}"
    );
    assert!(
        v["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .any(|w| w["code"] == "OPTIMIZE_NO_CLEAN_CANDIDATE"),
        "警告の一覧に code が無い"
    );
}

/// 明示した `--bbox` と `--background-model` も探索の軸から外れる。
///
/// **3 つの軸は別々の id で `ValueSource` を引いている。** 綴りを 1 つ外しても
/// clap は `None` を返すだけなので（未知の id で panic しない）、経路が切れても
/// 「明示した値が黙って探索される」という形でしか現れない。`--tolerance` は
/// `an_explicit_tolerance_is_not_searched` が見ているので、残る 2 つをここで見る。
#[test]
fn an_explicit_bbox_and_background_model_are_not_searched() {
    let dir = fixture_dir();
    let img = product_image(&ProductSpec {
        width: 200,
        height: 200,
        ..Default::default()
    });
    let input = write_png(dir.path(), "in.png", &img);
    let output = dir.path().join("out.png");

    let candidates = |extra: &[&str]| -> Vec<Value> {
        let mut args = vec![
            "cutout",
            input.to_str().unwrap(),
            "-o",
            output.to_str().unwrap(),
            "--optimize",
            "--json",
            "--force",
        ];
        args.extend_from_slice(extra);
        let out = kiri().args(&args).output().unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        json_stdout(&out)["optimize"]["candidates"]
            .as_array()
            .unwrap()
            .clone()
    };

    let boxed = candidates(&["--bbox", "10,10,180,180"]);
    assert!(
        boxed
            .iter()
            .all(|c| c["bbox"] == serde_json::json!([10, 10, 180, 180])),
        "--bbox を明示したのに別の矩形を試した: {boxed:?}"
    );

    // **既定値と同じ `auto` を明示した場合も外れる。** `--background-model` は
    // `default_value_t` を持つので、値だけでは明示と既定を区別できない
    for model in ["flat", "auto"] {
        let got = candidates(&["--background-model", model]);
        assert!(
            got.iter().all(|c| c["background_model"] == model),
            "--background-model {model} を明示したのに別のモデルを試した: {got:?}"
        );
    }
}

/// 順位の第 1 項は「出た警告の数」ではなく「それ + 前景比率の崩れ」である。
///
/// **`score.fatal` は警告の数だけを出す。** `OPTIMIZE_NO_CLEAN_CANDIDATE` が
/// 数える対象と同じものでなければ、`fatal > 0` なのに警告が出ない状態が生まれ、
/// schema の notes（0 でなければ同時に出る）がそのまま嘘になる。
#[test]
fn the_collapse_flag_is_reported_apart_from_the_warning_count() {
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
            "--optimize",
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

    let fatal_codes: Vec<&str> = kiri::cutout::optimize::FATAL_CODES
        .iter()
        .map(|c| c.as_str())
        .collect();
    for c in v["optimize"]["candidates"].as_array().unwrap() {
        assert!(c["collapsed"].is_boolean(), "collapsed が真偽ではない: {c}");
        let counted = c["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|w| fatal_codes.contains(&w.as_str().unwrap()))
            .count();
        assert_eq!(
            c["score"]["fatal"].as_u64().unwrap() as usize,
            counted,
            "score.fatal が出た警告の数と食い違う: {c}"
        );
    }
    // 契約の側（schema）が数える code の一覧と実装が揃っていること
    let notes = fields_of(&schema_json())
        .into_iter()
        .find(|f| f["path"] == "optimize.chosen.score.fatal")
        .expect("optimize.chosen.score.fatal がある")["notes"]
        .as_str()
        .unwrap()
        .to_string();
    for code in &fatal_codes {
        assert!(
            notes.contains(code),
            "schema が数える code に {code} が載っていない: {notes}"
        );
    }
    // **順位の致命と警告の致命は別である。** 0 でなくても
    // OPTIMIZE_NO_CLEAN_CANDIDATE が出るとは限らないことを notes が言うこと
    assert!(
        notes.contains("OPTIMIZE_NO_CLEAN_CANDIDATE が出るとは限らない"),
        "fatal と警告の関係が誤って読める notes: {notes}"
    );
}

/// **綺麗な候補に当たったら原寸で 1 回しか回さない。**
///
/// 原寸 1 回が 24.5MP で数秒あるので、早期打ち切りが所要時間の要である。
/// `stage` が `final` の候補の数がそのまま「原寸で回した回数」なので、
/// そこを固定すれば打ち切りが効いているかを結果 JSON だけで読める。
#[test]
fn a_clean_scene_only_runs_one_candidate_at_full_size() {
    let dir = fixture_dir();
    let img = product_image(&ProductSpec {
        width: 200,
        height: 200,
        noise: false,
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
            "--optimize",
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
    let candidates = v["optimize"]["candidates"].as_array().unwrap();
    let finals = candidates.iter().filter(|c| c["stage"] == "final").count();
    assert_eq!(
        finals, 1,
        "綺麗な素材なのに原寸で 2 回回している: {}",
        v["optimize"]
    );
    assert_eq!(v["optimize"]["chosen"]["stage"], "final");
    // 打ち切ったのだから、選ばれた候補には致命も品質の警告も無いはず
    assert_eq!(v["optimize"]["chosen"]["score"]["fatal"], 0);
    assert_eq!(v["optimize"]["chosen"]["collapsed"], false);
}

/// **1 位が原寸で商品を飲んだら打ち切らず、2 位まで回す。**
///
/// 商品を飲んだ結果は警告を 1 つも出さずに指標だけ良くなるので、`warnings`
/// だけを見ていると 1 つ目で止まってしまう。崩れは「同じ候補の探索段の
/// 前景比率」と比べて初めて分かる。
///
/// 背景と商品の色差を小さく取ると、許容量を上げた候補が商品ごと飲む。
/// 原寸でだけ飲むかどうかは素材次第なので、ここで問うのは
/// **「綺麗でない候補が 1 位なら 2 つ目も回す」**という打ち切りの条件そのもの。
#[test]
fn a_finalist_that_is_not_clean_does_not_stop_the_final_stage() {
    let dir = fixture_dir();
    let img = product_image(&ProductSpec {
        width: 200,
        height: 200,
        background: [200, 198, 196],
        product: [168, 166, 164],
        noise: true,
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
            "--optimize",
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
    let candidates = v["optimize"]["candidates"].as_array().unwrap();
    let finals: Vec<&Value> = candidates
        .iter()
        .filter(|c| c["stage"] == "final")
        .collect();
    assert_eq!(
        finals.len(),
        2,
        "綺麗でない 1 位で打ち切っている: {}",
        v["optimize"]
    );
    // 上限は `FINALISTS`。ここが増えると 24.5MP で所要時間が目標を超える
    assert_eq!(finals.len(), kiri::cutout::optimize::FINALISTS);
    assert!(
        candidates[0]["stage"] == "final" && candidates[1]["stage"] == "final",
        "原寸で回すのは探索段の上位から順のはず: {}",
        v["optimize"]
    );
}

/// `--trimap` と `--optimize` を同時に渡せること。
///
/// **探索段は縮小版で回るので、画素ごとの指示も縮めて渡さなければならない**
/// （`Constraints::resampled`）。そこが切れていれば、指示を渡した実行で
/// 探索だけが指示の無い世界を見ることになる。寸法の食い違いは
/// `foreground_mask` の規約で黙って無視されるので、**落ちずに静かに間違う。**
#[test]
fn a_trimap_survives_the_optimize_search() {
    let dir = fixture_dir();
    let input = constraint_fixture(dir.path());
    let output = dir.path().join("cut.png");

    let mut trimap = image::RgbaImage::from_pixel(200, 200, image::Rgba([128, 128, 128, 255]));
    paint(&mut trimap, (0, 0, 199, 19), 0);
    paint(&mut trimap, (0, 180, 199, 199), 0);
    paint(&mut trimap, (70, 70, 129, 129), 255);
    let path = write_png(dir.path(), "trimap.png", &trimap);

    let out = kiri()
        .args([
            "cutout",
            input.to_str().unwrap(),
            "-o",
            output.to_str().unwrap(),
            "--trimap",
            path.to_str().unwrap(),
            "--optimize",
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
    // 指示は原寸のまま報告される（最終段は原寸の指示そのままで回る）
    assert_eq!(v["constraints"]["sources"][0], "trimap");
    assert!(v["constraints"]["fg_ratio"].as_f64().unwrap() > 0.0);
    assert!(v["constraints"]["bg_ratio"].as_f64().unwrap() > 0.0);
    // 探索も走っている。指示と探索は排他ではない
    assert!(v["optimize"]["candidates"].as_array().unwrap().len() > 1);
    assert_eq!(v["settings"]["optimize"], true);
    // 確定前景は不透明のまま、確定背景は透明
    let cut = image::open(&output).unwrap().to_rgba8();
    assert_eq!(cut.get_pixel(100, 100)[3], 255, "確定前景が削られている");
    assert_eq!(cut.get_pixel(5, 5)[3], 0, "確定背景が残っている");
}

// --- cutout --rotate ---

/// 切り抜きと回転を 1 本の実行に畳む。
///
/// **順序は 切り抜き → 回転 → キャンバス → 影 で固定である。** `kiri rotate` で
/// 先に回してから `cutout` へ流すと、回転が四隅に作った透過の余白が外周に乗り、
/// 背景推定がそれを背景色の標本として数える。
#[test]
fn cutout_rotates_after_it_cuts_and_reports_the_angle() {
    let dir = fixture_dir();
    let img = product_image(&ProductSpec {
        width: 240,
        height: 160,
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
            "--rotate",
            "90",
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
    assert_eq!(v["rotate"]["angle"], 90.0);
    assert_eq!(v["rotate"]["resampled"], false, "90 度単位は補間し直さない");
    // 出力の縦横が入れ替わっている
    assert_eq!(v["outputs"][0]["width"], 160);
    assert_eq!(v["outputs"][0]["height"], 240);
    // 入力の寸法は回す前のまま
    assert_eq!(v["source"]["width"], 240);
    assert_eq!(v["source"]["height"], 160);
}

/// 負値は反時計回り。`kiri rotate` と同じく `[0, 360)` へ正規化して返す。
#[test]
fn cutout_normalizes_a_negative_rotation() {
    let dir = fixture_dir();
    let img = product_image(&ProductSpec {
        width: 160,
        height: 120,
        ..Default::default()
    });
    let input = write_png(dir.path(), "in.png", &img);

    let out = kiri()
        .args([
            "cutout",
            input.to_str().unwrap(),
            "-o",
            dir.path().join("cut.png").to_str().unwrap(),
            "--rotate",
            "-90",
            "--json",
        ])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(json_stdout(&out)["rotate"]["angle"], 270.0);
}

/// 回さなかった実行は `rotate` ブロックを持たない。
///
/// `canvas` / `shadow` と同じ規約で、「回さなかった」と「回せない（古い版）」を
/// `null` で混ぜない。`--rotate 360` は恒等変換なので回した側に数えない。
#[test]
fn cutout_without_a_rotation_has_no_rotate_block() {
    let dir = fixture_dir();
    let img = product_image(&ProductSpec {
        width: 120,
        height: 120,
        ..Default::default()
    });
    let input = write_png(dir.path(), "in.png", &img);

    for extra in [vec![], vec!["--rotate", "360"]] {
        let output = dir.path().join(format!("cut{}.png", extra.len()));
        let mut args = vec![
            "cutout",
            input.to_str().unwrap(),
            "-o",
            output.to_str().unwrap(),
            "--json",
        ];
        args.extend_from_slice(&extra);
        let out = kiri().args(&args).output().unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(
            json_stdout(&out).get("rotate").is_none(),
            "回していない実行に rotate ブロックが出ている: {extra:?}"
        );
    }
}

/// 回してからキャンバスへ載せる。**キャンバスの寸法は指定どおりに収まる。**
///
/// 逆順（載せてから回す）だと外接矩形が広がって指定の寸法を割る。
#[test]
fn cutout_places_the_rotated_subject_on_the_canvas() {
    let dir = fixture_dir();
    let img = product_image(&ProductSpec {
        width: 200,
        height: 140,
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
            "--rotate",
            "30",
            "--canvas",
            "400x400",
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
    assert_eq!(v["rotate"]["angle"], 30.0);
    assert_eq!(
        v["rotate"]["resampled"], true,
        "90 度単位でなければ補間する"
    );
    assert_eq!(v["outputs"][0]["width"], 400);
    assert_eq!(v["outputs"][0]["height"], 400);
    assert_eq!(v["canvas"]["width"], 400);

    // 四隅は余白のまま。回した外接矩形が中央へ載っている
    let placed = image::open(&output).unwrap().to_rgba8();
    assert_eq!(placed.get_pixel(0, 0)[3], 0);
    assert_eq!(placed.get_pixel(399, 399)[3], 0);
}

// --- cutout --rotate auto（Phase 23）---

/// `deg` だけ時計回りに倒した濃色の矩形を、白に近い背景へ 1 枚だけ置く。
///
/// **矩形にするのは `level_rotation` が測れる形だからである。** 丸いものでは
/// どの角度でも外接矩形の面積が変わらず、測れないほうの枝へ落ちる
/// （そちらは `disc_scene` が用意する）。
/// `the_reported_level_rotation_actually_levels_the_subject` と同じ作りで、
/// あちらは `info` → `rotate` の 2 本で閉じた往復を、ここでは `cutout` 1 本で閉じる
fn tilted_scene(deg: f64) -> image::RgbaImage {
    let (w, h) = (400u32, 400u32);
    let mut scene = image::RgbaImage::from_pixel(w, h, image::Rgba([250, 250, 249, 255]));
    let (sin, cos) = deg.to_radians().sin_cos();
    for y in 0..h {
        for x in 0..w {
            let (dx, dy) = (f64::from(x) - 200.0, f64::from(y) - 200.0);
            let (u, v) = (dx * cos + dy * sin, -dx * sin + dy * cos);
            if u.abs() <= 130.0 && v.abs() <= 55.0 {
                scene.put_pixel(x, y, image::Rgba([40, 40, 45, 255]));
            }
        }
    }
    scene
}

/// 円盤 1 枚。**`confidence` は high だが `level_rotation` は測れない。**
///
/// `subject.rs` の `a_disc_has_no_level_rotation` と同じ形を画像にしたもので、
/// 「信用はできるが角度が無い」という 3 つ目の枝をここだけが踏む
fn disc_scene() -> image::RgbaImage {
    let (w, h) = (400u32, 400u32);
    image::RgbaImage::from_fn(w, h, |x, y| {
        let d = (f64::from(x) - 200.0).hypot(f64::from(y) - 200.0);
        if d <= 130.0 {
            image::Rgba([40, 40, 45, 255])
        } else {
            image::Rgba([250, 250, 249, 255])
        }
    })
}

/// 半径 70px の円盤。**`level_rotation` は `45.0` 度を `high` で返す。**
///
/// `disc_scene`（半径 130px）と同じただの円なのに、こちらは `None` へ落ちない。
/// ラスタライズした輪郭には量子化の凹凸が残り、その凹凸が作る外接矩形面積の
/// 振れ幅が `TILT_AMBIGUOUS`（2%）を抜けてしまう。半径を 60 から 170 まで 5px
/// 刻みで振ると、`Some` を返したのは 60 / 65 / 70（いずれも **45.0 度**）と
/// 85（0.0 度）の 4 つだった。
///
/// **「丸いものは必ず `None` になる」は成り立たない。** 円を止めているのは
/// 面積の曖昧さではなく、量子化の運である。形の門がここを引き受ける
/// （充填率は 0.798 で、円の上限 π/4≈0.785 のすぐ上）
fn measurable_disc_scene() -> image::RgbaImage {
    let (w, h) = (400u32, 400u32);
    image::RgbaImage::from_fn(w, h, |x, y| {
        let d = (f64::from(x) - 200.0).hypot(f64::from(y) - 200.0);
        if d <= 70.0 {
            image::Rgba([40, 40, 45, 255])
        } else {
            image::Rgba([250, 250, 249, 255])
        }
    })
}

/// `deg` だけ倒した「取っ手つきの円」（フライパン）。円 1 枚＋右へ伸びる細い帯。
///
/// **`level_rotation` が測れてしまうのに、その値が物の向きを語らない形である。**
/// 円だけなら `TILT_AMBIGUOUS` が止めるが、取っ手が 1 本生えるだけで面積に
/// 振れ幅が生まれて門を抜ける。それでいて最小外接矩形の位置は取っ手と円の
/// 接し方——つまり輪郭の量子化——で決まるので、答えは形の向きと無関係に跳ぶ。
/// 実測では真値 0 度に対して **-21.8 度**、10 度に対して **-31.8 度**を
/// どちらも `confidence: high` で返す。
///
/// 帯の長さを円の半径より大きく取ってあるのは、取っ手を短くすると凸包が円に
/// 呑まれて `TILT_AMBIGUOUS` 側へ落ち、**この欠陥を踏まなくなる**ためである
fn handled_disc_scene(deg: f64) -> image::RgbaImage {
    let (w, h) = (400u32, 400u32);
    let (sin, cos) = deg.to_radians().sin_cos();
    image::RgbaImage::from_fn(w, h, |x, y| {
        let (dx, dy) = (f64::from(x) - 200.0, f64::from(y) - 200.0);
        // 形の重心ではなく外接矩形の中心を画面中央へ置く（+64 がそのずらし）
        let (u, v) = (dx * cos + dy * sin + 64.0, -dx * sin + dy * cos);
        let disc = u.hypot(v) <= 90.0;
        let handle = (86.0..=218.0).contains(&u) && v.abs() <= 11.0;
        if disc || handle {
            image::Rgba([40, 40, 45, 255])
        } else {
            image::Rgba([250, 250, 249, 255])
        }
    })
}

/// `deg` だけ倒した角丸長方形。`tilted_scene` と同じ寸法で四隅だけを落とす。
///
/// **角を丸めても矩形は矩形である**ことを言うためにある。形の門は「凸包が
/// 最小外接矩形をどれだけ埋めるか」で測るので、四隅を半径 24px で落とすと
/// 充填率はその面積ぶんだけ下がる（1.000 → 0.98 付近）。門がそこで止まると、
/// 携帯・本・箱という EC でいちばんありふれた主体が回らなくなる
fn rounded_tilted_scene(deg: f64) -> image::RgbaImage {
    let (w, h) = (400u32, 400u32);
    let (sin, cos) = deg.to_radians().sin_cos();
    let (half_w, half_h, radius) = (130.0f64, 55.0f64, 24.0f64);
    image::RgbaImage::from_fn(w, h, |x, y| {
        let (dx, dy) = (f64::from(x) - 200.0, f64::from(y) - 200.0);
        let (u, v) = (dx * cos + dy * sin, -dx * sin + dy * cos);
        let qx = (u.abs() - (half_w - radius)).max(0.0);
        let qy = (v.abs() - (half_h - radius)).max(0.0);
        if u.abs() <= half_w && v.abs() <= half_h && qx.hypot(qy) <= radius {
            image::Rgba([40, 40, 45, 255])
        } else {
            image::Rgba([250, 250, 249, 255])
        }
    })
}

/// まとまった塊 1 つと、その周りに散った雑音。**`confidence` が low になる。**
///
/// `subject.rs` の `scattered_noise_is_reported_with_low_confidence` と同じく
/// **捕捉率で落とす**（`not_one_blob`）。点だけを撒くと最大の塊が 1 点まで
/// 痩せて「塊がそもそも小さい」が先に立ち、切り抜きの結果も全面透過という
/// 縮退した絵になる——それでは「回さなかったこと」をバイト列で確かめる意味が薄い。
/// 塊を 1 つ置いてあるのは、**切り抜きに中身がある状態**で門を通すためである
fn scattered_noise_scene() -> image::RgbaImage {
    let mut img = image::RgbaImage::from_pixel(200, 200, image::Rgba([250, 250, 248, 255]));
    for y in 20..84u32 {
        for x in 20..84u32 {
            img.put_pixel(x, y, image::Rgba([30, 30, 30, 255]));
        }
    }
    // 塊から離して 8px 角を撒く。間隔 12px なので互いに繋がらない
    for gy in 0..9u32 {
        for gx in 0..5u32 {
            let (bx, by) = (96 + gx * 20, 16 + gy * 20);
            for y in by..by + 8 {
                for x in bx..bx + 8 {
                    img.put_pixel(x, y, image::Rgba([30, 30, 30, 255]));
                }
            }
        }
    }
    img
}

/// `cutout` を 1 本走らせて結果 JSON を返す。失敗したら stderr ごと落とす。
fn cutout_json(input: &Path, output: &Path, extra: &[&str]) -> Value {
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
        "{extra:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    json_stdout(&out)
}

/// 結果 JSON から `ROTATE_AUTO_SKIPPED` だけを抜く。
fn rotate_auto_skips(v: &Value) -> Vec<Value> {
    v["warnings"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .filter(|w| w["code"] == "ROTATE_AUTO_SKIPPED")
        .collect()
}

/// 受け入れ基準 (a)。`--rotate auto` が実際に水平を出す。
///
/// **書いた画像へもう一度 `info` を掛けて測り直す。** 角度を返しただけでは
/// 「符号が逆でも値はある」を捕まえられない——`--rotate` にそのまま渡せる値を
/// そのまま渡す実装なので、符号を取り違えれば傾きは倍になって現れる。
/// 許容は 0.5 度で、これは `level_rotation` の分解能そのものである
/// （§7.5 の Phase 23 が定める基準）。
///
/// `--flatten` を付けるのは、測り直す側が**背景を持った画像**を要るためである。
/// 透過のまま書くと外周は RGB が 0 の黒になり、濃色の主体との色差が消える
#[test]
fn cutout_rotate_auto_levels_the_subject() {
    let dir = fixture_dir();
    let input = write_png(dir.path(), "tilted.png", &tilted_scene(5.0));
    let output = dir.path().join("level.png");

    let v = cutout_json(&input, &output, &["--rotate", "auto", "--flatten"]);
    assert!(
        rotate_auto_skips(&v).is_empty(),
        "測れているのに飛ばしている: {v}"
    );
    assert!(
        v.get("rotate").is_some(),
        "回した実行なのに rotate ブロックが無い: {v}"
    );
    // 受け入れ基準 (f)。指定値は `"auto"` を名乗る。効いた角度は rotate ブロック
    assert_eq!(v["settings"]["rotate"], "auto", "{v}");

    let after = json_stdout(
        &kiri()
            .args(["info", output.to_str().unwrap(), "--json"])
            .output()
            .unwrap(),
    );
    let left = after["subject"]["level_rotation"]
        .as_f64()
        .unwrap_or_else(|| panic!("水平出しした画像の傾きが測れない: {after}"));
    assert!(
        left.abs() <= 0.5,
        "--rotate auto の後もまだ {left} 度傾いていると言う（許容 0.5）"
    );
}

/// 受け入れ基準 (b)。**信用できない見立てでは 1 画素も動かさない。**
///
/// `--rotate 0` の出力とバイト列が一致することまで見る。「角度 0 を適用した」と
/// 「回していない」が同じ結果になるのは当然だが、auto の門が黙って別の角度を
/// 選んでいれば 1 バイトも一致しない。`reason` を機械可読にしてあるのは、
/// **どの条件で落ちたかで次の一手が変わる**ためである（low なら `--bbox`、
/// 測れないなら角度を自分で決めるしかない）
#[test]
fn cutout_rotate_auto_does_not_turn_what_it_cannot_trust() {
    let dir = fixture_dir();
    for (name, scene, reason) in [
        ("noise", scattered_noise_scene(), "low_confidence"),
        ("disc", disc_scene(), "not_measurable"),
    ] {
        let input = write_png(dir.path(), &format!("{name}.png"), &scene);
        let auto = dir.path().join(format!("{name}_auto.png"));
        let zero = dir.path().join(format!("{name}_zero.png"));

        let a = cutout_json(&input, &auto, &["--rotate", "auto"]);
        let z = cutout_json(&input, &zero, &["--rotate", "0"]);
        assert_eq!(
            std::fs::read(&auto).unwrap(),
            std::fs::read(&zero).unwrap(),
            "{name}: 適用しないと言いながら画素が動いている"
        );
        assert!(
            a.get("rotate").is_none(),
            "{name}: 回していないのに rotate ブロックが出ている: {a}"
        );

        let skipped = rotate_auto_skips(&a);
        assert_eq!(skipped.len(), 1, "{name}: 警告がちょうど 1 回でない: {a}");
        assert_eq!(skipped[0]["data"]["reason"], reason, "{name}: {a}");
        if reason == "low_confidence" {
            // 低い理由も一緒に渡す。`info` の助言と同じ分岐を書けるようにする
            assert_eq!(skipped[0]["data"]["low_reason"], "not_one_blob", "{a}");
        }
        assert!(
            rotate_auto_skips(&z).is_empty(),
            "{name}: auto を頼んでいない実行で門の警告が出ている: {z}"
        );
    }
}

/// 受け入れ基準 (b) の拡張。**取っ手が 1 本生えた円は回さない。**
///
/// **これが Phase 23 前半で見つかった欠陥の回帰テストである。** 水平に置いた
/// フライパン（真値 0 度）に対して `level_rotation` は `-21.8` 度を
/// `confidence: high` で返す。最小面積外接矩形が「物の向き」を意味するのは、
/// **その物が実際に矩形に近いときだけ**だからで、円に取っ手が付いた形では
/// 最小の位置が取っ手と円の接し方——輪郭の量子化——で決まる。形の門が無ければ
/// auto はこれを適用し、正しく置かれた商品を 21.8 度回したうえで画素を
/// 補間し直していた。
///
/// `--rotate 0` とのバイト一致まで見るのは `cutout_rotate_auto_does_not_turn_
/// what_it_cannot_trust` と同じ理由で、**門が黙って別の角度を選んでいれば
/// 1 バイトも一致しない**ためである
#[test]
fn cutout_rotate_auto_leaves_a_handled_shape_alone() {
    let dir = fixture_dir();
    let input = write_png(dir.path(), "pan.png", &handled_disc_scene(0.0));
    let auto = dir.path().join("pan_auto.png");
    let zero = dir.path().join("pan_zero.png");

    let a = cutout_json(&input, &auto, &["--rotate", "auto"]);
    let z = cutout_json(&input, &zero, &["--rotate", "0"]);
    assert_eq!(
        std::fs::read(&auto).unwrap(),
        std::fs::read(&zero).unwrap(),
        "取っ手つきの円を回している（真値は 0 度）"
    );
    assert!(
        a.get("rotate").is_none(),
        "回していないのに rotate ブロックが出ている: {a}"
    );
    assert!(
        rotate_auto_skips(&z).is_empty(),
        "auto を頼んでいない実行で門の警告が出ている: {z}"
    );

    let skipped = rotate_auto_skips(&a);
    assert_eq!(skipped.len(), 1, "警告がちょうど 1 回でない: {a}");
    let data = &skipped[0]["data"];
    assert_eq!(data["reason"], "not_rectangular", "{a}");

    // **測った値としきい値の両方を渡す。** 「あと少しだった」と「全く違う」を
    // 受け手が分けられなければ、次の一手（--bbox で主体を教え直すのか、
    // 角度を自分で決めるのか）を選べない
    let fill = data["level_fill_ratio"]
        .as_f64()
        .unwrap_or_else(|| panic!("充填率を渡していない: {a}"));
    let min = data["min_fill_ratio"]
        .as_f64()
        .unwrap_or_else(|| panic!("しきい値を渡していない: {a}"));
    assert!(
        fill < min,
        "充填率 {fill} がしきい値 {min} を下回っていない"
    );
    assert!(
        fill < 0.85,
        "取っ手つきの円の充填率が {fill}（0.85 未満のはず）"
    );
}

/// 受け入れ基準 (b) の拡張。**形の門が効きすぎていないこと。**
///
/// 長方形と角丸長方形を 7.5 度倒す。どちらも今までどおり門を通り、水平が出る
/// ——`cutout_rotate_auto_levels_the_subject` と同じ往復（書いた画像へもう一度
/// `info` を掛けて測り直す）で見る。**角丸を並べてあるのが要点で**、四隅を
/// 落とすと充填率はその面積ぶん下がる（1.000 → 0.98 付近）。門をそこまで
/// 上げると携帯・本・箱が回らなくなるので、0.85 がその上側にどれだけ
/// 余裕を持っているかをここで固定する
#[test]
fn cutout_rotate_auto_still_levels_a_boxy_product() {
    let dir = fixture_dir();
    for (name, scene) in [
        ("sharp", tilted_scene(7.5)),
        ("rounded", rounded_tilted_scene(7.5)),
    ] {
        let input = write_png(dir.path(), &format!("{name}.png"), &scene);
        let output = dir.path().join(format!("{name}_level.png"));

        let v = cutout_json(&input, &output, &["--rotate", "auto", "--flatten"]);
        assert!(
            rotate_auto_skips(&v).is_empty(),
            "{name}: 矩形に近い形なのに形の門で止めている: {v}"
        );
        assert!(
            v.get("rotate").is_some(),
            "{name}: 回した実行なのに rotate ブロックが無い: {v}"
        );

        let after = json_stdout(
            &kiri()
                .args(["info", output.to_str().unwrap(), "--json"])
                .output()
                .unwrap(),
        );
        let left = after["subject"]["level_rotation"]
            .as_f64()
            .unwrap_or_else(|| panic!("{name}: 水平出しした画像の傾きが測れない: {after}"));
        assert!(
            left.abs() <= 0.5,
            "{name}: --rotate auto の後もまだ {left} 度傾いていると言う（許容 0.5）"
        );
    }
}

/// 受け入れ基準 (b) の拡張。**門の値そのものを `kiri info` で見る。**
///
/// 警告の `data` 越しではなく `subject` の項目として読めることを固定する。
/// `level_rotation` を採るかどうかの判断は `cutout` の外でも要る——`info` を
/// 見て `kiri rotate --angle` を叩く経路には auto が無いので、**そちらの
/// 受け手は自分でこの値を見るしかない。**
///
/// 並べる 3 枚は較正の谷の両側である。矩形は 0.9 以上、円と取っ手つきは
/// 0.85 未満。円に `measurable_disc_scene`（半径 70px）を使うのは、
/// `disc_scene`（半径 130px）だと `level_rotation` が `None` になって
/// **充填率も `null` になる**ためで、それでは「丸いものを止めた」ことの証拠に
/// ならない。ここで要るのは**角度を答えてしまう円**——半径 70px の円は
/// `45.0` 度を `high` で名乗る——である
#[test]
fn the_shape_gate_separates_what_the_rectangle_describes() {
    let dir = fixture_dir();
    let fill_of = |name: &str, scene: &image::RgbaImage| -> f64 {
        let input = write_png(dir.path(), &format!("{name}.png"), scene);
        let v = json_stdout(
            &kiri()
                .args(["info", input.to_str().unwrap(), "--json"])
                .output()
                .unwrap(),
        );
        assert_eq!(v["subject"]["confidence"], "high", "{name}: {v}");
        assert!(
            !v["subject"]["level_rotation"].is_null(),
            "{name}: 角度が測れていない標本では形の門を語れない: {v}"
        );
        v["subject"]["level_fill_ratio"]
            .as_f64()
            .unwrap_or_else(|| panic!("{name}: level_fill_ratio が無い: {v}"))
    };

    let boxy = fill_of("gate_rect", &tilted_scene(7.5));
    let round = fill_of("gate_disc", &measurable_disc_scene());
    let handled = fill_of("gate_pan", &handled_disc_scene(0.0));

    assert!(boxy >= 0.9, "矩形の充填率が {boxy}（0.9 以上のはず）");
    // 円の上限は π/4≈0.785 である。量子化の凹凸で数 % 上下しうるので 0.85 と
    // 比べる——**谷の中央であって、円の実測値への当てはめではない**
    assert!(round < 0.85, "円の充填率が {round}（0.85 未満のはず）");
    assert!(
        handled < 0.85,
        "取っ手つきの充填率が {handled}（0.85 未満のはず）"
    );
    assert!(
        handled < round,
        "取っ手つき {handled} は円 {round} より低いはず（四隅が空く）"
    );
}

/// 受け入れ基準 (c)。**`--rotate` を渡さない実行は auto の門を 1 度も通らない。**
///
/// バイト列も結果 JSON も `--rotate 0` と同じところへ着く。既存の決定性 /
/// md5 のテストを 1 本も書き換えずに通すことが第一の証拠で、この 1 本は
/// 「`settings.rotate` が数値のままであること」——union にしたせいで
/// 指定しなかった実行の JSON が動いていないこと——まで固定する
#[test]
fn a_run_without_a_rotation_never_mentions_the_auto_gate() {
    let dir = fixture_dir();
    let input = write_png(dir.path(), "tilted.png", &tilted_scene(5.0));
    let silent = dir.path().join("silent.png");
    let zero = dir.path().join("zero.png");

    let a = cutout_json(&input, &silent, &[]);
    let z = cutout_json(&input, &zero, &["--rotate", "0"]);

    assert_eq!(
        std::fs::read(&silent).unwrap(),
        std::fs::read(&zero).unwrap(),
        "--rotate を渡さない実行のバイト列が動いた"
    );
    assert_eq!(a["settings"]["rotate"], 0.0, "{a}");
    assert_eq!(z["settings"]["rotate"], 0.0, "{z}");
    assert!(a.get("rotate").is_none(), "{a}");
    assert!(rotate_auto_skips(&a).is_empty(), "{a}");
}

/// 受け入れ基準 (d)。同じ入力からは毎回同じ角度が出る。
///
/// 測る側（`level_rotation`）は凸包の辺を舐めて最小を採る——**ほぼ同じ面積の
/// 最小が 2 つある形では入力の微差で答えが跳ぶ**ことを `subject.rs` が明記して
/// いるので、同じ入力に対して跳ばないことはここで固定しておく
#[test]
fn cutout_rotate_auto_is_deterministic() {
    let dir = fixture_dir();
    let input = write_png(dir.path(), "tilted.png", &tilted_scene(12.0));

    let angles: Vec<Value> = (0..3)
        .map(|i| {
            let output = dir.path().join(format!("out{i}.png"));
            cutout_json(&input, &output, &["--rotate", "auto"])["rotate"]["angle"].clone()
        })
        .collect();
    assert_eq!(angles[0], angles[1], "{angles:?}");
    assert_eq!(angles[1], angles[2], "{angles:?}");
    assert!(!angles[0].is_null(), "12 度傾けた主体が回っていない");
}

/// 受け入れ基準 (f)。**`auto` の綴りは契約として配られる。**
///
/// `--rotate` は自由な数値も取るので `PossibleValuesParser` では作れないが、
/// `accepts` が空のままだと**綴りを外したときに code 無しの exit 2 で落ちる
/// 項目の候補を、呼ぶ前に知る手段が無くなる**（`profile_parser` のコメントが
/// 明記している事情）。ヘルプと `kiri schema` の両方に載ることをここで固定する
#[test]
fn the_rotate_option_publishes_auto_as_a_choice() {
    let v = schema_json();
    let rotate = v["commands"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["name"] == "cutout")
        .expect("cutout が schema に無い")["options"]
        .as_array()
        .unwrap()
        .iter()
        .find(|o| o["name"] == "--rotate")
        .expect("--rotate が schema に無い")
        .clone();

    let accepts: Vec<&str> = rotate["accepts"]
        .as_array()
        .unwrap_or_else(|| panic!("--rotate が accepts を持たない: {rotate}"))
        .iter()
        .map(|a| a.as_str().unwrap())
        .collect();
    assert!(accepts.contains(&"auto"), "{rotate}");

    let detail = rotate["detail"].as_str().unwrap_or_default();
    assert!(
        detail.contains("auto"),
        "長いヘルプが auto を語らない: {detail}"
    );
    // 分解能と適用条件は**指定の前に知っていないと選びようがない**
    assert!(detail.contains("0.5"), "分解能の注意が無い: {detail}");
    assert!(detail.contains("high"), "適用条件が無い: {detail}");

    let help =
        String::from_utf8(kiri().args(["cutout", "--help"]).output().unwrap().stdout).unwrap();
    assert!(help.contains("auto"), "--help が auto を配っていない");
}

/// 読めない `--rotate` は clap が断る。**code は伴わない**（`--max-bytes` と同じ）。
#[test]
fn a_malformed_rotation_on_the_command_line_is_refused_by_the_parser() {
    let dir = fixture_dir();
    let input = write_png(dir.path(), "a.png", &disc_scene());
    for written in ["ななめ", "AUTO", "nan", "inf"] {
        let out = kiri()
            .args([
                "cutout",
                input.to_str().unwrap(),
                "-o",
                dir.path().join("x.png").to_str().unwrap(),
                "--rotate",
                written,
                "--json",
            ])
            .output()
            .unwrap();
        assert_eq!(out.status.code(), Some(2), "--rotate {written}");
        assert!(out.stdout.is_empty(), "--rotate {written} で stdout が出た");
    }
}

/// spec からも同じ角度を書ける。**CLI と同じ 1 本の経路を通る。**
#[test]
fn the_batch_spec_accepts_a_rotation() {
    let dir = fixture_dir();
    let img = product_image(&ProductSpec {
        width: 200,
        height: 140,
        ..Default::default()
    });
    write_png(dir.path(), "a.png", &img);
    let spec = write_spec(
        dir.path(),
        r#"{"defaults":{"format":"png"},
            "items":[{"input":"a.png","output":"out/a.png","rotate":90},
                     {"input":"a.png","output":"out/b.png","rotate":-90}]}"#,
    );

    let out = run_batch(&spec, &[]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let v = json_stdout(&out);
    assert_eq!(v["succeeded"], 2);
    assert_eq!(v["results"][0]["result"]["rotate"]["angle"], 90.0);
    // 負値は反時計回り。CLI と同じ正規化を通る
    assert_eq!(v["results"][1]["result"]["rotate"]["angle"], 270.0);
    assert_eq!(v["results"][0]["result"]["outputs"][0]["width"], 140);
}

/// 有限でない角度は spec へ書けない。**CLI の `finite` と同じ関門である。**
///
/// JSON に nan は書けず、桁が溢れた指数は JSON の段で断られる。`angle` の
/// `is_finite` は（`offset` と同じく）そこを抜けてきた場合の受け皿として
/// 残してある——spec を組み立てる経路が増えたときに黙って通らないように。
#[test]
fn the_batch_spec_rejects_an_angle_that_is_not_finite() {
    let dir = fixture_dir();
    let img = product_image(&ProductSpec {
        width: 120,
        height: 120,
        ..Default::default()
    });
    write_png(dir.path(), "a.png", &img);
    let spec = write_spec(
        dir.path(),
        r#"{"items":[{"input":"a.png","output":"out.png","rotate":1e999}]}"#,
    );

    let out = run_batch(&spec, &[]);
    assert!(!out.status.success());
    assert_eq!(json_stdout(&out)["error"]["code"], "SPEC_INVALID_JSON");
}

/// 文字列で書いた角度も CLI と同じに読む。
///
/// **`max_bytes` とまったく同じ事情である。** エージェントが書く JSON には
/// `90` と `"90"` の両方が現れ、片方を断ると CLI では通る書き方が spec でだけ
/// 通らない道具になる。`auto` を受けるために型を `Value` へ広げた以上、
/// 数値の綴りも CLI と同じ `finite` が読む（Phase 23 で挙動を変えた点）
#[test]
fn the_batch_spec_reads_an_angle_written_as_a_string() {
    let dir = fixture_dir();
    let img = product_image(&ProductSpec {
        width: 200,
        height: 140,
        ..Default::default()
    });
    write_png(dir.path(), "a.png", &img);
    let spec = write_spec(
        dir.path(),
        r#"{"items":[{"input":"a.png","output":"out.png","rotate":"90"}]}"#,
    );

    let out = run_batch(&spec, &[]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        json_stdout(&out)["results"][0]["result"]["rotate"]["angle"],
        90.0
    );
}

/// 受け入れ基準 (e)。spec でも `"auto"` と数値の両方が通る。
///
/// **CLI と同じパーサ（`cli::parse_rotate`）を通す。** 別に書くと、`auto` の
/// 綴りが片方でだけ通る／通らないが起きる
#[test]
fn the_batch_spec_accepts_auto_and_a_number_for_the_rotation() {
    let dir = fixture_dir();
    write_png(dir.path(), "tilted.png", &tilted_scene(5.0));
    let spec = write_spec(
        dir.path(),
        r#"{"defaults":{"format":"png"},
            "items":[{"input":"tilted.png","output":"out/a.png","rotate":"auto"},
                     {"input":"tilted.png","output":"out/b.png","rotate":12.5}]}"#,
    );

    let out = run_batch(&spec, &[]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let v = json_stdout(&out);
    assert_eq!(v["succeeded"], 2, "{v}");
    assert_eq!(
        v["results"][0]["result"]["settings"]["rotate"], "auto",
        "{v}"
    );
    // 5 度傾けた主体なので、auto は 355 付近（時計回りへ -5）を選ぶ
    let auto = v["results"][0]["result"]["rotate"]["angle"]
        .as_f64()
        .unwrap_or_else(|| panic!("auto が何も回していない: {v}"));
    assert!((auto - 355.0).abs() < 1.5, "auto が {auto} 度を選んだ: {v}");
    assert_eq!(v["results"][1]["result"]["settings"]["rotate"], 12.5, "{v}");
    assert_eq!(v["results"][1]["result"]["rotate"]["angle"], 12.5, "{v}");
}

/// 受け入れ基準 (e)。読めない角度は `INVALID_ROTATE` でその項目だけを落とす。
///
/// **黙って 0 度へ落とさない。** 水平出しを頼んだつもりの項目が回らないまま
/// 数百点に混ざると、気づけるのは目で見たときになる。`INVALID_MAX_BYTES` /
/// `INVALID_FAIL_ON` と同じ扱いで、batch は 1 件の失敗で全体を止めないので
/// 終了コードは処理失敗の 4 になる
#[test]
fn a_malformed_rotation_in_a_spec_is_refused_with_a_code() {
    let dir = fixture_dir();
    let img = product_image(&ProductSpec {
        width: 200,
        height: 140,
        ..Default::default()
    });
    write_png(dir.path(), "a.png", &img);
    let spec = write_spec(
        dir.path(),
        r#"{"defaults":{"format":"png"},
            "items":[{"input":"a.png","output":"out/a.png","rotate":90},
                     {"input":"a.png","output":"out/b.png","rotate":"ななめ"},
                     {"input":"a.png","output":"out/c.png","rotate":"auto"}]}"#,
    );

    let out = run_batch(&spec, &[]);
    assert_eq!(out.status.code(), Some(4));
    let v = json_stdout(&out);
    assert_eq!(v["failed"], 1, "{v}");
    assert_eq!(v["succeeded"], 2, "落ちた項目が伝播している: {v}");
    assert_eq!(v["results"][1]["error"]["code"], "INVALID_ROTATE", "{v}");
    assert_eq!(v["results"][0]["result"]["rotate"]["angle"], 90.0, "{v}");
}

/// 読めない角度は他の綴りでも同じ code で落ちる。
#[test]
fn every_unreadable_rotation_in_a_spec_lands_on_the_same_code() {
    let dir = fixture_dir();
    let img = product_image(&ProductSpec {
        width: 60,
        height: 60,
        ..Default::default()
    });
    write_jpeg(dir.path(), "p.jpg", &img);

    for written in [
        "\"\"",
        "\"AUTO\"",
        "\"auto \"",
        "\"nan\"",
        "\"inf\"",
        "true",
        "[90]",
    ] {
        let spec = write_spec(
            dir.path(),
            &format!(
                r#"{{"items":[{{"input":"p.jpg","output":"out/x.png","rotate":{written}}}]}}"#
            ),
        );
        let out = run_batch(&spec, &[]);
        assert_eq!(out.status.code(), Some(4), "rotate:{written}");
        let v = json_stdout(&out);
        assert_eq!(
            v["results"][0]["error"]["code"], "INVALID_ROTATE",
            "rotate:{written}"
        );
    }
}

/// 影の換算基準は**回した後の長辺**である。
///
/// 任意角で回すと外接矩形は必ず元より大きくなる（45 度なら約 1.41 倍）。
/// そこを元画像の長辺で換算すると、回した実行でだけ影が小さく出る。
#[test]
fn the_shadow_scales_against_the_rotated_long_side() {
    let dir = fixture_dir();
    let img = product_image(&ProductSpec {
        width: 240,
        height: 160,
        ..Default::default()
    });
    let input = write_png(dir.path(), "in.png", &img);

    let dy = |extra: &[&str], name: &str| -> f64 {
        let output = dir.path().join(name);
        let mut args = vec![
            "cutout",
            input.to_str().unwrap(),
            "-o",
            output.to_str().unwrap(),
            "--shadow",
            "synth",
            "--shadow-offset",
            "0,400",
            "--json",
        ];
        args.extend_from_slice(extra);
        let out = kiri().args(&args).output().unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        json_stdout(&out)["shadow"]["offset"][1].as_f64().unwrap()
    };

    // 回さなければ元画像の長辺 240 が基準（400 * 0.240 = 96）
    let flat = dy(&[], "flat.png");
    assert_eq!(flat, 96.0);

    // 45 度回すと外接矩形は 283x283 になり、基準もそちらへ移る
    let turned = dy(&["--rotate", "45"], "turned.png");
    assert!(
        turned > flat,
        "回した後の長辺で換算していない: {turned} vs {flat}"
    );
}

/// キャンバスへ載せる範囲の定義は**回転の有無で変わらない**。
///
/// 透過つき PNG を入力にすると、マスクは立っているのに画素は透明という
/// 画素が出る（`apply_alpha` は元画像の透過と小さいほうを採る）。マスクと
/// アルファで定義を分けていると、同じ画像が `--rotate 0` と `--rotate 90` で
/// 違う切り詰め方をされる。
#[test]
fn the_canvas_trims_by_the_same_rule_with_and_without_a_rotation() {
    let dir = fixture_dir();
    // 外周を透明にした入力。マスクは立ちうるが、見えない余白である
    let mut img = product_image(&ProductSpec {
        width: 200,
        height: 200,
        ..Default::default()
    });
    for (x, y, pixel) in img.enumerate_pixels_mut() {
        if x < 20 || y < 20 || x >= 180 || y >= 180 {
            pixel[3] = 0;
        }
    }
    let input = write_png(dir.path(), "in.png", &img);

    let content = |angle: &str, name: &str| -> (u64, u64) {
        let output = dir.path().join(name);
        let out = kiri()
            .args([
                "cutout",
                input.to_str().unwrap(),
                "-o",
                output.to_str().unwrap(),
                "--canvas",
                "400x400",
                "--rotate",
                angle,
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
        (
            v["canvas"]["content"][0].as_u64().unwrap(),
            v["canvas"]["content"][1].as_u64().unwrap(),
        )
    };

    let (w, h) = content("0", "flat.png");
    let (rw, rh) = content("90", "turned.png");
    assert_eq!(
        (rw, rh),
        (h, w),
        "90 度回しただけで、載せた中身の縦横が入れ替わる以上の差が出ている"
    );
}

/// **主体の判定は `--border` に依らない。**
///
/// `--border` は背景色の推定範囲を決める値であって、主体の較正のための値では
/// ない。しかし帯が変われば外周 ΔE の分布——`far` のしきい値そのもの——も
/// 変わるので、放っておくと較正が `--border` にぶら下がる。実際、上限を置く
/// 前は合成の「画面外へ抜ける大きな物体」と実写 IMG_0238 が `--border 110`
/// で low → high（どちらも誤り）へ裏返っていた。
///
/// 較正の 11 点すべてで、帯を 2 から 400 まで振っても判定が動かないことを
/// 固定する。**数値ではなく判定を見る**——`area_ratio` などは帯とともに
/// 多少動いてよく、動いてはいけないのは「この矩形に従ってよいか」の答えである。
#[test]
fn the_subject_verdict_does_not_follow_the_border() {
    for (name, image, want_high) in common::subject_scenes() {
        let dir = fixture_dir();
        let input = write_png(dir.path(), "s.png", &image);
        for border in ["2", "8", "32", "110", "400"] {
            let v = json_stdout(
                &kiri()
                    .args([
                        "info",
                        input.to_str().unwrap(),
                        "--border",
                        border,
                        "--json",
                    ])
                    .output()
                    .unwrap(),
            );
            let got = v["subject"]["confidence"].as_str().unwrap_or("null");
            assert_eq!(
                got,
                if want_high { "high" } else { "low" },
                "{name}: --border {border} で判定が裏返った（{v}）"
            );
        }
    }
}

/// 上限が効いたときは、測った帯を `subject.border` が名乗ること。
///
/// **黙って別の帯で測らない。** `settings.border` と食い違う理由が JSON から
/// 読めなければ、エージェントには「指定が効いていない」としか見えない。
#[test]
fn the_subject_reports_the_band_it_measured() {
    let dir = fixture_dir();
    let (_, image, _) = common::subject_scenes()
        .into_iter()
        .next()
        .expect("較正シーンがある");
    let (w, h) = (image.width(), image.height());
    let input = write_png(dir.path(), "s.png", &image);
    let cap = ((f64::from(w.min(h)) * 0.03).round() as u32).max(2);

    for (given, want) in [(2u32, 2u32), (cap, cap), (cap * 4, cap)] {
        let v = json_stdout(
            &kiri()
                .args([
                    "info",
                    input.to_str().unwrap(),
                    "--border",
                    &given.to_string(),
                    "--json",
                ])
                .output()
                .unwrap(),
        );
        assert_eq!(
            v["subject"]["border"].as_u64().unwrap(),
            u64::from(want),
            "--border {given} で subject.border が {want} でない: {v}"
        );
        // `info` の JSON には `settings` が無い（切り抜きの設定を持たない）ので、
        // 食い違いは `subject.border` と渡した値を並べて読む
    }
}

// --- --fail-on / exit 5 ---

/// 前景の無い素材。**測れない指標を作るためだけに要る。**
///
/// `separability` / `halo_ratio` / `edge_width` / `contour_roughness` /
/// `rim_contamination` の 5 つは、測る境界が 1 本も無ければ `null` になる。
/// 商品を描いた素材ではその状態を作れない。
fn featureless_scene() -> image::RgbaImage {
    image::RgbaImage::from_pixel(200, 200, image::Rgba([250, 250, 249, 255]))
}

/// `cutout --dry-run --json` を走らせ、終了コードと結果 JSON を返す。
///
/// **`--dry-run` で回す。** 合否は書き出しの前に決まる値（`mask`）だけから
/// 出るので、ファイルを書く必要が無い。
fn cutout_gated(input: &Path, output: &Path, extra: &[&str]) -> (i32, Value) {
    let mut args = vec![
        "cutout",
        input.to_str().unwrap(),
        "-o",
        output.to_str().unwrap(),
        "--dry-run",
        "--json",
    ];
    args.extend_from_slice(extra);
    let out = kiri().args(&args).output().unwrap();
    (out.status.code().unwrap(), json_stdout(&out))
}

/// `compliance.checks[]` から 1 つの指標の判定を引く。
fn check_of<'a>(v: &'a Value, name: &str) -> &'a Value {
    v["compliance"]["checks"]
        .as_array()
        .unwrap_or_else(|| panic!("checks が配列ではない: {v}"))
        .iter()
        .find(|c| c["name"] == name)
        .unwrap_or_else(|| panic!("{name} の判定が無い: {v}"))
}

/// 外周まで商品が伸びた素材。**`touches_edge` を true にするためだけに要る。**
///
/// 全面を商品にすると外周サンプルまで商品色になり、背景推定そのものが成立しない
/// （`cutout_flags_a_product_running_off_the_frame` と同じ作り）。
fn cropped_scene() -> image::RgbaImage {
    let mut img = image::RgbaImage::from_pixel(120, 120, image::Rgba([250, 250, 249, 255]));
    for y in 70..120 {
        for x in 40..80 {
            img.put_pixel(x, y, image::Rgba([40, 40, 40, 255]));
        }
    }
    img
}

/// 受け入れ基準 (a)。**各指標 × しきい値 → exit code の対応表。**
///
/// 7 指標それぞれを「触れる」「触れない」の 2 通りで回す。数値のしきい値は実測値
/// そのものから組む——`halo_ratio>=<実測>` は必ず触れ、`halo_ratio<<実測>` は
/// 必ず触れない。**定数を書き写さない**ので、較正で指標の出方が動いても
/// この表は意味を保つ。
///
/// **真偽の指標だけは素材のほうを取り替える。** 裸のトークンは向きを持たない
/// （「外周に接していたら不合格」の 1 通りしか書けない）ので、接している素材と
/// 接していない素材の 2 枚で 2 通りを作る。向きを書けるようにすると
/// 「接していないことを咎める」指定が書けてしまい、良い画像が落ちる。
#[test]
fn every_metric_maps_a_threshold_to_an_exit_code() {
    let dir = fixture_dir();
    let input = write_jpeg(
        dir.path(),
        "product.jpg",
        &product_image(&ProductSpec::default()),
    );
    let cropped = write_png(dir.path(), "cropped.png", &cropped_scene());
    let output = dir.path().join("out.png");

    let (code, base) = cutout_gated(&input, &output, &[]);
    assert_eq!(code, 0, "素材そのものが通らない: {base}");

    for metric in kiri::compliance::FAIL_ON_METRICS {
        let measured = &base["mask"][metric];
        let (touching, clear) = match measured {
            Value::Bool(false) => {
                let (code, v) = cutout_gated(&input, &output, &["--fail-on", metric]);
                assert_eq!(code, 0, "接していない素材が {metric} で落ちた: {v}");
                assert_eq!(check_of(&v, metric)["status"], "pass");

                let (code, v) = cutout_gated(&cropped, &output, &["--fail-on", metric]);
                assert_eq!(code, 5, "接している素材が {metric} で落ちない: {v}");
                assert_eq!(check_of(&v, metric)["status"], "fail");
                // **明示でも code を名乗る。** 同じ失敗が書き方によって
                // `checks[].code` で拾えたり拾えなかったりしない
                assert_eq!(check_of(&v, metric)["code"], "SUBJECT_TOUCHES_EDGE");
                continue;
            }
            Value::Bool(true) => panic!("{metric} の前提が崩れている: {}", base["mask"]),
            // 実測が 0 なら「0 未満」は発火しえない門として断られるので、
            // 触れない側は「0 を超えたら」で書く（どちらも落ちない条件である）
            Value::Number(n) => {
                let x = n.as_f64().unwrap();
                let clear = if x > 0.0 {
                    format!("{metric}<{x}")
                } else {
                    format!("{metric}>0")
                };
                (format!("{metric}>={x}"), clear)
            }
            other => panic!("{metric} が測れていない: {other}"),
        };

        let (code, v) = cutout_gated(&input, &output, &["--fail-on", &touching]);
        assert_eq!(code, 5, "{touching} が落ちない: {v}");
        assert_eq!(check_of(&v, metric)["status"], "fail", "{touching}");
        assert_eq!(v["compliance"]["passed"], Value::Bool(false), "{touching}");
        assert_eq!(
            v["compliance"]["code"], "QUALITY_GATE_FAILED",
            "不合格は code を名乗る: {touching}"
        );

        let (code, v) = cutout_gated(&input, &output, &["--fail-on", &clear]);
        assert_eq!(code, 0, "{clear} が落ちた: {v}");
        assert_eq!(check_of(&v, metric)["status"], "pass", "{clear}");
        assert_eq!(v["compliance"]["passed"], Value::Bool(true), "{clear}");
        assert!(
            v["compliance"]["code"].is_null(),
            "合格で code を名乗っている: {v}"
        );
    }
}

/// 受け入れ基準 (a) の 3 通り目。**測れなかったものは合格にしない。**
///
/// `separability` の null は「前景が無い」、`halo_ratio` の null は「測る境界が
/// 無い」である。0 と報告するのと同じで、黙って通すと最も知りたい失敗が
/// 合格として返る。**`fail` とは別の名前で名乗る**——次の一手が違う
/// （測れなかったのは素材か指示の問題で、しきい値をいじっても動かない）。
#[test]
fn an_unmeasurable_metric_is_rejected_under_its_own_name() {
    let dir = fixture_dir();
    let input = write_png(dir.path(), "flat.png", &featureless_scene());
    let output = dir.path().join("out.png");

    let (_, base) = cutout_gated(&input, &output, &[]);
    let nullable: Vec<&str> = kiri::compliance::FAIL_ON_METRICS
        .into_iter()
        .filter(|m| base["mask"][m].is_null())
        .collect();
    assert_eq!(
        nullable.len(),
        5,
        "測れない指標が 5 つ揃っていない: {}",
        base["mask"]
    );

    for metric in nullable {
        let spec = format!("{metric}>0.5");
        let (code, v) = cutout_gated(&input, &output, &["--fail-on", &spec]);
        assert_eq!(code, 5, "{spec} が通ってしまった: {v}");
        let check = check_of(&v, metric);
        assert_eq!(check["status"], "unmeasurable", "{spec}");
        assert!(check["actual"].is_null(), "{spec}");
        // 頼んだ条件は残す。測れたかどうかに関わらず事実である
        assert_eq!(check["operator"], "gt", "{spec}");
    }
}

/// 受け入れ基準 (b)。**`--fail-on` を指定しない実行は 1 バイトも変わらない。**
///
/// 既存のテストを 1 本も書き換えずに通すことが第一の証拠だが、それは
/// 「変わっていない」を直接は言わない。ここでは同じ実行の結果 JSON を
/// `compliance` ごと突き合わせる——所要時間だけが実行ごとに動くので落とす。
#[test]
fn no_fail_on_means_no_compliance_block_and_no_new_exit_code() {
    let dir = fixture_dir();
    let input = write_jpeg(
        dir.path(),
        "product.jpg",
        &product_image(&ProductSpec::default()),
    );
    let output = dir.path().join("out.png");

    let (code, plain) = cutout_gated(&input, &output, &[]);
    assert_eq!(code, 0);
    assert!(
        plain.get("compliance").is_none(),
        "--fail-on 無しで compliance が現れた: {plain}"
    );

    let (code, gated) = cutout_gated(&input, &output, &["--fail-on", "foreground_ratio>0.99"]);
    assert_eq!(code, 0);

    let strip = |v: &Value| {
        let mut o = v.as_object().unwrap().clone();
        o.remove("compliance");
        o.remove("elapsed_ms");
        Value::Object(o)
    };
    assert_eq!(
        strip(&plain),
        strip(&gated),
        "--fail-on が結果の他の部分を動かしている"
    );
}

/// 受け入れ基準 (b) の裏。**exit 5 でも結果 JSON は通常どおり全部返る。**
///
/// 処理は成功していて成果物も存在する。`ErrorReport` へ差し替えると
/// 「何が不合格だったか」も「何が書かれたか」も追えなくなる——batch が
/// 数百枚書いた後に `BatchReport` を捨てていた Phase 20 の失敗と同じ形である。
#[test]
fn a_rejected_cutout_still_returns_the_whole_result_json() {
    let dir = fixture_dir();
    let input = write_jpeg(
        dir.path(),
        "product.jpg",
        &product_image(&ProductSpec::default()),
    );
    let written = dir.path().join("out.png");

    // **本当に書かせる。** dry-run では「成果物がある」ことを確かめられない
    let out = kiri()
        .args([
            "cutout",
            input.to_str().unwrap(),
            "-o",
            written.to_str().unwrap(),
            "--json",
            "--fail-on",
            "foreground_ratio>=0.0",
        ])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(5));
    let v = json_stdout(&out);

    assert!(
        v.get("error").is_none(),
        "エラー本体に差し替わっている: {v}"
    );
    assert_eq!(v["schema_version"], 2);
    assert!(!v["outputs"].as_array().unwrap().is_empty(), "{v}");
    assert!(!v["mask"]["foreground_ratio"].is_null(), "{v}");
    assert!(!v["background"]["uniformity"].is_null(), "{v}");
    assert!(written.is_file(), "成果物が書かれていない");
    assert_eq!(
        v["outputs"][0]["bytes"].as_u64().unwrap(),
        std::fs::metadata(&written).unwrap().len(),
        "報告したバイト数と実ファイルが食い違う"
    );
}

/// `default` と明示が同じ指標に当たったら**明示が勝つ。**
///
/// 利用者が書いた値のほうが強い、という `--optimize` の規約と同じ。同じ指標を
/// 2 通りに測って 2 つの答えを持たせない。
#[test]
fn an_explicit_threshold_beats_the_default_for_the_same_metric() {
    let dir = fixture_dir();
    let input = write_jpeg(
        dir.path(),
        "product.jpg",
        &product_image(&ProductSpec::default()),
    );
    let output = dir.path().join("out.png");

    let (code, v) = cutout_gated(&input, &output, &["--fail-on", "default,halo_ratio>=0.0"]);
    assert_eq!(code, 5, "明示のほうが厳しいのに通った: {v}");

    let halo: Vec<&Value> = v["compliance"]["checks"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|c| c["name"] == "halo_ratio")
        .collect();
    assert_eq!(halo.len(), 1, "同じ指標が 2 度出ている: {v}");
    assert_eq!(halo[0]["threshold"], 0.0);
    assert_eq!(halo[0]["status"], "fail");
    // `default` の他の指標はそのまま残る
    assert!(
        v["compliance"]["checks"]
            .as_array()
            .unwrap()
            .iter()
            .any(|c| c["code"] == "NOT_SEPARABLE"),
        "{v}"
    );
}

/// `checks[]` の並びは決定的である。
///
/// 指定の順にも `HashMap` の順にも依らない。並びが動くと、結果の差分で
/// 品質を見張れなくなる。
#[test]
fn the_compliance_checks_are_in_a_deterministic_order() {
    let dir = fixture_dir();
    let input = write_jpeg(
        dir.path(),
        "product.jpg",
        &product_image(&ProductSpec::default()),
    );
    let output = dir.path().join("out.png");

    let names = |v: &Value| -> Vec<String> {
        v["compliance"]["checks"]
            .as_array()
            .unwrap()
            .iter()
            .map(|c| c["name"].as_str().unwrap().to_string())
            .collect()
    };

    // 指定の順を逆にしても同じ並びになる
    let (_, a) = cutout_gated(
        &input,
        &output,
        &["--fail-on", "touches_edge,foreground_ratio>0.99"],
    );
    let (_, b) = cutout_gated(
        &input,
        &output,
        &["--fail-on", "foreground_ratio>0.99,touches_edge"],
    );
    assert_eq!(names(&a), names(&b));
    assert_eq!(names(&a), vec!["foreground_ratio", "touches_edge"]);

    // 2 回走らせても同じ
    let (_, first) = cutout_gated(&input, &output, &["--fail-on", "default"]);
    let (_, second) = cutout_gated(&input, &output, &["--fail-on", "default"]);
    assert_eq!(first["compliance"], second["compliance"]);
    // 並びは FAIL_ON_METRICS の順で、指標ごとにまとまっている
    let order: Vec<usize> = names(&first)
        .iter()
        .map(|n| {
            kiri::compliance::FAIL_ON_METRICS
                .iter()
                .position(|m| m == n)
                .unwrap()
        })
        .collect();
    assert!(order.windows(2).all(|w| w[0] <= w[1]), "{order:?}");
}

/// `--fail-on` が受ける指標と、schema が配る `mask.*` は**過不足なく一致する。**
///
/// 片方に足してもう片方を忘れる、を構造的に防ぐ。忘れると「JSON に出ている
/// 値の名前を書いたのに指標ではないと断られる」という、綴りを疑いようのない
/// 失敗になる。
#[test]
fn the_fail_on_metrics_are_exactly_the_published_mask_fields() {
    let mut published: Vec<String> = fields_of(&schema_json())
        .into_iter()
        .filter_map(|f| f["path"].as_str().map(str::to_string))
        .filter_map(|p| p.strip_prefix("mask.").map(str::to_string))
        .collect();
    let mut ours: Vec<String> = kiri::compliance::FAIL_ON_METRICS
        .into_iter()
        .map(str::to_string)
        .collect();
    published.sort();
    ours.sort();
    assert_eq!(ours, published);
}

/// `--fail-on default` の集合は `FATAL_CODES` ∪ `QUALITY_CODES` と一致する。
///
/// どちらかへ code を足したときにここが落ちる。別の集合を新しく定義すると、
/// **`--optimize` が「きれい」と言った結果を `--fail-on default` が落とす**
/// という食い違いが起こりうる。
#[test]
fn the_default_gate_is_exactly_the_fatal_and_quality_codes() {
    let dir = fixture_dir();
    let input = write_jpeg(
        dir.path(),
        "product.jpg",
        &product_image(&ProductSpec::default()),
    );
    let output = dir.path().join("out.png");
    let (_, v) = cutout_gated(&input, &output, &["--fail-on", "default"]);

    let mut seen: Vec<String> = v["compliance"]["checks"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["code"].as_str().unwrap().to_string())
        .collect();
    let mut want: Vec<String> = kiri::cutout::optimize::FATAL_CODES
        .iter()
        .chain(kiri::cutout::optimize::QUALITY_CODES.iter())
        .map(|c| c.as_str().to_string())
        .collect();
    seen.sort();
    want.sort();
    assert_eq!(seen, want);
}

/// CLI 側の書式違いは clap が断る。**code は伴わない**（`INVALID_MAX_BYTES` /
/// `INVALID_DERIVATION` と同じ前例）ので、stdout は空のままになる。
///
/// **入力を読む前に断る。** 切り抜きを回し切ってから綴り違いに気づく形だと、
/// `--optimize` 込みの 1 枚で十数秒を捨てることになる。
#[test]
fn a_malformed_fail_on_on_the_command_line_is_refused_by_the_parser() {
    let dir = fixture_dir();
    let input = dir.path().join("does-not-exist.jpg");
    let output = dir.path().join("out.png");

    for spec in [
        "",
        "halo",
        "halo_ration>0.1",
        "halo_ratio",
        "halo_ratio=0.1",
        "halo_ratio>abc",
        "halo_ratio>2",
        // **値域の端で発火しえない門も断る。** 比率を % と取り違えた
        // `foreground_ratio>1.0` は、断らなければ全件を黙って通し続ける
        "halo_ratio>1.0",
        "foreground_ratio>1.0",
        "halo_ratio<0.0",
        "edge_width<0.0",
        "touches_edge>0.5",
        "touches_edge=yes",
        // 真偽の指標に `=` は付けない（向きを選べる形を廃した）
        "touches_edge=true",
        "touches_edge=false",
        "default,default",
        "halo_ratio>0.1,halo_ratio>0.2",
    ] {
        let out = kiri()
            .args([
                "cutout",
                input.to_str().unwrap(),
                "-o",
                output.to_str().unwrap(),
                "--json",
                "--fail-on",
                spec,
            ])
            .output()
            .unwrap();
        // **入力が存在しないのに exit 3 にならない**ことが、引数の検査が
        // 先に走っている証拠である
        assert_eq!(out.status.code(), Some(2), "--fail-on '{spec}'");
        assert!(out.stdout.is_empty(), "--fail-on '{spec}' で stdout が出た");
        assert!(!out.stderr.is_empty(), "--fail-on '{spec}' で stderr が空");
    }
}

/// spec 経由の書式違いは `INVALID_FAIL_ON` でその項目を落とす。
///
/// clap を通らないので、CLI と同じ関門（`FailOn::parse`）を `commands::batch`
/// が通す。抜けていると、CLI では断る書き方が spec でだけ通る。
#[test]
fn a_malformed_fail_on_in_a_spec_is_refused_with_a_code() {
    let dir = fixture_dir();
    write_jpeg(
        dir.path(),
        "p.jpg",
        &product_image(&ProductSpec {
            width: 60,
            height: 60,
            ..Default::default()
        }),
    );

    for written in [
        "\"halo\"",
        "\"halo_ratio\"",
        "\"touches_edge=yes\"",
        "\"touches_edge=true\"",
        "\"halo_ratio>1.0\"",
        "\"\"",
    ] {
        let spec = write_spec(
            dir.path(),
            &format!(
                r#"{{"items":[{{"input":"p.jpg","output":"out/x.png","fail_on":{written}}}]}}"#
            ),
        );
        let out = run_batch(&spec, &[]);
        assert_eq!(out.status.code(), Some(4), "fail_on:{written}");
        let v = json_stdout(&out);
        assert_eq!(
            v["results"][0]["error"]["code"], "INVALID_FAIL_ON",
            "fail_on:{written}"
        );
    }
}

/// 受け入れ基準 (c)。**1 件でも不合格なら 5。ただし `failed > 0` の 4 が優先。**
///
/// 両方あるときに 5 を返すと、成果物が 1 つも無い項目があることが番号から
/// 消え、「見れば分かる結果」として扱われてしまう。
#[test]
fn a_batch_rejection_exits_five_unless_something_actually_failed() {
    let dir = fixture_dir();
    write_jpeg(
        dir.path(),
        "p.jpg",
        &product_image(&ProductSpec {
            width: 60,
            height: 60,
            ..Default::default()
        }),
    );

    // 1 件不合格 + 0 件失敗 → 5
    let spec = write_spec(
        dir.path(),
        r#"{"items":[
             {"input":"p.jpg","output":"out/a.png"},
             {"input":"p.jpg","output":"out/b.png","fail_on":"foreground_ratio>=0.0"}
           ]}"#,
    );
    let out = run_batch(&spec, &[]);
    assert_eq!(out.status.code(), Some(5));
    let v = json_stdout(&out);
    assert_eq!(v["total"], 2);
    assert_eq!(v["failed"], 0);
    assert_eq!(v["rejected"], 1);
    // **`succeeded` の定義は変えていない。** 不合格でも処理は成功しており、
    // 成果物は書かれている
    assert_eq!(v["succeeded"], 2);
    assert_eq!(v["results"][0]["status"], "ok");
    assert_eq!(v["results"][1]["status"], "rejected");
    // 不合格の項目も `result` を通常どおり持つ
    assert!(
        !v["results"][1]["result"]["outputs"]
            .as_array()
            .unwrap()
            .is_empty(),
        "{v}"
    );
    assert_eq!(
        v["results"][1]["result"]["compliance"]["passed"],
        Value::Bool(false)
    );
    assert!(dir.path().join("out/b.png").is_file(), "成果物が無い");

    // 1 件失敗 + 1 件不合格 → 4
    let spec = write_spec(
        dir.path(),
        r#"{"items":[
             {"input":"missing.jpg","output":"out/c.png"},
             {"input":"p.jpg","output":"out/d.png","fail_on":"foreground_ratio>=0.0"}
           ]}"#,
    );
    let out = run_batch(&spec, &[]);
    assert_eq!(out.status.code(), Some(4), "4 が 5 に優先する");
    let v = json_stdout(&out);
    assert_eq!(v["failed"], 1);
    assert_eq!(v["rejected"], 1);
}

/// spec の `fail_on` は `defaults` から継げる。
///
/// **`batch.rs` の 3 箇所（`ItemSettings` / `pick!` / `SETTING_KEYS`）が
/// 揃っていること**を実行で確かめる。綴りの揃いそのものは
/// `setting_keys_are_exactly_what_serde_reads` が構造で守る。
#[test]
fn a_spec_inherits_fail_on_from_the_defaults() {
    let dir = fixture_dir();
    write_jpeg(
        dir.path(),
        "p.jpg",
        &product_image(&ProductSpec {
            width: 60,
            height: 60,
            ..Default::default()
        }),
    );
    // 項目側は「必ず合格する条件」だが、**発火しうる値で書く**——`>1.0` は
    // 割合が 1 を超えないので「永久に落ちない門」として断られる
    let spec = write_spec(
        dir.path(),
        r#"{"defaults":{"fail_on":"foreground_ratio>=0.0"},
             "items":[
               {"input":"p.jpg","output":"out/a.png"},
               {"input":"p.jpg","output":"out/b.png","fail_on":"foreground_ratio>0.999"}
             ]}"#,
    );
    let out = run_batch(&spec, &[]);
    assert_eq!(out.status.code(), Some(5));
    let v = json_stdout(&out);
    assert_eq!(v["results"][0]["status"], "rejected", "既定が効いていない");
    assert_eq!(v["results"][1]["status"], "ok", "項目の指定が勝つべき");
    assert_eq!(v["rejected"], 1);
}

/// README が並べる `--fail-on default` の code は、実装の集合と一致する。
///
/// `the_readme_warning_table_lists_every_warning_in_the_contract` は
/// **表**しか見ない。`default` の内訳は表とは別の場所にあるので、`FATAL_CODES` /
/// `QUALITY_CODES` へ code を足して README を直し忘れると素通りする——そして
/// その表は「この条件で落ちる」と名乗っているので、外れたまま読まれると
/// **落ちない条件を落ちると信じて運用される。**
#[test]
fn the_readme_spells_the_real_default_gate() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let readme = std::fs::read_to_string(root.join("README.md")).unwrap();
    // `default` の説明の直後にある囲みだけを見る
    let after = readme
        .split_once("いずれかが出たら不合格**とする。")
        .expect("README が default の集合を説明していない")
        .1;
    let block = after
        .split_once("```")
        .expect("囲みが無い")
        .1
        .split_once("```")
        .expect("囲みが閉じていない")
        .0;

    let mut listed: Vec<String> = block
        .split(|c: char| !(c.is_ascii_uppercase() || c == '_'))
        .filter(|w| w.contains('_'))
        .map(str::to_string)
        .collect();
    let mut want: Vec<String> = kiri::cutout::optimize::FATAL_CODES
        .iter()
        .chain(kiri::cutout::optimize::QUALITY_CODES.iter())
        .map(|c| c.as_str().to_string())
        .collect();
    listed.sort();
    want.sort();
    assert_eq!(listed, want);
}

/// 値域の**端**で発火しえない門は、入力を読む前に断る。
///
/// `halo_ratio>2` を断る理由（永久に発火せず、書いた本人は合格が出続けるのを
/// 見て「見ている」と読む）が `>1.0` にそっくり当てはまる。**片方だけ閉じた
/// 関門にしない。** 比率を % と取り違えた `foreground_ratio>1.0` は典型で、
/// 断らなければ kiri は何も言わずに全件を通し続けた。
///
/// 端ちょうどで発火しうる向き（`>=1.0` / `<=0.0`）と、逆向きの「必ず発火する門」
/// （`foreground_ratio>=0.0` は 1 枚目の exit 5 で気づく）は通す。
#[test]
fn a_gate_that_can_never_fire_is_refused_before_the_image_is_read() {
    let dir = fixture_dir();
    let input = write_jpeg(
        dir.path(),
        "product.jpg",
        &product_image(&ProductSpec::default()),
    );
    let output = dir.path().join("out.png");

    for spec in [
        "halo_ratio>1.0",
        "foreground_ratio>1.0",
        "rim_contamination>1",
        "halo_ratio<0.0",
        "edge_width<0.0",
        "separability<0",
    ] {
        let out = kiri()
            .args([
                "cutout",
                input.to_str().unwrap(),
                "-o",
                output.to_str().unwrap(),
                "--dry-run",
                "--json",
                "--fail-on",
                spec,
            ])
            .output()
            .unwrap();
        assert_eq!(out.status.code(), Some(2), "'{spec}' が通ってしまった");
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(
            stderr.contains("発火しません"),
            "'{spec}' が別の理由で断られた: {stderr}"
        );
    }

    // 端ちょうどで落ちうる書き方は受けて、実際に評価する。**合否は素材しだい**
    // （この素材の halo_ratio は 0 なので `<=0.0` は落ちる）で、ここで見るのは
    // 「書式として断られない」ことと「見た結果が返る」ことである
    for spec in ["halo_ratio>=1.0", "halo_ratio<=0.0", "edge_width<=0"] {
        let (code, v) = cutout_gated(&input, &output, &["--fail-on", spec]);
        assert!(code == 0 || code == 5, "'{spec}' が断られた: exit {code}");
        assert_eq!(
            v["compliance"]["checks"].as_array().unwrap().len(),
            1,
            "'{spec}' が評価されていない: {v}"
        );
    }
}

/// 明示した外周接触は、`default` とまったく同じ事実を同じ code で名乗る。
///
/// 旧 `touches_edge=true` は `code` が常に `null` で、`checks[].code` で分岐する
/// エージェントは同じ失敗を書き方によって拾えたり拾えなかったりした。
/// 旧 `=false` はさらに悪く、**外周に接していない良い画像を落としながら、
/// 既定の外周接触の検査を消していた**（8 本が 7 本に減っていた）。
#[test]
fn an_explicit_edge_token_keeps_the_default_verdict_for_a_clean_image() {
    let dir = fixture_dir();
    let input = write_jpeg(
        dir.path(),
        "product.jpg",
        &product_image(&ProductSpec::default()),
    );
    let output = dir.path().join("out.png");

    let (code, plain) = cutout_gated(&input, &output, &["--fail-on", "default"]);
    assert_eq!(code, 0, "素材そのものが通らない: {plain}");

    // **良い画像は落ちない。** 明示を足しても答えが変わらないことが要点である
    let (code, both) = cutout_gated(&input, &output, &["--fail-on", "default,touches_edge"]);
    assert_eq!(code, 0, "外周に接していない画像が落ちた: {both}");
    assert_eq!(both["compliance"]["passed"], Value::Bool(true));

    // 明示は `default` のその指標の検査（2 つの code）を 1 本に置き換える
    let edges: Vec<&Value> = both["compliance"]["checks"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|c| c["name"] == "touches_edge")
        .collect();
    assert_eq!(edges.len(), 1, "明示が勝っていない: {both}");
    assert_eq!(edges[0]["code"], "SUBJECT_TOUCHES_EDGE", "code が消えた");
    assert!(edges[0]["operator"].is_null(), "比べる相手は無い");
    assert!(edges[0]["threshold"].is_null());
    assert_eq!(
        both["compliance"]["checks"].as_array().unwrap().len(),
        plain["compliance"]["checks"].as_array().unwrap().len() - 1,
        "置き換えたのは外周接触の 2 本だけのはず"
    );

    // 接している画像では、明示も `default` も同じ code で落ちる
    let cropped = write_png(dir.path(), "cropped.png", &cropped_scene());
    let (code, v) = cutout_gated(&cropped, &output, &["--fail-on", "touches_edge"]);
    assert_eq!(code, 5, "接している画像が落ちない: {v}");
    assert_eq!(check_of(&v, "touches_edge")["code"], "SUBJECT_TOUCHES_EDGE");
}

/// 人間向けの行は**見た件数と落ちた件数**を言い、markdown を漏らさない。
///
/// 「不合格です」とだけ言うと「見た上で通った」が伝わらず、かといって pass を
/// 全部並べると 20 行が埋まる。`**` は端末に出しても強調にならない——
/// `src/main.rs` の他の `println!` に `**` を含むものは 1 つも無い。
#[test]
fn the_human_readable_compliance_line_counts_what_it_looked_at() {
    let dir = fixture_dir();
    let input = write_jpeg(
        dir.path(),
        "product.jpg",
        &product_image(&ProductSpec::default()),
    );
    let output = dir.path().join("out.png");

    let run = |spec: &str| -> String {
        let out = kiri()
            .args([
                "cutout",
                input.to_str().unwrap(),
                "-o",
                output.to_str().unwrap(),
                "--dry-run",
                "--fail-on",
                spec,
            ])
            .output()
            .unwrap();
        String::from_utf8_lossy(&out.stdout).to_string()
    };

    let passed = run("default");
    let line = passed
        .lines()
        .find(|l| l.contains("規格"))
        .unwrap_or_else(|| panic!("合否の行が無い:\n{passed}"));
    assert!(line.contains("合格"), "{line}");
    assert!(
        line.contains("8 件中 0 件不合格"),
        "見た件数が出ていない: {line}"
    );

    let rejected = run("foreground_ratio>=0.0");
    let line = rejected
        .lines()
        .find(|l| l.contains("規格"))
        .unwrap_or_else(|| panic!("合否の行が無い:\n{rejected}"));
    assert!(line.contains("1 件中 1 件不合格"), "{line}");
    assert!(
        !rejected.contains("**"),
        "markdown が漏れている:\n{rejected}"
    );
}

// --- --profile / kiri lint（Phase 22）---

/// `kiri schema` が配る `profiles[]` から 1 つの規格の `rules` を引く。
///
/// **Rust 側の定数を 1 つも書き写さないための入口である。** しきい値を
/// テストへ写すと、較正や規格の改訂で表が動いたときに**テストだけが古い世界を
/// 語る**——しかも緑のまま語るので、誰も気づかない。ここを通しておけば、
/// 表が動いた瞬間に素材の寸法も期待値も一緒に動く。
fn profile_rules(name: &str) -> Value {
    schema_json()["profiles"]
        .as_array()
        .unwrap_or_else(|| panic!("profiles が配列ではない"))
        .iter()
        .find(|p| p["name"] == name)
        .unwrap_or_else(|| panic!("{name} が profiles[] に無い"))
        .clone()["rules"]
        .clone()
}

/// 単色の枠に矩形を 1 つ置いただけの素材。**規格の項目を 1 つずつ動かすために要る。**
///
/// `tests/common` の `product_image` は角丸・陰影・センサーノイズを持ち、外接矩形は
/// 常に幅の 0.56 / 高さの 0.68 になる。背景も 248,248,247 で純白ではない——
/// つまり `fill_ratio_min` 0.85 と「純白背景」を同時に満たせない。
///
/// **「1 項目だけ外した画像」は、外した項目以外が確実に合格していなければ
/// 意味を持たない。** ここでは矩形を 1 つ置くだけにして、`kiri lint` が測る
/// 外接矩形が置いた矩形そのものになるようにする。角も陰影もノイズも無いので、
/// 実測とのずれは主体検出のにじみ（数 px）だけに収まる。
fn flat_product(
    width: u32,
    height: u32,
    fill: f64,
    background: [u8; 3],
    background_alpha: u8,
) -> image::RgbaImage {
    let pw = (f64::from(width) * fill).round().max(1.0) as u32;
    let ph = (f64::from(height) * fill).round().max(1.0) as u32;
    let (x0, y0) = ((width - pw) / 2, (height - ph) / 2);
    image::RgbaImage::from_fn(width, height, |x, y| {
        if (x0..x0 + pw).contains(&x) && (y0..y0 + ph).contains(&y) {
            image::Rgba([190, 70, 55, 255])
        } else {
            image::Rgba([
                background[0],
                background[1],
                background[2],
                background_alpha,
            ])
        }
    })
}

/// その規格を素直に満たす占有率。**公開された下限から組む。**
///
/// 下限ちょうどに置かないのは、`kiri lint` が測るのが**色から見立てた主体の
/// 外接矩形**で、置いた矩形より数 px 広く出るためである（にじみは外側へ転ぶ）。
/// 余裕は合格の側へ寄せる——外したい項目以外で落ちると、テストが何を固定して
/// いるのか読めなくなる。規定の無い規格では、どの規格の下限も超える値を採る。
fn conforming_fill(rules: &Value) -> f64 {
    rules["fill_ratio_min"].as_f64().unwrap_or(0.85) + 0.03
}

/// その規格を素直に満たす 1 辺(px)。**上下限の間から採る。**
///
/// 下限ちょうどに置くと、「下限を 1px 下回る」という外し方との差が 1px しか
/// 無くなり、どちらの画像も境目の上に乗る。規定が無い側は、他の規格の下限も
/// 上限も気にしなくてよい大きさで埋める。
fn conforming_side(rules: &Value) -> u32 {
    let min = rules["longest_side_min"].as_u64().unwrap_or(600) as u32;
    let side = min + 200;
    match rules["longest_side_max"].as_u64() {
        Some(max) => side.min(max as u32),
        None => side,
    }
}

/// その規格を素直に満たす画像を書く。
///
/// **sRGB の ICC を埋める。** `srgb_required` を持つ規格（amazon /
/// square-white）では「色空間を 1 つも名乗っていない」は `pass` ではなく
/// `unmeasurable` なので、名乗りの無いファイルでは「素直に満たす」画像に
/// ならない（`write_srgb_png` の doc を参照）。
fn write_conforming(dir: &Path, name: &str, rules: &Value) -> PathBuf {
    let side = conforming_side(rules);
    write_srgb_png(
        dir,
        name,
        &flat_product(side, side, conforming_fill(rules), [255, 255, 255], 255),
    )
}

/// 商品だけが半透明で、外周は不透明な白のままの素材。
///
/// **`alpha` の 1 項目だけを外すために要る。** `flat_product` の
/// `background_alpha` を 0 にすると外周まで透明になり、背景色を測れる画素が
/// 1 つも無くなる（`background` が `unmeasurable` に落ちる）。
fn semi_transparent_product(side: u32, fill: f64) -> image::RgbaImage {
    let mut img = flat_product(side, side, fill, [255, 255, 255], 255);
    for pixel in img.pixels_mut() {
        if pixel.0 != [255, 255, 255, 255] {
            pixel.0[3] = 200;
        }
    }
    img
}

/// `kiri lint` を走らせ、終了コードと結果 JSON を返す。
fn lint(input: &Path, profile: &str) -> (i32, Value) {
    let out = kiri()
        .args([
            "lint",
            input.to_str().unwrap(),
            "--profile",
            profile,
            "--json",
        ])
        .output()
        .unwrap();
    (out.status.code().unwrap(), json_stdout(&out))
}

/// `checks[]` の名前を並びのまま返す。
fn lint_names(v: &Value) -> Vec<String> {
    v["checks"]
        .as_array()
        .unwrap_or_else(|| panic!("checks が配列ではない: {v}"))
        .iter()
        .map(|c| c["name"].as_str().unwrap().to_string())
        .collect()
}

/// `pass` ではなかった項目の名前。**`fail` だけを拾わない**——`skipped` も
/// `unmeasurable` も合格ではないので、「ちょうどその 1 項目だけが落ちた」を
/// 言うにはここで一緒に数えるしかない。
fn not_passing(v: &Value) -> Vec<String> {
    v["checks"]
        .as_array()
        .unwrap_or_else(|| panic!("checks が配列ではない: {v}"))
        .iter()
        .filter(|c| c["status"] != "pass")
        .map(|c| format!("{}={}", c["name"].as_str().unwrap(), c["status"]))
        .collect()
}

/// 1 つの項目の判定。
fn lint_check<'a>(v: &'a Value, name: &str) -> &'a Value {
    v["checks"]
        .as_array()
        .unwrap_or_else(|| panic!("checks が配列ではない: {v}"))
        .iter()
        .find(|c| c["name"] == name)
        .unwrap_or_else(|| panic!("{name} の判定が無い: {v}"))
}

/// 受け入れ基準 (a)。**配る表と、解釈器が実際に受ける名前が一致する。**
///
/// `profiles[]` を `profile::ALL` と直に突き合わせると、表を 2 度書いた検査に
/// なる（同じ定数を左右に置いても、食い違いようがないので何も守らない）。
/// ここが見るのは**表の形**である——配っている名前で本当に呼べるか、名前が
/// 一意か、どの行も `revision` と `source` を名乗るか、`rules` の各項目が
/// 約束した型で出ているか。`accepts` は clap の候補（`--help` が
/// `[possible values: ...]` として印字するもの）なので、これが一致していれば
/// 「候補として案内した名前が引けない」は起こりえない。
#[test]
fn the_published_profiles_are_the_names_the_parser_accepts() {
    let schema = schema_json();
    let profiles = schema["profiles"].as_array().expect("profiles が無い");
    assert!(!profiles.is_empty(), "profiles[] が空");

    let names: Vec<&str> = profiles
        .iter()
        .map(|p| p["name"].as_str().expect("name が文字列でない"))
        .collect();
    let mut sorted = names.clone();
    sorted.sort_unstable();
    sorted.dedup();
    assert_eq!(sorted.len(), names.len(), "name が重複している: {names:?}");

    // `--profile` を持つコマンドはどれも同じ表から候補を組む。片方だけが
    // 古い一覧を語ると、「書く側では通る名前が lint では通らない」になる
    for command in ["cutout", "lint"] {
        let accepts: Vec<String> = schema["commands"]
            .as_array()
            .unwrap()
            .iter()
            .find(|c| c["name"] == command)
            .unwrap_or_else(|| panic!("{command} が commands[] に無い"))["options"]
            .as_array()
            .unwrap()
            .iter()
            .find(|o| o["name"] == "--profile")
            .unwrap_or_else(|| panic!("{command} に --profile が無い"))["accepts"]
            .as_array()
            .unwrap_or_else(|| panic!("{command}/--profile が候補を配っていない"))
            .iter()
            .map(|v| v.as_str().unwrap().to_string())
            .collect();
        assert_eq!(accepts, names, "{command} の候補が表と違う");
    }

    for p in profiles {
        let name = p["name"].as_str().unwrap();
        assert!(
            p["revision"].as_str().is_some_and(|s| !s.is_empty()),
            "{name}: revision が無い"
        );
        assert!(
            p["source"].as_str().is_some_and(|s| !s.is_empty()),
            "{name}: source が無い"
        );
        assert!(
            p["summary"].as_str().is_some_and(|s| !s.is_empty()),
            "{name}: summary が無い"
        );

        // **`null` は「規定なし」である。** 0 や巨大な数で代用されていたら、
        // 「検査しない」と「必ず落ちる」が同じ形になっている
        let rules = &p["rules"];
        for key in ["longest_side_min", "longest_side_max", "max_pixels"] {
            assert!(
                rules[key].is_null() || rules[key].as_u64().is_some_and(|n| n > 0),
                "{name}/{key}: 数としても規定なしとしても読めない: {}",
                rules[key]
            );
        }
        let ratio = &rules["fill_ratio_min"];
        assert!(
            ratio.is_null() || ratio.as_f64().is_some_and(|r| r > 0.0 && r <= 1.0),
            "{name}/fill_ratio_min: 値域の外: {ratio}"
        );
        for key in ["square", "alpha_allowed", "srgb_required"] {
            assert!(rules[key].is_boolean(), "{name}/{key} が真偽でない");
        }
        assert!(
            rules["background"].is_null()
                || rules["background"]
                    .as_array()
                    .is_some_and(|rgb| rgb.len() == 3 && rgb.iter().all(|c| c.as_u64().is_some())),
            "{name}/background が RGB 3 つでない"
        );
        let formats = rules["formats"].as_array().expect("formats が配列でない");
        assert!(
            formats.iter().all(|f| f.as_str().is_some()),
            "{name}/formats に形式名でないものがある"
        );

        // **配った名前で本当に呼べる。** 合否は素材しだい（ここでは通らない）
        // なので、見るのは「書式として断られない」ことだけである
        let dir = fixture_dir();
        let input = write_png(
            dir.path(),
            "p.png",
            &flat_product(64, 64, 0.5, [9, 9, 9], 255),
        );
        let (code, _) = lint(&input, name);
        assert_ne!(code, 2, "{name}: 配った名前が引数として断られた");
    }
}

/// 受け入れ基準 (b) の前半。**規格を満たす画像は全項目 `pass` で exit 0。**
///
/// プリセットを全部回すのは、1 つ足したときに**その 1 つだけが検査されない**
/// 状態を構造的に作らないためである（`profile.rs` の単体テストと同じ作法）。
/// 素材の寸法も占有率も `kiri schema` が配った下限から組むので、規格が改訂
/// されれば素材のほうが追随する。
#[test]
fn a_conforming_image_passes_every_check_of_its_profile() {
    let dir = fixture_dir();
    for name in ["amazon", "shopify", "square-white"] {
        let rules = profile_rules(name);
        let input = write_conforming(dir.path(), &format!("{name}.png"), &rules);
        let (code, v) = lint(&input, name);
        assert_eq!(
            code,
            0,
            "{name}: 規格を満たす画像が落ちた: {:?}",
            not_passing(&v)
        );
        assert_eq!(v["passed"], Value::Bool(true), "{name}: {v}");
        assert!(v["code"].is_null(), "{name}: 合格で code を名乗った: {v}");
        assert!(!lint_names(&v).is_empty(), "{name}: 1 項目も検査していない");
        assert_eq!(v["profile"]["name"], name);
        assert_eq!(
            v["profile"]["revision"],
            schema_json()["profiles"]
                .as_array()
                .unwrap()
                .iter()
                .find(|p| p["name"] == name)
                .unwrap()["revision"]
        );
    }
}

/// 受け入れ基準 (b) の後半。**1 項目だけ外した画像は、ちょうどその 1 項目で落ちる。**
///
/// 「不合格になった」だけでは何も固定できない——外した項目と違うところが
/// 落ちていても赤にならないし、全項目が落ちていても赤にならない。
/// `checks[].name` を**ちょうど 1 つ**に固定して初めて、「この規格のこの条件を
/// kiri が見ている」と言える。
///
/// 外す値はすべて `kiri schema` が配った数から組む（下限の 1px 下、下限の半分、
/// 上限の 200px 上）。**灰色だけは実測で確かめる**——ΔE の許容は schema では
/// なく結果の `expected.delta_e_max` が配るので、その値を実測の `delta_e` と
/// 並べて「許容を超えている」ことまで assert する。
#[test]
fn breaking_one_rule_fails_exactly_that_check() {
    let dir = fixture_dir();

    let amazon = profile_rules("amazon");
    let a_side = conforming_side(&amazon);
    let a_fill = conforming_fill(&amazon);
    let a_min = amazon["longest_side_min"].as_u64().unwrap() as u32;
    let square = profile_rules("square-white");
    let w_side = conforming_side(&square);
    let w_fill = conforming_fill(&square);
    let shopify = profile_rules("shopify");
    let s_max = shopify["longest_side_max"].as_u64().unwrap() as u32;

    let cases: Vec<(&str, &str, image::RgbaImage, &str)> = vec![
        (
            "amazon",
            "short.png",
            // 下限を 1px 下回る。他は素直に合格する寸法のまま
            flat_product(a_min - 1, a_min - 1, a_fill, [255, 255, 255], 255),
            "longest_side",
        ),
        (
            "amazon",
            "gray.png",
            // **目で見ても灰色と分かる値を採る。** 250 は JPEG の白背景として
            // 普通にありうる（ΔE 1.7）ので、境目の内側では外したことにならない
            flat_product(a_side, a_side, a_fill, [200, 200, 200], 255),
            "background",
        ),
        (
            "amazon",
            "small-subject.png",
            flat_product(a_side, a_side, a_fill / 2.0, [255, 255, 255], 255),
            "fill_ratio",
        ),
        (
            "amazon",
            "alpha.png",
            // **透かすのは商品のほうで、外周は不透明な白のまま置く。**
            // 外周まで透明にすると背景の検査も道連れになり（外周に不透明な
            // 画素が 1 つも無いので `background` は `unmeasurable`）、
            // 「1 項目だけ」でなくなる。外周が透明な画像で「背景は白だった」と
            // 答えるほうが誤りなので、道連れは lint 側の取りこぼしではない
            // ——アルファ 0 の画素が持つ RGB は表示に使われない値である。
            //
            // 占有率はアルファの外接矩形から測るが、外周が不透明なので矩形は
            // 画像全体になり、下限（0.85）を割らない
            semi_transparent_product(a_side, a_fill),
            "alpha",
        ),
        (
            "square-white",
            "oblong.png",
            // 長辺は下限のまま。正方形でないことだけが変わる
            flat_product(w_side, w_side - w_side / 8, w_fill, [255, 255, 255], 255),
            "square",
        ),
        (
            "shopify",
            "too-wide.png",
            // 上限を超えた長辺。**総画素数は上限の内側に留める**——短辺を
            // 小さく採るので、max_pixels は道連れにならない
            flat_product(s_max + 200, 100, 0.8, [255, 255, 255], 255),
            "longest_side",
        ),
    ];

    for (profile, file, image, broken) in cases {
        // **外した 1 項目以外は素直に合格させる。** 名乗りの無い PNG では
        // `color_space` が `unmeasurable` になり、「落ちたのはちょうど 1 項目」
        // が言えなくなる（`write_conforming` と同じ理由）
        let input = write_srgb_png(dir.path(), file, &image);
        let (code, v) = lint(&input, profile);
        assert_eq!(code, 5, "{file}: 外したのに exit 5 でない: {v}");
        assert_eq!(v["passed"], Value::Bool(false), "{file}");
        assert_eq!(v["code"], "PROFILE_VIOLATION", "{file}");
        assert_eq!(
            not_passing(&v),
            vec![format!("{broken}=\"fail\"")],
            "{file}: 落ちたのが {broken} ちょうどではない: {v}"
        );
    }

    // 背景だけは、許容そのものが結果に載っているので実測で確かめられる。
    // **完全一致を求めていないこと**と、それでも 200 は外れていることの両方を
    // 1 つの行から読む
    let input = write_srgb_png(
        dir.path(),
        "gray2.png",
        &flat_product(a_side, a_side, a_fill, [200, 200, 200], 255),
    );
    let (_, v) = lint(&input, "amazon");
    let check = lint_check(&v, "background");
    let tolerance = check["expected"]["delta_e_max"]
        .as_f64()
        .expect("許容が無い");
    assert!(tolerance > 0.0, "完全一致を求めている: {check}");
    assert!(
        check["actual"]["delta_e"].as_f64().unwrap() > tolerance,
        "許容の内側なのに落ちた: {check}"
    );
}

/// **外周 2px の白い縁だけを見て「背景は純白」と答えない。**
///
/// `--border` の既定（2）は切り抜きのための値で、そこから取った色は cutout では
/// 閾値の種にしかならない。lint では外周の中央値が**そのまま合否になる**ので、
/// 1600px の画像の縁 0.125% を見た結果を「この画像の背景」と名乗るわけにいかない
/// ——灰色一面の画像が全項目 pass / exit 0 になっていたのがこれだった。
///
/// **測った帯幅を結果から読めることまで見る。** 同じ delta_e でも、どれだけの
/// 幅を見た数なのかが分からなければ、読み手はそれを検算できない。
#[test]
fn a_thin_white_rim_does_not_make_a_grey_image_white() {
    let dir = fixture_dir();
    let side = 1600u32;
    // 外周 2px だけ白、その内側は一面の灰色、中央に被写体
    let mut img = flat_product(side, side, 0.5, [170, 170, 170], 255);
    for (x, y, p) in img.enumerate_pixels_mut() {
        if x < 2 || y < 2 || x >= side - 2 || y >= side - 2 {
            *p = image::Rgba([255, 255, 255, 255]);
        }
    }
    let input = write_srgb_png(dir.path(), "rim.png", &img);

    let (code, v) = lint(&input, "amazon");
    assert_eq!(code, 5, "灰色一面の画像が amazon を通った: {v}");
    let check = lint_check(&v, "background");
    assert_eq!(check["status"], "fail", "{v}");
    assert_eq!(check["actual"]["rgb"], serde_json::json!([170, 170, 170]));
    assert!(
        check["actual"]["delta_e"].as_f64().unwrap()
            > check["expected"]["delta_e_max"].as_f64().unwrap()
    );

    // **何を測った数なのかを名乗る。** 帯は --border そのものではない
    let band = check["actual"]["border_px"].as_u64().expect("帯幅が無い");
    assert!(band > 2, "外周 2px のままで判定している: {check}");

    // **`kiri info` に同じ帯幅を渡せば同じ色が出る。** 測り方は 1 つのままで、
    // 違うのは帯幅だけである——どちらが本当かを確かめる手段が残っている
    let out = kiri()
        .args([
            "info",
            input.to_str().unwrap(),
            "--border",
            &band.to_string(),
            "--json",
        ])
        .output()
        .unwrap();
    let info = json_stdout(&out);
    assert_eq!(info["background"]["rgb"], check["actual"]["rgb"], "{info}");
}

/// **色空間を 1 つも名乗っていないファイルを `pass` と言わない。**
///
/// ICC も EXIF ColorSpace も無い入力に対して `kiri info` は色空間 "sRGB" を
/// 返す——kiri がその画素を sRGB として扱うのは正しく、その出力は動かさない。
/// だが「sRGB として扱った」と「ファイルが sRGB を名乗っていた」は別の事実で、
/// `srgb_required` が問うているのは後者である。
///
/// **AVIF と同じ扱いにすることまで見る。** AVIF の CICP が unspecified なら
/// `unmeasurable` になるので、同じ「名乗っていない」が PNG でだけ合格すると
/// 形式で合否が変わる枝ができる。
#[test]
fn a_file_that_names_no_colour_space_is_unmeasurable_not_a_pass() {
    let dir = fixture_dir();
    let rules = profile_rules("amazon");
    let side = conforming_side(&rules);
    let img = flat_product(side, side, conforming_fill(&rules), [255, 255, 255], 255);

    // 名乗りが 1 つも無い PNG（image クレートが素で書くもの）
    let bare = write_png(dir.path(), "bare.png", &img);
    let (code, v) = lint(&bare, "amazon");
    assert_eq!(code, 5, "名乗りの無いファイルが合格した: {v}");
    let check = lint_check(&v, "color_space");
    assert_eq!(check["status"], "unmeasurable", "{v}");
    assert_eq!(check["actual"]["color_named"], false);
    assert_eq!(check["actual"]["icc_profile"], false);
    // **落としてはいない。** 「sRGB ではない」と断じる材料も 1 つも無い
    assert_ne!(check["status"], "fail");
    // 名乗り以外は素直に合格している（外したのはちょうど 1 項目）
    assert_eq!(not_passing(&v), vec!["color_space=\"unmeasurable\""], "{v}");

    // **`kiri info` の出力は 1 文字も動かさない。** 画素を sRGB として扱った
    // ことは事実であり、`info` が答えているのはそちらの問いである
    let out = kiri()
        .args(["info", bare.to_str().unwrap(), "--json"])
        .output()
        .unwrap();
    assert_eq!(json_stdout(&out)["color_space"], "sRGB");

    // 同じ画素でも、sRGB の ICC を埋めれば名乗りがあるので pass になる
    let named = write_srgb_png(dir.path(), "named.png", &img);
    let (code, v) = lint(&named, "amazon");
    assert_eq!(code, 0, "{v}");
    let check = lint_check(&v, "color_space");
    assert_eq!(check["status"], "pass");
    assert_eq!(check["actual"]["color_named"], true);
}

/// **`checks[].name` の綴りは機械可読で配られる。**
///
/// `--fail-on` の指標が `accepts` で配られるのと同じ役目である。ここが無いと、
/// エージェントは日本語の散文から名前を抜くことになる——**書き写した表は必ず
/// 実装から離れる**という、`kiri schema` そのものが避けている失敗である。
///
/// `needs_pixels` まで配るのは、AVIF を渡す前にどの項目が `skipped` になるかを
/// 予測できるようにするためで、**実際に AVIF を lint した結果と突き合わせる**。
#[test]
fn the_published_lint_checks_are_the_ones_that_appear() {
    let schema = schema_json();
    let published = schema["lint_checks"]
        .as_array()
        .expect("lint_checks[] が無い");
    assert!(!published.is_empty());

    let names: Vec<&str> = published
        .iter()
        .map(|c| c["name"].as_str().expect("name が文字列でない"))
        .collect();
    let mut sorted = names.clone();
    sorted.sort_unstable();
    sorted.dedup();
    assert_eq!(sorted.len(), names.len(), "name が重複している: {names:?}");

    // **どの規格の checks[] も、配った一覧の内側に収まる。**
    let dir = fixture_dir();
    for profile in ["amazon", "shopify", "square-white"] {
        let rules = profile_rules(profile);
        let input = write_conforming(dir.path(), &format!("{profile}-published.png"), &rules);
        let (_, v) = lint(&input, profile);
        for name in lint_names(&v) {
            assert!(
                names.contains(&name.as_str()),
                "{name} が lint_checks[] に無い"
            );
        }
    }

    // **needs_pixels が真の項目だけが AVIF で skipped になる。**
    let source = write_conforming(dir.path(), "published-source.png", &profile_rules("amazon"));
    let avif = dir.path().join("published.avif");
    convert_json(&source, &avif, &[]);
    let (_, v) = lint(&avif, "amazon");
    let skipped: Vec<String> = v["checks"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|c| c["status"] == "skipped")
        .map(|c| c["name"].as_str().unwrap().to_string())
        .collect();
    assert!(!skipped.is_empty(), "AVIF で 1 つも飛んでいない: {v}");
    for name in &skipped {
        let entry = published
            .iter()
            .find(|c| c["name"] == name.as_str())
            .unwrap();
        assert_eq!(
            entry["needs_pixels"], true,
            "{name} は画素を要さないのに飛ばされた"
        );
    }
}

/// **規定の無い項目は検査もしない。** shopify は構図を 1 つも規定していない。
///
/// 背景を灰色にしても占有率を落としても透過を残しても**落ちない**——これは
/// 取りこぼしではなく規格の内容である。`Rules` の `None` を「どんな値でも
/// 合格」として `checks[]` に `pass` で並べると、shopify が構図を見ていない
/// ことが結果から読めなくなる（`pass` は「見た上で通った」を意味する語である）。
#[test]
fn a_profile_that_regulates_no_composition_checks_none_of_it() {
    let dir = fixture_dir();
    let rules = profile_rules("shopify");
    let side = conforming_side(&rules);

    // amazon なら 4 項目が落ちる作り（灰色・低占有率・透過）
    let input = write_png(
        dir.path(),
        "loose.png",
        &flat_product(side, side, 0.2, [128, 128, 128], 0),
    );

    let (code, v) = lint(&input, "shopify");
    assert_eq!(
        code,
        0,
        "構図を規定しない規格で落ちた: {:?}",
        not_passing(&v)
    );
    let names = lint_names(&v);
    for absent in ["background", "fill_ratio", "square", "alpha", "color_space"] {
        assert!(
            !names.contains(&absent.to_string()),
            "規定の無い {absent} を検査している: {names:?}"
        );
    }
    assert!(names.contains(&"longest_side".to_string()), "{names:?}");

    // 同じ画像が amazon では落ちる。**規格ごとに見るものが違う**ことの裏取り
    // ——shopify が通したのは kiri が見落としたからではない
    let (code, amazon) = lint(&input, "amazon");
    assert_eq!(code, 5, "同じ画像が amazon でも通った: {amazon}");
    assert!(not_passing(&amazon).len() >= 3, "{amazon}");
}

/// 受け入れ基準 (c)。**AVIF では画素の検査が `skipped` になり、黙って合格にしない。**
///
/// AVIF のフィクスチャは `kiri convert` で**実際に書く**。`tests/common` に
/// AVIF を書く道具は無いし、手で組んだバイト列だと「kiri が書いた AVIF を
/// kiri が読めるか」という肝心の往復が抜ける。
#[test]
fn an_avif_skips_the_pixel_checks_and_says_which() {
    let dir = fixture_dir();
    let rules = profile_rules("amazon");
    let source = write_conforming(dir.path(), "source.png", &rules);
    let avif = dir.path().join("out.avif");
    convert_json(&source, &avif, &[]);

    for profile in ["amazon", "square-white"] {
        let (code, v) = lint(&avif, profile);
        assert_eq!(v["format"], "avif", "{profile}: {v}");
        assert_eq!(
            lint_check(&v, "background")["status"],
            "skipped",
            "{profile}"
        );
        assert_eq!(
            lint_check(&v, "fill_ratio")["status"],
            "skipped",
            "{profile}"
        );
        // **求められていた値は飛ばしても返す。** 検査できなかったことと、
        // 何を求められていたかは別の事実である
        assert!(
            !lint_check(&v, "background")["expected"].is_null(),
            "{profile}: 飛ばした項目が期待値を落とした: {v}"
        );
        // 飛ばした項目も合格ではない
        assert_eq!(v["passed"], Value::Bool(false), "{profile}");
        assert_eq!(code, 5, "{profile}: 飛ばしたのに exit 0 で返した: {v}");

        let warning = v["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .find(|w| w["code"] == "PROFILE_UNCHECKABLE")
            .unwrap_or_else(|| panic!("{profile}: PROFILE_UNCHECKABLE が無い: {v}"));
        // **どれを飛ばしたかは配列で受け取れる。** 文面から項目名を抜き直さない
        let skipped: Vec<String> = v["checks"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|c| c["status"] == "skipped")
            .map(|c| c["name"].as_str().unwrap().to_string())
            .collect();
        assert_eq!(warning["data"]["checks"], Value::from(skipped), "{profile}");
        assert_eq!(warning["data"]["format"], "avif", "{profile}");

        // コンテナだけで分かる項目はちゃんと見ている。amazon も square-white も
        // AVIF を許容に並べていない（kiri が書ける形式であっても、規格が
        // 受けると確かめられていないものは載せない）
        assert_eq!(lint_check(&v, "format")["status"], "fail", "{profile}");
    }
}

/// 受け入れ基準 (c) の裏。**画素を要求しない規格なら AVIF でも素直に検査が終わる。**
///
/// `skipped` は「この形式では構造的に測れない」であって「AVIF だから諦めた」
/// ではない。shopify は寸法とバイト数と形式しか規定していないので、飛ばす項目は
/// 1 つも無く、`PROFILE_UNCHECKABLE` も出ない——**飛ばした項目が無いときに
/// 警告が出ないこと**は、警告が `checks[]` から数えられている証拠でもある。
#[test]
fn a_profile_without_pixel_rules_skips_nothing_in_an_avif() {
    let dir = fixture_dir();
    let source = write_conforming(dir.path(), "source.png", &profile_rules("shopify"));
    let avif = dir.path().join("out.avif");
    convert_json(&source, &avif, &[]);

    let (_, v) = lint(&avif, "shopify");
    assert_eq!(v["format"], "avif", "{v}");
    let skipped: Vec<&Value> = v["checks"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|c| c["status"] == "skipped")
        .collect();
    assert!(skipped.is_empty(), "飛ばした項目がある: {skipped:?}");
    assert!(
        !has_warning(&v, "PROFILE_UNCHECKABLE"),
        "飛ばしていないのに警告が出た: {v}"
    );
    // 寸法とバイト数は AVIF でも測れている
    assert_eq!(lint_check(&v, "longest_side")["status"], "pass", "{v}");
    assert_eq!(lint_check(&v, "file_size")["status"], "pass", "{v}");
}

/// 受け入れ基準 (d)。**明示指定が profile を押しのけ、押しのけた値が実際に効く。**
///
/// ※ 計画書の当初の基準は `--profile amazon --quality 60` だったが、**これは
/// 成立しない。** 品質を規定しているモール規格が無いので `Rules` にも
/// `write_defaults` にも品質の項目が無く、profile は quality を一度も求めない
/// ——求めていない値を「押しのけた」とは言えない。**profile が実際に求める
/// 項目**（canvas / fill_ratio / format）で書き換えた。
///
/// **警告が出ることだけを見ない。** 「指定したのに効かない」を防ぐのが
/// `PROFILE_OVERRIDDEN` の用件なので、押しのけた値が成果物に現れていることを
/// 結果 JSON か書いたファイルから確かめる。警告だけを見ると、**警告を出しながら
/// profile の値で書く**という最悪の形を通してしまう。
#[test]
fn an_explicit_option_beats_the_profile_and_actually_takes_effect() {
    let dir = fixture_dir();
    let input = write_jpeg(
        dir.path(),
        "product.jpg",
        &product_image(&ProductSpec {
            width: 300,
            height: 300,
            ..Default::default()
        }),
    );

    let run = |output: &Path, extra: &[&str]| -> Value {
        let mut args = vec![
            "cutout",
            input.to_str().unwrap(),
            "-o",
            output.to_str().unwrap(),
            "--json",
            "--profile",
            "amazon",
        ];
        args.extend_from_slice(extra);
        let out = kiri().args(&args).output().unwrap();
        assert_eq!(
            out.status.code(),
            Some(0),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        json_stdout(&out)
    };
    let overridden = |v: &Value, key: &str| -> Value {
        v["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .find(|w| w["code"] == "PROFILE_OVERRIDDEN" && w["data"]["key"] == key)
            .unwrap_or_else(|| panic!("{key} の PROFILE_OVERRIDDEN が無い: {v}"))
            .clone()
    };

    // profile が求める値を先に控える。**押しのけたことを言うには、押しのけ
    // られた側の値が要る**——警告の `data.profile` がそれを名乗っているかまで見る
    let rules = profile_rules("amazon");

    // 1. --format で押しのける。profile は JPEG を求める
    let png = dir.path().join("forced.png");
    let v = run(&png, &["--format", "png"]);
    assert_eq!(v["outputs"][0]["format"], "png", "{v}");
    assert_eq!(overridden(&v, "format")["data"]["used"], "png");
    assert_eq!(
        overridden(&v, "format")["data"]["profile"],
        rules["formats"][0],
        "profile が求めた形式を名乗っていない"
    );

    // 2. --canvas で押しのける
    let sized = dir.path().join("sized.jpg");
    let v = run(&sized, &["--canvas", "640x480"]);
    assert_eq!(v["outputs"][0]["width"], 640, "{v}");
    assert_eq!(v["outputs"][0]["height"], 480, "{v}");
    assert_eq!(
        overridden(&v, "canvas")["data"]["used"],
        Value::from(vec![640, 480])
    );

    // 3. --fill-ratio で押しのける。**効いたかどうかは成果物を測って言う**
    //    ——`settings` は指示の写しなので、書いたものが本当にそうなっている
    //    ことの証拠にならない
    let loose = dir.path().join("loose.jpg");
    let want_ratio = rules["fill_ratio_min"].as_f64().unwrap();
    let half = format!("{:.2}", want_ratio / 2.0);
    let v = run(&loose, &["--fill-ratio", &half]);
    assert_eq!(
        overridden(&v, "fill_ratio")["data"]["used"],
        half.parse::<f64>().unwrap()
    );
    let (code, linted) = lint(&loose, "amazon");
    assert_eq!(code, 5, "半分の占有率で書いたのに規格を満たした: {linted}");
    let measured = lint_check(&linted, "fill_ratio")["actual"]["value"]
        .as_f64()
        .unwrap();
    assert!(
        measured < want_ratio,
        "明示した占有率が効いていない: {measured} >= {want_ratio}"
    );
}

/// 受け入れ基準 (d) の 4 通り目。**出力先の拡張子も形式の明示指定として数える。**
///
/// 優先順位に素直に従って profile が勝つと、`-o out.png --profile amazon` が
/// **`.png` という名前のファイルに JPEG を書く。** 拡張子と中身が食い違えば
/// `outputs[].path` が嘘をつき、配信側も他のツールも拡張子で形式を判断するので、
/// その嘘は kiri の外まで運ばれる。**書いたファイルの先頭バイトまで見る**のは、
/// `outputs[].format` が同じ嘘をついても気づけるようにするためである。
#[test]
fn the_output_extension_overrides_the_profile_format() {
    let dir = fixture_dir();
    let input = write_jpeg(
        dir.path(),
        "product.jpg",
        &product_image(&ProductSpec {
            width: 300,
            height: 300,
            ..Default::default()
        }),
    );
    let output = dir.path().join("out.png");

    let out = kiri()
        .args([
            "cutout",
            input.to_str().unwrap(),
            "-o",
            output.to_str().unwrap(),
            "--json",
            "--profile",
            "amazon",
        ])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(0));
    let v = json_stdout(&out);

    assert_eq!(v["outputs"][0]["format"], "png", "{v}");
    let bytes = std::fs::read(&output).unwrap();
    assert_eq!(
        &bytes[..8],
        b"\x89PNG\r\n\x1a\n",
        ".png という名前のファイルの中身が PNG でない"
    );

    let warning = v["warnings"]
        .as_array()
        .unwrap()
        .iter()
        .find(|w| w["code"] == "PROFILE_OVERRIDDEN" && w["data"]["key"] == "format")
        .unwrap_or_else(|| panic!("拡張子が押しのけたことを言っていない: {v}"));
    assert_eq!(warning["data"]["used"], "png");
    assert_eq!(
        warning["data"]["profile"],
        profile_rules("amazon")["formats"][0]
    );

    // **同じ値なら黙っている。** shopify は PNG を第一候補にするので、
    // 同じ `-o out.png` でも押しのけたことにはならない——「profile を指定した
    // のに効かなかった項目はどれか」という問いに、同じ値の行は答えを足さない
    let same = dir.path().join("same.png");
    let out = kiri()
        .args([
            "cutout",
            input.to_str().unwrap(),
            "-o",
            same.to_str().unwrap(),
            "--json",
            "--profile",
            "shopify",
        ])
        .output()
        .unwrap();
    let v = json_stdout(&out);
    assert!(
        !has_warning(&v, "PROFILE_OVERRIDDEN"),
        "同じ値で押しのけたと言っている: {v}"
    );
}

/// **`--profile` を付けただけで `UNKNOWN_OUTPUT_FORMAT` が消えない。**
///
/// `OutputFormat::from_path` の `None` は「拡張子が無い」と「拡張子はあるが
/// 未知」の 2 つを兼ねている。後者で profile に形式を決めさせると
/// `-o out.xyz --profile amazon` が `.xyz` という名前の JPEG を黙って書く
/// ——`the_output_extension_overrides_the_profile_format` が守っている
/// 「拡張子と中身を食い違わせない」を、同じ指定の別の綴りで破ることになる。
///
/// **profile の有無で結果が変わらないことまで見る。** 片方だけを見ても
/// 「断った」としか言えず、`--profile` が判断を動かしていないことは言えない。
#[test]
fn an_unknown_extension_is_refused_with_or_without_a_profile() {
    let dir = fixture_dir();
    let input = write_jpeg(
        dir.path(),
        "product.jpg",
        &product_image(&ProductSpec {
            width: 300,
            height: 300,
            ..Default::default()
        }),
    );
    let output = dir.path().join("out.xyz");

    for profile in [None, Some("amazon")] {
        let mut args = vec![
            "cutout",
            input.to_str().unwrap(),
            "-o",
            output.to_str().unwrap(),
            "--dry-run",
            "--json",
        ];
        if let Some(name) = profile {
            args.extend(["--profile", name]);
        }
        let out = kiri().args(&args).output().unwrap();
        let v = json_stdout(&out);
        assert_eq!(
            out.status.code(),
            Some(2),
            "profile={profile:?}: 未知の拡張子を通した: {v}"
        );
        assert_eq!(v["error"]["code"], "UNKNOWN_OUTPUT_FORMAT", "{v}");
    }

    // **拡張子を綴っていないパスは今までどおり profile が決めてよい。**
    // `--naming` の雛形がそれで、そこでは拡張子は雛形の `{ext}` が供給する
    let stem = dir.path().join("named");
    let out = kiri()
        .args([
            "cutout",
            input.to_str().unwrap(),
            "-o",
            stem.to_str().unwrap(),
            "--naming",
            "{stem}_{width}.{ext}",
            "--dry-run",
            "--json",
            "--profile",
            "amazon",
        ])
        .output()
        .unwrap();
    let v = json_stdout(&out);
    assert_eq!(out.status.code(), Some(0), "{v}");
    assert_eq!(
        v["outputs"][0]["format"],
        profile_rules("amazon")["formats"][0],
        "拡張子の無いパスで profile が形式を決めていない: {v}"
    );
}

/// **派生が自分で書いた形式が profile の外へ出たら、派生ごとに 1 件報せる。**
///
/// `--derive 'format=avif'` は `--format` も `--output` の拡張子も通らないので、
/// profile の形式指定を丸ごと迂回する。黙って通すと **amazon で書いたものが
/// 同じ amazon の `kiri lint` で落ちる**——`FILL_RATIO_MARGIN` の doc が
/// 「最も高くつく失敗」と名指ししているものである。
///
/// **許容の内側にある派生では黙る。** amazon は PNG も許すので、
/// `format=png` の派生に警告を出すと `PROFILE_OVERRIDDEN` が
/// 「profile を指定したのに効かなかった項目」以外を語り始める。
#[test]
fn a_derivation_that_leaves_the_profile_formats_says_so_one_by_one() {
    let dir = fixture_dir();
    let input = write_jpeg(
        dir.path(),
        "product.jpg",
        &product_image(&ProductSpec {
            width: 300,
            height: 300,
            ..Default::default()
        }),
    );
    let output = dir.path().join("e.jpg");

    let out = kiri()
        .args([
            "cutout",
            input.to_str().unwrap(),
            "-o",
            output.to_str().unwrap(),
            "--profile",
            "amazon",
            "--derive",
            "width=800,format=avif,role=hero",
            "--derive",
            "width=400,format=png,role=thumb",
            "--dry-run",
            "--json",
        ])
        .output()
        .unwrap();
    let v = json_stdout(&out);
    assert_eq!(out.status.code(), Some(0), "{v}");

    let overridden: Vec<&Value> = v["warnings"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|w| w["code"] == "PROFILE_OVERRIDDEN" && w["data"]["key"] == "format")
        .collect();
    assert_eq!(
        overridden.len(),
        1,
        "許容の外へ出た派生はちょうど 1 本のはず: {v}"
    );
    let warning = overridden[0];
    assert_eq!(warning["data"]["used"], "avif");
    // **どの出力の話かを名乗る。** パスはまだ綴れないので、並びの添字と role で指す
    assert_eq!(warning["data"]["derive"], 0);
    assert_eq!(warning["data"]["role"], "hero");
    // 求めた値は許容の並びそのもの——第一候補 1 つでは「png でも通る」が読めない
    assert_eq!(
        warning["data"]["profile"],
        profile_rules("amazon")["formats"]
    );

    // **その出力は実際に同じ profile の lint で落ちる。** 警告が指している
    // のがまさにこれであることを、作り話ではなく実測で固定する
    let out = kiri()
        .args([
            "cutout",
            input.to_str().unwrap(),
            "-o",
            output.to_str().unwrap(),
            "--profile",
            "amazon",
            "--derive",
            "width=800,format=avif,role=hero",
            "--json",
        ])
        .output()
        .unwrap();
    let v = json_stdout(&out);
    assert_eq!(out.status.code(), Some(0), "{v}");
    let written = Path::new(v["outputs"][0]["path"].as_str().unwrap()).to_path_buf();
    let (code, lint_result) = lint(&written, "amazon");
    assert_eq!(code, 5, "amazon で書いた AVIF が amazon の lint を通った");
    assert_eq!(lint_check(&lint_result, "format")["status"], "fail");
}

/// **派生の寸法が profile の外へ出たら、派生ごとに 1 件報せる。**
///
/// 形式と同じ形の迂回である。`--sizes 400` は `--canvas` も `--longest-side` も
/// 通らないので、profile が決めた寸法を丸ごと素通りする。**黙って通すと
/// `--profile amazon` で書いた 400x400 が同じ amazon の `kiri lint` で
/// `longest_side` fail になる**——形式の迂回と違って出力が実際に規格違反に
/// なるので、実害はこちらのほうが直接的である。
///
/// **規格の内側に収まる派生では黙る**（`--sizes 800`）。そこで警告を出すと
/// `PROFILE_OVERRIDDEN` が「profile を指定したのに効かなかった項目」以外を
/// 語り始める（形式の側とまったく同じ規約）。
#[test]
fn a_derivation_that_leaves_the_profile_dimensions_says_so_one_by_one() {
    let dir = fixture_dir();
    let input = write_jpeg(
        dir.path(),
        "product.jpg",
        &product_image(&ProductSpec {
            width: 2000,
            height: 2000,
            ..Default::default()
        }),
    );
    let output = dir.path().join("w.jpg");

    let out = kiri()
        .args([
            "cutout",
            input.to_str().unwrap(),
            "-o",
            output.to_str().unwrap(),
            "--profile",
            "amazon",
            "--sizes",
            "400,800",
            "--json",
        ])
        .output()
        .unwrap();
    let v = json_stdout(&out);
    assert_eq!(out.status.code(), Some(0), "{v}");

    let overridden: Vec<&Value> = v["warnings"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|w| w["code"] == "PROFILE_OVERRIDDEN" && w["data"]["key"] == "longest_side")
        .collect();
    assert_eq!(
        overridden.len(),
        1,
        "規格の外へ出た派生はちょうど 1 本のはず: {v}"
    );
    let warning = overridden[0];
    // **どの出力の話かを名乗る。** 並びの添字は `outputs[]` の添字と同じ
    assert_eq!(warning["data"]["derive"], 0);
    // 求めた値は上下限の組、効いた値は実際に書く長辺
    assert_eq!(
        warning["data"]["profile"],
        serde_json::json!({
            "min": profile_rules("amazon")["longest_side_min"],
            "max": profile_rules("amazon")["longest_side_max"],
        })
    );
    assert_eq!(warning["data"]["used"], 400);

    // **その出力は実際に同じ profile の lint で落ちる。** 警告が指している
    // のがまさにこれであることを、作り話ではなく実測で固定する
    let small = Path::new(v["outputs"][0]["path"].as_str().unwrap()).to_path_buf();
    assert_eq!(v["outputs"][0]["width"], 400, "{v}");
    let (code, lint_result) = lint(&small, "amazon");
    assert_eq!(code, 5, "amazon で書いた 400px が amazon の lint を通った");
    assert_eq!(lint_check(&lint_result, "longest_side")["status"], "fail");

    // 規格の内側の派生は黙って書かれる
    let big = Path::new(v["outputs"][1]["path"].as_str().unwrap()).to_path_buf();
    assert_eq!(v["outputs"][1]["width"], 800, "{v}");
    let (_, big_result) = lint(&big, "amazon");
    assert_eq!(lint_check(&big_result, "longest_side")["status"], "pass");
}

/// **`--profile` を渡さない実行は 1 バイトも変わらない。**
///
/// Phase 21 の `no_fail_on_means_no_compliance_block_and_no_new_exit_code` と
/// 同じ位置づけの検査である。既存のテストを 1 本も書き換えずに通すことが第一の
/// 証拠だが、それは「変わっていない」を直接は言わない。
///
/// **`--profile` を渡した実行との比較にはしない。** profile は canvas を必ず
/// 決めるので、明示で打ち消そうにも「canvas を指定しない」とは書けず、
/// 両者が同じバイト列になる指定は存在しない。代わりに、profile が触ると
/// 宣言している項目を**全部明示した**実行を 2 通り（profile あり / なし）
/// 走らせる——このとき profile は 1 つの値も決められないので、**成果物が
/// 1 バイトでも違えば、profile が宣言の外へ手を伸ばしている**ことになる。
#[test]
fn a_profile_decides_nothing_outside_the_items_it_declares() {
    let dir = fixture_dir();
    let input = write_jpeg(
        dir.path(),
        "product.jpg",
        &product_image(&ProductSpec {
            width: 300,
            height: 300,
            ..Default::default()
        }),
    );

    // profile が触ると名乗る 6 項目をすべて明示する
    // （`--flatten` と `--max-bytes` は amazon では同じ値・規定なしに落ち着くので
    //   警告にならないが、明示そのものは効いている）
    let explicit = [
        "--canvas",
        "480x480",
        "--fill-ratio",
        "0.7",
        "--format",
        "png",
        "--background",
        "#FAFAFA",
        "--flatten",
        "--max-bytes",
        "5M",
    ];
    let run = |output: &Path, extra: &[&str]| -> Value {
        let mut args = vec![
            "cutout",
            input.to_str().unwrap(),
            "-o",
            output.to_str().unwrap(),
            "--json",
        ];
        args.extend_from_slice(&explicit);
        args.extend_from_slice(extra);
        let out = kiri().args(&args).output().unwrap();
        assert_eq!(
            out.status.code(),
            Some(0),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        json_stdout(&out)
    };

    let plain_path = dir.path().join("plain.png");
    let profiled_path = dir.path().join("profiled.png");
    let plain = run(&plain_path, &[]);
    let profiled = run(&profiled_path, &["--profile", "amazon"]);

    assert_eq!(
        std::fs::read(&plain_path).unwrap(),
        std::fs::read(&profiled_path).unwrap(),
        "明示で埋めたのに profile がバイト列を動かした"
    );
    assert!(
        plain["settings"].get("profile").is_none(),
        "--profile 無しで settings.profile が現れた: {}",
        plain["settings"]
    );
    assert_eq!(
        profiled["settings"]["profile"]["name"], "amazon",
        "効いた profile を名乗っていない: {profiled}"
    );
    assert!(
        !has_warning(&plain, "PROFILE_OVERRIDDEN"),
        "--profile 無しで profile の警告が出た: {plain}"
    );

    // 結果 JSON も `settings.profile` と `PROFILE_OVERRIDDEN` 以外は動かない。
    // 所要時間とパスだけが実行ごとに違う
    let strip = |v: &Value| -> Value {
        let mut o = v.as_object().unwrap().clone();
        o.remove("elapsed_ms");
        let mut settings = o["settings"].as_object().unwrap().clone();
        settings.remove("profile");
        o.insert("settings".into(), Value::Object(settings));
        let warnings: Vec<Value> = v["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|w| w["code"] != "PROFILE_OVERRIDDEN")
            .cloned()
            .collect();
        o.insert("warnings".into(), Value::Array(warnings));
        let outputs: Vec<Value> = v["outputs"]
            .as_array()
            .unwrap()
            .iter()
            .map(|entry| {
                let mut e = entry.as_object().unwrap().clone();
                e.remove("path");
                Value::Object(e)
            })
            .collect();
        o.insert("outputs".into(), Value::Array(outputs));
        Value::Object(o)
    };
    assert_eq!(
        strip(&plain),
        strip(&profiled),
        "profile が結果の他の部分を動かしている"
    );
}

/// **書いた側と検査する側が同じ規格を指していることの、唯一の実行による保証。**
///
/// `profile.rs` と `lint.rs` の単体テストは、どちらも組み立てた `Facts` の上で
/// 表と設定の整合を見る。実際に書いた画素を通していないので、JPEG の量子化も
/// キャンバス配置の丸めも `--feather` のにじみも 1 つも乗らない。
/// **profile で書いたものが同じ profile の lint で落ちるのが Phase 22 で
/// 最も高くつく失敗**なので、ここだけは本物のファイルで往復させる。
#[test]
fn what_a_profile_writes_passes_the_same_profiles_lint_end_to_end() {
    let dir = fixture_dir();
    // 拡大を避けるために大きめの素材を採る。`CANVAS_UPSCALED` が出ても合否には
    // 関わらないが、拡大したものを検査すると「規格を満たしたのは拡大のおかげ」
    // という読み方が混ざる
    let input = write_jpeg(
        dir.path(),
        "product.jpg",
        &product_image(&ProductSpec {
            width: 2000,
            height: 2000,
            ..Default::default()
        }),
    );

    for name in ["amazon", "shopify", "square-white"] {
        let rules = profile_rules(name);
        // **出力先の拡張子を profile の第一候補に揃える。** 揃えないと拡張子が
        // 形式を押しのけ、検査しているものが profile の決めた形式でなくなる
        let ext = rules["formats"][0].as_str().unwrap();
        let output = dir.path().join(format!("{name}.{ext}"));
        let out = kiri()
            .args([
                "cutout",
                input.to_str().unwrap(),
                "-o",
                output.to_str().unwrap(),
                "--json",
                "--profile",
                name,
            ])
            .output()
            .unwrap();
        assert_eq!(
            out.status.code(),
            Some(0),
            "{name}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let written = json_stdout(&out);
        assert!(
            !has_warning(&written, "PROFILE_OVERRIDDEN"),
            "{name}: 何も明示していないのに押しのけが起きた: {written}"
        );

        let (code, v) = lint(&output, name);
        assert_eq!(
            code,
            0,
            "{name}: profile で書いたものが同じ profile の lint で落ちた: {:?}",
            not_passing(&v)
        );
        assert_eq!(v["passed"], Value::Bool(true), "{name}: {v}");
    }
}

/// **占有率を極端に寄せて書いたものも、同じ profile の lint を通る。**
///
/// 上の往復は `--fill-ratio` を profile の既定（0.85）に任せているので、
/// **主体が外周の帯に入る領域を 1 度も踏まない。** `conforming_fill` が
/// `fill_ratio_min + 0.03` を採っているのも同じで、下限のすぐ上しか見ない。
///
/// lint は背景を短辺の 3% の帯で測るので、占有率が `1 - 2/33 ≒ 0.939` を
/// 超えると**主体そのものがその帯に入る**。そこで落ちた `uniformity` を
/// 「背景が単色でない」と読んで `unmeasurable` を返していたことがあり、
/// **`kiri cutout --profile amazon --fill-ratio 0.95` で書いた純白背景の
/// 正方形が、同じ amazon の `kiri lint` で落ちていた**（0.94 は通る）。
///
/// どの profile にも占有率の**上限**は無い。寄りのトリミングは全規格で
/// 合法なので、規格が許す構図で書いたものが規格で落ちること自体が誤りである。
///
/// **背景を規定する規格だけを回す。** 規定の無い規格（shopify）では
/// `background` の行がそもそも出ないので、この往復で守れるものが無い。
#[test]
fn a_tightly_cropped_profile_output_still_passes_the_same_profiles_lint() {
    let dir = fixture_dir();
    let input = write_jpeg(
        dir.path(),
        "tight.jpg",
        &product_image(&ProductSpec {
            width: 2000,
            height: 2000,
            ..Default::default()
        }),
    );

    for name in ["amazon", "square-white"] {
        let rules = profile_rules(name);
        if rules["background"].is_null() {
            continue;
        }
        let ext = rules["formats"][0].as_str().unwrap();
        // 回帰した帯（0.95 以上）と、当時も通っていた 0.94 の両方を踏む。
        // **通っていた側を残す**のは、直した結果として広いほうの帯が
        // 使われなくなっていないかを同じテストで見るためである
        for fill in ["0.94", "0.95", "0.96", "0.97", "0.98"] {
            let output = dir.path().join(format!("{name}_{fill}.{ext}"));
            let out = kiri()
                .args([
                    "cutout",
                    input.to_str().unwrap(),
                    "-o",
                    output.to_str().unwrap(),
                    "--json",
                    "--profile",
                    name,
                    "--fill-ratio",
                    fill,
                ])
                .output()
                .unwrap();
            assert_eq!(
                out.status.code(),
                Some(0),
                "{name} / fill {fill}: {}",
                String::from_utf8_lossy(&out.stderr)
            );

            let (code, v) = lint(&output, name);
            assert_eq!(
                code,
                0,
                "{name} / fill {fill}: profile で書いたものが同じ profile の lint で落ちた: {:?}",
                not_passing(&v)
            );
            // **測れた帯と色まで見る。** exit 0 だけでは、`background` の行が
            // 出ていない実行（規定なしの規格を取り違えた）でも緑になる
            let check = lint_check(&v, "background");
            assert_eq!(check["status"], "pass", "{name} / fill {fill}: {check}");
            assert_eq!(
                check["actual"]["rgb"], rules["background"],
                "{name} / fill {fill}: 測った背景色が規格の色と違う: {check}"
            );
            // 帯は主体の手前で止まる。**`--border` の既定 2 まで落ちていない**
            // ——そこまで狭めると、外周 2px の縁 1 本で「背景は純白」と
            // 答える側の穴が開く
            let band = check["actual"]["border_px"].as_u64().expect("帯幅が無い");
            assert!(
                band > 2,
                "{name} / fill {fill}: 外周 2px で判定している: {check}"
            );
        }
    }
}

/// **同じ入力を 2 回検査した結果はバイト列として一致する。**
///
/// `checks[]` の並びが指定や `HashMap` の順に依れば、結果の差分で納品物を
/// 見張れなくなる（`compliance::Metric::ALL` と同じ理由）。文字列として
/// 比べるのは、`serde_json::Value` へ読み直すと `Map` が綴りで並べ替えてしまい、
/// **ファイルの中の順序が分からなくなる**ためである（Phase 20 のマニフェストの
/// ゴールデンと同じ作法）。
#[test]
fn two_lint_runs_agree_byte_for_byte() {
    let dir = fixture_dir();
    // 合格と不合格の両方で見る。**不合格の側は `code` と `actual` が増える**ので、
    // 決定性を崩す余地も合格の側より広い
    let ok = write_conforming(dir.path(), "ok.png", &profile_rules("amazon"));
    let bad = write_png(
        dir.path(),
        "bad.png",
        &flat_product(600, 600, 0.3, [200, 200, 200], 255),
    );

    for input in [&ok, &bad] {
        let run = || {
            kiri()
                .args([
                    "lint",
                    input.to_str().unwrap(),
                    "--profile",
                    "amazon",
                    "--json",
                ])
                .output()
                .unwrap()
                .stdout
        };
        let first = run();
        assert_eq!(first, run(), "{}: 2 回の結果が違う", input.display());

        // 並びが `checks[]` の中で決定的であることまで見る（同じバイト列なら
        // 自明だが、**何を約束しているか**を名前で残す）
        let v: Value = serde_json::from_slice(&first).unwrap();
        let names = lint_names(&v);
        assert!(
            names.windows(2).all(|w| w[0] != w[1]),
            "name が重複: {names:?}"
        );
        assert!(!names.is_empty());
    }
}

/// **`kiri lint` は 1 バイトも書かない。**
///
/// 検査の道具が対象を書き換えたら、「検査に通ったもの」と「納品するもの」が
/// 別物になりうる。`--dry-run` のような打ち消しの指定が無いので、**書かない
/// ことそのものが契約**である。
#[test]
fn lint_writes_nothing_at_all() {
    let dir = fixture_dir();
    let input = write_png(
        dir.path(),
        "subject.png",
        &flat_product(600, 600, 0.3, [200, 200, 200], 255),
    );
    let before = std::fs::read(&input).unwrap();
    let listing = |dir: &Path| -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    };
    let files = listing(dir.path());

    for profile in ["amazon", "shopify", "square-white"] {
        lint(&input, profile);
    }

    assert_eq!(
        before,
        std::fs::read(&input).unwrap(),
        "検査対象のバイト列が変わった"
    );
    assert_eq!(files, listing(dir.path()), "ディレクトリにファイルが増えた");
}

/// spec の `profile` は CLI と同じ関門を通り、`defaults` から継げる。
///
/// **`batch.rs` の 3 箇所（`ItemSettings` / `pick!` / `SETTING_KEYS`）が揃って
/// いること**を実行で確かめる（Phase 21 の `fail_on` と同じ作法）。
/// 未知の名前を spec でだけ黙って既定へ落とすと、**数百点を書き切った後に
/// 「規格に収めたはずのものが収まっていない」**という最も遅い気づき方になる。
#[test]
fn a_spec_names_a_profile_and_inherits_it_from_the_defaults() {
    let dir = fixture_dir();
    write_jpeg(
        dir.path(),
        "p.jpg",
        &product_image(&ProductSpec {
            width: 300,
            height: 300,
            ..Default::default()
        }),
    );

    let spec = write_spec(
        dir.path(),
        r#"{"defaults":{"profile":"square-white"},
             "items":[
               {"input":"p.jpg","output":"out/a.jpg"},
               {"input":"p.jpg","output":"out/b.jpg","profile":"amazon"},
               {"input":"p.jpg","output":"out/c.jpg","canvas":"320x320"}
             ]}"#,
    );
    let out = run_batch(&spec, &[]);
    assert_eq!(
        out.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let v = json_stdout(&out);

    // 既定が効く / 項目の指定が勝つ
    assert_eq!(
        v["results"][0]["result"]["settings"]["profile"]["name"],
        "square-white"
    );
    assert_eq!(
        v["results"][1]["result"]["settings"]["profile"]["name"],
        "amazon"
    );
    // **明示 > profile は spec 経由でも同じ。** `canvas` を書いた項目では
    // profile の canvas が押しのけられ、PROFILE_OVERRIDDEN が出る
    assert_eq!(v["results"][2]["result"]["outputs"][0]["width"], 320);
    let codes: Vec<&str> = v["results"][2]["result"]["warnings"]
        .as_array()
        .unwrap()
        .iter()
        .map(|w| w["code"].as_str().unwrap())
        .collect();
    assert!(codes.contains(&"PROFILE_OVERRIDDEN"), "{codes:?}");

    // 継いだ項目は規格の寸法で書かれている（square-white は正方形を求める）
    let written = v["results"][0]["result"]["outputs"][0].clone();
    assert_eq!(written["width"], written["height"], "{written}");
}

/// spec の未知の profile 名は `UNKNOWN_PROFILE` でその項目を落とす。
///
/// clap を通らないので、CLI と同じ関門（`profile::named`）を
/// `commands::batch` が通す。抜けていると、**CLI では断る名前が spec でだけ
/// 黙って既定へ落ちる**——規格へ収めたつもりの納品物が素のまま出ていく。
#[test]
fn an_unknown_profile_in_a_spec_is_refused_with_a_code() {
    let dir = fixture_dir();
    write_jpeg(
        dir.path(),
        "p.jpg",
        &product_image(&ProductSpec {
            width: 60,
            height: 60,
            ..Default::default()
        }),
    );

    // **載せていない規格の名前**と、綴り違いと、空。どれも「近いから通す」を
    // してはいけない（`rakuten` は載せていないだけで実在する規格の名前である）
    for written in ["\"rakuten\"", "\"Amazon\"", "\"amazon \"", "\"\""] {
        let spec = write_spec(
            dir.path(),
            &format!(
                r#"{{"items":[{{"input":"p.jpg","output":"out/x.png","profile":{written}}}]}}"#
            ),
        );
        let out = run_batch(&spec, &[]);
        assert_eq!(out.status.code(), Some(4), "profile:{written}");
        let v = json_stdout(&out);
        assert_eq!(
            v["results"][0]["error"]["code"], "UNKNOWN_PROFILE",
            "profile:{written}"
        );
        // 候補を必ず添える。**綴りを疑いようのない失敗**にしない
        assert!(
            v["results"][0]["error"]["hint"]
                .as_str()
                .is_some_and(|h| h.contains("amazon")),
            "候補を案内していない: {v}"
        );
    }
}

/// `kiri lint` の断り方。**引数の誤りと入力の誤りを取り違えない。**
///
/// `--profile` は必須なので、渡さない実行は clap が断る（exit 2、code 無し、
/// stdout は空）。未知の名前も同じ段で落ちる——**入力を 1 バイトも読む前**に
/// 断ることが、数百点を回す呼び出し側にとっての速さそのものである。
/// 一方、読めないファイルと辿れないファイルは exit 3 で、**結果 JSON では
/// なく `ErrorReport` が返る**（検査が成立していないので `checks[]` を組めない）。
#[test]
fn lint_separates_a_bad_request_from_a_bad_input() {
    let dir = fixture_dir();
    let ok = write_conforming(dir.path(), "ok.png", &profile_rules("amazon"));
    let broken = dir.path().join("broken.jpg");
    std::fs::write(&broken, b"this is not an image").unwrap();
    let missing = dir.path().join("does-not-exist.png");

    // --profile を渡さない / 未知の名前 → exit 2、stdout は空
    for args in [
        vec!["lint", ok.to_str().unwrap(), "--json"],
        vec![
            "lint",
            // **入力が存在しなくても exit 3 にならない**ことが、引数の検査が
            // 先に走っている証拠である
            missing.to_str().unwrap(),
            "--profile",
            "rakuten",
            "--json",
        ],
    ] {
        let out = kiri().args(&args).output().unwrap();
        assert_eq!(out.status.code(), Some(2), "{args:?}");
        assert!(out.stdout.is_empty(), "{args:?} で stdout が出た");
        assert!(!out.stderr.is_empty(), "{args:?} で stderr が空");
    }

    // 読めない / 辿れない → exit 3。code を名乗る JSON が返る
    for (input, code) in [
        (&missing, "INPUT_UNREADABLE"),
        (&broken, "UNSUPPORTED_FORMAT"),
    ] {
        let (exit, v) = lint(input, "amazon");
        assert_eq!(exit, 3, "{}", input.display());
        assert_eq!(v["error"]["code"], code, "{v}");
        assert!(
            v.get("checks").is_none(),
            "検査が成立していないのに checks を返した: {v}"
        );
    }
}

/// 上限バイト数を超えたファイルは、**サイズの項目だけで落ちる。**
///
/// `breaking_one_rule_fails_exactly_that_check` と同じ形だが、素材が 20MB 近く
/// あるので 1 本に分けた。寸法は**公開された上限から組む**——上限を 4 バイト
/// （RGBA 1 画素）で割った平方根が、圧縮の効かない画素で上限に届く 1 辺である。
/// 数を書き写すと、規格が改訂されたときに素材だけが古い上限を狙い続ける。
///
/// **単色では作れない。** 単色の PNG は数 KB まで縮むので、上限に届かせるには
/// 圧縮しにくいノイズが要る（`noisy_image` を置いた理由そのものである）。
#[test]
fn a_file_over_the_byte_budget_fails_only_the_size_check() {
    let dir = fixture_dir();
    let rules = profile_rules("shopify");
    let limit = rules["max_bytes"].as_u64().expect("上限バイト数が無い");
    // RGBA 1 画素 4 バイト。正方形なので 1 辺は √(上限 ÷ 4)。余白を足して確実に超す
    let side = (limit as f64 / 4.0).sqrt().ceil() as u32 + 100;
    let input = write_png(dir.path(), "heavy.png", &noisy_image(side, side));
    let size = std::fs::metadata(&input).unwrap().len();
    assert!(size > limit, "素材が上限を超えていない: {size} <= {limit}");

    let (code, v) = lint(&input, "shopify");
    assert_eq!(code, 5, "上限を超えたのに通った: {v}");
    assert_eq!(
        not_passing(&v),
        vec!["file_size=\"fail\"".to_string()],
        "落ちたのが file_size ちょうどではない: {v}"
    );
    // **求めた上限は結果が名乗る。** 読む側が README を引かずに、どれだけ
    // 超えたかをその場で測れる
    let check = lint_check(&v, "file_size");
    assert_eq!(check["expected"], Value::from(limit));
    assert_eq!(check["actual"], Value::from(size));

    // 同じ規格で上限の内側にあるものは通る。**「以下が合格」である**ことを、
    // 上限を跨いだ 2 枚で挟んで言う
    let light = write_conforming(dir.path(), "light.png", &rules);
    let (code, v) = lint(&light, "shopify");
    assert_eq!(code, 0, "{v}");
    assert!(
        lint_check(&v, "file_size")["actual"].as_u64().unwrap() <= limit,
        "{v}"
    );
}

// --- セット内のスケール・余白の統一（batch の `set`） ---

/// 「同じ商品を距離を変えて撮った」合成シーン。
///
/// `product_image` は商品を**画像に対する固定の割合**で描くので、寸法だけを
/// 変えても占有率が 1 ビットも動かない。距離の違いを作るには、小さく描いた
/// 1 枚を大きな背景の中央へ貼る——背景色は同じで雑音も切ってあるので、
/// 貼り合わせた継ぎ目は 1 画素も見えない。
fn distance_scene(canvas: u32, product: (u32, u32)) -> image::RgbaImage {
    let background = ProductSpec::default().background;
    let mut scene = image::RgbaImage::from_pixel(
        canvas,
        canvas,
        image::Rgba([background[0], background[1], background[2], 255]),
    );
    let shot = product_image(&ProductSpec {
        width: product.0,
        height: product.1,
        noise: false,
        ..Default::default()
    });
    image::imageops::overlay(
        &mut scene,
        &shot,
        i64::from((canvas - product.0) / 2),
        i64::from((canvas - product.1) / 2),
    );
    scene
}

/// 書かれた画像の中で、不透明な画素が縦に占める高さ(px)。
///
/// **報告の数値ではなく成果物そのものを測る。** `canvas.fill_ratio` が
/// 正しくても、`plan()` へ渡す経路がどこかで落ちていれば画像は揃わない。
fn written_subject_height(path: &Path) -> u32 {
    let img = image::open(path).unwrap().to_rgba8();
    let rows: Vec<u32> = (0..img.height())
        .filter(|&y| (0..img.width()).any(|x| img.get_pixel(x, y).0[3] > 0))
        .collect();
    assert!(!rows.is_empty(), "{} が全面透明", path.display());
    rows[rows.len() - 1] - rows[0] + 1
}

/// 受け入れ基準 (a) — 同じ商品を 3 通りの距離で撮ったセットを
/// `align: "height"` に掛けると、**出力上の商品高さが ±1px で揃う。**
///
/// 4 点目に**同じ商品を横に寝かせた 1 枚**を混ぜてある。縦横比が違う点が
/// 無いと、`--fill-ratio` を 1 つ書くだけでも高さは揃ってしまう
/// （正方キャンバスでは縦長の商品の高さは常に `round(CH*f)` になる）ので、
/// **この検査は `set` が何もしなくても通る。** 寝かせた 1 枚は
/// `align: "bbox"`（＝ただの `fill_ratio`）なら別の高さになるので、
/// 4 点が揃ったことが「高さで揃えた」ことの証拠になる。
#[test]
fn a_set_aligns_the_subject_height_across_shooting_distances() {
    let dir = fixture_dir();
    for (name, product) in [
        ("near.png", (280, 280)),
        ("mid.png", (200, 200)),
        ("far.png", (120, 120)),
        ("lying.png", (400, 200)),
    ] {
        write_png(dir.path(), name, &distance_scene(400, product));
    }

    let items = r#"[{"input":"near.png","output":"out/near.png"},
                    {"input":"mid.png","output":"out/mid.png"},
                    {"input":"far.png","output":"out/far.png"},
                    {"input":"lying.png","output":"out/lying.png"}]"#;
    let spec = write_spec(
        dir.path(),
        &format!(
            r#"{{"set":{{"align":"height"}},
                 "defaults":{{"canvas":"500x500","format":"png"}},
                 "items":{items}}}"#
        ),
    );
    let out = run_batch(&spec, &[]);
    assert_eq!(
        out.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let v = json_stdout(&out);

    let heights: Vec<u32> = ["near", "mid", "far", "lying"]
        .iter()
        .map(|n| written_subject_height(&dir.path().join(format!("out/{n}.png"))))
        .collect();
    let (lo, hi) = (
        *heights.iter().min().unwrap(),
        *heights.iter().max().unwrap(),
    );
    assert!(hi - lo <= 1, "高さが揃っていない: {heights:?}（{v}）");

    // 目標は T * CH。報告の T からそのまま綴れる
    let target = v["set"]["fill_ratio"].as_f64().expect("set が無い") * 500.0;
    assert!(
        heights
            .iter()
            .all(|h| (f64::from(*h) - target).abs() <= 1.0),
        "目標 {target} と食い違う: {heights:?}"
    );

    // **横に寝かせた 1 枚は `bbox` 揃えなら別の高さになる。** ここが同じなら
    // 上の表明は `set` を通さなくても成り立ってしまう
    let bbox_spec = write_spec(
        dir.path(),
        &format!(
            r#"{{"set":{{"align":"bbox","fill_ratio":{}}},
                 "defaults":{{"canvas":"500x500","format":"png"}},
                 "items":[{{"input":"lying.png","output":"out/bbox.png"}}]}}"#,
            v["set"]["fill_ratio"]
        ),
    );
    let out = run_batch(&bbox_spec, &[]);
    assert_eq!(out.status.code(), Some(0));
    let by_bbox = written_subject_height(&dir.path().join("out/bbox.png"));
    assert!(
        by_bbox.abs_diff(heights[3]) > 1,
        "bbox 揃えと同じ高さになった（検査に意味が無い）: {by_bbox} vs {}",
        heights[3]
    );
}

/// 受け入れ基準 (b) — `set` を書かない実行の結果 JSON に `set` のキーは無い。
///
/// `compliance` / `optimize` と同じ規約である。**渡さない実行の結果 JSON が
/// 1 バイトも変わらない**ことが加算だけの変更の条件で、`null` を置くと
/// 「揃えていない」と「揃えられない（古い版）」が同じ形になる。
///
/// 出力そのものが変わっていないことは**既存のテストを 1 本も書き換えずに
/// 通すこと**が言う。ここはキーの有無だけを固定する。
#[test]
fn a_spec_without_set_carries_no_set_block() {
    let dir = fixture_dir();
    write_png(dir.path(), "p.png", &distance_scene(400, (200, 200)));
    let spec = write_spec(
        dir.path(),
        r#"{"defaults":{"canvas":"500x500","format":"png"},
             "items":[{"input":"p.png","output":"out/a.png"}]}"#,
    );
    let out = run_batch(&spec, &[]);
    assert_eq!(out.status.code(), Some(0));
    let v = json_stdout(&out);
    assert!(v.get("set").is_none(), "set を書いていないのに現れた: {v}");
}

/// 受け入れ基準 (c) — `align: "bbox"` は全点で `f_i = T` になる。
///
/// **`set` 無しで同じ `fill_ratio` を全項目へ書いた実行とバイト一致する。**
/// `f_i = T * min(CW/cw, CH/ch) / min(CW/cw, CH/ch) = T` なので、外接矩形を
/// 揃えるとは「同じ占有率を配る」ことそのものである。ここが食い違うなら、
/// `f_i` を綴る式か渡す経路のどちらかが壊れている。
#[test]
fn a_bbox_aligned_set_writes_the_same_bytes_as_one_fill_ratio() {
    let dir = fixture_dir();
    for (name, product) in [("a.png", (280, 280)), ("b.png", (140, 220))] {
        write_png(dir.path(), name, &distance_scene(400, product));
    }

    let by_set = write_spec(
        dir.path(),
        r#"{"set":{"align":"bbox","fill_ratio":0.7},
             "defaults":{"canvas":"500x500","format":"png"},
             "items":[{"input":"a.png","output":"set/a.png"},
                      {"input":"b.png","output":"set/b.png"}]}"#,
    );
    assert_eq!(run_batch(&by_set, &[]).status.code(), Some(0));

    let plain = dir.path().join("plain.json");
    std::fs::write(
        &plain,
        r#"{"defaults":{"canvas":"500x500","format":"png","fill_ratio":0.7},
             "items":[{"input":"a.png","output":"plain/a.png"},
                      {"input":"b.png","output":"plain/b.png"}]}"#,
    )
    .unwrap();
    assert_eq!(run_batch(&plain, &[]).status.code(), Some(0));

    for name in ["a.png", "b.png"] {
        assert_eq!(
            std::fs::read(dir.path().join("set").join(name)).unwrap(),
            std::fs::read(dir.path().join("plain").join(name)).unwrap(),
            "{name} のバイト列が食い違う"
        );
    }
}

/// 受け入れ基準 (d) — `fill_ratio` を省いた `set` の T は、全点の占有率の
/// **中央値**である。偶数個は中央 2 つの平均を採る。
///
/// 占有率は結果 JSON の `subject.normalized_bbox` から綴り直せる。pass 1 は
/// `see_background` だけを通し、pass 2 の切り抜きも同じ見立てを使うので、
/// **測り方が 2 つある余地が無い**ことをここで確かめる。
#[test]
fn the_set_target_is_the_median_of_the_measured_occupancies() {
    let dir = fixture_dir();
    for (name, product) in [
        ("a.png", (120, 120)),
        ("b.png", (200, 200)),
        ("c.png", (280, 280)),
        ("d.png", (340, 340)),
    ] {
        write_png(dir.path(), name, &distance_scene(400, product));
    }

    // 測った高さの割合を結果から集める
    let heights = |v: &Value| -> Vec<f64> {
        v["results"]
            .as_array()
            .unwrap()
            .iter()
            .map(|r| {
                let b = &r["result"]["subject"]["normalized_bbox"];
                b[3].as_f64().unwrap() - b[1].as_f64().unwrap()
            })
            .collect()
    };
    let median = |mut xs: Vec<f64>| -> f64 {
        xs.sort_by(f64::total_cmp);
        let n = xs.len();
        if n % 2 == 1 {
            xs[n / 2]
        } else {
            (xs[n / 2 - 1] + xs[n / 2]) / 2.0
        }
    };

    for (label, items) in [
        (
            "奇数個",
            r#"[{"input":"a.png","output":"out/a.png"},
                {"input":"b.png","output":"out/b.png"},
                {"input":"c.png","output":"out/c.png"}]"#,
        ),
        (
            "偶数個",
            r#"[{"input":"a.png","output":"even/a.png"},
                {"input":"b.png","output":"even/b.png"},
                {"input":"c.png","output":"even/c.png"},
                {"input":"d.png","output":"even/d.png"}]"#,
        ),
    ] {
        let spec = write_spec(
            dir.path(),
            &format!(
                r#"{{"set":{{"align":"height"}},
                     "defaults":{{"canvas":"500x500","format":"png"}},
                     "items":{items}}}"#
            ),
        );
        let out = run_batch(&spec, &[]);
        assert_eq!(out.status.code(), Some(0), "{label}");
        let v = json_stdout(&out);
        let want = median(heights(&v));
        let got = v["set"]["fill_ratio"].as_f64().unwrap();
        assert!(
            (got - want).abs() < 1e-4,
            "{label}: T {got} が中央値 {want} と違う: {v}"
        );
        assert_eq!(v["set"]["source"], "median", "{label}");
        assert_eq!(v["set"]["align"], "height", "{label}");
        assert_eq!(
            v["set"]["measured"].as_u64().unwrap() as usize,
            v["results"].as_array().unwrap().len(),
            "{label}: 測れた件数が合わない"
        );
    }
}

/// 受け入れ基準 (e) — 極端に横長の 1 点は `f_i > 1.0` になり、1.0 で止めて
/// `SET_SCALE_CLAMPED` を出す。**他の点は 1 バイトも影響を受けない。**
///
/// **これは「1 点だけ外れた異常」ではなく物理である。** 正方キャンバスで
/// 高さを `T*CH` にすると、横幅は `T*CH*(cw/ch)` を要求する。縦横比が
/// `1/T` を超える横長では、それがキャンバスの幅を越える。黙って
/// 「高さが揃った」ことにせず、止めたことと目標との差を報せる。
#[test]
fn an_extremely_wide_item_is_clamped_without_touching_the_others() {
    let dir = fixture_dir();
    write_png(dir.path(), "a.png", &distance_scene(400, (200, 200)));
    write_png(dir.path(), "b.png", &distance_scene(400, (280, 280)));
    // 縦横比 1.12/0.68 ≈ 1.65。T=0.85 では f_i ≈ 1.40 になる
    write_png(dir.path(), "wide.png", &distance_scene(400, (400, 160)));

    // **T は書いて固定する。** 中央値のままだと「横長を外した spec」で
    // T が動いてしまい、残りの点のバイト一致を比べる意味が無くなる
    let with_wide = write_spec(
        dir.path(),
        r#"{"set":{"align":"height","fill_ratio":0.85},
             "defaults":{"canvas":"500x500","format":"png"},
             "items":[{"input":"a.png","output":"all/a.png"},
                      {"input":"wide.png","output":"all/wide.png"},
                      {"input":"b.png","output":"all/b.png"}]}"#,
    );
    let out = run_batch(&with_wide, &[]);
    assert_eq!(
        out.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let v = json_stdout(&out);

    let clamped = &v["results"][1]["result"]["warnings"];
    let warning = clamped
        .as_array()
        .unwrap()
        .iter()
        .find(|w| w["code"] == "SET_SCALE_CLAMPED")
        .unwrap_or_else(|| panic!("横長で止まっていない: {clamped}"));
    assert_eq!(warning["data"]["used_fill_ratio"], 1.0);
    assert!(
        warning["data"]["requested_fill_ratio"].as_f64().unwrap() > 1.0,
        "{warning}"
    );
    assert!(
        warning["data"]["height_shortfall"].as_i64().unwrap() > 0,
        "目標との差が出ていない: {warning}"
    );
    assert_eq!(v["set"]["clamped"], 1, "{v}");

    // 他の 2 点には 1 件も出ない
    for i in [0, 2] {
        assert!(
            !has_warning(&v["results"][i]["result"], "SET_SCALE_CLAMPED"),
            "{i} 件目に波及した: {v}"
        );
    }

    // 横長を外した実行と、残りの点の出力がバイト一致する
    let without = dir.path().join("without.json");
    std::fs::write(
        &without,
        r#"{"set":{"align":"height","fill_ratio":0.85},
             "defaults":{"canvas":"500x500","format":"png"},
             "items":[{"input":"a.png","output":"rest/a.png"},
                      {"input":"b.png","output":"rest/b.png"}]}"#,
    )
    .unwrap();
    assert_eq!(run_batch(&without, &[]).status.code(), Some(0));
    for name in ["a.png", "b.png"] {
        assert_eq!(
            std::fs::read(dir.path().join("all").join(name)).unwrap(),
            std::fs::read(dir.path().join("rest").join(name)).unwrap(),
            "{name} が横長の有無で変わった"
        );
    }
}

/// 受け入れ基準 (f) — 1 点も測れなければ `set` は効かず `SET_NOT_MEASURED` を
/// 実行全体の警告として出す。
///
/// 各項目は自分で解決した `fill_ratio` をそのまま使う。**本当のエラーは
/// pass 2 が報告する**——pass 1 は測れなかったことしか知らないので、
/// 読めない理由をここで名乗ると同じ失敗が 2 通りの綴りで返る。
#[test]
fn a_set_that_measures_nothing_says_so_and_stands_aside() {
    let dir = fixture_dir();
    let spec = write_spec(
        dir.path(),
        r#"{"set":{"align":"height"},
             "defaults":{"canvas":"500x500","format":"png"},
             "items":[{"input":"missing.png","output":"out/a.png"}]}"#,
    );
    let out = run_batch(&spec, &[]);
    assert_eq!(out.status.code(), Some(4), "pass 2 の失敗で 4 になるはず");
    let v = json_stdout(&out);
    assert!(
        has_warning(&v, "SET_NOT_MEASURED"),
        "実行全体の警告に出ていない: {v}"
    );
    assert!(
        v.get("set").is_none(),
        "効いていないのにブロックがある: {v}"
    );
    assert_eq!(v["results"][0]["status"], "error");
}

/// 受け入れ基準 (g) — 同じ spec を 3 回走らせて、T も全項目の出力バイト列も
/// 同じである。
#[test]
fn the_same_set_spec_is_deterministic() {
    let dir = fixture_dir();
    for (name, product) in [
        ("a.png", (140, 200)),
        ("b.png", (220, 220)),
        ("c.png", (300, 180)),
    ] {
        write_png(dir.path(), name, &distance_scene(400, product));
    }
    let mut seen: Option<(f64, Vec<Vec<u8>>)> = None;
    for round in 0..3 {
        let spec = write_spec(
            dir.path(),
            &format!(
                r#"{{"set":{{"align":"height"}},
                     "defaults":{{"canvas":"500x500","format":"png"}},
                     "items":[{{"input":"a.png","output":"r{round}/a.png"}},
                              {{"input":"b.png","output":"r{round}/b.png"}},
                              {{"input":"c.png","output":"r{round}/c.png"}}]}}"#
            ),
        );
        assert_eq!(run_batch(&spec, &[]).status.code(), Some(0));
        let v = json_stdout(&run_batch(&spec, &["--force"]));
        let target = v["set"]["fill_ratio"].as_f64().unwrap();
        let bytes: Vec<Vec<u8>> = ["a.png", "b.png", "c.png"]
            .iter()
            .map(|n| std::fs::read(dir.path().join(format!("r{round}")).join(n)).unwrap())
            .collect();
        match &seen {
            None => seen = Some((target, bytes)),
            Some((first, first_bytes)) => {
                assert_eq!(*first, target, "{round} 回目で T が動いた");
                assert_eq!(*first_bytes, bytes, "{round} 回目で出力が動いた");
            }
        }
    }
}

/// 受け入れ基準 (i) — `--jobs` を変えても T と全項目の出力は 1 ビットも
/// 動かない。
///
/// pass 1 は並べて回してよいが、**集計は順序に依らない**（並べ替えてから
/// 中央を採る）。ここが崩れると、同じ spec が機械の負荷で違う仕上がりを出す。
#[test]
fn the_number_of_jobs_does_not_move_the_set_target() {
    let dir = fixture_dir();
    for (name, product) in [
        ("a.png", (120, 180)),
        ("b.png", (200, 200)),
        ("c.png", (260, 200)),
        ("d.png", (320, 240)),
    ] {
        write_png(dir.path(), name, &distance_scene(400, product));
    }
    let mut seen: Option<(f64, Vec<Vec<u8>>)> = None;
    for jobs in ["1", "4"] {
        let spec = write_spec(
            dir.path(),
            &format!(
                r#"{{"set":{{"align":"height"}},
                     "defaults":{{"canvas":"500x500","format":"png"}},
                     "items":[{{"input":"a.png","output":"j{jobs}/a.png"}},
                              {{"input":"b.png","output":"j{jobs}/b.png"}},
                              {{"input":"c.png","output":"j{jobs}/c.png"}},
                              {{"input":"d.png","output":"j{jobs}/d.png"}}]}}"#
            ),
        );
        let out = run_batch(&spec, &["--jobs", jobs]);
        assert_eq!(out.status.code(), Some(0));
        let v = json_stdout(&out);
        let target = v["set"]["fill_ratio"].as_f64().unwrap();
        let bytes: Vec<Vec<u8>> = ["a.png", "b.png", "c.png", "d.png"]
            .iter()
            .map(|n| std::fs::read(dir.path().join(format!("j{jobs}")).join(n)).unwrap())
            .collect();
        match &seen {
            None => seen = Some((target, bytes)),
            Some((first, first_bytes)) => {
                assert_eq!(*first, target, "--jobs {jobs} で T が動いた");
                assert_eq!(*first_bytes, bytes, "--jobs {jobs} で出力が動いた");
            }
        }
    }
}

/// 受け入れ基準 (h) — `set` と噛み合わない spec は断る。
///
/// **「指定したのに効かない」を作らない**のが要点である。`fill_ratio` を
/// 書いた人はその値を望んでいるので、`set` が上から別の値を配るなら
/// spec を読んだ時点で断る。`canvas` の無い項目も同じで、`set` は
/// キャンバス上の占有率の話なので canvas 無しでは意味を持たない。
#[test]
fn a_set_that_cannot_take_effect_is_refused() {
    let dir = fixture_dir();
    write_png(dir.path(), "p.png", &distance_scene(400, (200, 200)));

    let refuse = |json: &str| -> (Option<i32>, Value) {
        let spec = write_spec(dir.path(), json);
        let out = run_batch(&spec, &[]);
        (out.status.code(), json_stdout(&out))
    };

    // defaults に fill_ratio がある
    let (code, v) = refuse(
        r#"{"set":{"align":"height"},
             "defaults":{"canvas":"500x500","fill_ratio":0.8},
             "items":[{"input":"p.png","output":"out/a.png"}]}"#,
    );
    assert_eq!(code, Some(2), "{v}");
    assert_eq!(v["error"]["code"], "INVALID_SET", "{v}");

    // 項目に fill_ratio がある
    let (code, v) = refuse(
        r#"{"set":{"align":"height"},
             "defaults":{"canvas":"500x500"},
             "items":[{"input":"p.png","output":"out/a.png","fill_ratio":0.8}]}"#,
    );
    assert_eq!(code, Some(2), "{v}");
    assert_eq!(v["error"]["code"], "INVALID_SET", "{v}");

    // align の綴り違い
    let (code, v) = refuse(
        r#"{"set":{"align":"heigth"},
             "defaults":{"canvas":"500x500"},
             "items":[{"input":"p.png","output":"out/a.png"}]}"#,
    );
    assert_eq!(code, Some(2), "{v}");
    assert_eq!(v["error"]["code"], "INVALID_SET", "{v}");

    // set の中の未知のキーは既存の綴り検査が断る
    let (code, v) = refuse(
        r#"{"set":{"align":"height","fill_ration":0.8},
             "defaults":{"canvas":"500x500"},
             "items":[{"input":"p.png","output":"out/a.png"}]}"#,
    );
    // 綴り違いは仕様ファイルの異常なので exit 3（`SPEC_UNKNOWN_FIELD` は
    // `ErrorKind::Input`）。`set` の中でも他の階層と同じ 1 本を通る
    assert_eq!(code, Some(3), "{v}");
    assert_eq!(v["error"]["code"], "SPEC_UNKNOWN_FIELD", "{v}");
    assert!(
        v["error"]["hint"].as_str().unwrap().contains("fill_ratio"),
        "{v}"
    );

    // canvas がどこからも取れない項目。**その項目だけが落ちる**（batch は
    // 1 件の失敗で全体を止めない規約なので、実行全体は 4 で返る）
    let (code, v) = refuse(
        r#"{"set":{"align":"height"},
             "items":[{"input":"p.png","output":"out/a.png"}]}"#,
    );
    assert_eq!(code, Some(4), "{v}");
    assert_eq!(v["results"][0]["error"]["code"], "INVALID_SET", "{v}");
}

/// profile の `fill_ratio` は `set` が上書きし、`PROFILE_OVERRIDDEN` で報せる。
///
/// 優先順位は **明示指定 > set > profile > 既定**。上書きを黙ると、
/// 「規格の占有率で書いたはず」のセットが別の値で揃ったまま納品される。
#[test]
fn a_set_overrides_the_profile_fill_ratio_and_says_so() {
    let dir = fixture_dir();
    write_png(dir.path(), "p.png", &distance_scene(400, (200, 200)));
    let spec = write_spec(
        dir.path(),
        r#"{"set":{"align":"bbox","fill_ratio":0.6},
             "defaults":{"profile":"amazon"},
             "items":[{"input":"p.png","output":"out/a.jpg"}]}"#,
    );
    let out = run_batch(&spec, &[]);
    assert_eq!(
        out.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let v = json_stdout(&out);
    let warnings = &v["results"][0]["result"]["warnings"];
    let w = warnings
        .as_array()
        .unwrap()
        .iter()
        .find(|w| w["code"] == "PROFILE_OVERRIDDEN" && w["data"]["key"] == "fill_ratio")
        .unwrap_or_else(|| panic!("上書きを報せていない: {warnings}"));
    assert_eq!(w["data"]["by"], "set", "{w}");
    assert_eq!(w["data"]["used"], 0.6, "{w}");
    // **効いた占有率は canvas ブロックが言う。** `align: "bbox"` なので T そのもの
    assert_eq!(
        v["results"][0]["result"]["canvas"]["fill_ratio"], 0.6,
        "{v}"
    );
}
