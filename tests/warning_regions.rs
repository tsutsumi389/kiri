//! 警告の `data.regions` と、`outputs[]` の写像（`source_rect` / `content_rect`）の
//! 統合テスト。
//!
//! 見たいのは 3 つ。**区間が既知の欠陥を指すこと**、**指した区間をそのまま
//! 次の指示（`--bbox --normalized`）へ渡せること**、**警告の出ない実行には
//! 何も足されないこと**である。

mod common;

use common::{ProductSpec, json_stdout, kiri, product_image, write_png};
use image::{Rgba, RgbaImage};
use serde_json::Value;
use tempfile::TempDir;

/// 右辺だけがギザギザの暗い矩形（白地）。左・上・下の辺はまっすぐである。
///
/// 色そのものがギザギザなので、切り抜きは正しく色に沿い、その結果として
/// 輪郭が蛇行する——`CONTOUR_ROUGH` が出るのは右辺だけでなければならない。
fn jagged_right_edge() -> RgbaImage {
    let (w, h) = (800u32, 600u32);
    RgbaImage::from_fn(w, h, |x, y| {
        let tooth: i32 = if (y / 4) % 2 == 0 { 4 } else { -4 };
        let right = (600 + tooth) as u32;
        if (150..450).contains(&y) && (200..right).contains(&x) {
            Rgba([40, 40, 45, 255])
        } else {
            Rgba([250, 250, 249, 255])
        }
    })
}

fn cutout(input: &std::path::Path, dir: &TempDir, extra: &[&str]) -> Value {
    let out = dir.path().join("out.png");
    let mut args = vec![
        "cutout",
        input.to_str().unwrap(),
        "-o",
        out.to_str().unwrap(),
        "--dry-run",
        "--json",
    ];
    args.extend_from_slice(extra);
    let run = kiri().args(&args).output().unwrap();
    assert!(
        run.status.success(),
        "{args:?}: {}",
        String::from_utf8_lossy(&run.stderr)
    );
    json_stdout(&run)
}

fn warning<'a>(v: &'a Value, code: &str) -> Option<&'a Value> {
    v["warnings"]
        .as_array()
        .unwrap()
        .iter()
        .find(|w| w["code"] == code)
}

fn rect(v: &Value) -> [u64; 4] {
    let a = v.as_array().unwrap();
    [0, 1, 2, 3].map(|i| a[i].as_u64().unwrap())
}

#[test]
fn contour_rough_points_at_the_jagged_edge() {
    let dir = TempDir::new().unwrap();
    let input = write_png(dir.path(), "jagged.png", &jagged_right_edge());
    let v = cutout(&input, &dir, &[]);
    let w = warning(&v, "CONTOUR_ROUGH").expect("右辺の蛇行で CONTOUR_ROUGH が出るはず");
    let regions = w["data"]["regions"].as_array().unwrap();
    assert!(!regions.is_empty(), "{w}");
    assert!(regions.len() <= 5, "{w}");
    let mut last = f64::INFINITY;
    for r in regions {
        let [x1, y1, x2, y2] = rect(&r["bbox"]);
        // 右辺（x ≈ 596〜604）を含み、左辺（x = 200）には届かない
        assert!(x1 <= 604 && x2 >= 596, "右辺を指していない: {r}");
        assert!(x1 > 400, "左へ広がりすぎている: {r}");
        assert!(y1 >= 140 && y2 <= 460, "辺の外を指している: {r}");
        let score = r["score"].as_f64().unwrap();
        assert!(score > 0.16, "しきい値を越えない区間を名指した: {r}");
        assert!(score <= last, "悪い順に並んでいない: {w}");
        last = score;
    }
}

/// `normalized_bbox` は `--bbox <値> --normalized` へそのまま渡せる。
///
/// 戻ってきた `applied_bbox` は元の矩形を**必ず含み**、始点は 1px、終点は
/// 2px までしか外へ広がらない。始点は切り捨て、終点は切り上げで丸めてあり、
/// 終点は `subject.normalized_bbox` と同じく画素の右端（`(x2 + 1) / 幅`）で
/// 言うので、`--bbox` が終点を両端を含む添字へ切り上げるぶん 1px 足される。
#[test]
fn a_region_can_be_handed_back_as_a_normalized_bbox() {
    let dir = TempDir::new().unwrap();
    let input = write_png(dir.path(), "jagged.png", &jagged_right_edge());
    let v = cutout(&input, &dir, &[]);
    let w = warning(&v, "CONTOUR_ROUGH").unwrap();
    for r in w["data"]["regions"].as_array().unwrap() {
        let n: Vec<String> = r["normalized_bbox"]
            .as_array()
            .unwrap()
            .iter()
            .map(|x| x.to_string())
            .collect();
        let again = cutout(&input, &dir, &["--bbox", &n.join(","), "--normalized"]);
        let [x1, y1, x2, y2] = rect(&r["bbox"]);
        let [a1, b1, a2, b2] = rect(&again["applied_bbox"]);
        assert!(
            a1 <= x1 && b1 <= y1 && a2 >= x2 && b2 >= y2,
            "{r} → {again}"
        );
        assert!(
            x1 - a1 <= 1 && y1 - b1 <= 1 && a2 - x2 <= 2 && b2 - y2 <= 2,
            "{r} → {}",
            again["applied_bbox"]
        );
    }
}

/// 警告の出ない実行には `regions` がどこにも現れない。
#[test]
fn a_clean_cut_has_no_regions_anywhere() {
    let dir = TempDir::new().unwrap();
    let img = product_image(&ProductSpec {
        noise: false,
        ..Default::default()
    });
    let input = write_png(dir.path(), "clean.png", &img);
    let v = cutout(&input, &dir, &[]);
    for code in ["HALO_REMAINS", "CONTOUR_ROUGH", "RIM_CONTAMINATED"] {
        assert!(warning(&v, code).is_none(), "前提が崩れている: {v}");
    }
    let text = v.to_string();
    assert!(!text.contains("\"regions\""), "{text}");
}

/// キャンバスが無ければ、入力の全面がそのまま出力の全面へ写る。
#[test]
fn without_a_canvas_the_whole_input_maps_onto_the_whole_output() {
    let dir = TempDir::new().unwrap();
    let input = write_png(dir.path(), "jagged.png", &jagged_right_edge());
    let v = cutout(&input, &dir, &[]);
    let o = &v["outputs"][0];
    assert_eq!(rect(&o["source_rect"]), [0, 0, 799, 599]);
    assert_eq!(rect(&o["content_rect"]), [0, 0, 799, 599]);
}

/// キャンバスがあれば、切り詰めた範囲が `canvas.offset` から `canvas.content` の
/// 寸法で置かれる。派生の縮小はその上に掛かる。
#[test]
fn a_canvas_and_a_derivation_are_both_in_the_mapping() {
    let dir = TempDir::new().unwrap();
    let input = write_png(dir.path(), "jagged.png", &jagged_right_edge());
    let v = cutout(
        &input,
        &dir,
        &[
            "--canvas",
            "1000x1000",
            "--derive",
            "width=1000",
            "--derive",
            "width=500",
            "--naming",
            "{stem}-{index}.{ext}",
        ],
    );
    let canvas = &v["canvas"];
    let (ox, oy) = (
        canvas["offset"][0].as_u64().unwrap(),
        canvas["offset"][1].as_u64().unwrap(),
    );
    let (cw, ch) = (
        canvas["content"][0].as_u64().unwrap(),
        canvas["content"][1].as_u64().unwrap(),
    );
    let full = &v["outputs"][0];
    assert_eq!(
        rect(&full["content_rect"]),
        [ox, oy, ox + cw - 1, oy + ch - 1]
    );
    // 切り詰めた範囲は商品（x 200〜603、y 150〜449）を含む
    let [sx1, sy1, sx2, sy2] = rect(&full["source_rect"]);
    assert!(
        sx1 <= 200 && sy1 <= 150 && sx2 >= 600 && sy2 >= 449,
        "{full}"
    );
    // 半分の派生は、同じ入力の矩形を半分の位置へ写す
    let half = &v["outputs"][1];
    assert_eq!(rect(&half["source_rect"]), rect(&full["source_rect"]));
    let [hx1, hy1, hx2, hy2] = rect(&half["content_rect"]);
    assert_eq!((hx1, hy1), (ox / 2, oy / 2));
    assert!(hx2.abs_diff((ox + cw) / 2 - 1) <= 1 && hy2.abs_diff((oy + ch) / 2 - 1) <= 1);
}

/// 回した実行は矩形を矩形へ写さないので、写像を出さない。
#[test]
fn a_rotated_output_has_no_mapping() {
    let dir = TempDir::new().unwrap();
    let input = write_png(dir.path(), "jagged.png", &jagged_right_edge());
    for angle in ["90", "7"] {
        let v = cutout(&input, &dir, &["--rotate", angle]);
        let o = v["outputs"][0].as_object().unwrap();
        assert!(!o.contains_key("source_rect"), "{angle}: {o:?}");
        assert!(!o.contains_key("content_rect"), "{angle}: {o:?}");
        assert!(
            v.get("rotate").is_some(),
            "{angle}: 回した事実は rotate が言う"
        );
    }
}
