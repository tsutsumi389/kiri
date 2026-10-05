# kiri model

> [README](../../README.md) › [コマンド別ドキュメント](README.md) › kiri model

セグメンテーションモデルの素性と置き場所を返す。**kiri はネットワークを触らない**
ので、取得の案内はここが唯一の窓口である。

```
$ kiri model list
$ kiri model list --json
```

`--json` は URL・MD5・SHA-256・ライセンス・想定パス・存在の有無・検証結果と、
そのまま貼れる `curl` の 1 行（`hint`）を返す。`segment_available` は
**この build が推論できるか**で、false なら `--segment` は
`SEGMENT_UNAVAILABLE` で断られる（一覧そのものは返る）。

詳しくは [意味の事前知識](05-4-guidance.md#意味の事前知識モデルに何を聞くか)を参照。
