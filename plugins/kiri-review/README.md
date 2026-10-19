# kiri-review

kiri が書き出した画像を Claude Code のペインに出し、場所ごとに「ここを直して」とコメントして Claude に渡す mod（関数フック製のプラグイン）。

## 使い方

- `/kiri-review <画像のパス>` か、Claude に「この画像をペインで見せて」と頼む（Claude は `mcp__kiri-review__open` を呼ぶ）。
- 画像に 8 列の格子（A1, B2, …）が重なる。「列」「行」でマスを選び、「コメント」に書いて Enter で追加する。先頭に `B2:D4 …` と書けば範囲で指定できる。
- 「Claude に送る」で、画像のパスと寸法、各コメントのマスと元画像の画素範囲がプロンプトとして届く。
- 一度開いたあとは、Claude が `kiri ... -o <画像>` を実行するたびにペインの画像が差し替わる。

## 制約

- 画像を描くのは Claude Code Desktop のペインだけ（`Svg` 要素）。ターミナルでは格子とコメント欄のみ。
- ペインはクリック座標を取れないので、場所は格子で指定する。
- 縮小画像は macOS の `qlmanage` と `sips` で作る（HEIC の回転指定を反映するため Quick Look を使う）。macOS 専用。
- `Svg` の source は 131072 文字まで、`Select` の選択肢は 64 個まで。超えるとペインごと描かれない。

## インストール

```
/plugin install kiri-review --marketplace tsutsumi389/kiri
```

## 開発

```
claude plugin validate plugins/kiri-review
claude plugin test plugins/kiri-review
```
