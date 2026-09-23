//! 出力へ埋め込む sRGB の ICC プロファイルを組み立てる。
//!
//! 外部のプロファイル（HP / ICC 配布の sRGB）を同梱せず自前で組むのは、依存と権利を
//! 軽くするため。形は v2 の行列 + TRC 型で、`color::icc` が読む形の逆向きである。
//! 読み戻すと `Interpretation::Srgb` と判定されることをテストで固定する——外れると
//! kiri の出力を kiri に戻したとき、毎回色変換が走ってしまう。

use std::sync::LazyLock;

/// 他のソフトが名前で sRGB を見分けることがあるので、慣用の名乗りに合わせる
pub const SRGB_PROFILE_NAME: &str = "sRGB IEC61966-2.1";
/// v2 の必須タグ。読み手の多くは無くても通るが、規格の検証器は落とす
const COPYRIGHT: &str = "CC0";

/// sRGB の復号曲線を 32 点で標本化した `curv` 表（×65535）。
///
/// 実行時に `powf` で作らず定数で持つのは、libm の違いで最下位ビットが揺れても
/// プロファイルのバイト列（＝全出力）が動かないようにするため。26 点以下では
/// kiri と同じ線形補間で 8bit の値が動く（26 点で 4 値）。32 点で 0 値、
/// 誤差 0.00039 は `icc::looks_like_srgb` の許容 0.002 に収まる
const TRC: [u16; 32] = [
    0, 164, 352, 625, 992, 1461, 2040, 2734, 3550, 4492, 5565, 6775, 8127, 9623, 11269, 13069,
    15026, 17143, 19426, 21877, 24499, 27295, 30270, 33426, 36766, 40292, 44009, 47918, 52022,
    56325, 60828, 65535,
];

/// 線形 sRGB → XYZ(D50) の列 [r, g, b]。`synthetic::srgb_v2` と同じ値
const PRIMARIES: [[f64; 3]; 3] = [
    [0.436_065_673, 0.222_488_403, 0.013_916_015],
    [0.385_147_094, 0.716_873_168, 0.097_076_416],
    [0.143_066_406, 0.060_607_910, 0.714_096_069],
];
const D50: [f64; 3] = [0.9642, 1.0, 0.8249];

/// 作成日時。実行時刻を入れるとバイト列が毎回変わるので定数にする
const CREATED: [u16; 6] = [2026, 1, 1, 0, 0, 0];

/// 1 度だけ組んで使い回す。出力ごとに同じバイト列であることを構造で保証する
static SRGB_ICC: LazyLock<Vec<u8>> = LazyLock::new(build);

/// 出力に埋める sRGB の ICC（516 バイト）。
pub fn srgb_icc() -> &'static [u8] {
    &SRGB_ICC
}

fn s15(v: f64) -> [u8; 4] {
    ((v * 65536.0).round() as i32).to_be_bytes()
}

fn xyz(v: [f64; 3]) -> Vec<u8> {
    let mut out = b"XYZ \0\0\0\0".to_vec();
    for c in v {
        out.extend_from_slice(&s15(c));
    }
    out
}

fn build() -> Vec<u8> {
    // v2 の textDescriptionType。Unicode / ScriptCode の枠は空でも必ず持つ
    let mut desc = b"desc\0\0\0\0".to_vec();
    desc.extend_from_slice(&((SRGB_PROFILE_NAME.len() + 1) as u32).to_be_bytes());
    desc.extend_from_slice(SRGB_PROFILE_NAME.as_bytes());
    desc.push(0);
    desc.extend_from_slice(&[0u8; 8]); // Unicode の言語コード / 文字数
    desc.extend_from_slice(&[0u8; 3]); // ScriptCode のコード / 長さ
    desc.extend_from_slice(&[0u8; 67]);

    let mut cprt = b"text\0\0\0\0".to_vec();
    cprt.extend_from_slice(COPYRIGHT.as_bytes());
    cprt.push(0);

    let mut trc = b"curv\0\0\0\0".to_vec();
    trc.extend_from_slice(&(TRC.len() as u32).to_be_bytes());
    for v in TRC {
        trc.extend_from_slice(&v.to_be_bytes());
    }

    let blobs = [
        desc,
        cprt,
        xyz(D50),
        xyz(PRIMARIES[0]),
        xyz(PRIMARIES[1]),
        xyz(PRIMARIES[2]),
        trc,
    ];
    // TRC は 3 チャンネルで 1 つの実体を指す（実プロファイルもこの並び）
    let entries: [(&[u8; 4], usize); 9] = [
        (b"desc", 0),
        (b"cprt", 1),
        (b"wtpt", 2),
        (b"rXYZ", 3),
        (b"gXYZ", 4),
        (b"bXYZ", 5),
        (b"rTRC", 6),
        (b"gTRC", 6),
        (b"bTRC", 6),
    ];

    let data_start = 128 + 4 + entries.len() * 12;
    let mut offsets = Vec::with_capacity(blobs.len());
    let mut body = Vec::new();
    for blob in &blobs {
        offsets.push(data_start + body.len());
        body.extend_from_slice(blob);
        while body.len() % 4 != 0 {
            body.push(0); // タグは 4 バイト境界に揃える
        }
    }

    let mut out = vec![0u8; 128];
    out[0..4].copy_from_slice(&((data_start + body.len()) as u32).to_be_bytes());
    // 4..8 の CMM 欄は 0。"kiri" という CMM は実在しないので、名乗るのは creator 欄で
    out[8..12].copy_from_slice(&0x0210_0000u32.to_be_bytes());
    out[12..16].copy_from_slice(b"mntr");
    out[16..20].copy_from_slice(b"RGB ");
    out[20..24].copy_from_slice(b"XYZ ");
    for (i, v) in CREATED.into_iter().enumerate() {
        out[24 + i * 2..26 + i * 2].copy_from_slice(&v.to_be_bytes());
    }
    out[36..40].copy_from_slice(b"acsp");
    for (i, c) in D50.into_iter().enumerate() {
        out[68 + i * 4..72 + i * 4].copy_from_slice(&s15(c));
    }
    out[80..84].copy_from_slice(b"kiri");
    // 84..100 の profile ID は v2 では予約で 0 のまま

    out.extend_from_slice(&(entries.len() as u32).to_be_bytes());
    for (sig, k) in entries {
        out.extend_from_slice(sig);
        out.extend_from_slice(&(offsets[k] as u32).to_be_bytes());
        out.extend_from_slice(&(blobs[k].len() as u32).to_be_bytes());
    }
    out.extend_from_slice(&body);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::color::icc::{Interpretation, interpret};

    #[test]
    fn the_embedded_profile_reads_back_as_srgb() {
        let icc = interpret(srgb_icc());
        assert!(
            matches!(icc.interpretation, Interpretation::Srgb),
            "自前の sRGB が sRGB と判定されない。kiri の出力を戻すたびに色変換が走る"
        );
        assert_eq!(icc.name.as_deref(), Some(SRGB_PROFILE_NAME));
    }

    #[test]
    fn the_trc_table_is_the_srgb_curve() {
        // 解析式はテストの中でだけ持つ。本番は libm に触れない
        let srgb_decode = |x: f64| {
            if x <= 0.040_45 {
                x / 12.92
            } else {
                ((x + 0.055) / 1.055).powf(2.4)
            }
        };
        for (i, &v) in TRC.iter().enumerate() {
            let expected =
                (srgb_decode(i as f64 / (TRC.len() - 1) as f64) * 65535.0).round() as u16;
            assert_eq!(v, expected, "TRC[{i}]");
        }
    }

    /// 読み手の判定（`icc::looks_like_srgb`）はテスト用の合成 sRGB と同じ原色で
    /// 確かめてある。値を写し間違えると、判定の許容に紛れて気づけない
    #[test]
    fn the_profile_matches_the_synthetic_srgb() {
        assert_eq!(PRIMARIES, crate::color::synthetic::srgb_v2().primaries);
    }

    /// 変わったら全 PNG / JPEG 出力が動いたということ。意図した変更なら値を
    /// 取り直し、README の「516 バイト」も合わせる
    #[test]
    fn the_profile_bytes_are_pinned() {
        assert_eq!(srgb_icc().len(), 516);
        let mut hasher = crate::segment::sha256::Sha256::new();
        hasher.update(srgb_icc());
        assert_eq!(
            hasher.finish(),
            "05d5913ebef0648049b5eacf2a03688eab5fdb38529232739a66b79dc527f825"
        );
    }
}
