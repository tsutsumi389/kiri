//! エラー型と exit code のマッピング。
//!
//! AI エージェントから使われる前提のため、エラーは機械可読な `code` を必ず持つ。
//! `hint` には次に試すべき手がかりを入れ、エージェントが自力で回復できるようにする。

use std::fmt;

/// エラーの分類。exit code に 1:1 で対応する。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorKind {
    /// 分類不能な一般エラー
    General,
    /// 引数が不正
    Argument,
    /// 入力ファイルの異常（存在しない、破損、非対応形式）
    Input,
    /// 処理の失敗（背景を検出できない等）
    Processing,
}

impl ErrorKind {
    pub fn exit_code(self) -> i32 {
        match self {
            ErrorKind::General => 1,
            ErrorKind::Argument => 2,
            ErrorKind::Input => 3,
            ErrorKind::Processing => 4,
        }
    }
}

#[derive(Debug)]
pub struct Error {
    pub kind: ErrorKind,
    pub code: &'static str,
    pub message: String,
    pub hint: Option<String>,
}

impl Error {
    fn new(kind: ErrorKind, code: &'static str, message: impl Into<String>) -> Self {
        Self {
            kind,
            code,
            message: message.into(),
            hint: None,
        }
    }

    pub fn general(code: &'static str, message: impl Into<String>) -> Self {
        Self::new(ErrorKind::General, code, message)
    }

    pub fn argument(code: &'static str, message: impl Into<String>) -> Self {
        Self::new(ErrorKind::Argument, code, message)
    }

    pub fn input(code: &'static str, message: impl Into<String>) -> Self {
        Self::new(ErrorKind::Input, code, message)
    }

    pub fn processing(code: &'static str, message: impl Into<String>) -> Self {
        Self::new(ErrorKind::Processing, code, message)
    }

    /// 回復のための手がかりを添える。
    pub fn with_hint(mut self, hint: impl Into<String>) -> Self {
        self.hint = Some(hint.into());
        self
    }

    pub fn exit_code(&self) -> i32 {
        self.kind.exit_code()
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.code, self.message)?;
        if let Some(hint) = &self.hint {
            write!(f, " ({hint})")?;
        }
        Ok(())
    }
}

impl std::error::Error for Error {}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Error::general("IO_ERROR", e.to_string())
    }
}

pub type Result<T> = std::result::Result<T, Error>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exit_codes_match_the_documented_contract() {
        assert_eq!(ErrorKind::General.exit_code(), 1);
        assert_eq!(ErrorKind::Argument.exit_code(), 2);
        assert_eq!(ErrorKind::Input.exit_code(), 3);
        assert_eq!(ErrorKind::Processing.exit_code(), 4);
    }

    #[test]
    fn hint_is_included_in_display() {
        let e = Error::input("NOT_FOUND", "見つからない").with_hint("パスを確認");
        assert_eq!(e.to_string(), "NOT_FOUND: 見つからない (パスを確認)");
    }
}
