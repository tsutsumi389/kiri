//! `kiri compose --fail-on` — **組んだ結果を機械が合否で読めるようにする。**
//!
//! # なぜ `compliance::Metric` へ相乗りしないか
//!
//! あちらの doc は「名前は結果 JSON の `mask.*` のキーそのままである」と言って
//! いる。compose が返すのは `compose.layers[].*` で、同じ表に混ぜると**その一文が
//! 嘘になる。** それ以上に効くのは次の失敗で、混ぜると
//! `kiri cutout --fail-on text_overflow` が**書式として通ってしまう。**
//! cutout には文字が 1 つも無いので、その門は**永久に発火しない。** 書いた本人は
//! 合格が出続けるのを見て「通っている」と読む——`compliance::Metric::range` が
//! 値域を断る理由として挙げているのと、同じ形の事故である。
//!
//! 演算子（`compliance::Operator`）は共有する。**関係の綴りまで 2 通り持つと、
//! 受け手が `>` と `gt` の対応表を 2 度覚えることになる。**

use serde::Serialize;
use serde_json::Value;

use crate::commands::lint::SKIPPED;
use crate::compliance::{FAIL, Operator, PASS};
use crate::compose::display::{MIN_DISPLAY_SIZE, Reason};
use crate::compose::measure::{LayerReport, MAX_OBSCURED, MAX_SUBJECT_OVERLAP, MIN_CONTRAST};
use crate::error::ErrorCode;

/// `default` という予約語。`compliance::DEFAULT_TOKEN` と同じ綴りである
/// ——同じ意味の語に 2 つの綴りを与えない。
pub use crate::compliance::DEFAULT_TOKEN;

/// 見る指標。**名前は結果 JSON の `compose.layers[]` のキーそのままである。**
///
/// `display_*` の 3 つだけは例外で、`layers[].display[]` の値を全層・全表示に
/// わたって畳んだものである（キーが入れ子なので、そのままの綴りでは書けない）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Metric {
    TextOverflow,
    TextContrast,
    TextObscured,
    LayerOverlap,
    OutsideSafeArea,
    /// 全文字層・全表示での `size_px` の最小
    DisplayTextSize,
    /// 全文字層・全表示での `contrast` の最小（null は数えない）
    DisplayContrast,
    /// `strokes_lost` に当たった表示が 1 つでもあるか
    DisplayStrokesLost,
}

impl Metric {
    /// **この並びがそのまま `checks[]` の並びになる。** 指定の順に依存させない
    /// （`compliance::Metric::ALL` と同じ理由）。
    ///
    /// `display_*` は**末尾に足す。** 先頭へ混ぜると、`--profile` を渡さない
    /// 実行の `checks[]` の並びは同じでも、指標の綴りの一覧（`kiri schema` の
    /// `accepts`）の既存の位置が動く。
    pub const ALL: [Metric; 8] = [
        Metric::TextOverflow,
        Metric::TextContrast,
        Metric::TextObscured,
        Metric::LayerOverlap,
        Metric::OutsideSafeArea,
        Metric::DisplayTextSize,
        Metric::DisplayContrast,
        Metric::DisplayStrokesLost,
    ];

    pub const fn as_str(self) -> &'static str {
        match self {
            Metric::TextOverflow => "text_overflow",
            Metric::TextContrast => "text_contrast",
            Metric::TextObscured => "text_obscured",
            Metric::LayerOverlap => "layer_overlap",
            Metric::OutsideSafeArea => "outside_safe_area",
            Metric::DisplayTextSize => "display_text_size",
            Metric::DisplayContrast => "display_contrast",
            Metric::DisplayStrokesLost => "display_strokes_lost",
        }
    }

    pub fn named(name: &str) -> Option<Metric> {
        Metric::ALL.into_iter().find(|m| m.as_str() == name)
    }

    /// 真偽で報告される指標。しきい値を置けない。
    pub const fn is_flag(self) -> bool {
        matches!(self, Metric::OutsideSafeArea | Metric::DisplayStrokesLost)
    }

    pub const fn flag_meaning(self) -> Option<&'static str> {
        match self {
            Metric::OutsideSafeArea => Some("safe_area の外へ出ていたら不合格"),
            Metric::DisplayStrokesLost => {
                Some("--profile の縮小表示で線が潰れた文字があれば不合格")
            }
            _ => None,
        }
    }

    /// `--profile` の縮小表示でしか測らない指標か。
    pub const fn needs_profile(self) -> bool {
        matches!(
            self,
            Metric::DisplayTextSize | Metric::DisplayContrast | Metric::DisplayStrokesLost
        )
    }

    /// `default` が当てる条件。**較正済みの値はここ 1 箇所から来る。**
    ///
    /// `measure` の定数をそのまま引くので、しきい値を動かしたときに
    /// `default` だけが古い線で合否を出すことが起きない。
    const fn calibrated(self) -> Option<(Operator, f64)> {
        match self {
            Metric::TextOverflow => Some((Operator::Gt, 0.0)),
            Metric::TextContrast => Some((Operator::Lt, MIN_CONTRAST)),
            Metric::TextObscured => Some((Operator::Gt, MAX_OBSCURED)),
            Metric::LayerOverlap => Some((Operator::Gt, MAX_SUBJECT_OVERLAP)),
            Metric::DisplayTextSize => Some((Operator::Lt, MIN_DISPLAY_SIZE)),
            Metric::DisplayContrast => Some((Operator::Lt, MIN_CONTRAST)),
            // 真偽の指標はしきい値を取らない
            Metric::OutsideSafeArea | Metric::DisplayStrokesLost => None,
        }
    }

    /// この spec にその指標を当てる相手が居るか。
    ///
    /// **「測れなかった」と「測る対象がそもそも無い」を分ける。** 文字を 1 つも
    /// 置かない spec に `text_contrast` を当てても、落とせる相手が居ない——
    /// それを `unmeasurable` として不合格に数えると、`--fail-on default` が
    /// **誰にも通せない門**になる。
    ///
    /// `kiri lint` が規定の無い条件を `checks[]` に 1 行も出さないのと同じ
    /// 考え方だが、こちらは**書いた条件を黙って消さない**ために行は出し、
    /// 状態を `skipped` にする。`unmeasurable` のほうは残す——文字が在るのに
    /// 値が出ない（透過のまま書く実行のコントラスト）は、人が見るべき状態である
    fn applicable(self, layers: &[LayerReport]) -> bool {
        let text = || layers.iter().any(|l| l.kind == "text");
        match self {
            Metric::TextOverflow | Metric::TextObscured => text(),
            Metric::TextContrast => layers
                .iter()
                .any(|l| l.kind == "text" && l.role.wants_contrast()),
            Metric::LayerOverlap => {
                text()
                    && layers
                        .iter()
                        .any(|l| l.role == crate::compose::Role::Subject)
            }
            // spec が範囲を書いていなければ、どの層にも値が入らない
            Metric::OutsideSafeArea => layers.iter().any(|l| l.outside_safe_area.is_some()),
            // 縮小表示は読めることを要求する文字にだけ測る。**`--profile` が
            // 無い実行はここへ来ない**（`evaluate` が先に `unmeasurable` にする）
            Metric::DisplayTextSize | Metric::DisplayContrast | Metric::DisplayStrokesLost => {
                layers
                    .iter()
                    .any(|l| l.kind == "text" && l.role.wants_contrast())
            }
        }
    }

    /// 全層を 1 つの数へ畳む。**最も悪い層を採る。**
    ///
    /// 平均を採ると、1 行だけ読めない構図が他の行に薄められて通る。
    /// 測れた層が 1 つも無ければ `None`——`checks[].status` が
    /// `unmeasurable` になり、**合格としては数えない**
    fn worst(self, layers: &[LayerReport]) -> Option<f64> {
        let displays = || layers.iter().flat_map(|l| l.display.iter().flatten());
        let values: Vec<f64> = match self {
            Metric::TextOverflow => layers.iter().filter_map(|l| l.text_overflow).collect(),
            Metric::TextContrast => layers.iter().filter_map(|l| l.text_contrast).collect(),
            Metric::TextObscured => layers.iter().filter_map(|l| l.text_obscured).collect(),
            Metric::LayerOverlap => layers.iter().filter_map(|l| l.layer_overlap).collect(),
            Metric::OutsideSafeArea => layers
                .iter()
                .filter_map(|l| l.outside_safe_area.map(|out| if out { 1.0 } else { 0.0 }))
                .collect(),
            Metric::DisplayTextSize => displays().map(|d| d.size_px).collect(),
            Metric::DisplayContrast => displays().filter_map(|d| d.contrast).collect(),
            Metric::DisplayStrokesLost => displays()
                .map(|d| {
                    if d.reasons.contains(&Reason::StrokesLost) {
                        1.0
                    } else {
                        0.0
                    }
                })
                .collect(),
        };
        let values = values.into_iter();
        match self {
            // 小さいほど悪い
            Metric::TextContrast | Metric::DisplayTextSize | Metric::DisplayContrast => {
                values.reduce(f64::min)
            }
            _ => values.reduce(f64::max),
        }
    }
}

/// 書ける指標の綴り。ヘルプと `kiri schema` が引く。
pub const FAIL_ON_METRICS: [&str; Metric::ALL.len()] = spellings();

const fn spellings() -> [&'static str; Metric::ALL.len()] {
    let mut out = [""; Metric::ALL.len()];
    let mut i = 0;
    while i < Metric::ALL.len() {
        out[i] = Metric::ALL[i].as_str();
        i += 1;
    }
    out
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Rule {
    Number {
        metric: Metric,
        operator: Operator,
        threshold: f64,
    },
    Flag {
        metric: Metric,
    },
}

impl Rule {
    const fn metric(self) -> Metric {
        match self {
            Rule::Number { metric, .. } | Rule::Flag { metric } => metric,
        }
    }
}

/// 解いた `--fail-on`。
#[derive(Debug, Clone)]
pub struct FailOn {
    spec: String,
    default: bool,
    rules: Vec<Rule>,
}

impl FailOn {
    pub fn spec(&self) -> &str {
        &self.spec
    }

    /// 書式を解く。**組む前に終わっている**——綴り違いに全部組んでから
    /// 気づく形にはしない（`compliance::FailOn` と同じ約束）。
    ///
    /// **`default` は旗として持つ。** 4 つの条件へ展開してから重複を見ると、
    /// `default,text_contrast<4.5` が「`text_contrast` に 2 つの条件」として
    /// 断られる——`--help` が例として挙げている綴りそのものである。
    /// `compliance::FailOn` は旗にしてあり、README も
    /// 「明示が勝つ」と書いている。**同じ語に 2 つの効き方を持たせない。**
    pub fn parse(spec: &str) -> Result<FailOn, String> {
        let mut default = false;
        let mut rules: Vec<Rule> = Vec::new();
        for token in spec.split(',') {
            let token = token.trim();
            if token.is_empty() {
                return Err(format!(
                    "'{spec}' に空の項目があります（{DEFAULT_TOKEN} か \
                     <指標><演算子><値> をカンマで並べてください）"
                ));
            }
            if token == DEFAULT_TOKEN {
                if default {
                    return Err(format!("'{DEFAULT_TOKEN}' が 2 度書かれています"));
                }
                default = true;
                continue;
            }
            let rule = parse_rule(token)?;
            // **同じ指標に 2 つのしきい値を置かせない。** どちらが勝つかという
            // 規則が増えるだけで、意図した条件は 1 つに書ける
            if rules.iter().any(|r| r.metric() == rule.metric()) {
                return Err(format!(
                    "'{}' に 2 つの条件が置かれています",
                    rule.metric().as_str()
                ));
            }
            rules.push(rule);
        }
        if !default && rules.is_empty() {
            return Err(format!("'{spec}' に条件がありません"));
        }
        Ok(FailOn {
            spec: spec.to_string(),
            default,
            rules,
        })
    }

    /// その指標に当てる条件。**明示 > default** の 1 本である。
    ///
    /// **`default` は `--profile` の無い実行に `display_*` を足さない。**
    /// 足すと、測っていない 3 行が `unmeasurable` で並び、これまで通っていた
    /// `--fail-on default` が一斉に exit 5 へ変わる——`--profile` を渡さない
    /// 実行の結果を動かさないという約束（12-phase-31.md §12.1 の 3）が破れる。
    /// 明示して書いた `display_*` は、測っていないので `unmeasurable` として
    /// 残す（`evaluate`）。
    fn rule_for(&self, metric: Metric, profiled: bool) -> Option<Rule> {
        if let Some(rule) = self.rules.iter().find(|r| r.metric() == metric) {
            return Some(*rule);
        }
        if !self.default || (metric.needs_profile() && !profiled) {
            return None;
        }
        Some(match metric.calibrated() {
            Some((operator, threshold)) => Rule::Number {
                metric,
                operator,
                threshold,
            },
            None => Rule::Flag { metric },
        })
    }

    /// 測りに照らす。**`checks[]` の並びは `Metric::ALL` の順である。**
    ///
    /// `profiled` は `--profile` を渡したか。渡していなければ縮小表示を測って
    /// いないので、明示した `display_*` は相手の有無によらず `unmeasurable`
    /// になる——**測っていないものを `skipped`（合格）にしない**。
    pub fn evaluate(&self, layers: &[LayerReport], profiled: bool) -> GateReport {
        let mut checks = Vec::new();
        for metric in Metric::ALL {
            let Some(rule) = self.rule_for(metric, profiled) else {
                continue;
            };
            let applicable = metric.applicable(layers) || (metric.needs_profile() && !profiled);
            let actual = metric.worst(layers);
            let (operator, threshold) = match rule {
                Rule::Number {
                    operator,
                    threshold,
                    ..
                } => (Some(operator), Some(threshold)),
                Rule::Flag { .. } => (None, None),
            };
            // 測れなかったものは合格にしない。**「検査できなかったので人が
            // 見てほしい」は exit 5 の意味そのものである**（`LintReport.passed`
            // と同じ定義）
            let status = match (actual, rule) {
                // 相手が居ないものは飛ばす。**不合格には数えない**
                (None, _) if !applicable => SKIPPED,
                (None, _) => crate::compliance::UNMEASURABLE,
                (
                    Some(v),
                    Rule::Number {
                        operator,
                        threshold,
                        ..
                    },
                ) => {
                    if operator.touched(v, threshold) {
                        FAIL
                    } else {
                        PASS
                    }
                }
                (Some(v), Rule::Flag { .. }) => {
                    if v > 0.0 {
                        FAIL
                    } else {
                        PASS
                    }
                }
            };
            checks.push(Check {
                metric: metric.as_str(),
                operator: operator.map(Operator::as_str),
                threshold: threshold.and_then(number),
                // **真偽の指標は真偽で返す。** 0.0/1.0 にすると、同じ事実が
                // `layers[].outside_safe_area`（bool）と `checks[].actual`
                // （数）の 2 通りで配られる
                actual: actual.and_then(|v| {
                    if metric.is_flag() {
                        Some(Value::Bool(v > 0.0))
                    } else {
                        number(v)
                    }
                }),
                status,
            });
        }
        // **`skipped` は合格として数える。** 相手が居ないことは成果物の問題では
        // ない。`fail` と `unmeasurable` だけが人を呼ぶ
        let passed = checks
            .iter()
            .all(|c| c.status == PASS || c.status == SKIPPED);
        GateReport {
            fail_on: self.spec.clone(),
            passed,
            code: (!passed).then_some(ErrorCode::QualityGateFailed),
            checks,
        }
    }
}

fn parse_rule(token: &str) -> Result<Rule, String> {
    for (spelling, operator) in Operator::SPELLINGS {
        let Some((name, rhs)) = token.split_once(spelling) else {
            continue;
        };
        let name = name.trim();
        let metric = Metric::named(name).ok_or_else(|| unknown(name))?;
        if metric.is_flag() {
            return Err(format!(
                "'{name}' は真偽の指標なので、しきい値を置けません（{}）",
                metric.flag_meaning().unwrap_or("")
            ));
        }
        let threshold: f64 = rhs
            .trim()
            .parse()
            .map_err(|_| format!("'{}' を数値として読めません", rhs.trim()))?;
        if !threshold.is_finite() {
            return Err(format!("'{}' は有限な数値ではありません", rhs.trim()));
        }
        return Ok(Rule::Number {
            metric,
            operator,
            threshold,
        });
    }

    let metric = Metric::named(token).ok_or_else(|| unknown(token))?;
    if !metric.is_flag() {
        return Err(format!(
            "'{token}' は数値の指標なので、演算子としきい値が要ります（例 {token}>0）"
        ));
    }
    Ok(Rule::Flag { metric })
}

fn unknown(name: &str) -> String {
    let suggestion = crate::batch::closest(name, &FAIL_ON_METRICS);
    match suggestion {
        Some(s) => format!(
            "'{name}' は compose の指標にありません（'{s}' の綴り違いでは\
                            ありませんか）"
        ),
        None => format!(
            "'{name}' は compose の指標にありません（書けるのは {}）",
            FAIL_ON_METRICS.join(", ")
        ),
    }
}

fn number(value: f64) -> Option<Value> {
    serde_json::Number::from_f64(value).map(Value::Number)
}

/// 合否。**`LintReport` と同じ形**——検査は成功しているので `ErrorBody` は返さず、
/// 名乗りはここの `code` が行う。
#[derive(Debug, Clone, Serialize)]
pub struct GateReport {
    /// 利用者が書いた文字列そのまま
    pub fail_on: String,
    pub passed: bool,
    /// 不合格のときだけ名乗る code。合格なら null
    #[serde(skip_serializing_if = "Option::is_none")]
    pub code: Option<ErrorCode>,
    /// 検査したものを全部載せる（`pass` も）
    pub checks: Vec<Check>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Check {
    pub metric: &'static str,
    /// 真偽の指標では null
    #[serde(skip_serializing_if = "Option::is_none")]
    pub operator: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub threshold: Option<Value>,
    /// 測れなかったら null
    #[serde(skip_serializing_if = "Option::is_none")]
    pub actual: Option<Value>,
    /// `pass` / `fail` / `unmeasurable`
    pub status: &'static str,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compose::Role;
    use crate::compose::display::DisplayReport;

    fn text(display: Option<Vec<DisplayReport>>) -> LayerReport {
        LayerReport {
            id: "t".to_string(),
            kind: "text",
            role: Role::Heading,
            rect: [0.0, 0.0, 10.0, 10.0],
            placed: [0.0, 0.0, 10.0, 10.0],
            opaque_rect: None,
            line_widths: Some(vec![10.0]),
            text_overflow: Some(0.0),
            text_contrast: Some(21.0),
            text_obscured: Some(0.0),
            layer_overlap: None,
            outside_safe_area: None,
            align: None,
            font: None,
            display,
        }
    }

    fn shown(size_px: f64, contrast: Option<f64>, reasons: Vec<Reason>) -> DisplayReport {
        DisplayReport {
            name: "mobile_list",
            width: 144,
            size_px,
            contrast,
            legible: reasons.is_empty(),
            reasons,
            against: None,
        }
    }

    fn metrics(report: &GateReport) -> Vec<&'static str> {
        report.checks.iter().map(|c| c.metric).collect()
    }

    /// **`--profile` の無い `default` は縮小表示の行を足さない**——足すと、
    /// これまで通っていた `--fail-on default` が `unmeasurable` で落ちる。
    #[test]
    fn default_without_a_profile_keeps_the_old_checks() {
        let gate = FailOn::parse("default").unwrap();
        let report = gate.evaluate(&[text(None)], false);
        // 相手の居ない行も `skipped` として並ぶが、`display_*` は 1 行も無い
        assert_eq!(
            metrics(&report),
            [
                "text_overflow",
                "text_contrast",
                "text_obscured",
                "layer_overlap",
                "outside_safe_area"
            ]
        );
        assert!(report.passed);
    }

    /// profile があれば `default` が 3 行を足し、最も悪い表示で判定する。
    #[test]
    fn default_with_a_profile_gates_the_worst_display() {
        let gate = FailOn::parse("default").unwrap();
        let layer = text(Some(vec![
            shown(9.6, Some(11.2), vec![Reason::TooSmall]),
            shown(7.2, None, vec![Reason::TooSmall, Reason::StrokesLost]),
        ]));
        let report = gate.evaluate(&[layer], true);
        let check = |m: &str| report.checks.iter().find(|c| c.metric == m).unwrap();
        assert_eq!(check("display_text_size").status, FAIL);
        assert_eq!(
            check("display_text_size").actual,
            Some(serde_json::json!(7.2))
        );
        // null は数えない
        assert_eq!(check("display_contrast").status, PASS);
        assert_eq!(check("display_strokes_lost").status, FAIL);
        assert_eq!(
            check("display_strokes_lost").actual,
            Some(Value::Bool(true))
        );
        assert!(!report.passed);
    }

    /// 明示した `display_*` は、profile が無ければ測っていないので合格にしない。
    #[test]
    fn an_explicit_display_metric_without_a_profile_is_unmeasurable() {
        let gate = FailOn::parse("display_strokes_lost").unwrap();
        let report = gate.evaluate(&[], false);
        assert_eq!(report.checks[0].status, crate::compliance::UNMEASURABLE);
        assert!(!report.passed);

        // profile があって文字が無ければ、相手が居ないので飛ばす
        let report = gate.evaluate(&[], true);
        assert_eq!(report.checks[0].status, SKIPPED);
        assert!(report.passed);
    }
}
