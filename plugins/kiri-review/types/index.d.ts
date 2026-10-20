export type ReviewImage = {
  /** 元画像の絶対パス */
  path: string
  /** 元画像の画素寸法 */
  width: number
  height: number
  /** ペイン用に縮めた JPEG（base64）とその寸法 */
  preview: string
  previewWidth: number
  previewHeight: number
  /** 一覧に並べる小さな JPEG（base64）とその寸法 */
  thumb: string
  thumbWidth: number
  thumbHeight: number
  /** 格子の列数・行数 */
  cols: number
  rows: number
  mtimeMs: number
}

export type ReviewComment = {
  id: number
  /** どの画像へのコメントか（ReviewImage.path） */
  path: string
  /** 'C3' や 'B2:D4' */
  cells: string
  /** 元画像の画素での範囲 */
  x0: number
  y0: number
  x1: number
  y1: number
  text: string
}

declare module 'claude-code' {
  interface PluginState {
    'kiri-review': {
      /** ペインに並べている画像。順序は一覧の順 */
      images: ReviewImage[]
      /** 大きく出している画像のパス */
      current: string
      comments: ReviewComment[]
      cell: string
      error: string
      /** これまでに付けたコメントの番号の最大。番号を使い回さないため */
      lastId: number
    }
  }
}
