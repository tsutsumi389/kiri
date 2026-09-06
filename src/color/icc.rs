//! 埋め込み ICC プロファイルの解釈と sRGB への変換。
//!
//! iPhone で撮った素材は Display P3 で入ってくる。これを sRGB として素通しすると、
//! 彩度の高い色ほど誇張されて出る。EC では「届いた商品が写真と違う」に直結するため、
//! 変換は品質ではなく正しさの問題である。
//!
//! 対象は**行列 + TRC 型のディスプレイプロファイル**に限る。`lcms2` のような
//! C 実装を持ち込まない方針の下では、LUT 型（`A2B0` など）の完全な解釈まで
//! 自前で背負うと割に合わない。実写で問題になるのは Display P3 と AdobeRGB で、
//! どちらも行列 + TRC 型である。
//!
//! レンダリングインテントは相対比色のみ。EC 用途では白を白に保つことが最優先で、
//! 知覚的インテントに必要な LUT はそもそも解釈対象外である。

use std::sync::LazyLock;

use image::RgbaImage;
use rayon::prelude::*;

/// ICC を解釈した結果。
pub struct Icc {
    /// プロファイル自身の名乗り（`desc` / `mluc`）。
    ///
    /// sRGB 相当と判定しても書き換えない。報告する色空間名は呼び出し側が
    /// `interpretation` から決める。ここまで "sRGB" に潰すと、どのプロファイルが
    /// 素通しされたのかが結果から消え、色を疑ったときに追えなくなる
    pub name: Option<String>,
    pub interpretation: Interpretation,
}

pub enum Interpretation {
    /// sRGB 相当。変換すると往復の丸め誤差を足すだけなので何もしない
    Srgb,
    /// 行列 + TRC 型。sRGB へ変換できる
    Convertible(Box<Transform>),
    /// 解釈できない（LUT 型、CMYK、Gray など）
    Unsupported,
}

/// 埋め込みプロファイルの色を sRGB へ移す変換。
///
/// 12MP を 100ms 台で通すため、TRC のデコードと sRGB のエンコードはどちらも表で引く。
/// 入力が 8bit しか取り得ない以上、デコード側は 256 エントリで厳密に一致する。
pub struct Transform {
    /// TRC で 8bit 入力を線形値へ落とす表（チャンネルごと）
    decode: [[f32; 256]; 3],
    /// 線形プロファイル RGB → 線形 sRGB
    matrix: [[f32; 3]; 3],
}

/// sRGB のエンコード表の分割数。
///
/// 線形値を等間隔で刻んで隣と線形補間する。sRGB のガンマは暗部で最も曲がるが、
/// そこでも補間誤差は 8bit の 0.01 段に収まる（曲率 × 刻み幅² / 8）。
const ENCODE_STEPS: usize = 4096;

/// 線形 sRGB (0.0-1.0) → 8bit 値の表。境界を含めるため 1 つ多く持つ。
static ENCODE: LazyLock<[f32; ENCODE_STEPS + 1]> = LazyLock::new(|| {
    std::array::from_fn(|i| (255.0 * srgb_encode(i as f64 / ENCODE_STEPS as f64)) as f32)
});

impl Transform {
    /// 画像全体をその場で変換する。
    ///
    /// 複製を取らないのは、12MP で 48MB を余分に積むとバッチの並列度ぶんだけ
    /// ピークが膨らむため。アルファには触れない（ICC は色にしか効かない）。
    pub fn apply(&self, image: &mut RgbaImage) {
        let m = &self.matrix;
        let d = &self.decode;
        let enc: &[f32; ENCODE_STEPS + 1] = &ENCODE;
        let buf: &mut [u8] = image;
        // 画素ごとに分けると同期のほうが高くつくので、行に相当する塊で分ける
        buf.par_chunks_mut(4 * 8192).for_each(|block| {
            for px in block.chunks_exact_mut(4) {
                let (r, g, b) = (
                    d[0][px[0] as usize],
                    d[1][px[1] as usize],
                    d[2][px[2] as usize],
                );
                px[0] = encode(enc, m[0][0] * r + m[0][1] * g + m[0][2] * b);
                px[1] = encode(enc, m[1][0] * r + m[1][1] * g + m[1][2] * b);
                px[2] = encode(enc, m[2][0] * r + m[2][1] * g + m[2][2] * b);
            }
        });
    }

    /// 1 画素だけ変換する。検証と診断のための入口。
    pub fn convert_pixel(&self, rgb: [u8; 3]) -> [u8; 3] {
        let m = &self.matrix;
        let d = &self.decode;
        let enc: &[f32; ENCODE_STEPS + 1] = &ENCODE;
        let (r, g, b) = (
            d[0][rgb[0] as usize],
            d[1][rgb[1] as usize],
            d[2][rgb[2] as usize],
        );
        [
            encode(enc, m[0][0] * r + m[0][1] * g + m[0][2] * b),
            encode(enc, m[1][0] * r + m[1][1] * g + m[1][2] * b),
            encode(enc, m[2][0] * r + m[2][1] * g + m[2][2] * b),
        ]
    }

    /// sRGB と実質同じプロファイルか。
    ///
    /// 原色と TRC の両方が一致することを要求し、名前は見ない。「sRGB」を含む
    /// 名前で中身が別物のプロファイル（sRGB 原色にガンマ 1.0 や 1.8 を載せた類）は
    /// 実在し、名乗りを信じると 8bit で数十段ずれた画像をそのまま素通しさせる。
    /// 実プロファイルの側に名乗りへ頼る理由も無い。macOS の `sRGB Profile.icc` の
    /// `curv[1024]` は解析式との差が 7.6e-6 で、TRC の一致だけで十分に通る。
    fn looks_like_srgb(&self) -> bool {
        self.matrix_is_identity(0.003) && self.trc_matches_srgb(0.002)
    }

    fn matrix_is_identity(&self, tolerance: f32) -> bool {
        for (i, row) in self.matrix.iter().enumerate() {
            for (j, v) in row.iter().enumerate() {
                let expected = if i == j { 1.0 } else { 0.0 };
                if (v - expected).abs() > tolerance {
                    return false;
                }
            }
        }
        true
    }

    fn trc_matches_srgb(&self, tolerance: f32) -> bool {
        self.decode.iter().all(|table| {
            table.iter().enumerate().all(|(i, v)| {
                let expected = srgb_decode(i as f64 / 255.0) as f32;
                (v - expected).abs() <= tolerance
            })
        })
    }
}

#[inline]
fn encode(table: &[f32; ENCODE_STEPS + 1], v: f32) -> u8 {
    let t = v.clamp(0.0, 1.0) * ENCODE_STEPS as f32;
    let i = (t as usize).min(ENCODE_STEPS - 1);
    let frac = t - i as f32;
    (table[i] + (table[i + 1] - table[i]) * frac + 0.5) as u8
}

/// 埋め込み ICC を解釈する。壊れていても失敗させず「解釈できない」を返す。
///
/// 画像そのものは読めているのに、付随するメタデータのせいで処理を断るのは筋が悪い。
pub fn interpret(bytes: &[u8]) -> Icc {
    let interpretation = match parse_transform(bytes) {
        Some(t) if t.looks_like_srgb() => Interpretation::Srgb,
        Some(t) => Interpretation::Convertible(Box::new(t)),
        None => Interpretation::Unsupported,
    };
    Icc {
        name: profile_name(bytes),
        interpretation,
    }
}

// --- パース ---

fn be_u16(bytes: &[u8], offset: usize) -> Option<u16> {
    let b = bytes.get(offset..offset + 2)?;
    Some(u16::from_be_bytes([b[0], b[1]]))
}

fn be_u32(bytes: &[u8], offset: usize) -> Option<u32> {
    let b = bytes.get(offset..offset + 4)?;
    Some(u32::from_be_bytes([b[0], b[1], b[2], b[3]]))
}

/// s15Fixed16Number を読む。ICC の数値表現はほぼこれで統一されている。
fn s15(bytes: &[u8], offset: usize) -> Option<f64> {
    let b = bytes.get(offset..offset + 4)?;
    Some(f64::from(i32::from_be_bytes([b[0], b[1], b[2], b[3]])) / 65536.0)
}

fn signature(bytes: &[u8], offset: usize) -> Option<[u8; 4]> {
    let b = bytes.get(offset..offset + 4)?;
    Some([b[0], b[1], b[2], b[3]])
}

/// タグテーブルから 1 つのタグの中身を取り出す。
fn tag<'a>(bytes: &'a [u8], want: &[u8; 4]) -> Option<&'a [u8]> {
    let count = be_u32(bytes, 128)? as usize;
    // タグ数はヘッダの値をそのまま信じない。壊れた ICC で巨大な確保をしないため
    for i in 0..count.min(1024) {
        let entry = 132 + i * 12;
        if signature(bytes, entry)? != *want {
            continue;
        }
        let offset = be_u32(bytes, entry + 4)? as usize;
        let size = be_u32(bytes, entry + 8)? as usize;
        return bytes.get(offset..offset.checked_add(size)?);
    }
    None
}

/// ICC のヘッダとして最低限成立しているか。
fn header_is_sane(bytes: &[u8]) -> bool {
    bytes.len() >= 132 && signature(bytes, 36) == Some(*b"acsp")
}

fn profile_name(bytes: &[u8]) -> Option<String> {
    if !header_is_sane(bytes) {
        return None;
    }
    let data = tag(bytes, b"desc")?;
    let text = match signature(data, 0)? {
        // v4 は多言語文字列。最初のレコードを採る（英語以外しか無い場合もあるため）
        s if s == *b"mluc" => {
            let length = be_u32(data, 20)? as usize;
            let offset = be_u32(data, 24)? as usize;
            let raw = data.get(offset..offset.checked_add(length)?)?;
            let units: Vec<u16> = raw
                .chunks_exact(2)
                .map(|c| u16::from_be_bytes([c[0], c[1]]))
                .collect();
            char::decode_utf16(units)
                .map(|c| c.unwrap_or(char::REPLACEMENT_CHARACTER))
                .collect::<String>()
        }
        // v2 は先頭が ASCII 部
        s if s == *b"desc" => {
            let length = be_u32(data, 8)? as usize;
            let raw = data.get(12..12usize.checked_add(length)?)?;
            String::from_utf8_lossy(raw).to_string()
        }
        _ => return None,
    };
    let trimmed = text.trim_end_matches('\0').trim().to_string();
    (!trimmed.is_empty()).then_some(trimmed)
}

/// TRC の曲線。ICC が定める型のうち、ディスプレイプロファイルに現れるものだけ扱う。
enum Curve {
    Gamma(f64),
    /// 0.0-1.0 に正規化した等間隔のテーブル
    Table(Vec<f64>),
    /// parametricCurveType（タイプ 0〜4 を 1 つの式にまとめたもの）
    Parametric {
        g: f64,
        a: f64,
        b: f64,
        c: f64,
        d: f64,
        e: f64,
        f: f64,
    },
}

impl Curve {
    fn eval(&self, x: f64) -> f64 {
        let y = match self {
            Curve::Gamma(g) => x.powf(*g),
            Curve::Table(values) => match values.len() {
                0 => x,
                1 => x.powf(values[0]),
                n => {
                    let pos = x.clamp(0.0, 1.0) * (n - 1) as f64;
                    let i = (pos as usize).min(n - 2);
                    let frac = pos - i as f64;
                    values[i] + (values[i + 1] - values[i]) * frac
                }
            },
            Curve::Parametric {
                g,
                a,
                b,
                c,
                d,
                e,
                f,
            } => {
                if x >= *d {
                    (a * x + b).max(0.0).powf(*g) + e
                } else {
                    c * x + f
                }
            }
        };
        y.clamp(0.0, 1.0)
    }
}

fn parse_curve(data: &[u8]) -> Option<Curve> {
    match signature(data, 0)? {
        s if s == *b"curv" => {
            let count = be_u32(data, 8)? as usize;
            match count {
                // 0 点は恒等（ガンマ 1.0）
                0 => Some(Curve::Gamma(1.0)),
                // 1 点は u8Fixed8 のガンマ値
                1 => Some(Curve::Gamma(f64::from(be_u16(data, 12)?) / 256.0)),
                n => {
                    let raw = data.get(12..12usize.checked_add(n * 2)?)?;
                    Some(Curve::Table(
                        raw.chunks_exact(2)
                            .map(|c| f64::from(u16::from_be_bytes([c[0], c[1]])) / 65535.0)
                            .collect(),
                    ))
                }
            }
        }
        s if s == *b"para" => {
            let kind = be_u16(data, 8)?;
            let p = |i: usize| s15(data, 12 + i * 4);
            // タイプごとに与えられるパラメータ数が違う。欠けている項は
            // 恒等に効かない値（a=1, c=0 等）で埋め、評価側を 1 本にまとめる
            let curve = match kind {
                0 => Curve::Parametric {
                    g: p(0)?,
                    a: 1.0,
                    b: 0.0,
                    c: 0.0,
                    d: 0.0,
                    e: 0.0,
                    f: 0.0,
                },
                1 => {
                    let (g, a, b) = (p(0)?, p(1)?, p(2)?);
                    Curve::Parametric {
                        g,
                        a,
                        b,
                        c: 0.0,
                        d: safe_div(-b, a),
                        e: 0.0,
                        f: 0.0,
                    }
                }
                2 => {
                    let (g, a, b, c) = (p(0)?, p(1)?, p(2)?, p(3)?);
                    Curve::Parametric {
                        g,
                        a,
                        b,
                        c: 0.0,
                        d: safe_div(-b, a),
                        e: c,
                        f: c,
                    }
                }
                3 => Curve::Parametric {
                    g: p(0)?,
                    a: p(1)?,
                    b: p(2)?,
                    c: p(3)?,
                    d: p(4)?,
                    e: 0.0,
                    f: 0.0,
                },
                4 => Curve::Parametric {
                    g: p(0)?,
                    a: p(1)?,
                    b: p(2)?,
                    c: p(3)?,
                    d: p(4)?,
                    e: p(5)?,
                    f: p(6)?,
                },
                _ => return None,
            };
            Some(curve)
        }
        _ => None,
    }
}

fn safe_div(a: f64, b: f64) -> f64 {
    if b == 0.0 { 0.0 } else { a / b }
}

/// XYZType から 1 列ぶんの XYZ を読む。
fn parse_xyz(data: &[u8]) -> Option<[f64; 3]> {
    if signature(data, 0)? != *b"XYZ " {
        return None;
    }
    Some([s15(data, 8)?, s15(data, 12)?, s15(data, 16)?])
}

/// 行列 + TRC 型として解釈し、sRGB への変換を組み立てる。
/// LUT 型や CMYK / Gray はここで None になる。
fn parse_transform(bytes: &[u8]) -> Option<Transform> {
    if !header_is_sane(bytes) {
        return None;
    }
    // 扱うのは RGB → XYZ(PCS) のディスプレイ系だけ
    if signature(bytes, 16)? != *b"RGB " || signature(bytes, 20)? != *b"XYZ " {
        return None;
    }

    let columns = [
        parse_xyz(tag(bytes, b"rXYZ")?)?,
        parse_xyz(tag(bytes, b"gXYZ")?)?,
        parse_xyz(tag(bytes, b"bXYZ")?)?,
    ];
    let curves = [
        parse_curve(tag(bytes, b"rTRC")?)?,
        parse_curve(tag(bytes, b"gTRC")?)?,
        parse_curve(tag(bytes, b"bTRC")?)?,
    ];

    // 列ベクトルを並べて「線形プロファイル RGB → XYZ(D50)」の行列にする
    let mut to_pcs = [[0.0f64; 3]; 3];
    for (i, row) in to_pcs.iter_mut().enumerate() {
        for (j, v) in row.iter_mut().enumerate() {
            *v = columns[j][i];
        }
    }
    if !invertible(&to_pcs) {
        return None;
    }

    // PCS は必ず D50。sRGB は D65 なので白色順応を挟む。
    //
    // ここで `chad` を使ってはいけない。相対比色では PCS の値は媒体相対
    // （媒体の白 = PCS の白 = D50）で表され、`rXYZ`/`gXYZ`/`bXYZ` も既に D50 へ
    // 順応済みの値が入っている。`chad`（実測白色点 → D50）の逆を掛けると媒体の
    // 実測白へ引き戻してしまい、白色点が D65 でないプロファイル（D50 の ROMM、
    // DCI 白の DCI-P3、D60 の ACES）で白が白でなくなる。`chad` が要るのは
    // 絶対比色のときだけで、kiri は相対比色しか扱わない。
    let matrix64 = mul(&XYZ_D65_TO_SRGB, &mul(&bradford(D50, D65), &to_pcs));
    let mut matrix = [[0.0f32; 3]; 3];
    for (i, row) in matrix.iter_mut().enumerate() {
        for (j, v) in row.iter_mut().enumerate() {
            *v = matrix64[i][j] as f32;
        }
    }

    let decode =
        std::array::from_fn(|c| std::array::from_fn(|i| curves[c].eval(i as f64 / 255.0) as f32));

    Some(Transform { decode, matrix })
}

// --- 色の数値 ---

/// D50 白色点（ICC の PCS が常にこれ）
const D50: [f64; 3] = [0.9642, 1.0, 0.8249];
/// D65 白色点（sRGB）
const D65: [f64; 3] = [0.95047, 1.0, 1.08883];

/// Bradford の錐体応答行列。白色順応で最も広く使われている。
const BRADFORD: [[f64; 3]; 3] = [
    [0.8951, 0.2664, -0.1614],
    [-0.7502, 1.7135, 0.0367],
    [0.0389, -0.0685, 1.0296],
];

/// XYZ(D65) → 線形 sRGB。`color::lab` が使う順方向の逆行列。
const XYZ_D65_TO_SRGB: [[f64; 3]; 3] = [
    [3.240_454_2, -1.537_138_5, -0.498_531_4],
    [-0.969_266_0, 1.876_010_8, 0.041_556_0],
    [0.055_643_4, -0.204_025_9, 1.057_225_2],
];

fn mul(a: &[[f64; 3]; 3], b: &[[f64; 3]; 3]) -> [[f64; 3]; 3] {
    let mut out = [[0.0; 3]; 3];
    for (i, row) in out.iter_mut().enumerate() {
        for (j, v) in row.iter_mut().enumerate() {
            *v = (0..3).map(|k| a[i][k] * b[k][j]).sum();
        }
    }
    out
}

fn mul_vec(a: &[[f64; 3]; 3], v: [f64; 3]) -> [f64; 3] {
    std::array::from_fn(|i| (0..3).map(|k| a[i][k] * v[k]).sum())
}

fn determinant(m: &[[f64; 3]; 3]) -> f64 {
    m[0][0] * (m[1][1] * m[2][2] - m[1][2] * m[2][1])
        - m[0][1] * (m[1][0] * m[2][2] - m[1][2] * m[2][0])
        + m[0][2] * (m[1][0] * m[2][1] - m[1][1] * m[2][0])
}

fn invertible(m: &[[f64; 3]; 3]) -> bool {
    determinant(m).abs() > 1e-9
}

fn invert(m: &[[f64; 3]; 3]) -> [[f64; 3]; 3] {
    let d = determinant(m);
    [
        [
            (m[1][1] * m[2][2] - m[1][2] * m[2][1]) / d,
            (m[0][2] * m[2][1] - m[0][1] * m[2][2]) / d,
            (m[0][1] * m[1][2] - m[0][2] * m[1][1]) / d,
        ],
        [
            (m[1][2] * m[2][0] - m[1][0] * m[2][2]) / d,
            (m[0][0] * m[2][2] - m[0][2] * m[2][0]) / d,
            (m[0][2] * m[1][0] - m[0][0] * m[1][2]) / d,
        ],
        [
            (m[1][0] * m[2][1] - m[1][1] * m[2][0]) / d,
            (m[0][1] * m[2][0] - m[0][0] * m[2][1]) / d,
            (m[0][0] * m[1][1] - m[0][1] * m[1][0]) / d,
        ],
    ]
}

/// Bradford による白色順応行列。
fn bradford(from: [f64; 3], to: [f64; 3]) -> [[f64; 3]; 3] {
    let s = mul_vec(&BRADFORD, from);
    let d = mul_vec(&BRADFORD, to);
    let scale = [
        [d[0] / s[0], 0.0, 0.0],
        [0.0, d[1] / s[1], 0.0],
        [0.0, 0.0, d[2] / s[2]],
    ];
    mul(&invert(&BRADFORD), &mul(&scale, &BRADFORD))
}

/// sRGB のガンマ（線形 → 符号化）。
fn srgb_encode(v: f64) -> f64 {
    if v <= 0.003_130_8 {
        12.92 * v
    } else {
        1.055 * v.powf(1.0 / 2.4) - 0.055
    }
}

/// sRGB のガンマ（符号化 → 線形）。`color::lab` の表と同じ式。
fn srgb_decode(v: f64) -> f64 {
    if v <= 0.040_45 {
        v / 12.92
    } else {
        ((v + 0.055) / 1.055).powf(2.4)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::color::lab::srgb_to_lab;
    use crate::color::synthetic::{Trc, build, display_p3, srgb_v2};

    fn transform_of(bytes: &[u8]) -> Transform {
        parse_transform(bytes).expect("行列 + TRC 型として解釈できるはず")
    }

    #[test]
    fn a_display_p3_profile_is_recognised_by_name() {
        let icc = interpret(&build(&display_p3()));
        assert_eq!(icc.name.as_deref(), Some("Display P3"));
        assert!(
            matches!(icc.interpretation, Interpretation::Convertible(_)),
            "行列 + TRC 型なので変換できるはず"
        );
    }

    /// P3 の原色は sRGB の色域外にある。飽和した赤は 255 に張り付くのが正しい。
    #[test]
    fn saturated_p3_primaries_clamp_into_the_srgb_gamut() {
        let t = transform_of(&build(&display_p3()));
        assert_eq!(t.convert_pixel([255, 0, 0]), [255, 0, 0]);
        assert_eq!(t.convert_pixel([0, 255, 0]), [0, 255, 0]);
        assert_eq!(t.convert_pixel([0, 0, 255]), [0, 0, 255]);
    }

    /// 中性グレーは中性グレーのまま、明度も保たれること。
    ///
    /// 白色順応を取り違えると、ここに色が乗るか明度がずれる。輪郭の判定は
    /// 背景との ΔE で決まるので、背景のグレーが転ぶと切り抜きの結論が変わる。
    #[test]
    fn neutral_grays_stay_neutral_and_keep_their_lightness() {
        let t = transform_of(&build(&display_p3()));
        for level in [16u8, 64, 128, 200, 255] {
            let out = t.convert_pixel([level, level, level]);
            let spread = u32::from(out.iter().copied().max().unwrap())
                - u32::from(out.iter().copied().min().unwrap());
            assert!(spread <= 1, "{level} が中性でなくなった: {out:?}");

            let before = srgb_to_lab([level, level, level]);
            let after = srgb_to_lab(out);
            assert!(
                (before[0] - after[0]).abs() < 0.6,
                "{level} の L* がずれた: {} -> {}",
                before[0],
                after[0]
            );
        }
    }

    /// sRGB 相当のプロファイルでは変換しない。名乗りは潰さずに残す。
    #[test]
    fn an_srgb_profile_is_detected_and_left_alone() {
        let icc = interpret(&build(&srgb_v2()));
        assert!(
            matches!(icc.interpretation, Interpretation::Srgb),
            "sRGB 相当は変換対象にしてはいけない"
        );
        assert_eq!(
            icc.name.as_deref(),
            Some("sRGB IEC61966-2.1"),
            "どのプロファイルが素通しされたのかは残すべき"
        );
    }

    /// 媒体白色点が D65 でないプロファイルでも中性グレーが中性のまま出ること。
    ///
    /// 相対比色では PCS の値は媒体相対（媒体の白 = PCS の白 = D50）で表され、
    /// 原色の XYZ も既に D50 へ順応済みである。`chad`（実測白色点 → D50）の逆を
    /// 白色順応の代わりに使うと媒体の実測白へ引き戻してしまい、白が D65 でない
    /// プロファイルで白が白でなくなる。ROMM は媒体白が D50 で `chad` が恒等な
    /// ため順応が丸ごと抜け、白が 255,252,221 に転ぶ。
    #[test]
    fn a_media_white_that_is_not_d65_still_maps_white_to_white() {
        use crate::color::synthetic::{dci_p3, romm_rgb};
        for spec in [romm_rgb(), dci_p3()] {
            let name = spec.desc.clone();
            let t = transform_of(&build(&spec));
            for level in [64u8, 128, 192, 255] {
                let out = t.convert_pixel([level, level, level]);
                let spread = u32::from(out.iter().copied().max().unwrap())
                    - u32::from(out.iter().copied().min().unwrap());
                assert!(spread <= 1, "{name} の {level} が中性でなくなった: {out:?}");
            }
        }
    }

    /// グレー階調が ColorSync / littleCMS の相対比色と一致すること。
    ///
    /// 中性であるだけでは足りない。白色順応を丸ごと落としても「灰色のまま
    /// 明るさだけずれる」ことはあり得るので、絶対値を外部の実装で押さえる。
    /// 参照値は macOS の `sips --matchTo "sRGB Profile.icc"` と littleCMS 2.17
    /// （intent 1）が同じ値を返したもの。
    #[test]
    fn grays_agree_with_colorsync_and_littlecms() {
        use crate::color::synthetic::{dci_p3, romm_rgb};
        // 入力 64 / 128 / 192 / 255 に対する sRGB 側の期待値
        let cases = [
            (display_p3(), [64u8, 128, 192, 255]),
            (crate::color::synthetic::adobe_rgb(), [62, 129, 193, 255]),
            (romm_rgb(), [81, 146, 203, 255]),
            (dci_p3(), [46, 113, 184, 255]),
        ];
        for (spec, expected) in cases {
            let name = spec.desc.clone();
            let t = transform_of(&build(&spec));
            for (level, want) in [64u8, 128, 192, 255].into_iter().zip(expected) {
                let out = t.convert_pixel([level, level, level]);
                for c in out {
                    assert!(
                        u32::from(c).abs_diff(u32::from(want)) <= 1,
                        "{name} の {level}: 期待 {want} に対して {out:?}"
                    );
                }
            }
        }
    }

    /// 名前が sRGB を名乗っていても、TRC が違えば素通しにしてはいけない。
    ///
    /// sRGB 原色にガンマ 1.0 や 1.8 を載せたプロファイルは実在する。名乗りだけで
    /// 通すと、8bit で数十段ずれた画像がそのまま納品される。
    #[test]
    fn a_profile_that_merely_calls_itself_srgb_is_still_converted() {
        for (desc, trc) in [
            ("sRGB Linear", Trc::Gamma(1.0)),
            ("sRGB with gamma 1.8", Trc::Gamma(1.8)),
        ] {
            let mut spec = srgb_v2();
            spec.desc = desc.into();
            spec.trc = trc;
            let icc = interpret(&build(&spec));
            let Interpretation::Convertible(t) = icc.interpretation else {
                panic!("{desc} は sRGB ではないので変換対象であるべき");
            };
            let out = t.convert_pixel([128, 128, 128]);
            assert!(
                u32::from(out[0]).abs_diff(128) >= 10,
                "{desc} は 128 が大きく動くはずなのに {out:?}"
            );
        }
    }

    /// ガンマ 1 点の TRC（AdobeRGB がこの形）も読めること。
    #[test]
    fn an_adobe_rgb_profile_converts_toward_srgb() {
        let mut spec = crate::color::synthetic::adobe_rgb();
        spec.desc = "Adobe RGB (1998)".into();
        let icc = interpret(&build(&spec));
        assert_eq!(icc.name.as_deref(), Some("Adobe RGB (1998)"));
        let Interpretation::Convertible(t) = icc.interpretation else {
            panic!("AdobeRGB は変換できるはず");
        };
        // AdobeRGB の緑は sRGB より広い。飽和した緑は 255 に張り付く
        assert_eq!(t.convert_pixel([0, 255, 0]), [0, 255, 0]);
        // 中性グレーは保たれる
        let out = t.convert_pixel([128, 128, 128]);
        assert!(out.iter().all(|c| c.abs_diff(out[0]) <= 1), "{out:?}");
    }

    /// 行列を持たないプロファイル（LUT 型）は対象外として素通しにする。
    #[test]
    fn a_lut_only_profile_is_reported_as_unsupported() {
        let mut spec = display_p3();
        spec.include_matrix = false;
        let icc = interpret(&build(&spec));
        assert!(matches!(icc.interpretation, Interpretation::Unsupported));
        assert_eq!(icc.name.as_deref(), Some("Display P3"), "名前は読めるべき");
    }

    #[test]
    fn a_non_rgb_profile_is_reported_as_unsupported() {
        let mut spec = display_p3();
        spec.colour_space = *b"CMYK";
        spec.desc = "Generic CMYK Profile".into();
        let icc = interpret(&build(&spec));
        assert!(matches!(icc.interpretation, Interpretation::Unsupported));
    }

    /// 壊れた ICC でパニックしないこと。画像が読めているのに落ちるのは論外。
    #[test]
    fn malformed_profiles_are_rejected_without_panicking() {
        let full = build(&display_p3());
        for len in [0usize, 1, 4, 100, 131, 140, 200] {
            let truncated = &full[..len.min(full.len())];
            let icc = interpret(truncated);
            let _ = icc.name;
        }
        let garbage = vec![0xAAu8; 600];
        assert!(matches!(
            interpret(&garbage).interpretation,
            Interpretation::Unsupported
        ));
    }

    /// テーブル型の TRC も読めること（v2 の sRGB プロファイルがこの形）。
    #[test]
    fn table_and_parametric_curves_agree_on_the_srgb_shape() {
        let table = transform_of(&build(&srgb_v2()));
        let mut spec = srgb_v2();
        spec.trc = Trc::Parametric3([2.4, 1.0 / 1.055, 0.055 / 1.055, 1.0 / 12.92, 0.040_45]);
        let parametric = transform_of(&build(&spec));
        for i in 0..=255u8 {
            let a = table.convert_pixel([i, i, i]);
            let b = parametric.convert_pixel([i, i, i]);
            assert!(
                u32::from(a[0]).abs_diff(u32::from(b[0])) <= 1,
                "{i}: table={a:?} para={b:?}"
            );
        }
    }

    /// 画像全体への適用が 1 画素ずつの変換と一致すること。
    #[test]
    fn applying_to_an_image_matches_the_per_pixel_path() {
        let t = transform_of(&build(&display_p3()));
        let mut img = RgbaImage::from_fn(37, 11, |x, y| {
            image::Rgba([(x * 7 % 256) as u8, (y * 23 % 256) as u8, 199, 128])
        });
        let source = img.clone();
        t.apply(&mut img);
        for (before, after) in source.pixels().zip(img.pixels()) {
            let expected = t.convert_pixel([before[0], before[1], before[2]]);
            assert_eq!([after[0], after[1], after[2]], expected);
            assert_eq!(after[3], before[3], "アルファに触れてはいけない");
        }
    }

    /// 同じ入力からは同じ結果が出ること。
    ///
    /// 変換は rayon で塊ごとに走る。画素どうしは独立なので分割の仕方に
    /// 依存しないはずだが、依存していれば実行ごとに出力バイト列が揺れる。
    #[test]
    fn the_result_does_not_depend_on_how_the_work_is_split() {
        let t = transform_of(&build(&display_p3()));
        // 1 塊に収まらない大きさにして、分割の境界をまたがせる
        let source = RgbaImage::from_fn(1024, 64, |x, y| {
            image::Rgba([
                (x % 256) as u8,
                (y * 4 % 256) as u8,
                ((x + y) % 256) as u8,
                255,
            ])
        });
        let mut first = source.clone();
        t.apply(&mut first);
        for _ in 0..3 {
            let mut again = source.clone();
            t.apply(&mut again);
            assert_eq!(
                first.as_raw(),
                again.as_raw(),
                "実行ごとに結果が変わっている"
            );
        }
    }

    /// 12MP の変換にかかる時間を出す。判定はしない（機械によって何倍も違う）。
    #[ignore = "計測用。12MP を数回変換するので数秒かかる"]
    #[test]
    fn print_the_cost_on_a_twelve_megapixel_image() {
        let t = transform_of(&build(&display_p3()));
        let source = RgbaImage::from_fn(3000, 4000, |x, y| {
            image::Rgba([(x % 256) as u8, (y % 256) as u8, ((x ^ y) % 256) as u8, 255])
        });
        for round in 0..3 {
            let mut image = source.clone();
            let started = std::time::Instant::now();
            t.apply(&mut image);
            println!("12MP の変換 {round}: {:?}", started.elapsed());
        }
    }
}
