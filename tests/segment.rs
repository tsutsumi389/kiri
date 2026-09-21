//! `--segment` の統合テスト。
//!
//! **モデルのファイルはリポジトリにも CI にも置かない。** 176MB あり、
//! `--segment` を使う利用者だけが取ればよいものである。置いていない機械では、
//! モデルを要する検査だけが黙って飛ぶ（`#[ignore]` を付けて回らない検査に
//! すると、置いてある機械でも走らなくなる）。
//!
//! モデルが要らない検査——既定が `off` であること、断り方、`kiri model list`
//! の契約——は、どの機械でも必ず走る。

mod common;

use std::path::Path;

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

/// モデルのファイルが置いてあるか。**推論できるかは問わない。**
///
/// `kiri model list` の検証は feature を持たない build でも走る（一覧そのものは
/// どの build でも返す約束である）ので、置き場所だけを見る門を別に持つ。
fn model_present() -> bool {
    kiri::segment::model::ISNET
        .expected_path()
        .is_some_and(|p| p.is_file())
}

/// この build が推論でき、かつモデルのファイルが置いてあるか。
fn segment_ready() -> bool {
    cfg!(feature = "segment") && model_present()
}

/// モデルの想定パス。飛ばした理由を言うときに添える。
fn expected_model_path() -> String {
    kiri::segment::model::ISNET
        .expected_path()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|| "(置き場所を決められません)".to_string())
}

/// 飛ばすなら、**どこを見て飛ばしたのかを 1 行だけ言う。**
///
/// 黙って `return` すると、モデルを要する検査が 1 本も走っていない機械でも
/// 出力は「全部緑」と区別が付かない。**「緑だった」と「確かめた」は別**で、
/// その差は飛ばした側にしか書けない。
fn skip_without_model() -> bool {
    if segment_ready() {
        return false;
    }
    if cfg!(feature = "segment") {
        eprintln!("モデルが無いので飛ばす: {}", expected_model_path());
    } else {
        eprintln!(
            "この build に segment 機能が無いので飛ばす: {}",
            expected_model_path()
        );
    }
    true
}

fn fixture() -> (TempDir, std::path::PathBuf) {
    let dir = TempDir::new().unwrap();
    let input = write_png(
        dir.path(),
        "product.png",
        &product_image(&ProductSpec::default()),
    );
    (dir, input)
}

// --- モデルが要らない検査 ---

/// **既定は `off` で、渡しても渡さなくても出力は 1 バイトも変わらない。**
///
/// ここが崩れると、Phase 5 は既存の全利用者に影響する変更になる。
#[test]
fn off_is_the_default_and_produces_identical_bytes() {
    let (dir, input) = fixture();
    let mut bytes = Vec::new();
    for (name, extra) in [("implicit", vec![]), ("explicit", vec!["--segment", "off"])] {
        let out = dir.path().join(format!("{name}.png"));
        let mut args = vec![
            "cutout",
            input.to_str().unwrap(),
            "-o",
            out.to_str().unwrap(),
            "--json",
        ];
        args.extend(extra);
        let result = kiri().args(&args).output().unwrap();
        assert!(
            result.status.success(),
            "{name}: {}",
            String::from_utf8_lossy(&result.stderr)
        );
        let v = json_stdout(&result);
        assert_eq!(v["settings"]["segment"], "off", "{name}");
        assert_eq!(v["settings"]["segment_ran"], false, "{name}");
        assert!(
            v.get("segment").is_none(),
            "{name}: 走っていないのに segment ブロックがある"
        );
        bytes.push(std::fs::read(&out).unwrap());
    }
    assert_eq!(
        bytes[0], bytes[1],
        "--segment off を明示しただけで出力が変わった"
    );
}

/// `settings.segment` は**指定値**、`settings.segment_ran` は**実際**。
///
/// 2 つ持つのは `auto` のためである。片方しか無いと、走らなかった `auto` を
/// エージェントは `off` と区別できない。
///
/// **推論できる build でしか問えない。** そうでない build は `auto` も
/// `SEGMENT_UNAVAILABLE` で断るので（下の検査）、走らなかった理由が
/// 「色で解けたから」ではなくなる。
#[test]
#[cfg(feature = "segment")]
fn the_settings_tell_both_the_request_and_what_happened() {
    let (dir, input) = fixture();
    let out = dir.path().join("a.png");
    let result = kiri()
        .args([
            "cutout",
            input.to_str().unwrap(),
            "-o",
            out.to_str().unwrap(),
            "--segment",
            "auto",
            "--json",
        ])
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let v = json_stdout(&result);
    assert_eq!(v["settings"]["segment"], "auto");
    // 合成の単色背景は色で解ける。**モデルを触ってはいけない**
    // （feature の有無によらず、門は色だけを見て閉じる）
    assert_eq!(
        v["settings"]["segment_ran"], false,
        "単色背景でモデルが走っている: {v}"
    );
}

/// segment 機能の無い build は、黙って `off` に落ちずに断る。
///
/// **黙って落ちるのが最悪である。** エージェントは「モデルを使った結果」だと
/// 思ったまま数値を読み、切り抜きが悪いのはモデルのせいだと結論する。
///
/// **`auto` も断る。** 「要るなら使う」と言われて「使えないので使いませんでした」
/// を成功として返すと、`settings.segment_ran: false` が「色で解けた」と
/// 読めてしまう。走らなかった理由が 2 通りある状態を作らない。
#[test]
#[cfg(not(feature = "segment"))]
fn a_build_without_the_feature_refuses_instead_of_falling_back() {
    let (dir, input) = fixture();
    for mode in ["isnet", "auto"] {
        let out = dir.path().join(format!("{mode}.png"));
        let result = kiri()
            .args([
                "cutout",
                input.to_str().unwrap(),
                "-o",
                out.to_str().unwrap(),
                "--segment",
                mode,
                "--json",
            ])
            .output()
            .unwrap();
        assert_eq!(result.status.code(), Some(2), "{mode}");
        let v = json_stdout(&result);
        assert_eq!(v["error"]["code"], "SEGMENT_UNAVAILABLE", "{mode}");
        assert!(
            v["error"]["hint"]
                .as_str()
                .unwrap_or_default()
                .contains("--features segment"),
            "{mode}: 入れ直し方を言っていない: {v}"
        );
        assert!(!out.exists(), "{mode}: 断ったのに成果物を書いている");
    }
}

/// 置いていないモデルは「見つからない」として断る。
///
/// **「壊れている」と別の code にする。** 前者は取得すれば済み、後者は
/// 取り直す必要がある。同じ code にすると、エージェントは置いてあるはずの
/// パスを疑い始める。
#[test]
#[cfg(feature = "segment")]
fn a_missing_model_says_how_to_get_it() {
    let (dir, input) = fixture();
    let out = dir.path().join("a.png");
    let result = kiri()
        .args([
            "cutout",
            input.to_str().unwrap(),
            "-o",
            out.to_str().unwrap(),
            "--segment",
            "isnet",
            "--model-path",
            dir.path().join("nope.onnx").to_str().unwrap(),
            "--json",
        ])
        .output()
        .unwrap();
    assert_eq!(result.status.code(), Some(3));
    let v = json_stdout(&result);
    assert_eq!(v["error"]["code"], "MODEL_NOT_FOUND");
    assert!(
        v["error"]["hint"]
            .as_str()
            .unwrap_or_default()
            .contains("curl -L"),
        "取得の 1 行が無い: {v}"
    );
}

/// 途中で切れたダウンロードは、推論に入る前に大きさで捕まる。
#[test]
#[cfg(feature = "segment")]
fn a_truncated_model_is_refused_before_inference() {
    let (dir, input) = fixture();
    let broken = dir.path().join("broken.onnx");
    std::fs::write(&broken, b"not an onnx file").unwrap();
    let out = dir.path().join("a.png");
    let result = kiri()
        .args([
            "cutout",
            input.to_str().unwrap(),
            "-o",
            out.to_str().unwrap(),
            "--segment",
            "isnet",
            "--model-path",
            broken.to_str().unwrap(),
            "--json",
        ])
        .output()
        .unwrap();
    assert_eq!(result.status.code(), Some(3));
    let v = json_stdout(&result);
    assert_eq!(v["error"]["code"], "MODEL_UNREADABLE");
}

/// `kiri model list` は取得に要るものを全部返す。
///
/// **kiri はネットワークを触らない**ので、ここが唯一の案内である。
/// URL・ダイジェスト・ライセンス・置き場所が 1 つでも欠けると、利用者は
/// README を読みに行くことになる。
#[test]
fn model_list_publishes_everything_needed_to_fetch_it() {
    let result = kiri().args(["model", "list", "--json"]).output().unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let v = json_stdout(&result);
    assert_eq!(v["schema_version"], 1);
    assert_eq!(v["segment_available"], cfg!(feature = "segment"));

    let isnet = v["models"]
        .as_array()
        .expect("models が配列でない")
        .iter()
        .find(|m| m["name"] == "isnet")
        .unwrap_or_else(|| panic!("isnet が無い: {v}"));

    assert!(isnet["url"].as_str().unwrap().starts_with("https://"));
    assert_eq!(isnet["md5"].as_str().unwrap().len(), 32);
    assert_eq!(isnet["sha256"].as_str().unwrap().len(), 64);
    assert!(isnet["bytes"].as_u64().unwrap() > 100_000_000);
    assert!(isnet["license"].as_str().unwrap().contains("Apache-2.0"));
    assert_eq!(isnet["input_size"], 1024);
    assert!(isnet["hint"].as_str().unwrap().contains("curl -L"));
    assert!(isnet["present"].is_boolean());
    // 置いていなければ検証していない。**false（検証して駄目だった）と
    // 同じ形にしない**
    if isnet["present"] == Value::Bool(false) {
        assert!(isnet["verified"].is_null(), "{isnet}");
    }
}

/// **176MB の検証は終わる。** 上限を置いて、超えたら殺す。
///
/// # これは速さの検査ではない
///
/// 自前の SHA-256（`src/segment/sha256.rs`）は、1 ブロックに満たない切れ端を
/// 食わされたときに戻らない書き方をしていた時期がある。`finish` の詰め物は
/// まさに 1 バイトずつ食わせるので、`kiri model list` がそこで**永久に回り続けた**
/// ——4 つのプロセスが孤児化し、誰かが気づくまで 1 時間半 CPU を 100% 使った。
///
/// 終わらないことは「遅い」ではなく壊れているということなので、`output()` で
/// 待たずに `try_wait` で見張り、上限を超えたら必ず `kill` する。**検査自身が
/// 孤児を作らないこと**が、ここでいちばん大事な性質である。
///
/// 上限は実測（0.9 秒）から大きく離して置く。1 秒を 2 秒にした変更を落とすのは
/// この検査の仕事ではない。
///
/// 推論できない build でも走る——ダイジェストの突き合わせは feature に依らない。
#[test]
fn verifying_a_176mb_model_finishes_instead_of_spinning_forever() {
    use std::time::{Duration, Instant};

    if !model_present() {
        eprintln!("モデルが無いので飛ばす: {}", expected_model_path());
        return;
    }
    const LIMIT: Duration = Duration::from_secs(30);
    let started = Instant::now();
    let mut child = std::process::Command::new(env!("CARGO_BIN_EXE_kiri"))
        .args(["model", "list", "--json"])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    // **どの枝から抜けても子を残さない。** `try_wait` 自身が失敗したときに
    // そのまま panic すると、回り続けているかもしれないプロセスを置き去りに
    // する——この検査が防ごうとしている孤児を、この検査が作ることになる
    fn reap(child: &mut std::process::Child, reason: String) -> ! {
        let _ = child.kill();
        let _ = child.wait();
        panic!("{reason}");
    }
    let status = loop {
        let polled = match child.try_wait() {
            Ok(polled) => polled,
            Err(e) => reap(&mut child, format!("子プロセスを見張れなくなった: {e}")),
        };
        match polled {
            Some(status) => break status,
            None if started.elapsed() > LIMIT => reap(
                &mut child,
                format!(
                    "kiri model list が {} 秒で終わらなかった（殺した）。\
                     176MB のダイジェストは 1 秒ほどで出るはずで、\
                     終わらないのは sha256 が回り続けているということ",
                    LIMIT.as_secs()
                ),
            ),
            None => std::thread::sleep(Duration::from_millis(20)),
        }
    };
    assert!(status.success(), "kiri model list が失敗した: {status}");
}

/// **大きさが違うファイルは舐めない。**
///
/// `present` だけを門にすると、想定パスに置かれた何 GB の別ファイルを
/// `kiri model list` が最後まで読むことになる。大きさが違えば答えは
/// 分かっている（不一致）ので、そこで止めて実際のバイト数を返す——
/// **同じことを、待たせずに言える。**
///
/// 置き場所は子プロセスの環境として渡す。`std::env::set_var` は同じ
/// バイナリの他のテストと並列に走ると未定義動作になるので、
/// **テスト側のプロセスの環境は触らない。**
///
/// 推論できない build でも走る（一覧はどの build でも返す約束である）。
#[test]
fn a_file_of_the_wrong_size_is_refused_without_computing_a_digest() {
    let dir = TempDir::new().unwrap();
    std::fs::write(
        dir.path().join(kiri::segment::model::ISNET.file_name),
        b"not an onnx file",
    )
    .unwrap();
    let v = json_stdout(
        &kiri()
            .args(["model", "list", "--json"])
            .env(kiri::segment::model::KIRI_MODEL_DIR, dir.path())
            .output()
            .unwrap(),
    );
    let isnet = v["models"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["name"] == "isnet")
        .unwrap_or_else(|| panic!("isnet が無い: {v}"));
    assert_eq!(isnet["present"], true, "{isnet}");
    assert_eq!(isnet["verified"], false, "{isnet}");
    assert_eq!(isnet["actual_bytes"], 16, "{isnet}");
    // **ダイジェストを計算していないことが、これで分かる。** 計算していれば
    // 不一致の中身として `actual_sha256` が付く
    assert!(
        isnet["actual_sha256"].is_null(),
        "大きさで弾いたのにダイジェストを計算している: {isnet}"
    );
    assert!(
        isnet["hint"].as_str().unwrap().contains("curl -L"),
        "取り直し方を言っていない: {isnet}"
    );
}

/// 入れ子のサブコマンドも契約に載る。
///
/// **`kiri model` は単体では呼べない。** 葉だけを `"model list"` の形で
/// 並べるので、綴りをそのまま繋げば実行できる。
#[test]
fn the_schema_lists_the_nested_command_as_a_leaf() {
    let v = json_stdout(&kiri().args(["schema", "--json"]).output().unwrap());
    let names: Vec<&str> = v["commands"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["name"].as_str().unwrap())
        .collect();
    assert!(names.contains(&"model list"), "{names:?}");
    assert!(
        !names.contains(&"model"),
        "それ自身では呼べないコマンドを並べている: {names:?}"
    );
}

/// `--segment` の選択肢が契約に出る。綴りを外すと clap が code 無しで落ちる。
#[test]
fn the_schema_lists_the_segment_values() {
    let v = json_stdout(&kiri().args(["schema", "--json"]).output().unwrap());
    for command in ["cutout", "info"] {
        let arg = v["commands"]
            .as_array()
            .unwrap()
            .iter()
            .find(|c| c["name"] == command)
            .unwrap()["options"]
            .as_array()
            .unwrap()
            .iter()
            .find(|o| o["name"] == "--segment")
            .unwrap_or_else(|| panic!("{command} に --segment が無い"))
            .clone();
        assert_eq!(arg["default"], "off", "{command}");
        let accepts: Vec<&str> = arg["accepts"]
            .as_array()
            .unwrap_or_else(|| panic!("{command} の --segment が accepts を返さない"))
            .iter()
            .map(|x| x.as_str().unwrap())
            .collect();
        assert_eq!(accepts, ["off", "auto", "isnet"], "{command}");
    }
}

// --- モデルが要る検査（無ければ黙って飛ばす） ---

/// モデルが走ると、確定領域とその内訳が報告される。
///
/// **3 つの比率の和は 1 になる。** 崩れていれば、確定前景と確定背景が
/// 同じ画素で重なっている（衝突を数え落としている）。
#[test]
fn the_model_places_a_trimap_and_reports_what_it_placed() {
    if skip_without_model() {
        return;
    }
    let (dir, input) = fixture();
    let out = dir.path().join("a.png");
    let result = kiri()
        .args([
            "cutout",
            input.to_str().unwrap(),
            "-o",
            out.to_str().unwrap(),
            "--segment",
            "isnet",
            "--json",
        ])
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let v = json_stdout(&result);

    assert_eq!(v["settings"]["segment"], "isnet");
    assert_eq!(v["settings"]["segment_ran"], true);
    let s = &v["segment"];
    assert_eq!(s["model"], "isnet");
    assert_eq!(s["input_size"], 1024);
    let (fg, bg, unknown) = (
        s["fg_ratio"].as_f64().unwrap(),
        s["bg_ratio"].as_f64().unwrap(),
        s["uncertain_ratio"].as_f64().unwrap(),
    );
    assert!(
        (fg + bg + unknown - 1.0).abs() < 1e-3,
        "3 つの比率の和が 1 でない: {s}"
    );
    assert!(fg > 0.0, "確定前景が 1 画素も置かれていない: {s}");
    assert!(bg > 0.0, "確定背景が 1 画素も置かれていない: {s}");
    assert!(
        s["model_path"].as_str().unwrap().ends_with(".onnx"),
        "読んだファイルを名乗っていない: {s}"
    );

    // 指示として `constraints` にも現れる。**入口の名前は segment**
    let sources: Vec<&str> = v["constraints"]["sources"]
        .as_array()
        .unwrap_or_else(|| panic!("constraints が無い: {v}"))
        .iter()
        .map(|x| x.as_str().unwrap())
        .collect();
    assert_eq!(sources, ["segment"], "{v}");
}

/// **利用者の指示はモデルに勝つ。エラーにはしない。**
///
/// モデルが背景だと言った隅を `--fg-seed` で前景に固定する。衝突を
/// `CONSTRAINT_CONFLICT` にすると、「モデルを使うと指示が出せなくなる」
/// という筋の悪い規約になる。
#[test]
fn a_user_instruction_beats_the_model_without_an_error() {
    if skip_without_model() {
        return;
    }
    let (dir, input) = fixture();
    let out = dir.path().join("a.png");
    let mask = dir.path().join("mask.png");
    let result = kiri()
        .args([
            "cutout",
            input.to_str().unwrap(),
            "-o",
            out.to_str().unwrap(),
            "--segment",
            "isnet",
            // 画像の隅。モデルは確実に背景だと言う
            "--fg-seed",
            "8,8",
            "--debug-mask",
            mask.to_str().unwrap(),
            "--json",
        ])
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "指示とモデルの衝突でエラーになった: {}",
        String::from_utf8_lossy(&result.stderr)
    );
    let v = json_stdout(&result);
    let sources: Vec<&str> = v["constraints"]["sources"]
        .as_array()
        .unwrap()
        .iter()
        .map(|x| x.as_str().unwrap())
        .collect();
    assert_eq!(
        sources,
        ["segment", "fg_seed"],
        "モデルと利用者の両方が並んでいない: {v}"
    );

    let mask = image::open(&mask).unwrap().to_luma8();
    assert_eq!(
        mask.get_pixel(8, 8).0[0],
        255,
        "利用者が前景だと指した画素がモデルの背景に負けている"
    );
}

/// `info --segment` は主体をモデルから出し、何由来かを名乗る。
#[test]
fn info_reports_a_subject_measured_by_the_model() {
    if skip_without_model() {
        return;
    }
    let (_dir, input) = fixture();
    let v = json_stdout(
        &kiri()
            .args([
                "info",
                input.to_str().unwrap(),
                "--segment",
                "isnet",
                "--json",
            ])
            .output()
            .unwrap(),
    );
    assert_eq!(v["subject"]["source"], "segment", "{v}");
    assert_eq!(v["segment"]["model"], "isnet");

    // モデルを渡さなければ今までどおり色由来
    let plain = json_stdout(
        &kiri()
            .args(["info", input.to_str().unwrap(), "--json"])
            .output()
            .unwrap(),
    );
    assert_eq!(plain["subject"]["source"], "colour");
    assert!(plain.get("segment").is_none());
}

/// **モデルが迷ったことは `info` でも黙らない。**
///
/// `schema` は `segment.uncertain_ratio` を `info` と `cutout` の両方に配り、
/// 「0.3 を超えたら `SEGMENT_UNCERTAIN`」と書いている。それなのに `info` は
/// 値だけを返して警告を出し忘れていた——**配った値を自分で比べたエージェント
/// だけが気づける**状態で、しきい値を配る意味が半分になる。
///
/// 材料は暗いキーの格子。単色背景の合成商品ではモデルが素直に言い切って
/// しまい（不明の帯 5% 程度）、しきい値を越える側を確かめられない。
#[test]
fn info_does_not_stay_silent_when_the_model_is_lost() {
    if skip_without_model() {
        return;
    }
    let dir = TempDir::new().unwrap();
    let input = write_png(dir.path(), "keys.png", &common::dense_key_grid(256, 256));
    let out = dir.path().join("a.png");

    let info = json_stdout(
        &kiri()
            .args([
                "info",
                input.to_str().unwrap(),
                "--segment",
                "isnet",
                "--json",
            ])
            .output()
            .unwrap(),
    );
    let cutout = json_stdout(
        &kiri()
            .args([
                "cutout",
                input.to_str().unwrap(),
                "-o",
                out.to_str().unwrap(),
                "--dry-run",
                "--segment",
                "isnet",
                "--json",
            ])
            .output()
            .unwrap(),
    );

    let ratio = |v: &Value| v["segment"]["uncertain_ratio"].as_f64().unwrap();
    let warned = |v: &Value| {
        v["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .any(|w| w["code"] == "SEGMENT_UNCERTAIN")
    };
    assert!(
        ratio(&info) > kiri::segment::SEG_UNCERTAIN_WARN,
        "材料がしきい値を越えていない。警告が出る側を確かめられない: {}",
        ratio(&info)
    );
    assert!(
        warned(&info),
        "info が SEGMENT_UNCERTAIN を出していない: {info}"
    );
    assert!(
        warned(&cutout),
        "cutout が SEGMENT_UNCERTAIN を出していない"
    );
    // 同じ画像・同じモデルなので、迷い方も同じでなければならない
    assert!(
        (ratio(&info) - ratio(&cutout)).abs() < 1e-9,
        "info と cutout で不明の帯が違う: {} vs {}",
        ratio(&info),
        ratio(&cutout)
    );
}

/// モデルを走らせても、`--segment off` の出力は影響を受けない。
///
/// 同じ入力を 2 回走らせて、片方だけモデルを使う。**`off` 側のバイト列が
/// 動いていないこと**を同じ実行の中で確かめる。
#[test]
fn running_the_model_does_not_disturb_the_off_path() {
    if skip_without_model() {
        return;
    }
    let (dir, input) = fixture();
    let run = |name: &str, segment: &str| -> Vec<u8> {
        let out = dir.path().join(format!("{name}.png"));
        let result = kiri()
            .args([
                "cutout",
                input.to_str().unwrap(),
                "-o",
                out.to_str().unwrap(),
                "--segment",
                segment,
                "--json",
            ])
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "{name}: {}",
            String::from_utf8_lossy(&result.stderr)
        );
        std::fs::read(&out).unwrap()
    };
    let before = run("before", "off");
    let _ = run("with", "isnet");
    let after = run("after", "off");
    assert_eq!(before, after, "モデルを挟んだだけで off の出力が動いた");
}

/// 同じ入力・同じモデルからは同じバイト列が出る。
#[test]
fn the_model_path_is_deterministic() {
    if skip_without_model() {
        return;
    }
    let (dir, input) = fixture();
    let run = |name: &str| -> Vec<u8> {
        let out = dir.path().join(format!("{name}.png"));
        kiri()
            .args([
                "cutout",
                input.to_str().unwrap(),
                "-o",
                out.to_str().unwrap(),
                "--segment",
                "isnet",
            ])
            .output()
            .unwrap();
        std::fs::read(&out).unwrap()
    };
    assert_eq!(run("a"), run("b"), "同じ入力から違う結果が出た");
}

/// 置いてあるモデルは、名乗ったダイジェストと一致する。
///
/// **表を書き写した誤りはここでしか捕まらない。** 合っていなければ
/// `kiri model list` は正しいファイルを「壊れている」と報告し続ける。
#[test]
fn the_published_digest_matches_the_file_on_disk() {
    if skip_without_model() {
        return;
    }
    let v = json_stdout(&kiri().args(["model", "list", "--json"]).output().unwrap());
    let isnet = v["models"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["name"] == "isnet")
        .unwrap();
    assert_eq!(isnet["present"], true);
    assert_eq!(
        isnet["verified"], true,
        "置いてあるモデルのダイジェストが合わない: {isnet}"
    );
}

/// **入力の寸法はモデルが決める。`--segment-size` を置かなかった根拠。**
///
/// 設計では `512 | 1024` を選べるようにするつもりだった。ISNet の ONNX は
/// 1024 を graph に焼き込んでいる（`Resize` の出力寸法が定数）ので、512 を
/// 渡すと復号側の Concat が `Impossible to unify Val(32) with Val(16)` で
/// 解析に失敗する。**常に失敗する値を契約に載せるのは、誤った助言と同じ害を持つ。**
///
/// 表の `input_size` が graph と食い違ったときも、ここが先に鳴る。
#[test]
#[cfg(feature = "segment")]
fn the_input_size_is_the_models_to_choose_not_ours() {
    if skip_without_model() {
        return;
    }
    let halved = kiri::segment::model::KnownModel {
        input_size: 512,
        ..kiri::segment::model::ISNET
    };
    let options = kiri::segment::SegmentOptions {
        model: halved,
        ..kiri::segment::SegmentOptions::isnet()
    };
    let image = image::RgbaImage::from_pixel(64, 64, image::Rgba([200, 200, 200, 255]));
    let error = kiri::segment::run(&image, &options)
        .err()
        .expect("512 で graph が通ってしまった。--segment-size を置ける");
    assert_eq!(
        error.code,
        kiri::error::ErrorCode::ModelUnreadable,
        "{error}"
    );
}

// --- 計測用（`--ignored` を付けたときだけ走る） ---

/// **前処理の 2 方式を実写で比べる表。**
///
/// `KIRI_SEGMENT_DIR` に置いた画像（jpg / png）をすべて両方式で回し、
/// 確定前景・確定背景・不明の割合と所要時間を並べる。
///
/// ```text
/// KIRI_SEGMENT_DIR=~/shots cargo test --release --features segment \
///     --test segment -- --ignored --nocapture print_the_preprocessing_comparison
/// ```
#[test]
#[ignore = "計測用。KIRI_SEGMENT_DIR とモデルが無ければ何もしない"]
fn print_the_preprocessing_comparison() {
    if skip_without_model() {
        return;
    }
    let Ok(dir) = std::env::var(common::KIRI_SEGMENT_DIR) else {
        println!("{} が指定されていない", common::KIRI_SEGMENT_DIR);
        return;
    };
    println!(
        "\n{:<16} {:<10} {:>6} {:>6} {:>6} {:>7} {:>6} {:>7} {:>7} {:>5} {:>7}  警告",
        "画像",
        "前処理",
        "確定前",
        "確定背",
        "不明",
        "前景比",
        "境界ΔE",
        "粗さ",
        "縁汚染",
        "接触",
        "ms"
    );
    for path in images_in(Path::new(&dir)) {
        let name = path.file_name().unwrap().to_string_lossy().to_string();
        // **kiri 自身の読み込みを通す。** `image::open` で素のまま読むと EXIF
        // Orientation も ICC も当たらず、モデルには CLI が渡すのとは別の画素が
        // 届く。手元の実写は iPhone の縦位置（Orientation 6、Display P3）なので、
        // 素通しでは横倒しの広色域画像で比べることになり、そこで決めた既定は
        // `kiri cutout` の挙動を説明しない
        let image = match kiri::image_io::load::load(&path) {
            Ok(loaded) => loaded.image,
            Err(e) => {
                println!("{name}: 読めない ({e})");
                continue;
            }
        };
        for (label, fit) in [
            ("letterbox", kiri::segment::SegmentFit::Letterbox),
            ("stretch", kiri::segment::SegmentFit::Stretch),
        ] {
            let mut options = kiri::segment::SegmentOptions::isnet();
            options.fit = fit;
            let run = match kiri::segment::run(&image, &options) {
                Ok(run) => run,
                Err(e) => {
                    println!("{name} / {label}: {e}");
                    continue;
                }
            };
            let (constraints, stats) =
                kiri::segment::to_constraints(&run.probability, image.width(), image.height());
            // **確定領域の広さだけでは決められない。** 言い切っている面積が広い
            // ことと、正しい場所を言い切っていることは別である。既存の経路まで
            // 通して、kiri 自身の診断値で比べる
            let result = kiri::cutout::cutout(
                &image,
                &kiri::cutout::CutoutOptions {
                    constraints: Some(constraints),
                    ..Default::default()
                },
            );
            let show = |v: Option<f64>| match v {
                Some(v) => format!("{v:.3}"),
                None => "null".to_string(),
            };
            println!(
                "{name:<16} {label:<10} {:>5.1}% {:>5.1}% {:>5.1}% {:>6.1}% {:>6} {:>7} {:>7} {:>5} {:>7}  {}",
                stats.fg_ratio * 100.0,
                stats.bg_ratio * 100.0,
                stats.uncertain_ratio * 100.0,
                result.stats.foreground_ratio * 100.0,
                show(result.separability),
                show(result.diagnostics.contour_roughness),
                show(result.diagnostics.rim_contamination),
                if result.stats.touches_edge {
                    "あり"
                } else {
                    "なし"
                },
                run.elapsed_ms,
                result
                    .warnings
                    .iter()
                    .map(|w| w.code.as_str())
                    .collect::<Vec<_>>()
                    .join(",")
            );
            // **数値だけでは決められなかった。** 前処理の比較で効いたのは
            // 「商品の右列が丸ごと落ちる」という目で見なければ分からない差で、
            // `separability` はむしろ**削った側を高く**評価する（商品を削ると
            // 境界が銀の枠と暗い机のあいだへ移る）。比べた本人が見た画像を
            // 残しておかないと、後から表だけを読んだ人は逆の結論を引く
            let out = Path::new(&dir).join("out");
            if std::fs::create_dir_all(&out).is_ok() {
                let stem = path.file_stem().unwrap_or_default().to_string_lossy();
                let _ = result.image.save(out.join(format!("{stem}_{label}.png")));
            }
        }
    }
}

fn images_in(dir: &Path) -> Vec<std::path::PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        println!("{}: 読めない", dir.display());
        return Vec::new();
    };
    let mut paths: Vec<std::path::PathBuf> = entries
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| {
            p.extension()
                .and_then(|e| e.to_str())
                .is_some_and(|e| matches!(e, "jpg" | "jpeg" | "png"))
        })
        .collect();
    paths.sort();
    paths
}

/// `--segment off` に `--model-path` を添えた指定は黙って捨てない。
///
/// **「効いた値だけを報告する」規約の裏返しである。** 渡した指定が無言で
/// 落ちると、利用者はモデルで切ったつもりの結果を色だけの結果として受け取る。
/// この検査はモデルも feature も要らない——読まないことを確かめている。
#[test]
fn an_ignored_model_path_is_reported() {
    let (dir, input) = fixture();
    let out = kiri()
        .args([
            "cutout",
            input.to_str().unwrap(),
            "-o",
            dir.path().join("cut.png").to_str().unwrap(),
            "--segment",
            "off",
            "--model-path",
            dir.path().join("nowhere.onnx").to_str().unwrap(),
            "--json",
        ])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "読まない指定でエラーにはしない: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let v = json_stdout(&out);
    let warning = v["warnings"]
        .as_array()
        .unwrap()
        .iter()
        .find(|w| w["code"] == "MODEL_PATH_IGNORED")
        .unwrap_or_else(|| panic!("MODEL_PATH_IGNORED が出ていない: {v}"));
    assert!(warning["hint"].as_str().unwrap().contains("--segment"));
    assert!(warning["data"]["model_path"].is_string());
    assert_eq!(v["settings"]["segment_ran"], false);
}

/// `info` も同じ警告を出す。**2 つのコマンドで契約の形が違う理由が無い。**
#[test]
fn info_reports_an_ignored_model_path_too() {
    let (dir, input) = fixture();
    let out = kiri()
        .args([
            "info",
            input.to_str().unwrap(),
            "--model-path",
            dir.path().join("nowhere.onnx").to_str().unwrap(),
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
        v["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .any(|w| w["code"] == "MODEL_PATH_IGNORED"),
        "{v}"
    );
}

/// この build が推論できるかを `kiri schema` から 1 回で読める。
///
/// **`commands[]` に `--segment` の綴りが並ぶことは、走らせられることを
/// 意味しない。** 綴りは feature の有無によらず出る（断り方も契約である）。
#[test]
fn the_schema_states_whether_this_build_can_infer() {
    let out = kiri().args(["schema", "--json"]).output().unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let v = json_stdout(&out);
    assert_eq!(
        v["segment_available"],
        cfg!(feature = "segment"),
        "schema の segment_available が build と食い違う: {v}"
    );

    // `kiri model list` と同じ 1 つの事実を指していること
    let models = kiri().args(["model", "list", "--json"]).output().unwrap();
    assert_eq!(
        json_stdout(&models)["segment_available"],
        v["segment_available"]
    );
}

/// `--segment auto` が「走らせるまでもない」と決めたときは**黙っている**。
///
/// `--model-path` は「走るならこれを読め」という指定であり、走らせない判断を
/// したのは kiri 自身である。ここで `MODEL_PATH_IGNORED` を出すと、
/// `settings.segment_ran` が既に言っていることを二重に言うことになる。
///
/// **沈黙の側こそ固定する。** 放っておくと後から逆へ倒れる種類の判断である。
#[test]
fn auto_that_decides_not_to_run_says_nothing_about_the_model_path() {
    if !cfg!(feature = "segment") {
        eprintln!("この build に segment 機能が無いので飛ばす（auto は届く前に断られる）");
        return;
    }
    let (dir, input) = fixture();
    let out = kiri()
        .args([
            "cutout",
            input.to_str().unwrap(),
            "-o",
            dir.path().join("cut.png").to_str().unwrap(),
            "--segment",
            "auto",
            "--model-path",
            dir.path().join("nowhere.onnx").to_str().unwrap(),
            "--json",
        ])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "色で解ける画像なのでモデルは要らないはず: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let v = json_stdout(&out);
    assert_eq!(v["settings"]["segment_ran"], false, "{v}");
    assert!(
        !v["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .any(|w| w["code"] == "MODEL_PATH_IGNORED"),
        "走らせない判断まで警告にすると settings.segment_ran と二重になる: {v}"
    );
}

/// **`batch` が spec の `segment` でモデルを走らせること。**
///
/// 2 件を 1 度のプロセスで回す。読み込みは 1 度きりだが、`segment` ブロックと
/// `settings.segment_ran` は**どちらの件にも出る**——2 件目から黙ると、
/// 数百点の spec で「モデルが効いた件」を数えられなくなる。
#[test]
fn batch_runs_the_model_for_every_item() {
    if skip_without_model() {
        return;
    }
    let dir = TempDir::new().unwrap();
    for name in ["a.png", "b.png"] {
        write_png(dir.path(), name, &product_image(&ProductSpec::default()));
    }
    let spec = dir.path().join("spec.json");
    std::fs::write(
        &spec,
        r#"{"defaults":{"segment":"isnet","format":"png"},
             "items":[{"input":"a.png","output":"out/a.png"},
                      {"input":"b.png","output":"out/b.png"}]}"#,
    )
    .unwrap();

    let out = kiri()
        .args(["batch", spec.to_str().unwrap(), "--jobs", "2", "--json"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let v = json_stdout(&out);
    assert_eq!(v["succeeded"], 2, "{v}");
    for i in 0..2 {
        let item = &v["results"][i]["result"];
        assert_eq!(item["settings"]["segment"], "isnet", "{i} 件目: {item}");
        assert_eq!(item["settings"]["segment_ran"], true, "{i} 件目: {item}");
        assert_eq!(item["segment"]["model"], "isnet", "{i} 件目: {item}");
    }
}

/// spec の `model_path` は仕様ファイルの場所を基準に解決する
/// （`trimap` などと同じ規則）。
///
/// **見つからないことで確かめる。** 176MB の複製をテストのために作らずに、
/// 「どこを見に行ったか」だけをエラーの文面から読む。
#[test]
#[cfg(feature = "segment")]
fn a_spec_model_path_resolves_against_the_spec_file() {
    let dir = TempDir::new().unwrap();
    write_png(dir.path(), "a.png", &product_image(&ProductSpec::default()));
    let spec = dir.path().join("spec.json");
    std::fs::write(
        &spec,
        r#"{"items":[{"input":"a.png","output":"out/a.png",
                      "segment":"isnet","model_path":"models/nope.onnx"}]}"#,
    )
    .unwrap();

    let out = kiri()
        .args(["batch", spec.to_str().unwrap(), "--json"])
        .output()
        .unwrap();
    let v = json_stdout(&out);
    let error = &v["results"][0]["error"];
    assert_eq!(error["code"], "MODEL_NOT_FOUND", "{v}");
    let expected = dir.path().join("models").join("nope.onnx");
    assert!(
        error["message"]
            .as_str()
            .unwrap_or_default()
            .contains(expected.to_str().unwrap()),
        "spec の場所を基準にしていない: {error}"
    );
}
