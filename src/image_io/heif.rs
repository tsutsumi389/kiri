//! HEIF 系（HEIC / AVIF）の入力を見分けて断る。
//!
//! EC の入稿素材は iPhone 撮影が多く、そのまま渡せば HEIC で来る。だが HEVC の
//! デコーダは pure Rust に存在せず、C 依存を持ち込まない方針とは両立しない。
//! それなら「読めません」で終わらせず、変換の手順まで返すのが親切である。
//!
//! `image` に任せると形式の判別自体に失敗して「拡張子から判別できません」に
//! 落ちる。原因が HEIC だと分からないメッセージでは、エージェントは
//! 別の拡張子を試すような無駄な再試行に入る。

use crate::error::Error;

/// ISO 基本メディアファイルのブランド分類。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Family {
    Heif,
    Avif,
}

/// 先頭バイトから HEIF 系かどうかを見分ける。
///
/// `ftyp` ボックスのメジャーブランドと互換ブランドの両方を見る。iPhone の HEIC は
/// メジャーブランドが `heic` だが、`mif1` しか名乗らない書き出しもあるため。
pub fn detect(bytes: &[u8]) -> Option<Family> {
    if bytes.len() < 16 || &bytes[4..8] != b"ftyp" {
        return None;
    }
    let size = u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]) as usize;
    // メジャーブランド + マイナーバージョン + 互換ブランドの並び。
    // 長さはヘッダの値を信じず、実バイト数と上限で押さえる
    let end = size.clamp(16, bytes.len().min(256));
    let mut family = None;
    for brand in bytes[8..end].chunks_exact(4) {
        match brand {
            b"avif" | b"avis" => return Some(Family::Avif),
            b"heic" | b"heix" | b"hevc" | b"hevx" | b"heim" | b"heis" | b"mif1" | b"msf1" => {
                family = Some(Family::Heif);
            }
            _ => {}
        }
    }
    family
}

/// 非対応であることと、その場で打てる手を返す。
pub fn unsupported(family: Family) -> Error {
    let what = match family {
        Family::Heif => "HEIC/HEIF",
        Family::Avif => "AVIF",
    };
    Error::input(
        "UNSUPPORTED_FORMAT",
        format!("{what} は入力として未対応です（pure Rust の HEVC/AV1 デコーダが無いため）"),
    )
    .with_hint(
        "macOS: sips -s format jpeg -s formatOptions 95 in.HEIC --out in.jpg / \
         その他: magick in.heic -quality 95 in.jpg（または libheif の heif-convert）",
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `ftyp` ボックスだけを持つ最小のダミーを作る。
    pub(crate) fn ftyp(major: &[u8; 4], compatible: &[&[u8; 4]]) -> Vec<u8> {
        let size = 16 + compatible.len() * 4;
        let mut out = (size as u32).to_be_bytes().to_vec();
        out.extend_from_slice(b"ftyp");
        out.extend_from_slice(major);
        out.extend_from_slice(&0u32.to_be_bytes());
        for brand in compatible {
            out.extend_from_slice(*brand);
        }
        out
    }

    #[test]
    fn heif_brands_are_detected() {
        for major in [b"heic", b"heix", b"hevc", b"mif1", b"msf1"] {
            assert_eq!(
                detect(&ftyp(major, &[b"mif1"])),
                Some(Family::Heif),
                "{} を HEIF として検出できていない",
                String::from_utf8_lossy(major)
            );
        }
        assert_eq!(detect(&ftyp(b"avif", &[b"mif1"])), Some(Family::Avif));
    }

    /// 互換ブランドにしか現れない場合も拾うこと。
    #[test]
    fn a_compatible_brand_is_enough() {
        assert_eq!(detect(&ftyp(b"mif1", &[b"heic"])), Some(Family::Heif));
    }

    #[test]
    fn non_heif_bytes_are_left_alone() {
        assert_eq!(detect(&[0xFF, 0xD8, 0xFF, 0xE0, 0, 0, 0, 0]), None);
        assert_eq!(detect(b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR"), None);
        assert_eq!(detect(&ftyp(b"isom", &[b"mp42"])), None, "MP4 は対象外");
        assert_eq!(detect(&[]), None);
    }

    #[test]
    fn the_error_points_at_a_conversion_command() {
        let err = unsupported(Family::Heif);
        assert_eq!(err.code, "UNSUPPORTED_FORMAT");
        assert_eq!(err.exit_code(), 3, "入力ファイル異常として扱う");
        let hint = err.hint.unwrap();
        assert!(hint.contains("sips"), "macOS 向けの手順が要る: {hint}");
        assert!(hint.contains("magick"), "macOS 以外の手順が要る: {hint}");
    }
}
