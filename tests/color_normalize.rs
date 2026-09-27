//! 色の正規化（`--white-balance` / `--exposure`）の回帰テスト。
//!
//! **最上位の受け入れ条件は「既定 off で 1 バイトも変わらない」である。**
//! `off_does_not_change_a_single_byte` がそれを出力画像と結果 JSON の両方で
//! 見ている。残りは「戻せること」「戻せないものを黙って壊さないこと」を
//! 既知のゲインと既知の露出ずれで固定する。

mod common;

use std::path::{Path, PathBuf};
use std::process::Command;

use common::{ProductSpec, product_image, write_png};
use image::RgbaImage;
use kiri::color::lab::{linear_to_srgb_u8, srgb_linear_lut, srgb_to_lab};
use kiri::color::normalize::{NormalizeMode, normalise};
use kiri::cutout::background::DEFAULT_BORDER;
use kiri::cutout::{BackgroundModel, analyse_background};
use serde_json::Value;
use tempfile::TempDir;

fn kiri() -> Command {
    Command::new(env!("CARGO_BIN_EXE_kiri"))
}

fn fixture_dir() -> TempDir {
    TempDir::new().unwrap()
}

/// `cutout` を回して (出力の生バイト, 結果 JSON) を返す。
fn cut(dir: &Path, input: &Path, name: &str, extra: &[&str]) -> (Vec<u8>, Value) {
    let out = dir.join(name);
    let mut cmd = kiri();
    cmd.args([
        "cutout",
        input.to_str().unwrap(),
        "-o",
        out.to_str().unwrap(),
    ])
    .args(["--json"])
    .args(extra);
    let done = cmd.output().unwrap();
    assert!(
        done.status.success(),
        "{extra:?}: {}",
        String::from_utf8_lossy(&done.stderr)
    );
    let json: Value = serde_json::from_slice(&done.stdout).unwrap_or_else(|e| {
        panic!(
            "JSON として読めない: {e}\n{}",
            String::from_utf8_lossy(&done.stdout)
        )
    });
    (std::fs::read(&out).unwrap(), json)
}

/// 実行ごとに必ず動く項目を落とした結果 JSON。
///
/// `elapsed_ms` は時計、`outputs[].path` と `input` は一時ディレクトリの名前で、
/// どちらも「正規化を渡したかどうか」とは無関係に変わる。**それ以外は 1 バイトも
/// 変わらないことを見る**ので、落とす項目はこの 3 つに限る。
fn comparable(v: &Value) -> Value {
    let mut v = v.clone();
    let obj = v.as_object_mut().unwrap();
    obj.remove("elapsed_ms");
    obj.remove("input");
    if let Some(outputs) = obj.get_mut("outputs").and_then(|o| o.as_array_mut()) {
        for o in outputs {
            o.as_object_mut().unwrap().remove("path");
        }
    }
    v
}

/// 線形 RGB でチャンネルごとのゲインを掛けた画像。
///
/// **既知のゲインを掛けてから正規化で戻す**という筋書きを作るための細工である。
/// 掛ける向きを `color::normalize` と同じ（線形で掛けて sRGB へ戻す）に
/// しておかないと、戻らないのが実装のせいなのか細工のせいなのか分からない。
fn with_gain(img: &RgbaImage, gain: [f64; 3]) -> RgbaImage {
    let lut = srgb_linear_lut();
    let mut out = img.clone();
    for p in out.pixels_mut() {
        for c in 0..3 {
            p[c] = linear_to_srgb_u8(lut[p[c] as usize] * gain[c] as f32);
        }
    }
    out
}

/// 中性背景の商品画像。**既存の S / R シーンには手を触れない。**
///
/// 背景を 248 ではなく 226 に置くのは、既知のゲイン（1.10 倍）を掛けても
/// 255 に張り付かないようにするためである。張り付けば情報が失われ、
/// 「戻せるか」を測れなくなる。
fn neutral_scene() -> RgbaImage {
    product_image(&ProductSpec {
        width: 400,
        height: 400,
        background: [226, 226, 226],
        product: [150, 90, 70],
        ..Default::default()
    })
}

/// 彩度の高い背景の商品画像。
fn coloured_scene(background: [u8; 3]) -> RgbaImage {
    product_image(&ProductSpec {
        width: 400,
        height: 400,
        background,
        product: [230, 225, 220],
        ..Default::default()
    })
}

/// 一様な背景（ノイズなし）に暗い商品を置いたシーン。
///
/// **白飛びの門が閉じない素材を作るためにある。** 背景にノイズも織り目も無く、
/// 商品が暗いので、背景を狙いまで持ち上げても 255 を越える画素が出ない。
/// `EXPOSURE_MIN_L` を置かなければ、暗いグレーがそのまま白へ運ばれる。
fn uniform_scene(background: [u8; 3]) -> RgbaImage {
    product_image(&ProductSpec {
        width: 400,
        height: 400,
        background,
        product: [30, 30, 32],
        noise: false,
        ..Default::default()
    })
}

/// 背景だけ暗く、商品は明るいシーン。露出の下限の門を踏ませる。
fn dim_scene(background: [u8; 3]) -> RgbaImage {
    product_image(&ProductSpec {
        width: 400,
        height: 400,
        background,
        product: [240, 236, 230],
        ..Default::default()
    })
}

/// 代表画素（背景の隅と商品の中心）の sRGB。
fn probes(img: &RgbaImage) -> [[u8; 3]; 2] {
    let (w, h) = img.dimensions();
    let bg = img.get_pixel(4, 4);
    let fg = img.get_pixel(w / 2, h / 2);
    [[bg[0], bg[1], bg[2]], [fg[0], fg[1], fg[2]]]
}

fn delta_e(a: [u8; 3], b: [u8; 3]) -> f64 {
    kiri::color::lab::delta_e_rgb(a, b)
}

/// 出力 PNG を読み直す。
fn read_png(bytes: &[u8]) -> RgbaImage {
    image::load_from_memory(bytes).unwrap().to_rgba8()
}

// ---------------------------------------------------------------------------
// (a) 既定 off で 1 バイトも変わらない
// ---------------------------------------------------------------------------

/// **Phase 24 の最上位の受け入れ条件。**
///
/// フラグを渡さない実行と、`off` を明示した 3 通りが、出力画像も結果 JSON も
/// バイト一致する。`--shadow` / `--segment` と同じ規約で、渡さない実行の
/// 成果物と報告は Phase 24 を足す前のものと 1 バイトも変わらない。
#[test]
fn off_does_not_change_a_single_byte() {
    let dir = fixture_dir();
    let input = write_png(dir.path(), "scene.png", &neutral_scene());
    let (base_png, base_json) = cut(dir.path(), &input, "base.png", &[]);

    for (name, extra) in [
        ("both", vec!["--white-balance", "off", "--exposure", "off"]),
        ("wb", vec!["--white-balance", "off"]),
        ("ex", vec!["--exposure", "off"]),
    ] {
        let (png, json) = cut(dir.path(), &input, &format!("{name}.png"), &extra);
        assert_eq!(png, base_png, "{name}: 出力画像が変わった");
        assert_eq!(
            comparable(&json),
            comparable(&base_json),
            "{name}: 結果 JSON が変わった"
        );
        assert!(
            json.get("color").is_none(),
            "{name}: off なのに color ブロックが出ている"
        );
    }
}

// ---------------------------------------------------------------------------
// (b) 既知の色かぶりを戻せる
// ---------------------------------------------------------------------------

/// 既知のゲインを掛けた画像へ `--white-balance auto` を当てると元へ戻る。
///
/// しきい値 ΔE 1.5 は**丸めの往復ぶんの余裕**である。細工（線形で掛けて 8bit へ
/// 丸める）と正規化（線形で割って 8bit へ丸める）を通すと、往復で 1 段階の
/// 量子化誤差が 2 回乗る。実測は背景 0.3 / 商品 0.5 前後で、1.5 はその 3 倍。
/// **0.0 を要求してはならない**——8bit の往復で情報が落ちるのは実装の欠陥では
/// ないし、要求すれば「戻せている」ことを確かめられる筋書きが 1 つも無くなる。
#[test]
fn a_known_colour_cast_is_undone() {
    let dir = fixture_dir();
    let clean = neutral_scene();
    let cast = with_gain(&clean, [1.10, 1.00, 0.88]);
    let clean_path = write_png(dir.path(), "clean.png", &clean);
    let cast_path = write_png(dir.path(), "cast.png", &cast);

    let (want, _) = cut(dir.path(), &clean_path, "want.png", &[]);
    let (got, json) = cut(
        dir.path(),
        &cast_path,
        "got.png",
        &["--white-balance", "auto"],
    );
    assert_eq!(json["color"]["status"], "applied", "{}", json["color"]);
    assert_eq!(json["color"]["exposure"], "off");
    assert_eq!(json["color"]["exposure_stops"], 0.0);

    let want = probes(&read_png(&want));
    let got = probes(&read_png(&got));
    for (i, label) in ["背景", "商品"].iter().enumerate() {
        let d = delta_e(want[i], got[i]);
        assert!(
            d <= 1.5,
            "{label}の色が戻っていない: ΔE {d:.2}（{:?} を狙って {:?}）",
            want[i],
            got[i]
        );
    }
}

// ---------------------------------------------------------------------------
// (c) 既知の露出ずれを戻せる
// ---------------------------------------------------------------------------

/// 狙いの明度に置いたシーンへ既知の k を掛けてから `--exposure auto` で戻す。
#[test]
fn a_known_exposure_offset_is_undone() {
    let dir = fixture_dir();
    // 背景を狙い（L* 96 = sRGB 243 前後）に置く。ノイズを切るのは、
    // 「戻した後にどれだけ一致するか」をノイズの振幅で薄めないため
    let clean = product_image(&ProductSpec {
        width: 400,
        height: 400,
        background: [243, 243, 243],
        product: [120, 80, 60],
        noise: false,
        ..Default::default()
    });
    let dark = with_gain(&clean, [0.7, 0.7, 0.7]);
    let clean_path = write_png(dir.path(), "clean.png", &clean);
    let dark_path = write_png(dir.path(), "dark.png", &dark);

    let (want, _) = cut(dir.path(), &clean_path, "want.png", &[]);
    let (got, json) = cut(dir.path(), &dark_path, "got.png", &["--exposure", "auto"]);
    assert_eq!(json["color"]["status"], "applied", "{}", json["color"]);
    let stops = json["color"]["exposure_stops"].as_f64().unwrap();
    assert!(
        (stops - 0.7f64.recip().log2()).abs() < 0.05,
        "戻した段数が合わない: {stops}"
    );

    let want = probes(&read_png(&want));
    let got = probes(&read_png(&got));
    for (i, label) in ["背景", "商品"].iter().enumerate() {
        let d = delta_e(want[i], got[i]);
        assert!(
            d <= 1.5,
            "{label}の明るさが戻っていない: ΔE {d:.2}（{:?} を狙って {:?}）",
            want[i],
            got[i]
        );
    }
}

// ---------------------------------------------------------------------------
// (d) 色のある背景は中性化しない
// ---------------------------------------------------------------------------

/// 彩度の高い背景では白点を当てず、出力は off とバイト一致する。
///
/// **これが Phase 24 で最も守るべき「当てない」である。** 当ててしまえば
/// 背景は灰色になり、商品の色は背景の補色へ転ぶ——戻せない破壊になる。
#[test]
fn a_coloured_background_is_not_neutralised() {
    let dir = fixture_dir();
    for (name, bg) in [("blue", [60u8, 110, 200]), ("green", [70, 170, 90])] {
        let input = write_png(dir.path(), &format!("{name}.png"), &coloured_scene(bg));
        let (base, _) = cut(dir.path(), &input, &format!("{name}-base.png"), &[]);
        let (got, json) = cut(
            dir.path(),
            &input,
            &format!("{name}-wb.png"),
            &["--white-balance", "auto"],
        );
        assert_eq!(got, base, "{name}: 色背景なのに画素が動いた");
        assert_eq!(json["color"]["status"], "skipped", "{}", json["color"]);
        assert_eq!(json["color"]["gain"], serde_json::json!([1.0, 1.0, 1.0]));
        let w = warning(&json, "WHITE_BALANCE_SKIPPED");
        assert_eq!(w["data"]["reason"], "not_neutral", "{w}");
        assert!(
            w["data"]["white_point_shift"].as_f64().unwrap() > 20.0,
            "{w}"
        );
    }
}

fn warning<'a>(json: &'a Value, code: &str) -> &'a Value {
    json["warnings"]
        .as_array()
        .unwrap()
        .iter()
        .find(|w| w["code"] == code)
        .unwrap_or_else(|| panic!("{code} が出ていない: {}", json["warnings"]))
}

// ---------------------------------------------------------------------------
// (e) 白飛びする量は当てない
// ---------------------------------------------------------------------------

/// 白飛びする量のゲインは当てずに `would_clip` で断る。
#[test]
fn a_gain_that_would_clip_is_refused() {
    let dir = fixture_dir();
    // 背景は露出の下限（L* 60）を越える明るさに置き、商品はほぼ白にする。
    // 背景を狙いへ持ち上げる倍率では商品が 255 を越える
    let input = write_png(dir.path(), "clip.png", &dim_scene([155, 155, 155]));
    let (base, _) = cut(dir.path(), &input, "clip-base.png", &[]);
    let (got, json) = cut(dir.path(), &input, "clip-ex.png", &["--exposure", "auto"]);
    assert_eq!(got, base, "白飛びする量を当ててしまった");
    assert_eq!(json["color"]["status"], "skipped", "{}", json["color"]);
    assert_eq!(json["color"]["clipped_ratio"], 0.0);
    let w = warning(&json, "EXPOSURE_SKIPPED");
    assert_eq!(w["data"]["reason"], "would_clip", "{w}");
    assert!(w["data"]["clipped_ratio"].as_f64().unwrap() > 0.001, "{w}");
}

// ---------------------------------------------------------------------------
// (f) / (h) 暗い背景に露出を当てない（段ごとに落ちる）
// ---------------------------------------------------------------------------

/// 暗い背景は白へ持ち上げない。**白点は当たってよい。**
#[test]
fn a_dark_background_is_not_lifted_to_white() {
    let dir = fixture_dir();
    // **商品は暗くする。** 白点のゲインは弱いチャンネルを持ち上げるので、
    // 画面に白に近い面があると白点だけでも白飛びの門を踏む（それも正しい
    // 振る舞いで、実測でも clipped_ratio 0.106 で断られた）。ここで見たいのは
    // 「明度の下限で**露出だけが**落ちる」ことなので、白点が通る素材を使う
    let input = write_png(dir.path(), "dim.png", &uniform_scene([104, 100, 92]));
    let (_, json) = cut(
        dir.path(),
        &input,
        "dim-out.png",
        &["--white-balance", "auto", "--exposure", "auto"],
    );
    let w = warning(&json, "EXPOSURE_SKIPPED");
    assert_eq!(w["data"]["reason"], "not_light", "{w}");
    assert!(
        w["data"]["background_l"].as_f64().unwrap() < 60.0,
        "門が見た明度が下限の上にある: {w}"
    );
    assert_eq!(json["color"]["exposure_stops"], 0.0, "{}", json["color"]);
    assert!(
        json["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .all(|w| w["code"] != "WHITE_BALANCE_SKIPPED"),
        "白点まで落ちている: {}",
        json["warnings"]
    );
}

/// (f) と対。露出が落ちても白点のぶんはゲインに残る。
#[test]
fn white_balance_applies_even_when_exposure_is_refused() {
    let dir = fixture_dir();
    // 暗く、かつ暖色へかぶった背景。露出は下限で落ち、白点だけが当たる
    let input = write_png(dir.path(), "dim-warm.png", &uniform_scene([112, 100, 86]));
    let (_, json) = cut(
        dir.path(),
        &input,
        "dim-warm-out.png",
        &["--white-balance", "auto", "--exposure", "auto"],
    );
    assert_eq!(json["color"]["status"], "applied", "{}", json["color"]);
    assert_eq!(
        warning(&json, "EXPOSURE_SKIPPED")["data"]["reason"],
        "not_light"
    );
    let gain: Vec<f64> = json["color"]["gain"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_f64().unwrap())
        .collect();
    assert!(
        gain[0] < 1.0 && gain[2] > 1.0,
        "白点が当たっていない: {gain:?}"
    );
    // 露出が落ちているので、輝度を変えないゲインだけが残っている
    assert_eq!(json["color"]["exposure_stops"], 0.0);
}

// ---------------------------------------------------------------------------
// (g) 決定性
// ---------------------------------------------------------------------------

/// 3 回回して出力バイト列と `color` ブロックが一致する。
#[test]
fn the_same_normalisation_is_deterministic() {
    let dir = fixture_dir();
    let input = write_png(
        dir.path(),
        "det.png",
        &with_gain(&neutral_scene(), [1.08, 1.0, 0.9]),
    );
    let mut first: Option<(Vec<u8>, Value)> = None;
    for i in 0..3 {
        let (png, json) = cut(
            dir.path(),
            &input,
            &format!("det{i}.png"),
            &["--white-balance", "auto", "--exposure", "auto"],
        );
        match &first {
            None => first = Some((png, json["color"].clone())),
            Some((want_png, want_color)) => {
                assert_eq!(&png, want_png, "{i} 回目の出力が違う");
                assert_eq!(&json["color"], want_color, "{i} 回目の color が違う");
            }
        }
    }
}

// ---------------------------------------------------------------------------
// (i) --optimize の前に掛かる
// ---------------------------------------------------------------------------

/// `--white-balance auto --optimize` は「正規化した画素の上で」探索する。
///
/// **2 段に分けた実行と突き合わせる。** 1 段目はライブラリの `normalise` を
/// 直接呼んで正規化だけを済ませた画像を書き出し、2 段目はそれへ `--optimize`
/// だけを当てる。CLI が内部でやっているのと同じ関数・同じ材料
/// （正規化前の画像から測った場）を通すので、**出力はバイト一致しなければ
/// ならない。**
///
/// `cutout --white-balance auto` の出力を 1 段目に使うことはできない——
/// それは切り抜き済みの画像で、2 段目の探索が見る材料が別物になる。
#[test]
fn normalisation_runs_before_the_search() {
    let dir = fixture_dir();
    let cast = with_gain(&neutral_scene(), [1.10, 1.00, 0.88]);
    let cast_path = write_png(dir.path(), "cast.png", &cast);

    let mut pre = cast.clone();
    let analysis = analyse_background(&pre, DEFAULT_BORDER, BackgroundModel::Auto, None, None);
    let n = normalise(
        &mut pre,
        &analysis.field,
        NormalizeMode::Auto,
        NormalizeMode::Off,
    );
    assert_eq!(n.status, "applied", "{n:?}");
    let pre_path = write_png(dir.path(), "pre.png", &pre);

    let (one_pass, one_json) = cut(
        dir.path(),
        &cast_path,
        "one.png",
        &["--white-balance", "auto", "--optimize"],
    );
    let (two_pass, two_json) = cut(dir.path(), &pre_path, "two.png", &["--optimize"]);
    assert_eq!(
        one_pass, two_pass,
        "1 段実行と 2 段実行で出力が違う（探索が正規化前の画素を見ている）"
    );
    // `optimize.elapsed_ms` は時計なので落とす。**候補表そのものは 1 つも
    // 動いてはならない**——動けば探索が別の画素を見ている
    assert_eq!(
        without_elapsed(&one_json["optimize"]),
        without_elapsed(&two_json["optimize"]),
        "候補表が正規化後の数値で並んでいない"
    );
    assert_eq!(one_json["background"], two_json["background"]);
}

// ---------------------------------------------------------------------------
// (j) batch spec
// ---------------------------------------------------------------------------

fn without_elapsed(v: &Value) -> Value {
    let mut v = v.clone();
    v.as_object_mut().unwrap().remove("elapsed_ms");
    v
}

fn write_spec(dir: &Path, body: &str) -> PathBuf {
    let path = dir.join("spec.json");
    std::fs::write(&path, body).unwrap();
    path
}

fn run_batch(spec: &Path, extra: &[&str]) -> Value {
    let out = kiri()
        .args(["batch", spec.to_str().unwrap(), "--json"])
        .args(extra)
        .output()
        .unwrap();
    serde_json::from_slice(&out.stdout).unwrap_or_else(|e| {
        panic!(
            "JSON として読めない: {e}\n{}",
            String::from_utf8_lossy(&out.stdout)
        )
    })
}

/// spec の `white_balance` / `exposure` は CLI と同じ値を読む。
#[test]
fn the_batch_spec_reads_white_balance() {
    let dir = fixture_dir();
    write_png(
        dir.path(),
        "cast.png",
        &with_gain(&neutral_scene(), [1.10, 1.0, 0.88]),
    );
    let spec = write_spec(
        dir.path(),
        r#"{"items":[{"input":"cast.png","output":"out/a.png","white_balance":"auto"},
                     {"input":"cast.png","output":"out/b.png"}]}"#,
    );
    let v = run_batch(&spec, &["--dry-run"]);
    let items = v["results"].as_array().unwrap();
    assert_eq!(items[0]["result"]["color"]["white_balance"], "auto", "{v}");
    assert_eq!(items[0]["result"]["color"]["status"], "applied", "{v}");
    assert!(
        items[1]["result"].get("color").is_none(),
        "書かない項目に color が出ている: {v}"
    );
}

/// `defaults` からの継承が効く。
#[test]
fn the_batch_defaults_carry_the_white_balance() {
    let dir = fixture_dir();
    write_png(
        dir.path(),
        "cast.png",
        &with_gain(&neutral_scene(), [1.10, 1.0, 0.88]),
    );
    let spec = write_spec(
        dir.path(),
        r#"{"defaults":{"white_balance":"auto","exposure":"auto"},
             "items":[{"input":"cast.png","output":"out/a.png"},
                      {"input":"cast.png","output":"out/b.png","white_balance":"off"}]}"#,
    );
    let v = run_batch(&spec, &["--dry-run"]);
    let items = v["results"].as_array().unwrap();
    assert_eq!(items[0]["result"]["color"]["white_balance"], "auto", "{v}");
    // 項目の指定が defaults を押しのける。exposure は継承されたままなので
    // ブロックは出続ける
    assert_eq!(items[1]["result"]["color"]["white_balance"], "off", "{v}");
    assert_eq!(items[1]["result"]["color"]["exposure"], "auto", "{v}");
}

/// 読めない値はその項目を `SPEC_INVALID` で落とす。
#[test]
fn an_unknown_white_balance_in_the_spec_is_refused() {
    let dir = fixture_dir();
    write_png(dir.path(), "scene.png", &neutral_scene());
    let spec = write_spec(
        dir.path(),
        r#"{"items":[{"input":"scene.png","output":"out/a.png","white_balance":"yes"}]}"#,
    );
    let v = run_batch(&spec, &["--dry-run"]);
    assert_eq!(v["failed"], 1, "{v}");
    assert_eq!(v["results"][0]["error"]["code"], "SPEC_INVALID", "{v}");
}

// ---------------------------------------------------------------------------
// (k) CLI のパーサ
// ---------------------------------------------------------------------------

/// 綴りを外した値は clap が exit 2 で断る。
#[test]
fn an_unknown_white_balance_value_is_refused_by_the_parser() {
    let dir = fixture_dir();
    let input = write_png(dir.path(), "scene.png", &neutral_scene());
    for flag in ["--white-balance", "--exposure"] {
        let out = kiri()
            .args([
                "cutout",
                input.to_str().unwrap(),
                "-o",
                dir.path().join("o.png").to_str().unwrap(),
                flag,
                "yes",
            ])
            .output()
            .unwrap();
        assert_eq!(out.status.code(), Some(2), "{flag}: exit 2 で断っていない");
    }
}

// ---------------------------------------------------------------------------
// 較正（**実測して表を残すための入口**。design.md 4.15 の表はここから取った）
// ---------------------------------------------------------------------------

/// 5 つの定数を決めるための素の測定値を並べる。
///
/// `cargo test --release --test color_normalize -- --ignored --nocapture
///  print_the_calibration_table` で回す。実写を混ぜるときは
/// `KIRI_BENCH_DIR` にその画像を置いたディレクトリを指す
/// （`tests/real_backgrounds.rs` と同じ入口を使い、新しい環境変数を増やさない）。
///
/// **表は「どの値で何が裏返るか」を読むためにある。** 判定はしない——
/// 較正の材料に assert を付けると、素材を 1 枚足したときに落ちる検査になる。
#[test]
#[ignore = "較正用。表を読むために手で回す"]
fn print_the_calibration_table() {
    let mut rows: Vec<(String, RgbaImage)> = vec![
        ("合成 中性 226".into(), neutral_scene()),
        (
            "合成 暖色かぶり (1.10,1.00,0.88)".into(),
            with_gain(&neutral_scene(), [1.10, 1.00, 0.88]),
        ),
        (
            "合成 白背景 248".into(),
            product_image(&ProductSpec::default()),
        ),
        (
            "合成 織り目 177,174,168".into(),
            common::woven_background_image(600, 600),
        ),
        ("合成 青背景".into(), coloured_scene([60, 110, 200])),
        ("合成 緑背景".into(), coloured_scene([70, 170, 90])),
        ("合成 暗背景 104,100,92".into(), dim_scene([104, 100, 92])),
        ("合成 中背景 155,155,155".into(), dim_scene([155, 155, 155])),
        // **EXPOSURE_MIN_L だけが断れる帯を示す 2 枚。** どちらも一様
        // （ノイズなし）で暗い商品なので、持ち上げても白飛びしない。つまり
        // 白飛びの門は閉じず、明度の下限を置かなければそのまま白へ運ばれる
        (
            "合成 一様グレー 140 + 黒商品".into(),
            uniform_scene([140, 140, 140]),
        ),
        (
            "合成 一様 190（露出不足の白紙）+ 黒商品".into(),
            uniform_scene([190, 190, 190]),
        ),
    ];
    for scene in common::edge_scenes() {
        rows.push((
            format!("S {}", scene.name),
            common::edge_scene(&scene).image,
        ));
    }
    for scene in common::real_scenes() {
        rows.push((
            format!("R {}", scene.name),
            common::real_scene(&scene).image,
        ));
    }
    if let Ok(dir) = std::env::var(common::KIRI_BENCH_DIR) {
        let mut paths: Vec<PathBuf> = std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| {
                matches!(
                    p.extension().and_then(|e| e.to_str()),
                    Some("jpg" | "jpeg" | "png")
                )
            })
            .collect();
        paths.sort();
        for path in paths {
            rows.push((
                format!("実写 {}", path.file_name().unwrap().to_string_lossy()),
                load_like_the_cli(&path),
            ));
        }
    }

    println!(
        "\n| 素材 | source | white_point | C* | 背景 L* | k(92) | k(96) | k(98) | k(100) | \
         clip(92) | clip(96) | clip(98) | clip(100) |"
    );
    println!("|---|---|---|---|---|---|---|---|---|---|---|---|---|");
    for (name, img) in &rows {
        let analysis = analyse_background(img, DEFAULT_BORDER, BackgroundModel::Auto, None, None);
        let mut probe = img.clone();
        let n = normalise(
            &mut probe,
            &analysis.field,
            NormalizeMode::Auto,
            NormalizeMode::Off,
        );
        let lab = srgb_to_lab(n.white_point);
        let mut ks = Vec::new();
        let mut clips = Vec::new();
        for target in [92.0, 96.0, 98.0, 100.0] {
            let k = target_y(target) / y_of(lab[0]);
            ks.push(format!("{k:.3}"));
            clips.push(format!("{:.5}", clipped(img, [k, k, k])));
        }
        println!(
            "| {name} | {} | {:?} | {:.1} | {:.1} | {} | {} |",
            n.source,
            n.white_point,
            n.white_point_shift,
            lab[0],
            ks.join(" | "),
            clips.join(" | ")
        );
    }
}

/// 24.5MP での正規化の費用を測る。
///
/// **`cutout` 全体の `elapsed_ms` では測れない。** 切り抜きが 8 秒かかるので、
/// その中の 0.1 秒台は実行ごとのばらつきに埋もれる（`transform::shadow` の
/// ぼかしを測ったときと同じ事情。design.md 4.14）。見立てと画素パスを
/// `Instant` で挟んで別々に測る——**費用の大半は見立て（`analyse_background`）
/// のほう**で、画素パスは表を引くだけなので桁が違う。
///
/// `KIRI_BENCH_DIR` に実写を置いて手で回す。
#[test]
#[ignore = "費用の実測。表を読むために手で回す"]
fn print_the_cost_on_a_large_image() {
    let Ok(dir) = std::env::var(common::KIRI_BENCH_DIR) else {
        println!("KIRI_BENCH_DIR が無いので飛ばす");
        return;
    };
    let mut paths: Vec<PathBuf> = std::fs::read_dir(&dir)
        .unwrap()
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| {
            matches!(
                p.extension().and_then(|e| e.to_str()),
                Some("jpg" | "jpeg" | "png")
            )
        })
        .collect();
    paths.sort();
    println!("\n| 素材 | 画素数 | 見立て | 白飛びの数え + 画素パス | 合計 |");
    println!("|---|---|---|---|---|");
    for path in paths {
        let img = load_like_the_cli(&path);
        let mut work = img.clone();
        let t0 = std::time::Instant::now();
        let analysis = analyse_background(&work, DEFAULT_BORDER, BackgroundModel::Auto, None, None);
        let seen = t0.elapsed();
        let t1 = std::time::Instant::now();
        let n = normalise(
            &mut work,
            &analysis.field,
            NormalizeMode::Auto,
            NormalizeMode::Auto,
        );
        let applied = t1.elapsed();
        println!(
            "| {} ({}) | {:.1}MP | {:.0} ms | {:.0} ms | {:.0} ms |",
            path.file_name().unwrap().to_string_lossy(),
            n.status,
            f64::from(img.width()) * f64::from(img.height()) / 1e6,
            seen.as_secs_f64() * 1000.0,
            applied.as_secs_f64() * 1000.0,
            (seen + applied).as_secs_f64() * 1000.0
        );
    }
}

/// `cutout` と同じ経路で実写を読む。
///
/// **`image::open` では駄目である。** iPhone の素材は Display P3 で入ってくる
/// ので、ICC を解釈せずに読むと白点が実際の実行と食い違う（remote.jpg では
/// C* が 4.2 と 4.8 に分かれた）。較正の表が実行と違う色を測っていては、
/// そこから決めた定数に意味が無い。
fn load_like_the_cli(path: &Path) -> RgbaImage {
    kiri::image_io::load::load_with(path, &kiri::image_io::LoadOptions::default())
        .unwrap()
        .image
}

/// L* から線形の輝度へ（`lab.rs` の pivot の逆関数）。
fn target_y(l: f64) -> f64 {
    let fy = (l + 16.0) / 116.0;
    let cubed = fy * fy * fy;
    if cubed > 0.008_856 {
        cubed
    } else {
        (fy - 16.0 / 116.0) / 7.787
    }
}

fn y_of(l: f64) -> f64 {
    target_y(l)
}

/// ゲインを当てたとき**新たに** 255 へ張り付く画素の割合。
fn clipped(img: &RgbaImage, gain: [f64; 3]) -> f64 {
    let lut = srgb_linear_lut();
    let mut count = 0u64;
    for p in img.pixels() {
        let hit = (0..3).any(|c| p[c] < 255 && lut[p[c] as usize] * gain[c] as f32 > 1.0);
        if hit {
            count += 1;
        }
    }
    count as f64 / (u64::from(img.width()) * u64::from(img.height())) as f64
}
