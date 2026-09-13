//! SHA-256。**モデルファイルの検証のためだけに置いている。**
//!
//! 配布元（rembg のリリース）が名乗るのは MD5 だが、MD5 を自前で書く理由が
//! 無い——壊れたダウンロードを見つけるのが目的で、衝突耐性は要らない一方、
//! 既に破られた関数を新しく実装として増やすのは筋が悪い。既知の SHA-256 を
//! kiri 自身が持ち、それと突き合わせる。配布元の MD5 は `kiri model list` が
//! そのまま配る（利用者が `curl` で取った直後に確かめられるように）。
//!
//! 依存を足さないのは 3.1 の方針どおりである。FIPS 180-4 の定義そのままで、
//! 60 行に収まる。**速さを狙っていない**——176MB を数百 ms で舐められれば
//! 足りる用途にしか使わない。

/// FIPS 180-4 の丸め定数（最初の 64 個の素数の立方根の小数部）。
const K: [u32; 64] = [
    0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5,
    0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174,
    0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da,
    0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967,
    0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85,
    0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070,
    0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
    0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
];

/// 逐次的に読み込める SHA-256。176MB のファイルを丸ごと確保しないために、
/// 64 バイトのブロック単位で食わせる形にしてある。
pub struct Sha256 {
    state: [u32; 8],
    buffer: [u8; 64],
    buffered: usize,
    length: u64,
}

impl Default for Sha256 {
    fn default() -> Self {
        Self::new()
    }
}

impl Sha256 {
    pub fn new() -> Self {
        Self {
            state: [
                0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab,
                0x5be0cd19,
            ],
            buffer: [0u8; 64],
            buffered: 0,
            length: 0,
        }
    }

    pub fn update(&mut self, mut data: &[u8]) {
        self.length = self.length.wrapping_add(data.len() as u64);
        if self.buffered > 0 {
            let take = (64 - self.buffered).min(data.len());
            self.buffer[self.buffered..self.buffered + take].copy_from_slice(&data[..take]);
            self.buffered += take;
            data = &data[take..];
            // **1 ブロックに満たなければここで戻る。** 戻らずに先へ進むと、
            // 末尾の切れ端を書き戻すところで `buffered` を 0 に潰してしまい、
            // 1 バイトずつ食わせた入力が丸ごと消える（`finish` の詰め物は
            // まさに 1 バイトずつ食わせるので、そこが無限ループになる）
            if self.buffered < 64 {
                return;
            }
            let block = self.buffer;
            self.compress(&block);
            self.buffered = 0;
        }
        let mut chunks = data.chunks_exact(64);
        for block in &mut chunks {
            let mut fixed = [0u8; 64];
            fixed.copy_from_slice(block);
            self.compress(&fixed);
        }
        let rest = chunks.remainder();
        self.buffer[..rest.len()].copy_from_slice(rest);
        self.buffered = rest.len();
    }

    /// 16 進小文字 64 文字。`shasum -a 256` と同じ綴りにする。
    pub fn finish(mut self) -> String {
        let bits = self.length.wrapping_mul(8);
        self.update(&[0x80]);
        // 長さのために末尾 8 バイトを空ける。`update` が `length` を動かすので、
        // 書き込む値は先に控えてある
        while self.buffered != 56 {
            self.update(&[0x00]);
        }
        self.buffer[56..64].copy_from_slice(&bits.to_be_bytes());
        let block = self.buffer;
        self.compress(&block);

        let mut out = String::with_capacity(64);
        for word in self.state {
            out.push_str(&format!("{word:08x}"));
        }
        out
    }

    fn compress(&mut self, block: &[u8; 64]) {
        let mut w = [0u32; 64];
        for (i, slot) in w.iter_mut().take(16).enumerate() {
            *slot = u32::from_be_bytes([
                block[i * 4],
                block[i * 4 + 1],
                block[i * 4 + 2],
                block[i * 4 + 3],
            ]);
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16]
                .wrapping_add(s0)
                .wrapping_add(w[i - 7])
                .wrapping_add(s1);
        }

        let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut h] = self.state;
        for i in 0..64 {
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let ch = (e & f) ^ ((!e) & g);
            let t1 = h
                .wrapping_add(s1)
                .wrapping_add(ch)
                .wrapping_add(K[i])
                .wrapping_add(w[i]);
            let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let maj = (a & b) ^ (a & c) ^ (b & c);
            let t2 = s0.wrapping_add(maj);
            h = g;
            g = f;
            f = e;
            e = d.wrapping_add(t1);
            d = c;
            c = b;
            b = a;
            a = t1.wrapping_add(t2);
        }
        for (slot, v) in self.state.iter_mut().zip([a, b, c, d, e, f, g, h]) {
            *slot = slot.wrapping_add(v);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hash(data: &[u8]) -> String {
        let mut h = Sha256::new();
        h.update(data);
        h.finish()
    }

    /// FIPS 180-4 の公式ベクタ。**自前実装を置く以上、ここが唯一の根拠である。**
    #[test]
    fn matches_the_published_vectors() {
        assert_eq!(
            hash(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(
            hash(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert_eq!(
            hash(b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq"),
            "248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1"
        );
    }

    /// ちょうど 1 ブロック（64 バイト）と、その境界の前後。
    ///
    /// **詰め物の分岐はここでしか踏めない。** 56 バイトを超えると長さを書く
    /// ための 1 ブロックが余分に要り、境界を 1 つ間違えると「ほとんどの
    /// ファイルでは合うのに、ある長さでだけ違う」壊れ方になる。
    #[test]
    fn the_block_boundaries_are_handled() {
        // 55 / 56 / 63 / 64 / 65 バイト
        for len in [55usize, 56, 63, 64, 65, 1000] {
            let data = vec![b'a'; len];
            let mut split = Sha256::new();
            // 1 バイトずつ食わせても、まとめて食わせても同じであること
            for byte in &data {
                split.update(std::slice::from_ref(byte));
            }
            assert_eq!(split.finish(), hash(&data), "len={len}");
        }
    }

    /// 千万バイト級でも長さの扱いが崩れないこと（ビット数は 64bit）。
    #[test]
    fn a_large_input_keeps_its_length() {
        let block = vec![0u8; 1 << 20];
        let mut h = Sha256::new();
        for _ in 0..16 {
            h.update(&block);
        }
        // 16MiB のゼロ。`head -c 16777216 /dev/zero | shasum -a 256` と一致する
        assert_eq!(
            h.finish(),
            "080acf35a507ac9849cfcba47dc2ad83e01b75663a516279c8b9d243b719643e"
        );
    }
}
