//! 複数面の積分画像。
//!
//! 窓の合計を O(1) で引くための道具で、**guided filter と境界帯の推定が同じ
//! 1 本を使う**。2 つ持っていた頃は、片方だけ f32 にすれば桁落ちの性質が
//! 黙って食い違う——どちらも「大きな累積どうしの差」で合計を取り出すので、
//! そこが違えば同じ絵から違うアルファが出る。

/// 複数面の積分画像。面ごとに連続に持つ。
///
/// **合計は f64 で持つ。** 窓の合計を大きな累積どうしの差として取り出すので、
/// f32 では桁落ちして窓の位置で答えが揺れる。数えた画素数を面の 1 つとして
/// 混ぜてよいのも f64 だからで、24.5MP を足しても 2^53 には遠く届かない。
pub struct Integral<const N: usize> {
    stride: usize,
    plane: usize,
    data: Vec<f64>,
}

impl<const N: usize> Default for Integral<N> {
    fn default() -> Self {
        Self {
            stride: 0,
            plane: 0,
            data: Vec::new(),
        }
    }
}

impl<const N: usize> Integral<N> {
    pub fn build(&mut self, w: usize, h: usize, value: impl Fn(usize) -> [f64; N]) {
        let stride = w + 1;
        let plane = stride * (h + 1);
        self.stride = stride;
        self.plane = plane;
        self.data.clear();
        self.data.resize(plane * N, 0.0);
        for y in 0..h {
            let (row, prev) = ((y + 1) * stride, y * stride);
            let mut acc = [0f64; N];
            for x in 0..w {
                let v = value(y * w + x);
                for (k, slot) in acc.iter_mut().enumerate() {
                    *slot += v[k];
                }
                for (k, a) in acc.iter().enumerate() {
                    let p = k * plane;
                    self.data[p + row + x + 1] = self.data[p + prev + x + 1] + a;
                }
            }
        }
    }

    /// 局所座標の矩形 [x0,x1] × [y0,y1]（両端を含む）の合計。
    pub fn sum(&self, x0: usize, y0: usize, x1: usize, y1: usize) -> [f64; N] {
        let s = self.stride;
        let (a, b) = (y0 * s + x0, y0 * s + x1 + 1);
        let (c, d) = ((y1 + 1) * s + x0, (y1 + 1) * s + x1 + 1);
        std::array::from_fn(|k| {
            let p = k * self.plane;
            self.data[p + d] + self.data[p + a] - self.data[p + b] - self.data[p + c]
        })
    }
}
