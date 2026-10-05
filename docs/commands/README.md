# コマンド別ドキュメント

> [README](../../README.md) › コマンド別ドキュメント

[README](../../README.md) は看板である。**ここから下は、必要になった 1 つだけを開く
ための置き場所**で、README に戻らなくても読み切れる単位に分けてある。

| | コマンド / 主題 | 中身 |
|---|---|---|
| 01 | [kiri schema](01-schema.md) | 契約を自分で配る。`fields` の読み方と `schema_version` |
| 02 | [kiri info](02-info.md) | 素材を測る。`subject` / 傾き / `--border` に依らない判定 |
| 03 | [出力の共通事項](03-output.md) | 色空間、sRGB の名乗り、`--max-bytes`、`--derive` と `--manifest` |
| 04 | [convert / resize / rotate](04-convert-resize-rotate.md) | 形式変換・リサイズ・回転 |
| 05 | [kiri cutout](05-cutout.md) | 背景透過の切り抜き。**全オプションの表と、6 つの主題への索引** |
| 06 | [kiri batch](06-batch.md) | spec を渡してまとめて処理する。`set` で揃える |
| 07 | [kiri lint](07-lint.md) | 既存画像をモール規格で検査する |
| 08 | [kiri compose](08-compose.md) | 素材と文字を 1 枚へ組む |
| 09 | [kiri model](09-model.md) | セグメンテーションモデルの素性と置き場所 |

`kiri cutout` だけは分量が大きいので、更に 6 つへ分けてある。入口は
[kiri cutout](05-cutout.md#読む順) の「読む順」。

## この分け方について

**節の見出しは分割前のまま変えていない。** README やソースのコメントが見出しの名前で
参照しているので、名前は識別子として固定し、どのファイルにあるかはこの索引で引く。
`docs/design/` と `docs/phases/` が番号に対して取っているのと同じ扱いである。

ここは**使い方**の文書である。**なぜその方式を選んだか**は
[設計ドキュメント](../design.md)、**いつ何を作ったか**は
[実装計画](../implementation-plan.md) と [docs/phases/](../phases/) にある。
