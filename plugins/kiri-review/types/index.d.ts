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
  /** 格子の列数・行数 */
  cols: number
  rows: number
  mtimeMs: number
}

export type ReviewComment = {
  id: number
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
      image: ReviewImage | null
      comments: ReviewComment[]
      cell: string
      error: string
    }
  }
}
