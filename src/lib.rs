//! kiri — EC商品画像のための切り抜き・変換ライブラリ。
//!
//! CLI 本体は `main.rs` にあるが、統合テストから触れるようライブラリとして
//! 公開している。

pub mod batch;
pub mod cli;
pub mod color;
pub mod commands;
pub mod cutout;
pub mod error;
pub mod image_io;
pub mod preview;
pub mod report;
pub mod transform;
pub mod warning;

pub use error::{Error, ErrorKind, Result};
pub use warning::Warning;
