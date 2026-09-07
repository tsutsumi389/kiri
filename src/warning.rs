//! 警告型。
//!
//! `error.rs` の冒頭で宣言しているのと同じ原則を警告にも適用する。
//! **AI エージェントから使われる前提のため、警告も機械可読な `code` を必ず持つ。**
//! 日本語の散文を文字列マッチさせるのは、文言を推敲した瞬間に呼び出し側が壊れる
//! 契約であり、エラー側で避けたものを警告側でだけ許す理由が無い。
//!
//! `hint` には次に試すべき手がかりを、`data` には判断に使った数値そのものを入れる。
//! `data` を分けて持つのは、`message` に埋め込んだ数値をエージェントが正規表現で
//! 抜き直す事態を避けるためである。文言は変わりうるが、キーと数値は契約になる。

use serde::Serialize;
use serde_json::{Map, Value};

#[derive(Debug, Clone, Serialize)]
pub struct Warning {
    pub code: &'static str,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hint: Option<String>,
    /// 判断に使った数値。エージェントが message をパースせずに済むようにする
    #[serde(skip_serializing_if = "Map::is_empty")]
    pub data: Map<String, Value>,
}

impl Warning {
    pub fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            hint: None,
            data: Map::new(),
        }
    }

    /// 回復のための手がかりを添える。`Error::with_hint` と同じ名前にしてあるのは、
    /// 両者が同じ役目を負っており、呼び出し側が使い分けを覚える必要が無いため。
    pub fn with_hint(mut self, hint: impl Into<String>) -> Self {
        self.hint = Some(hint.into());
        self
    }

    /// 判断に使った数値を 1 つ添える。
    ///
    /// 丸めはここでは行わない。どの桁まで意味があるかは値の種類ごとに違い
    /// （比率は小数第 4 位、しきい値は第 1 位）、呼び出し側でしか決められない。
    pub fn with_data(mut self, key: &str, value: impl Into<Value>) -> Self {
        self.data.insert(key.to_string(), value.into());
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn code_message_hint_and_data_all_reach_the_json() {
        let w = Warning::new("LOW_UNIFORMITY", "背景の均一度が低い")
            .with_hint("--bbox を指定してください")
            .with_data("uniformity", 0.201);
        let v: Value = serde_json::to_value(&w).unwrap();

        assert_eq!(v["code"], "LOW_UNIFORMITY");
        assert_eq!(v["message"], "背景の均一度が低い");
        assert_eq!(v["hint"], "--bbox を指定してください");
        assert_eq!(v["data"]["uniformity"], 0.201);
    }

    /// 空の `hint` / `data` はキーごと消す。
    ///
    /// `null` や `{}` を出すと、エージェントは「中身のない手がかりがある」と
    /// 読んで分岐を書いてしまう。無い情報は無いと分かる形で出さない。
    #[test]
    fn empty_hint_and_data_disappear_from_the_json() {
        let w = Warning::new("SUBJECT_TOUCHES_EDGE", "外周に接しています");
        let v: Value = serde_json::to_value(&w).unwrap();

        assert_eq!(v.as_object().unwrap().len(), 2, "{v}");
        assert!(v.get("hint").is_none(), "{v}");
        assert!(v.get("data").is_none(), "{v}");
    }

    /// `data` は複数の値を持てる。`EDGE_THRESHOLD_RAISED` は
    /// 「いくつからいくつへ」と「その根拠」を同時に語る必要がある。
    #[test]
    fn several_numbers_can_be_attached() {
        let w = Warning::new("EDGE_THRESHOLD_RAISED", "堤防を引き上げました")
            .with_data("from", 8.0)
            .with_data("to", 41.8)
            .with_data("texture_p50", 11.3)
            .with_data("texture_p90", 27.9);
        let v: Value = serde_json::to_value(&w).unwrap();

        assert_eq!(v["data"]["from"], 8.0);
        assert_eq!(v["data"]["to"], 41.8);
        assert_eq!(v["data"]["texture_p50"], 11.3);
        assert_eq!(v["data"]["texture_p90"], 27.9);
    }

    /// 配列も入れられる。bbox のような「4 つで 1 つの意味」を持つ値を
    /// 4 つのキーへばらすと、そのまま `--bbox` へ渡せなくなる。
    #[test]
    fn an_array_survives_the_round_trip() {
        let w = Warning::new("BBOX_RECOMMENDED", "bbox を指定してください")
            .with_data("normalized_bbox", vec![0.0, 0.35, 0.98, 0.67]);
        let v: Value = serde_json::to_value(&w).unwrap();

        assert_eq!(v["data"]["normalized_bbox"][1], 0.35);
        assert_eq!(v["data"]["normalized_bbox"].as_array().unwrap().len(), 4);
    }
}
