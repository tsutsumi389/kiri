//! CLI の統合テスト。
//!
//! AI エージェントから使われる前提のため、「stdout が常に valid JSON であること」と
//! 「exit code が仕様どおりであること」を最重要の検証項目とする。

mod common;

use std::path::{Path, PathBuf};

use assert_cmd::Command;
use common::{ProductSpec, product_image, transparent_product, write_jpeg, write_png};
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
    assert!(warnings[0].as_str().unwrap().contains("均一度"));
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
    let warnings = v["warnings"].as_array().unwrap();
    assert!(
        warnings
            .iter()
            .any(|w| w.as_str().unwrap().contains("透過を保持できない")),
        "透過が失われる旨の警告がない: {warnings:?}"
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
    let warnings = v["warnings"].as_array().unwrap();
    assert!(
        warnings
            .iter()
            .any(|w| w.as_str().unwrap().contains("拡大しました")),
        "拡大した旨の警告がない: {warnings:?}"
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
    assert_eq!(v["tolerance"], 9.0);
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

/// 設計の核心。淡い商品が背景ごと消えないのはエッジ堤防が効いているため。
#[test]
fn the_edge_dam_saves_a_light_product_on_a_light_background() {
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
        "堤防が効いていれば商品全体が残るはず: {with_dam}"
    );

    let without_dam = ratio(&["--edge-threshold", "0"]);
    assert!(
        without_dam < with_dam / 2.0,
        "堤防を切っても結果が変わらない（堤防が効いていない）: {without_dam} vs {with_dam}"
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
    let warnings = v["warnings"].as_array().unwrap();
    assert!(
        warnings.iter().any(|w| w
            .as_str()
            .unwrap()
            .contains("背景がほとんど除去されていません")),
        "失敗が検出できていない: {warnings:?}"
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
    let warnings = v["warnings"].as_array().unwrap();
    assert!(
        warnings
            .iter()
            .any(|w| w.as_str().unwrap().contains("見切れ"))
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
        v["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .any(|w| w.as_str().unwrap().contains("透過を保持できない"))
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
        v["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .any(|w| w.as_str().unwrap().contains("拡大して配置")),
        "拡大の警告がない"
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
        json["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .any(|w| w.as_str().unwrap().contains("プレビューを")),
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
    assert!(
        legacy > refined + 0.05,
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
    assert_eq!(
        args.edge_threshold, defaults.edge_threshold,
        "--edge-threshold の既定値"
    );
    assert_eq!(!args.no_despill, defaults.despill, "デスピルの既定");
    assert_eq!(!args.no_refine, defaults.refine, "アルファ再推定の既定");
}
