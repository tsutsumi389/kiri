//! テスト用の ICC バイト列を組み立てる。
//!
//! 実写ファイルをリポジトリに置かずに変換の正しさを固定するために持つ。
//! 数値は macOS の ColorSync が配っている実プロファイルから採ったもので、
//! s15Fixed16 に丸めた値そのままである。

/// TRC の与え方。ICC の 3 つの型を書き分けられるようにしてある。
pub(crate) enum Trc {
    /// `curv` 1 点（AdobeRGB がこの形）
    Gamma(f64),
    /// `curv` N 点（v2 の sRGB がこの形）
    Table(Vec<f64>),
    /// `para` タイプ 3（Display P3 がこの形）: [g, a, b, c, d]
    Parametric3([f64; 5]),
}

pub(crate) struct ProfileSpec {
    pub version: u32,
    pub desc: String,
    /// v4 の多言語文字列で書くか（false なら v2 の textDescriptionType）
    pub desc_is_mluc: bool,
    pub colour_space: [u8; 4],
    pub pcs: [u8; 4],
    /// 線形 RGB → XYZ(D50) の列ベクトル [r, g, b]
    pub primaries: [[f64; 3]; 3],
    pub trc: Trc,
    pub chad: Option<[f64; 9]>,
    /// false にすると rXYZ/gXYZ/bXYZ を落とし、LUT 型のプロファイルを模す
    pub include_matrix: bool,
}

/// Apple の Display P3（v4、`para` タイプ 3、`chad` あり）。
pub(crate) fn display_p3() -> ProfileSpec {
    ProfileSpec {
        version: 0x0400_0000,
        desc: "Display P3".into(),
        desc_is_mluc: true,
        colour_space: *b"RGB ",
        pcs: *b"XYZ ",
        primaries: [
            [0.515_121_459, 0.241_195_678, -0.001_052_856],
            [0.291_976_928, 0.692_245_483, 0.041_885_375],
            [0.157_104_492, 0.066_574_096, 0.784_072_875],
        ],
        trc: Trc::Parametric3([
            2.399_993_896,
            0.947_860_717,
            0.052_139_282,
            0.077_392_578,
            0.040_451_049,
        ]),
        chad: Some([
            1.047_882_080,
            0.022_918_701,
            -0.050_201_416,
            0.029_586_791,
            0.990_478_515,
            -0.017_059_326,
            -0.009_231_567,
            0.015_075_683,
            0.751_678_466,
        ]),
        include_matrix: true,
    }
}

/// IEC 61966-2.1 sRGB（v2、`curv` テーブル、`chad` なし）。
pub(crate) fn srgb_v2() -> ProfileSpec {
    ProfileSpec {
        version: 0x0210_0000,
        desc: "sRGB IEC61966-2.1".into(),
        desc_is_mluc: false,
        colour_space: *b"RGB ",
        pcs: *b"XYZ ",
        primaries: [
            [0.436_065_673, 0.222_488_403, 0.013_916_015],
            [0.385_147_094, 0.716_873_168, 0.097_076_416],
            [0.143_066_406, 0.060_607_910, 0.714_096_069],
        ],
        // 実プロファイルと同じ 1024 点でサンプリングする
        trc: Trc::Table(
            (0..1024)
                .map(|i| {
                    let x = i as f64 / 1023.0;
                    if x <= 0.040_45 {
                        x / 12.92
                    } else {
                        ((x + 0.055) / 1.055).powf(2.4)
                    }
                })
                .collect(),
        ),
        chad: None,
        include_matrix: true,
    }
}

/// Adobe RGB (1998)（v2、`curv` 1 点のガンマ、`chad` なし）。
pub(crate) fn adobe_rgb() -> ProfileSpec {
    ProfileSpec {
        version: 0x0210_0000,
        desc: "Adobe RGB (1998)".into(),
        desc_is_mluc: false,
        colour_space: *b"RGB ",
        pcs: *b"XYZ ",
        primaries: [
            [0.609_741_210, 0.311_111_450, 0.019_470_214],
            [0.205_276_489, 0.625_671_386, 0.060_867_309],
            [0.149_185_180, 0.063_217_163, 0.744_567_871],
        ],
        trc: Trc::Gamma(2.199_218_75),
        chad: None,
        include_matrix: true,
    }
}

fn s15(v: f64) -> [u8; 4] {
    ((v * 65536.0).round() as i32).to_be_bytes()
}

fn xyz_tag(v: [f64; 3]) -> Vec<u8> {
    let mut out = b"XYZ \0\0\0\0".to_vec();
    for c in v {
        out.extend_from_slice(&s15(c));
    }
    out
}

fn trc_tag(trc: &Trc) -> Vec<u8> {
    match trc {
        Trc::Gamma(g) => {
            let mut out = b"curv\0\0\0\0".to_vec();
            out.extend_from_slice(&1u32.to_be_bytes());
            out.extend_from_slice(&(((g * 256.0).round() as u16).to_be_bytes()));
            out
        }
        Trc::Table(values) => {
            let mut out = b"curv\0\0\0\0".to_vec();
            out.extend_from_slice(&(values.len() as u32).to_be_bytes());
            for v in values {
                out.extend_from_slice(&(((v * 65535.0).round() as u16).to_be_bytes()));
            }
            out
        }
        Trc::Parametric3(p) => {
            let mut out = b"para\0\0\0\0".to_vec();
            out.extend_from_slice(&3u16.to_be_bytes());
            out.extend_from_slice(&0u16.to_be_bytes());
            for v in p {
                out.extend_from_slice(&s15(*v));
            }
            out
        }
    }
}

fn desc_tag(text: &str, mluc: bool) -> Vec<u8> {
    if mluc {
        let units: Vec<u16> = text.encode_utf16().collect();
        let bytes: Vec<u8> = units.iter().flat_map(|u| u.to_be_bytes()).collect();
        let mut out = b"mluc\0\0\0\0".to_vec();
        out.extend_from_slice(&1u32.to_be_bytes()); // レコード数
        out.extend_from_slice(&12u32.to_be_bytes()); // レコード長
        out.extend_from_slice(b"enUS");
        out.extend_from_slice(&(bytes.len() as u32).to_be_bytes());
        out.extend_from_slice(&28u32.to_be_bytes()); // タグ先頭からの位置
        out.extend_from_slice(&bytes);
        out
    } else {
        let ascii = text.as_bytes();
        let mut out = b"desc\0\0\0\0".to_vec();
        out.extend_from_slice(&((ascii.len() + 1) as u32).to_be_bytes());
        out.extend_from_slice(ascii);
        out.push(0);
        // v2 の textDescriptionType は Unicode / ScriptCode の枠を必ず持つ
        out.extend_from_slice(&0u32.to_be_bytes()); // Unicode 言語コード
        out.extend_from_slice(&0u32.to_be_bytes()); // Unicode 文字数
        out.extend_from_slice(&0u16.to_be_bytes()); // ScriptCode コード
        out.push(0); // ScriptCode 長
        out.extend_from_slice(&[0u8; 67]);
        out
    }
}

/// 仕様から ICC のバイト列を組み立てる。
pub(crate) fn build(spec: &ProfileSpec) -> Vec<u8> {
    let mut tags: Vec<([u8; 4], Vec<u8>)> =
        vec![(*b"desc", desc_tag(&spec.desc, spec.desc_is_mluc))];
    if spec.include_matrix {
        tags.push((*b"rXYZ", xyz_tag(spec.primaries[0])));
        tags.push((*b"gXYZ", xyz_tag(spec.primaries[1])));
        tags.push((*b"bXYZ", xyz_tag(spec.primaries[2])));
        tags.push((*b"rTRC", trc_tag(&spec.trc)));
        tags.push((*b"gTRC", trc_tag(&spec.trc)));
        tags.push((*b"bTRC", trc_tag(&spec.trc)));
    }
    tags.push((*b"wtpt", xyz_tag([0.9642, 1.0, 0.8249])));
    if let Some(chad) = spec.chad {
        let mut data = b"sf32\0\0\0\0".to_vec();
        for v in chad {
            data.extend_from_slice(&s15(v));
        }
        tags.push((*b"chad", data));
    }

    let table_len = 4 + tags.len() * 12;
    let mut body = Vec::new();
    let mut table = Vec::new();
    table.extend_from_slice(&(tags.len() as u32).to_be_bytes());
    for (sig, data) in &tags {
        let offset = 128 + table_len + body.len();
        table.extend_from_slice(sig);
        table.extend_from_slice(&(offset as u32).to_be_bytes());
        table.extend_from_slice(&(data.len() as u32).to_be_bytes());
        body.extend_from_slice(data);
        // タグは 4 バイト境界に揃える
        while body.len() % 4 != 0 {
            body.push(0);
        }
    }

    let total = 128 + table.len() + body.len();
    let mut header = vec![0u8; 128];
    header[0..4].copy_from_slice(&(total as u32).to_be_bytes());
    header[4..8].copy_from_slice(b"kiri");
    header[8..12].copy_from_slice(&spec.version.to_be_bytes());
    header[12..16].copy_from_slice(b"mntr");
    header[16..20].copy_from_slice(&spec.colour_space);
    header[20..24].copy_from_slice(&spec.pcs);
    header[36..40].copy_from_slice(b"acsp");
    for (i, c) in [0.9642, 1.0, 0.8249].into_iter().enumerate() {
        header[68 + i * 4..72 + i * 4].copy_from_slice(&s15(c));
    }

    let mut out = header;
    out.extend_from_slice(&table);
    out.extend_from_slice(&body);
    out
}

/// JPEG に APP2 の `ICC_PROFILE` セグメントを差し込む。
///
/// `image` クレートのエンコーダは ICC を書けないので、読み込み側の経路を
/// 実ファイルで確かめるには自前で埋め込むしかない。
pub(crate) fn embed_in_jpeg(jpeg: &[u8], icc: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(jpeg.len() + icc.len() + 32);
    out.extend_from_slice(&jpeg[..2]); // SOI
    let payload_len = 2 + 12 + 2 + icc.len();
    out.extend_from_slice(&[0xFF, 0xE2]);
    out.extend_from_slice(&(payload_len as u16).to_be_bytes());
    out.extend_from_slice(b"ICC_PROFILE\0");
    out.push(1); // 通し番号
    out.push(1); // 総数
    out.extend_from_slice(icc);
    out.extend_from_slice(&jpeg[2..]);
    out
}
