//! 1 枚の最終画像から、書き出す派生を作る。
//!
//! Phase 19（--max-bytes）と Phase 20（多派生）はどちらも「エンコードして書く」を
//! 奪い合う。先に 1 本の道へ畳み、N = 1 で ICC を付けない（`IccPolicy::None`）とき
//! Phase 17 と同じバイト列になることを固定してから、その上へ機能を載せる。既定の
//! `Embed` との差は iCCP / APP2 の 1 つだけで、それは `save.rs` のテストが見ている。
//!
//! Phase 20 で派生が N 本になったが、**N = 1 の道は 1 バイトも変わっていない。**
//! 指定が無ければ `DeriveSpec::default()` が 1 本だけ立ち、`resize` は `None`、
//! パスは `--output` そのものになる。増えたのは `outputs[].role` の 1 キーだけで、
//! それは `SCHEMA_VERSION` 2 の側で名乗る

use std::path::{Path, PathBuf};

use image::RgbaImage;

use super::save::{IccPolicy, OutputFormat, SaveOptions, encode_prepared, prepare, write_encoded};
use crate::error::Result;
use crate::report::{OutputReport, quality_number};
use crate::transform::resize::{FitMode, ResizeSpec, apply as resize_apply, plan as resize_plan};
use crate::warning::{Warning, WarningCode};

/// 利用者が書いた 1 本ぶんの指定。**まだ何も解決していない。**
///
/// `Derivation` と分けているのは、省いたキーが `OutputOpts` の対応する値を継ぐ
/// からである。継ぐ前の「書かなかった」を `None` として持てないと、
/// 「`--quality 82` を書いた」と「既定の 75 が効いた」が同じ形になり、
/// **`--derive` を 1 本だけ書いた実行が既定値を上書きしてしまう**。
///
/// CLI（`k=v,k=v` の文字列）と spec（JSON のオブジェクト）は同じ `set` を通る。
/// 片方だけ緩いと、spec 経由でだけ綴り違いのキーが黙って無視される
#[derive(Debug, Clone, Default, PartialEq)]
pub struct DeriveSpec {
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub fit: Option<FitMode>,
    pub allow_upscale: Option<bool>,
    pub format: Option<OutputFormat>,
    pub quality: Option<f32>,
    pub effort: Option<u8>,
    pub max_bytes: Option<u64>,
    /// この派生の役目（`{role}` と `outputs[].role` に出る自由な短い文字列）
    pub role: Option<String>,
}

/// `--derive` と spec の `derive[]` が受け付けるキー。
///
/// **綴り違いの候補を出すために一覧で持つ。** 未知のキーを黙って捨てると、
/// 指定したはずの上限や役目が効かないまま数百点が書き出される
pub const DERIVE_KEYS: &[&str] = &[
    "width",
    "height",
    "fit",
    "allow_upscale",
    "format",
    "quality",
    "effort",
    "max_bytes",
    "role",
];

impl DeriveSpec {
    /// キーと値を 1 組だけ読む。**値はどちらの入口でも文字列で渡す。**
    ///
    /// spec の JSON は数値や真偽値で書かれるが、呼ぶ側が綴り直して渡すことで
    /// 解釈の規則が 1 箇所になる——`max_bytes` の `"500k"` も `512000` も
    /// CLI と同じ `parse_max_bytes` が読む。
    ///
    /// 返すのは `String` の誤り文面である。CLI では clap の `value_parser` が
    /// これを受けて code 無しの exit 2 にし、spec では呼ぶ側が
    /// `INVALID_DERIVATION` を被せる（`INVALID_MAX_BYTES` と同じ前例）
    pub fn set(&mut self, key: &str, value: &str) -> std::result::Result<(), String> {
        let number = |what: &str| format!("{key} には{what}を指定してください（'{value}'）");
        // **同じキーを 2 度書いたら断る。** 黙って後勝ちにすると
        // `--derive 'width=100,width=250'` で 250 だけが効き、「書いたのに効かない」が
        // ここにだけ残る——未知のキーをきちんと断っているのと食い違う。
        // JSON のオブジェクトは serde_json の時点で重複が潰れるので、塞ぐのは CLI の穴
        if self.already_has(key) {
            return Err(format!(
                "{key} が 2 回指定されています（後の値だけが黙って効くのを避けるため断ります）"
            ));
        }
        match key {
            "width" => self.width = Some(dimension(value, key)?),
            "height" => self.height = Some(dimension(value, key)?),
            "fit" => {
                self.fit = Some(match value.to_ascii_lowercase().as_str() {
                    "contain" => FitMode::Contain,
                    "cover" => FitMode::Cover,
                    // **exact は受けない。** 縦横比を無視して枠へ変形するのは
                    // 商品画像では事故でしかなく、`kiri resize --fit exact` という
                    // 明示の入口が別にある
                    _ => {
                        return Err(format!(
                            "fit には contain / cover を指定してください（'{value}'）"
                        ));
                    }
                })
            }
            "allow_upscale" => {
                self.allow_upscale = Some(match value.to_ascii_lowercase().as_str() {
                    "true" => true,
                    "false" => false,
                    _ => {
                        return Err(format!(
                            "allow_upscale には true / false を指定してください（'{value}'）"
                        ));
                    }
                })
            }
            "format" => {
                self.format = Some(OutputFormat::from_name(value).ok_or_else(|| {
                    format!("format には avif / png / jpeg / jpg を指定してください（'{value}'）")
                })?)
            }
            "quality" => {
                let q: f32 = value.parse().map_err(|_| number("数値"))?;
                if !q.is_finite() || !(0.0..=100.0).contains(&q) {
                    return Err(number("0 から 100 の数値"));
                }
                self.quality = Some(q);
            }
            "effort" => {
                let e: u8 = value.parse().map_err(|_| number("1 から 10 の整数"))?;
                if !(1..=10).contains(&e) {
                    return Err(number("1 から 10 の整数"));
                }
                self.effort = Some(e);
            }
            "max_bytes" => self.max_bytes = Some(crate::cli::parse_max_bytes(value)?),
            "role" => {
                if value.is_empty() {
                    return Err("role に空文字は指定できません".to_string());
                }
                // **役目の札にパスを書かせない。** `{role}` はそのまま
                // ファイル名の一部になるので、区切りや `..` を通すと
                // `--output` の親の外へ書ける。断るのは `naming::beside` でも
                // 同じだが、ここで断るほうが原因（どの派生の role か）が読める
                if value.contains('/') || value.contains('\\') || value == "." || value == ".." {
                    return Err(format!(
                        "role にディレクトリの区切りや '..' は指定できません（'{value}'）"
                    ));
                }
                self.role = Some(value.to_string());
            }
            _ => {
                return Err(format!(
                    "'{key}' は --derive の知らないキーです（指定できるキー: {}）",
                    DERIVE_KEYS.join(" / ")
                ));
            }
        }
        Ok(())
    }

    /// そのキーに既に値が入っているか。**キーの綴りは `set` の `match` と同じ
    /// 並びで持つ**——ここに書き忘れたキーだけが黙って後勝ちに戻る。
    /// 未知のキーは `false` を返し、`set` の側の「知らないキーです」へ落とす
    fn already_has(&self, key: &str) -> bool {
        match key {
            "width" => self.width.is_some(),
            "height" => self.height.is_some(),
            "fit" => self.fit.is_some(),
            "allow_upscale" => self.allow_upscale.is_some(),
            "format" => self.format.is_some(),
            "quality" => self.quality.is_some(),
            "effort" => self.effort.is_some(),
            "max_bytes" => self.max_bytes.is_some(),
            "role" => self.role.is_some(),
            _ => false,
        }
    }

    /// 寸法の指定があるか。**無ければリサイズしない**（最終画像そのまま）
    pub fn resizes(&self) -> bool {
        self.width.is_some() || self.height.is_some()
    }

    /// この派生のリサイズ指定。寸法を 1 つも書いていなければ `None`
    pub fn resize(&self) -> Option<ResizeSpec> {
        self.resizes().then(|| ResizeSpec {
            width: self.width,
            height: self.height,
            fit: self.fit.unwrap_or(FitMode::Contain),
            allow_upscale: self.allow_upscale.unwrap_or(false),
        })
    }
}

fn dimension(value: &str, key: &str) -> std::result::Result<u32, String> {
    match value.parse::<u32>() {
        Ok(0) | Err(_) => Err(format!(
            "{key} には 1 以上の整数を指定してください（'{value}'）"
        )),
        Ok(v) => Ok(v),
    }
}

/// 書き出す 1 本。**パスも寸法もここへ来る前に解決してある。**
///
/// 命名と衝突の検査を `render` より前に済ませる理由は、1 枚でも書いた後に
/// 落ちると半端な成果物が残るためである（計画 7.2）。
#[derive(Debug, Clone)]
pub struct Derivation {
    /// 解決済みの書き出し先。命名や衝突の検査は render より前に済ませる
    pub path: PathBuf,
    pub format: OutputFormat,
    pub quality: f32,
    pub effort: u8,
    pub background: [u8; 3],
    pub flatten: bool,
    pub icc: IccPolicy,
    /// 出力の上限バイト数。品質を梯子状に落として収める（`QUALITY_LADDER`）。
    /// `None` なら 1 回エンコードして終わりで、Phase 18 と 1 バイトも変わらない
    pub max_bytes: Option<u64>,
    /// この派生だけのリサイズ。`None` なら最終画像をそのまま書く。
    ///
    /// **計画（`ResizePlan`）ではなく指定を持つ。** `plan` は寸法と指定だけの
    /// 純関数なので、パスを決めた側と `render` が別々に呼んでも同じ答えになる。
    /// 計画を持ち回ると「どの寸法で名前を付けたか」と「どの寸法で書いたか」が
    /// 2 つの値になり、食い違っても型は何も言わない
    pub resize: Option<ResizeSpec>,
    /// この派生の役目（`--derive` の `role`）。指定が無ければ `None`
    pub role: Option<String>,
}

impl Derivation {
    pub fn save_options(&self) -> SaveOptions {
        SaveOptions {
            format: self.format,
            quality: self.quality,
            effort: self.effort,
            background: self.background,
            flatten: self.flatten,
            icc: self.icc,
        }
    }

    /// この派生が実際に書き出す寸法。命名（`{width}` / `{height}`）と
    /// `outputs[].width` が同じ 1 つの答えを引くための入口である
    pub fn dimensions(&self, source: (u32, u32)) -> Result<(u32, u32)> {
        match &self.resize {
            Some(spec) => Ok(resize_plan(source, spec)?.output),
            None => Ok(source),
        }
    }
}

#[derive(Debug)]
pub struct Rendered {
    pub report: OutputReport,
    /// 派生ごとに分けて持つ。Phase 20 で全警告の `data` に `output` を付けるため
    pub warnings: Vec<Warning>,
}

/// 品質の梯子。**時刻にもタイムアウトにも依存させない。** 決定性は kiri の
/// 中核の約束で、同じ入力なら `quality_used` と `attempts` が毎回同じでなければ
/// ならない。「制限時間まで二分探索する」形は 1 段あたり数秒かかる 24.5MP の
/// AVIF で機械ごとに違う答えを出すので採らなかった。
///
/// **段は固定の絶対値である。** 要求品質からの相対（-10 ずつ）にすると、
/// `--quality 80` と `--quality 78` が別の着地点へ落ちる。絶対値なら要求品質が
/// 違っても綴りが揃い、数百点のセットの中で品質が 2 種類か 3 種類に収まる。
///
/// **公開しているのは文面と食い違わせないためである。** 同じ 7 つの数が
/// `--max-bytes` の長いヘルプと `kiri schema` の notes にも出るので、
/// `the_published_prose_spells_the_real_ladder` がここと突き合わせる
pub const QUALITY_LADDER: &[f32] = &[85.0, 75.0, 65.0, 55.0, 45.0, 35.0, 25.0];

/// 1 派生ぶんの探索の結果。
struct Encoded {
    bytes: Vec<u8>,
    /// エンコーダが実際に受け取った品質。持たない形式（PNG）では None
    quality_used: Option<f32>,
    attempts: u32,
    warnings: Vec<Warning>,
}

/// 派生を順にリサイズ・エンコードし、`dry_run` でなければ書く。
///
/// **ICC はエンコーダの内側で埋まる**ので、`report.bytes` は ICC 込みの大きさで
/// ある（Phase 19 の探索はこの値だけを見ればよい）。dry-run でもエンコードまでは
/// 同じ道を通る。
///
/// **1 本ずつ「リサイズ → エンコード → 書き出し → 解放」を回す。** リサイズ済みの
/// 画像もバイト列も、そのイテレーションを抜けるまでしか生きていない。N 本ぶんを
/// 同時に持つと 24.5MP × N のピークになり、**派生を増やすほど落ちやすい道具**に
/// なる（計画 7.2 の「逐次処理して都度解放する」）。並列は batch の項目単位に任せる。
///
/// リサイズの要らない派生（`resize` が `None`、または計画寸法が元と同じ）では
/// 元画像を借りる。無駄な複製をしないのは Phase 18 から変わらない約束である。
///
/// **最初の失敗でそこから先を書かない。** 1 入力の中の派生は部分失敗を許さない
/// ——黙って 1 枚落とすと成果物の欠けに気づけない。部分失敗を扱うのは batch の
/// 側（`MANIFEST_PARTIAL`）だけである
pub fn render(
    image: &RgbaImage,
    derivations: &[Derivation],
    dry_run: bool,
) -> Result<Vec<Rendered>> {
    derivations
        .iter()
        .map(|d| {
            let plan = d
                .resize
                .as_ref()
                .map(|spec| resize_plan((image.width(), image.height()), spec))
                .transpose()?;
            // 計画寸法が元と同じなら借りる。`apply` も同寸では複製するので、
            // ここで分けないと「リサイズしない派生」が 1 枚ぶん余計に積む
            let resized = match &plan {
                Some(plan) if plan.output != (image.width(), image.height()) => {
                    Some(resize_apply(image, plan)?)
                }
                _ => None,
            };
            let target = resized.as_ref().unwrap_or(image);

            let Encoded {
                bytes,
                quality_used,
                attempts,
                warnings,
            } = encode_within_budget(target, d)?;
            if !dry_run {
                write_encoded(&d.path, &bytes)?;
            }
            Ok(Rendered {
                report: OutputReport {
                    path: d.path.display().to_string(),
                    format: d.format.as_str().to_string(),
                    width: target.width(),
                    height: target.height(),
                    bytes: bytes.len() as u64,
                    icc: d.icc.signal(d.format),
                    quality_used: quality_used.map(quality_number),
                    attempts,
                    role: d.role.clone(),
                },
                // **ここを通った警告は必ず「どの派生か」を名乗る。** 1 実行で
                // 複数の派生を書くようになった以上、`ALPHA_FLATTENED` が
                // どの出力の話なのか分からない報告は分岐の材料にならない
                warnings: warnings.into_iter().map(|w| tag(w, &d.path)).collect(),
            })
            // ここで `resized` と `bytes` が落ちる。次の派生へ持ち越さない
        })
        .collect()
}

/// 派生に紐づく警告へ「どの派生か」を書き込む。
///
/// キーを `output` に揃えるのは、`outputs[].path` と同じ文字列を指すためである。
/// 受け手は `warnings[].data.output` で `outputs[]` を引ける
pub fn tag(warning: Warning, path: &Path) -> Warning {
    warning.with_data("output", path.display().to_string())
}

/// `max_bytes` に収まるバイト列を探す。
///
/// **1 回目は必ず要求品質である。** `max_bytes` が無ければそこで返すので、
/// 指定しない実行は Phase 18 と 1 バイトも変わらず、`attempts` も 1 のままになる。
///
/// 収まらなければ `QUALITY_LADDER` のうち**要求品質より小さい段だけ**を上から
/// 順に試し、最初に収まった段で止める。全部外したら**1 回目のバッファを書く**
/// ——どうせ制約は破れているので、画質まで捨てる理由が無い。利用者は `kiri resize`
/// や形式の変更へ進める。最上段のバッファは最初に取ったものを持ち続け、
/// **再エンコードはしない**（決定性とコストの両面）。
///
/// 動かすのは `quality` だけで、**AVIF の `effort` には触らない**。時間が桁で
/// 変わるつまみを探索の軸にすると、最大 8 回のエンコードが数分では済まなくなる。
///
/// 画素の下ごしらえ（アルファの走査と合成）は `prepare` で 1 回だけ行う。
/// 段ごとにやり直すと 24.5MP の JPEG で 1 段あたり 98MB の複製が積む
fn encode_within_budget(image: &RgbaImage, d: &Derivation) -> Result<Encoded> {
    let mut opts = d.save_options();
    let mut prepared = prepare(image, &opts)?;
    // 警告は 1 組しか無いので、先に引き取る。段ごとに集めて捨てる後始末が要らない
    let mut warnings = std::mem::take(&mut prepared.warnings);
    let first = encode_prepared(&prepared, &opts)?;
    // 品質を持たない形式では「要求品質」を名乗らない。null は
    // 「この形式に品質は無い」を意味する（`OutputReport::quality_used`）。
    // 持つ形式では**エンコーダが受け取った値**を名乗る（JPEG は丸めた後）
    let asked = d.format.effective_quality(d.quality);
    let done = |bytes: Vec<u8>, quality_used, attempts, warnings| Encoded {
        bytes,
        quality_used,
        attempts,
        warnings,
    };

    let Some(max) = d.max_bytes else {
        return Ok(done(first, asked, 1, warnings));
    };
    if first.len() as u64 <= max {
        return Ok(done(first, asked, 1, warnings));
    }

    // 降りられる段。**要求品質より下だけ**を上から順に試す。PNG は無損失で
    // バイト列が動かないので空になり、`--quality 20` のように梯子の下限より
    // 低い要求でも空になる。どちらも 1 回で降参する
    let rungs: Vec<f32> = if d.format.has_quality() {
        QUALITY_LADDER
            .iter()
            .copied()
            .filter(|&q| q < d.quality)
            .collect()
    } else {
        Vec::new()
    };

    // 「あとどれだけ足りないか」を言えるのは**実際に降りた段**で測った値だけ。
    // **JPEG は品質を下げてもサイズが単調に減らない区間がある**ので、最下段が
    // 最小とは限らない。等しいときは先に出たほう（＝より高い品質）を残す
    let mut smallest: Option<(u64, f32)> = None;
    let mut attempts = 1;
    for quality in rungs {
        opts.quality = quality;
        let bytes = encode_prepared(&prepared, &opts)?;
        attempts += 1;
        let len = bytes.len() as u64;
        if len <= max {
            warnings.push(reduced(d, max, len, quality, attempts));
            return Ok(done(
                bytes,
                d.format.effective_quality(quality),
                attempts,
                warnings,
            ));
        }
        if smallest.is_none_or(|(smallest, _)| len < smallest) {
            smallest = Some((len, quality));
        }
    }

    warnings.push(unreachable(d, max, first.len() as u64, attempts, smallest));
    Ok(done(first, asked, attempts, warnings))
}

/// 要求品質から落として収まった。**直すものは無い**ので hint は付けない。
///
/// `quality_used` は `outputs[].quality_used` と同じ値・同じ字面である
/// （`quality_number` を通す）。エージェントは 2 つを突き合わせる
fn reduced(d: &Derivation, max: u64, bytes: u64, quality: f32, attempts: u32) -> Warning {
    let used = d.format.effective_quality(quality).unwrap_or(quality);
    Warning::new(
        WarningCode::QualityReduced,
        format!(
            "--max-bytes {max} に収めるため品質を {} から {used} へ落としました（{bytes} バイト）",
            quality_number(d.quality)
        ),
    )
    .with_data("requested", quality_number(d.quality))
    .with_data("quality_used", quality_number(used))
    .with_data("max_bytes", max)
    .with_data("bytes", bytes)
    .with_data("attempts", attempts)
    .with_data("format", d.format.as_str())
}

/// `--max-bytes` に届かなかった。**書いたのは要求品質のもの**である。
///
/// 文面は「降りる段があったか」で分かれる。**下限まで降りたときにしか
/// 「下限まで落としても」とは言わない**——`--quality 20` は梯子の下限 25 より
/// 低いので 1 段も降りておらず、降りたふりをすると次の一手を誤らせる。
///
/// `smallest` は**実際に降りた段**で得た最小の大きさとその品質で、「あとどれ
/// だけ足りないか」が分かる唯一の値である。段を 1 つも降りていないとき
/// （PNG、要求品質が下限より低いとき）は入れない。書いたものと同じ数を「最小」と
/// 名乗ると「品質を落とせばあと少し」という読み方を誘うためで、これは
/// 2 つの枝に同じ理由で効く
fn unreachable(
    d: &Derivation,
    max: u64,
    bytes: u64,
    attempts: u32,
    smallest: Option<(u64, f32)>,
) -> Warning {
    let used = d.format.effective_quality(d.quality);
    let message = match (used, attempts) {
        (None, _) => format!(
            "{} は無損失で品質を持たないため、--max-bytes {max} に対して {bytes} バイトを\
             そのまま書きました",
            d.format.as_str()
        ),
        // 1 回で終わったのは、降りられる段が無かったからである
        (Some(used), 1) => format!(
            "要求品質 {used} より下に降りられる段がありません（梯子は {} まで）。\
             --max-bytes {max} に対して {bytes} バイトをそのまま書きました",
            bottom_rung()
        ),
        (Some(used), _) => format!(
            "品質を下限 {} まで落としても --max-bytes {max} に届かないので、\
             要求品質 {used} の {bytes} バイトを書きました",
            bottom_rung()
        ),
    };
    let hint = match d.format {
        // 既に AVIF なら、同じ寸法でこれより小さくなる形式が kiri に無い
        OutputFormat::Avif => "kiri resize で寸法を落としてください",
        OutputFormat::Jpeg => "kiri resize で寸法を落とすか、--format avif を試してください",
        OutputFormat::Png => {
            "PNG は無損失で品質を持ちません。--format jpeg / avif なら品質で収められます"
        }
    };
    let mut warning = Warning::new(WarningCode::MaxBytesUnreachable, message)
        .with_hint(hint)
        .with_data("max_bytes", max)
        .with_data("bytes", bytes)
        // 書いたものの品質。`QUALITY_REDUCED` と同じキーが同じ意味を持つ
        // （どちらも `outputs[].quality_used` と一致する）。PNG では null
        .with_data("quality_used", used.map(quality_number))
        .with_data("attempts", attempts)
        .with_data("format", d.format.as_str());
    if let Some((smallest_bytes, smallest_quality)) = smallest {
        let smallest_quality = d
            .format
            .effective_quality(smallest_quality)
            .unwrap_or(smallest_quality);
        warning = warning
            .with_data("smallest_bytes", smallest_bytes)
            .with_data("smallest_quality", quality_number(smallest_quality));
    }
    warning
}

/// 梯子のいちばん下の段。文面が数を手書きしないために引く
fn bottom_rung() -> f32 {
    *QUALITY_LADDER.last().expect("梯子が空ではない")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::image_io::save::encode;

    fn derivation(path: PathBuf, format: OutputFormat, icc: IccPolicy) -> Derivation {
        Derivation {
            path,
            format,
            quality: 75.0,
            effort: 6,
            background: [255, 255, 255],
            flatten: false,
            icc,
            max_bytes: None,
            resize: None,
            role: None,
        }
    }

    /// 圧縮しにくいノイズ。単色で梯子を回すと、どの段でも同じ大きさに収まって
    /// しまい「落とした段」が見えない
    fn noisy(width: u32, height: u32) -> RgbaImage {
        let mut state = 0x2545_F491_4F6C_DD1Du64;
        RgbaImage::from_fn(width, height, |_, _| {
            let mut next = || {
                state ^= state << 13;
                state ^= state >> 7;
                state ^= state << 17;
                (state >> 33) as u8
            };
            image::Rgba([next(), next(), next(), 255])
        })
    }

    /// `DERIVE_KEYS` のどのキーも、2 度目の値を断る。
    ///
    /// **`already_has` への書き忘れはコンパイラが何も言わない。** 落ちたキーだけが
    /// 黙って後勝ちに戻るので、一覧と突き合わせて総当たりする
    #[test]
    fn every_derive_key_refuses_a_second_value() {
        let samples = [
            ("width", "100"),
            ("height", "100"),
            ("fit", "cover"),
            ("allow_upscale", "true"),
            ("format", "png"),
            ("quality", "50"),
            ("effort", "3"),
            ("max_bytes", "500k"),
            ("role", "hero"),
        ];
        let covered: Vec<&str> = samples.iter().map(|(k, _)| *k).collect();
        assert_eq!(covered, DERIVE_KEYS, "一覧に載ったキーを試していない");
        for (key, value) in samples {
            let mut spec = DeriveSpec::default();
            spec.set(key, value).unwrap();
            let err = spec.set(key, value).unwrap_err();
            assert!(err.contains("2 回"), "{key}: {err}");
        }
    }

    /// 後勝ちを断った側の値は残らない。
    ///
    /// 「書いたのに効かない」を 1 つも残さないための検査である
    #[test]
    fn the_first_value_survives_when_a_duplicate_is_refused() {
        let mut spec = DeriveSpec::default();
        spec.set("width", "100").unwrap();
        assert!(spec.set("width", "250").is_err());
        assert_eq!(spec.width, Some(100));
    }

    /// 役目の札にパスは書けない。`{role}` はそのままファイル名の一部になる
    #[test]
    fn a_role_that_spells_a_path_is_refused() {
        for value in ["/tmp/kiri_escape_test", "../escaped", "a/b", "..", "."] {
            let mut spec = DeriveSpec::default();
            assert!(
                spec.set("role", value).is_err(),
                "通してはいけない: '{value}'"
            );
        }
        let mut spec = DeriveSpec::default();
        spec.set("role", "hero-2x").unwrap();
        assert_eq!(spec.role.as_deref(), Some("hero-2x"));
    }

    fn codes(warnings: &[Warning]) -> Vec<WarningCode> {
        warnings.iter().map(|w| w.code).collect()
    }

    fn bytes_at(img: &RgbaImage, d: &Derivation, quality: f32) -> u64 {
        let opts = SaveOptions {
            quality,
            ..d.save_options()
        };
        encode(img, &opts).unwrap().0.len() as u64
    }

    /// 要求品質では収まらず、梯子のどこかでは収まる上限を作る。
    ///
    /// **「基準の半分」のような割合で決め打ちにしない。** 素材が小さいと
    /// 下限まで落としても半分に届かず、達成を見るはずの検査が未達の道を通った
    /// まま緑になる（実際に一度そうなった）。下限で何バイトになるかを測ってから
    /// 決める
    fn reachable_budget(img: &RgbaImage, d: &Derivation) -> u64 {
        let baseline = bytes_at(img, d, d.quality);
        let floor = bytes_at(img, d, bottom_rung());
        assert!(
            floor < baseline,
            "品質を落としても縮まない素材では梯子を試せない（{floor} / {baseline}）"
        );
        (baseline + floor) / 2
    }

    /// 受け入れ基準 (a)。render は encode の結果に何も足さず、何も引かない。
    /// ここがずれると、Phase 19 の探索が見た大きさと書かれたファイルが食い違う
    #[test]
    fn render_with_one_derivation_writes_what_encode_returns() {
        let img = RgbaImage::from_fn(20, 16, |x, y| {
            image::Rgba([
                (x * 12) as u8,
                (y * 15) as u8,
                90,
                if x < 10 { 255 } else { 128 },
            ])
        });
        let dir = tempfile::tempdir().unwrap();
        for format in [OutputFormat::Png, OutputFormat::Jpeg, OutputFormat::Avif] {
            for icc in [IccPolicy::Embed, IccPolicy::None] {
                let name = format!("{}-{icc:?}.{}", format.as_str(), format.as_str());
                let d = derivation(dir.path().join("out").join(&name), format, icc);
                let (expected, expected_warnings) = encode(&img, &d.save_options()).unwrap();

                let rendered = render(&img, std::slice::from_ref(&d), false).unwrap();
                assert_eq!(rendered.len(), 1);
                let written = std::fs::read(&d.path).unwrap();
                assert_eq!(written, expected, "{name}");
                let report = &rendered[0].report;
                assert_eq!(report.bytes, written.len() as u64, "{name}");
                assert_eq!(report.icc, icc.signal(format), "{name}");
                assert_eq!(report.path, d.path.display().to_string());
                assert_eq!((report.width, report.height), (20, 16));
                assert_eq!(
                    codes(&rendered[0].warnings),
                    codes(&expected_warnings),
                    "{name}"
                );
                // 受け入れ基準 (d)。**--max-bytes を渡さない実行は 1 回で終わる。**
                // ここが 1 を超えたら、指定していない機能が時間を食っている
                assert_eq!(report.attempts, 1, "{name}");
                assert_eq!(
                    report.quality_used,
                    format.effective_quality(75.0).map(quality_number),
                    "{name}"
                );
            }
        }
    }

    /// dry-run は書かないだけで、報告は本番と同じ値になる
    #[test]
    fn dry_run_encodes_but_writes_nothing() {
        let img = RgbaImage::from_pixel(8, 8, image::Rgba([10, 20, 30, 255]));
        let dir = tempfile::tempdir().unwrap();
        for format in [OutputFormat::Png, OutputFormat::Jpeg, OutputFormat::Avif] {
            let d = derivation(
                dir.path().join(format!("dry.{}", format.as_str())),
                format,
                IccPolicy::Embed,
            );
            let rendered = render(&img, std::slice::from_ref(&d), true).unwrap();
            assert!(!d.path.exists(), "{format:?}");
            let (expected, _) = encode(&img, &d.save_options()).unwrap();
            assert_eq!(rendered[0].report.bytes, expected.len() as u64);
            assert_eq!(rendered[0].report.icc, IccPolicy::Embed.signal(format));
        }
    }

    /// 要求品質で既に収まっているなら、梯子は 1 段も降りない。
    ///
    /// **ここが動くと `--max-bytes` は「付けても損の無い指定」でなくなる。**
    /// 余裕のある上限を一律に付けたセットで、全件が 8 倍の時間を払うことになる
    #[test]
    fn a_budget_that_already_fits_changes_nothing() {
        let img = noisy(64, 64);
        let dir = tempfile::tempdir().unwrap();
        let mut d = derivation(
            dir.path().join("fits.jpg"),
            OutputFormat::Jpeg,
            IccPolicy::Embed,
        );
        let (plain, _) = encode(&img, &d.save_options()).unwrap();

        d.max_bytes = Some(plain.len() as u64);
        let rendered = render(&img, std::slice::from_ref(&d), false).unwrap();
        assert_eq!(
            std::fs::read(&d.path).unwrap(),
            plain,
            "上限ちょうどは収まり"
        );
        assert_eq!(rendered[0].report.attempts, 1);
        assert_eq!(rendered[0].report.quality_used, Some(75.0));
        assert!(
            rendered[0].warnings.is_empty(),
            "{:?}",
            rendered[0].warnings
        );
    }

    /// 受け入れ基準 (a) と (i)。収まった段は梯子の値で、報告と実ファイルの
    /// 両方が上限以下になる
    #[test]
    fn the_ladder_stops_at_the_first_rung_that_fits() {
        let img = noisy(200, 200);
        let dir = tempfile::tempdir().unwrap();
        let mut d = derivation(
            dir.path().join("budget.jpg"),
            OutputFormat::Jpeg,
            IccPolicy::Embed,
        );
        let max = reachable_budget(&img, &d);
        d.max_bytes = Some(max);

        let rendered = render(&img, std::slice::from_ref(&d), false).unwrap();
        let report = &rendered[0].report;
        let written = std::fs::read(&d.path).unwrap();
        assert!(
            report.bytes <= max,
            "報告 {} が上限 {max} を超えた",
            report.bytes
        );
        assert_eq!(
            written.len() as u64,
            report.bytes,
            "実ファイルと報告がずれた"
        );

        let quality = report.quality_used.expect("JPEG は品質を持つ") as f32;
        assert!(
            QUALITY_LADDER.contains(&quality) && quality < 75.0,
            "{quality} は要求品質より下の梯子の段ではない"
        );
        assert_eq!(
            report.attempts,
            1 + QUALITY_LADDER
                .iter()
                .filter(|&&q| q < 75.0)
                .position(|&q| q == quality)
                .unwrap() as u32
                + 1,
            "止まった段より後まで試している"
        );

        assert_eq!(
            codes(&rendered[0].warnings),
            vec![WarningCode::QualityReduced]
        );
        let data = &rendered[0].warnings[0].data;
        assert_eq!(data["requested"], 75.0);
        // 報告と警告で同じ数が同じ字面で出る（`quality_number`）
        assert_eq!(data["quality_used"], report.quality_used.unwrap());
        assert_eq!(data["max_bytes"], max);
        assert_eq!(data["bytes"], report.bytes);
        assert_eq!(data["attempts"], report.attempts);
        assert_eq!(data["format"], "jpeg");
        assert!(rendered[0].warnings[0].hint.is_none(), "直すものは無い");
    }

    /// 受け入れ基準 (b)。**未達なら要求品質のものを書く。**
    /// 書いたバイト列が `--max-bytes` 無しの出力と 1 バイトも違わないことで、
    /// 「どうせ制約は破れているので画質まで捨てない」という決定を固定する
    #[test]
    fn an_unreachable_budget_writes_the_requested_quality_untouched() {
        let img = noisy(200, 200);
        let dir = tempfile::tempdir().unwrap();
        let mut d = derivation(
            dir.path().join("tiny.jpg"),
            OutputFormat::Jpeg,
            IccPolicy::Embed,
        );
        let (plain, _) = encode(&img, &d.save_options()).unwrap();
        d.max_bytes = Some(64);

        let rendered = render(&img, std::slice::from_ref(&d), false).unwrap();
        assert_eq!(std::fs::read(&d.path).unwrap(), plain);
        let report = &rendered[0].report;
        assert_eq!(report.bytes, plain.len() as u64);
        assert_eq!(report.quality_used, Some(75.0), "要求品質へ戻る");
        // 要求品質の 1 回 + 75 より下の段の数
        assert_eq!(
            report.attempts,
            1 + QUALITY_LADDER.iter().filter(|&&q| q < 75.0).count() as u32
        );

        assert_eq!(
            codes(&rendered[0].warnings),
            vec![WarningCode::MaxBytesUnreachable]
        );
        let w = &rendered[0].warnings[0];
        assert_eq!(w.data["max_bytes"], 64);
        assert_eq!(
            w.data["bytes"], report.bytes,
            "書いたファイルの大きさを言う"
        );
        assert_eq!(w.data["attempts"], report.attempts);
        assert_eq!(w.data["format"], "jpeg");
        assert_eq!(
            w.data["quality_used"],
            report.quality_used.unwrap(),
            "書いたものの品質は報告と同じ数である"
        );

        // 最小は**実際に降りた段**のものである。上限を超えていることまで見ないと、
        // 「収まったのに未達と言っている」実装を通してしまう
        let smallest_bytes = w.data["smallest_bytes"].as_u64().unwrap();
        let smallest_quality = w.data["smallest_quality"].as_f64().unwrap() as f32;
        assert!(smallest_bytes > 64, "収まっているのに未達と言っている");
        assert!(
            QUALITY_LADDER.contains(&smallest_quality) && smallest_quality < 75.0,
            "{smallest_quality} は降りた段ではない"
        );
        assert_eq!(
            smallest_bytes,
            bytes_at(&img, &d, smallest_quality),
            "smallest_bytes が smallest_quality で得た大きさと違う"
        );
        assert!(w.hint.is_some());
    }

    /// 受け入れ基準 (e)。PNG は無損失なので段を降りない。
    /// ファイルは `--max-bytes` 無しの PNG と 1 バイトも変わらない
    #[test]
    fn png_never_walks_down_the_ladder() {
        let img = noisy(64, 64);
        let dir = tempfile::tempdir().unwrap();
        let mut d = derivation(
            dir.path().join("lossless.png"),
            OutputFormat::Png,
            IccPolicy::Embed,
        );
        let (plain, _) = encode(&img, &d.save_options()).unwrap();
        d.max_bytes = Some(32);

        let rendered = render(&img, std::slice::from_ref(&d), false).unwrap();
        assert_eq!(std::fs::read(&d.path).unwrap(), plain);
        let report = &rendered[0].report;
        assert_eq!(report.attempts, 1, "段を降りてはいけない");
        assert_eq!(report.quality_used, None, "PNG に品質は無い");
        assert_eq!(
            codes(&rendered[0].warnings),
            vec![WarningCode::MaxBytesUnreachable]
        );
        let w = &rendered[0].warnings[0];
        assert!(
            !w.data.contains_key("smallest_bytes") && !w.data.contains_key("smallest_quality"),
            "段を降りていないので最小は語れない: {:?}",
            w.data
        );
        assert!(
            w.data["quality_used"].is_null(),
            "PNG の品質は報告と同じく null: {:?}",
            w.data
        );
    }

    /// 受け入れ基準 (c)。同じ入力からは毎回同じ着地点になる。
    /// 梯子が時刻やタイムアウトを見た瞬間にここが割れる。
    ///
    /// **段で止まる経路を必ず通す。** 未達の上限を渡すと 3 回とも「要求品質へ
    /// 戻した」同じ答えになり、探索を 1 度も通らずに一致してしまう
    #[test]
    fn the_landing_spot_is_the_same_every_time() {
        let img = noisy(160, 160);
        let dir = tempfile::tempdir().unwrap();
        let mut d = derivation(
            dir.path().join("stable.jpg"),
            OutputFormat::Jpeg,
            IccPolicy::Embed,
        );
        d.max_bytes = Some(reachable_budget(&img, &d));

        let once = |d: &Derivation| {
            let r = render(&img, std::slice::from_ref(d), true).unwrap();
            (
                r[0].report.quality_used,
                r[0].report.attempts,
                r[0].report.bytes,
            )
        };
        let first = once(&d);
        assert!(first.1 > 1, "段を 1 つも降りていない: {first:?}");
        assert!(
            first.0.unwrap() < d.quality as f64,
            "要求品質のまま止まっている: {first:?}"
        );
        assert_eq!(once(&d), first);
        assert_eq!(once(&d), first);
    }

    /// 要求品質より下に段が無ければ、探索は 1 回で終わる。
    /// **`--quality 20` で 7 段を試すのは、どの段も要求品質より高いという
    /// 意味になる**——収めるどころか大きくしにいくことになる。
    ///
    /// 降りていない以上、`smallest_*` は名乗らない（PNG と同じ理由）。文面も
    /// 「下限まで落としても」とは言わない
    #[test]
    fn a_quality_below_the_bottom_rung_has_nowhere_to_descend() {
        let img = noisy(64, 64);
        let dir = tempfile::tempdir().unwrap();
        let mut d = derivation(
            dir.path().join("low.jpg"),
            OutputFormat::Jpeg,
            IccPolicy::Embed,
        );
        d.quality = 20.0;
        d.max_bytes = Some(64);

        let rendered = render(&img, std::slice::from_ref(&d), true).unwrap();
        assert_eq!(rendered[0].report.attempts, 1);
        assert_eq!(rendered[0].report.quality_used, Some(20.0));
        let w = &rendered[0].warnings[0];
        assert_eq!(w.code, WarningCode::MaxBytesUnreachable);
        assert!(
            !w.data.contains_key("smallest_bytes") && !w.data.contains_key("smallest_quality"),
            "降りた段が無いのに最小を名乗っている: {:?}",
            w.data
        );
        assert_eq!(w.data["quality_used"], 20.0);
        assert!(
            !w.message.contains("下限"),
            "1 段も降りていないのに下限まで降りたと言っている: {}",
            w.message
        );
        assert!(
            w.message.contains("降りられる段がありません"),
            "なぜ 1 回で終わったかを言っていない: {}",
            w.message
        );
    }
}
