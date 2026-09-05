//! CLI の統合テスト。
//!
//! AI エージェントから使われる前提のため、「stdout が常に valid JSON であること」と
//! 「exit code が仕様どおりであること」を最重要の検証項目とする。

mod common;

use std::path::Path;

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
