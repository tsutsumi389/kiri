# テスト用フィクスチャ

## `backgrounds/` — 実写の背景

**合成では実写の布を再現できない。** `tests/common/mod.rs` の `weave`（縦横の正弦の積）は
周期も振幅も一定で、照明ムラ・しわ・繊維の向きを持たない。`docs/design.md` も
「周期 8px では再現しない」と認めているとおり、崩れが出るかどうかが周期の選び方に
依存してしまう。

そこで**背景だけを実写にする**。この上に「正解の被覆率が解析的に分かる合成商品」を
線形 RGB で載せると、テクスチャは本物のまま、輪郭には解析的な正解が残る
（`tests/real_backgrounds.rs` の R シーン）。

出所はいずれも**リポジトリの所有者が iPhone で撮影した写真**で、商品が写っていない
背景だけを切り出してある。ライセンスはリポジトリ本体（MIT）に従う。EXIF の向きは
適用済み（`exif_orientation` 1）、色は sRGB へ変換済みで ICC は埋め込んでいない。
`kiri info` が返す値をそのまま添える。

| ファイル | 寸法 | 素材 | uniformity | 外周 ΔE p50/p90 | 外周勾配 p50/p90 |
|---|---|---|---|---|---|
| `fabric_a.jpg` | 1200x1200 | 白い不織布。中央にしわ、右に照明の段差 | 0.256 | 9.95 / 24.21 | 11.88 / 25.61 |
| `fabric_b.jpg` | 1200x1200 | 同じ不織布の別の場所。上が明るく下が暗い照明勾配 | 0.318 | 7.75 / 17.66 | 8.38 / 19.85 |
| `desk_a.jpg` | 700x1400 | 暗い机。埃と照明ムラ。不織布よりなめらか | 0.827 | 3.41 / 5.89 | 4.65 / 9.19 |

`fabric_a.jpg` の外周勾配 11.9 / 25.6 は、この欠陥の出どころである 20MP の実写
（白い不織布の上の黒いリモコン）の 11.3 / 27.9 とほぼ同じである。**縮小ではなく
等倍で切り出してある**ので、繊維の粒は実写と同じ大きさを保っている。

## `webp/` — lossy の WebP

**kiri は lossy の WebP を書けない**（エンコーダが libwebp（C）にしか無い）ので、
入力の lossy 経路は外の道具で作った素材で確かめる（`tests/cli.rs` の
`a_lossy_webp_with_alpha_is_read_by_every_command` /
`a_lossy_webp_with_an_icc_profile_is_converted`）。どちらも 96x96、数百バイト〜1.4KB。

| ファイル | チャンク | 中身 |
|---|---|---|
| `lossy_alpha.webp` | VP8X + ALPH + VP8 | 透明な地に赤い円（縁はアンチエイリアス）。q80 |
| `lossy_icc.webp` | VP8X + ICCP + VP8 | 白地に赤い矩形。ICC は kiri の sRGB プロファイルの赤と緑の原色を入れ替えたもの（名乗りは `kiri: R/G swapped`、`tests/cli.rs` の `swapped_primaries_icc` と同じ手順）。q80 |

Pillow 12.1.0（`features.check('webp')` が True）で作った。ICC の元は kiri 自身が
PNG に埋める sRGB プロファイルなので、先に kiri で 1 枚書いてから渡す。

```bash
python3 -c "from PIL import Image; Image.new('RGB',(4,4)).save('seed.png')"
kiri convert seed.png -o srgb.png
python3 make_lossy_webp.py srgb.png tests/fixtures/webp
```

```python
# tests/fixtures/webp/ の lossy WebP を作る。
# 使い方: python3 make_lossy_webp.py <kiri が書いた ICC 付き PNG> <出力ディレクトリ>
import struct, sys
from PIL import Image, ImageDraw

srgb_png, out = sys.argv[1], sys.argv[2]

# 1) 透過付き lossy: 透明な地に赤い円（縁はアンチエイリアス）
img = Image.new("RGBA", (96 * 4, 96 * 4), (0, 0, 0, 0))
ImageDraw.Draw(img).ellipse((24 * 4, 24 * 4, 72 * 4, 72 * 4), fill=(200, 40, 40, 255))
img = img.resize((96, 96), Image.LANCZOS)
img.save(f"{out}/lossy_alpha.webp", "WEBP", lossless=False, quality=80, method=6)

# 2) ICC 付き lossy: kiri の sRGB プロファイルの赤と緑の原色を入れ替えたもの
#    （tests/cli.rs の swapped_primaries_icc と同じ手順）
icc = bytearray(Image.open(srgb_png).info["icc_profile"])
count = struct.unpack(">I", icc[128:132])[0]
def entry(sig):
    for k in range(count):
        at = 132 + 12 * k
        if icc[at:at + 4] == sig:
            return at
    raise SystemExit(f"{sig} が無い")
r, g = entry(b"rXYZ"), entry(b"gXYZ")
icc[r + 4:r + 12], icc[g + 4:g + 12] = icc[g + 4:g + 12], icc[r + 4:r + 12]
src, dst = b"sRGB IEC61966-2.1", b"kiri: R/G swapped"
assert len(src) == len(dst)
at = icc.index(src)
icc[at:at + len(dst)] = dst

img = Image.new("RGB", (96, 96), (255, 255, 255))
ImageDraw.Draw(img).rectangle((28, 28, 68, 68), fill=(200, 40, 40))
img.save(f"{out}/lossy_icc.webp", "WEBP", lossless=False, quality=80, method=6,
         icc_profile=bytes(icc))
```

## 正解つきの実写を足したいとき

リポジトリには置かない。実写に正解アルファを付ける作業（Photoshop / GIMP /
Pixelmator で切り抜いてアルファをグレー PNG に書き出す）は人にしかできず、
素材ごとに権利も違う。代わりに環境変数で差し込める。

```text
KIRI_BENCH_DIR=~/bench cargo test --release --test real_backgrounds -- --ignored --nocapture
```

`<name>.jpg|png` と `<name>.alpha.png`（8bit グレー、255 = 商品）の対を置く。
`<name>.json` があれば設定に使う（キーは `kiri cutout` のオプション名と同じ）。

```json
{ "bbox": [0, 0.354, 0.9834, 0.662], "normalized": true, "tolerance": 60 }
```
