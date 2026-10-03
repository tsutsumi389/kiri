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

use crate::compliance::{FAIL, Operator, PASS};
use crate::compose::measure::{LayerReport, MAX_SUBJECT_OVERLAP, MIN_CONTRAST};
use crate::error::ErrorCode;

/// `default` という予約語。`compliance::DEFAULT_TOKEN` と同じ綴りである
/// ——同じ意味の語に 2 つの綴りを与えない。
pub use crate::compliance::DEFAULT_TOKEN;

/// 見る指標。**名前は結果 JSON の `compose.layers[]` のキーそのままである。**
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Metric {
    TextOverflow,
    TextContrast,
    LayerOverlap,
    OutsideSafeArea,
}

impl Metric {
    /// **この並びがそのまま `checks[]` の並びになる。** 指定の順に依存させない
    /// （`compliance::Metric::ALL` と同じ理由）。
    pub const ALL: [Metric; 4] = [
        Metric::TextOverflow,
        Metric::TextContrast,
        Metric::LayerOverlap,
        Metric::OutsideSafeArea,
    ];

    pub const fn as_str(self) -> &'static str {
        match self {
            Metric::TextOverflow => "text_overflow",
            Metric::TextContrast => "text_contrast",
            Metric::LayerOverlap => "layer_overlap",
            Metric::OutsideSafeArea => "outside_safe_area",
        }
    }

    pub fn named(name: &str) -> Option<Metric> {
        Metric::ALL.into_iter().find(|m| m.as_str() == name)
    }

    /// 真偽で報告される指標。しきい値を置けない。
    pub const fn is_flag(self) -> bool {
        matches!(self, Metric::OutsideSafeArea)
    }

    pub const fn flag_meaning(self) -> Option<&'static str> {
        match self {
            Metric::OutsideSafeArea => Some("safe_area の外へ出ていたら不合格"),
            _ => None,
        }
    }

    /// `default` が当てる条件。**較正済みの値はここ 1 箇所から来る。**
    ///
    /// `measure` の定数をそのまま引くので、しきい値を動かしたときに
    /// `default` だけが古い線で合否を出すことが起きない。
    const fn calibrated(self) -> Option<(Operator, f64)> {
        match self {
            Metric::TextOverflow => Some((Operator::Gt, 0.0)),
            Metric::TextContrast => Some((Operator::Lt, MIN_CONTRAST)),
            Metric::LayerOverlap => Some((Operator::Gt, MAX_SUBJECT_OVERLAP)),
            // 真偽の指標はしきい値を取らない
            Metric::OutsideSafeArea => None,
        }
    }

    /// 全層を 1 つの数へ畳む。**最も悪い層を採る。**
    ///
    /// 平均を採ると、1 行だけ読めない構図が他の行に薄められて通る。
    /// 測れた層が 1 つも無ければ `None`——`checks[].status` が
    /// `unmeasurable` になり、**合格としては数えない**
    fn worst(self, layers: &[LayerReport]) -> Option<f64> {
        let values = layers.iter().filter_map(|l| match self {
            Metric::TextOverflow => l.text_overflow,
            Metric::TextContrast => l.text_contrast,
            Metric::LayerOverlap => l.layer_overlap,
            Metric::OutsideSafeArea => l.outside_safe_area.map(|out| if out { 1.0 } else { 0.0 }),
        });
        match self {
            // 小さいほど悪い
            Metric::TextContrast => values.fold(None, |acc: Option<f64>, v| {
                Some(acc.map_or(v, |a| a.min(v)))
            }),
            _ => values.fold(None, |acc: Option<f64>, v| {
                Some(acc.map_or(v, |a| a.max(v)))
            }),
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
    rules: Vec<Rule>,
}

impl FailOn {
    pub fn spec(&self) -> &str {
        &self.spec
    }

    /// 書式を解く。**組む前に終わっている**——綴り違いに全部組んでから
    /// 気づく形にはしない（`compliance::FailOn` と同じ約束）。
    pub fn parse(spec: &str) -> Result<FailOn, String> {
        let mut rules: Vec<Rule> = Vec::new();
        for token in spec.split(',') {
            let token = token.trim();
            if token.is_empty() {
                return Err(format!(
                    "'{spec}' に空の項目があります（{DEFAULT_TOKEN} か \
                     <指標><演算子><値> をカンマで並べてください）"
                ));
            }
            let parsed = if token == DEFAULT_TOKEN {
                Metric::ALL
                    .into_iter()
                    .map(|metric| match metric.calibrated() {
                        Some((operator, threshold)) => Rule::Number {
                            metric,
                            operator,
                            threshold,
                        },
                        None => Rule::Flag { metric },
                    })
                    .collect()
            } else {
                vec![parse_rule(token)?]
            };
            for rule in parsed {
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
        }
        Ok(FailOn {
            spec: spec.to_string(),
            rules,
        })
    }

    /// 測りに照らす。**`checks[]` の並びは `Metric::ALL` の順である。**
    pub fn evaluate(&self, layers: &[LayerReport]) -> GateReport {
        let mut checks = Vec::new();
        for metric in Metric::ALL {
            let Some(rule) = self.rules.iter().find(|r| r.metric() == metric) else {
                continue;
            };
            let actual = metric.worst(layers);
            let (operator, threshold) = match *rule {
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
            let status = match (actual, *rule) {
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
                actual: actual.and_then(number),
                status,
            });
        }
        let passed = checks.iter().all(|c| c.status == PASS);
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
