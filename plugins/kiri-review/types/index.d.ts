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
  mtimeMs: number
}

/** 画像の上をドラッグして選んだ、まだコメントを付けていない範囲（元画像の画素） */
export type ReviewSelection = {
  path: string
  /** 選んだときの画像の寸法。読み直して変わっていたら選択を外す */
  width: number
  height: number
  x0: number
  y0: number
  x1: number
  y1: number
}

export type ReviewComment = {
  id: number
  /** どの画像へのコメントか（ReviewImage.path） */
  path: string
  /** 元画像の画素での範囲。画像全体なら 0, 0, width, height */
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
      /** ドラッグで選んだ範囲。無ければ次のコメントは画像全体へ */
      selection: ReviewSelection | null
      error: string
      /** これまでに付けたコメントの番号の最大。番号を使い回さないため */
      lastId: number
      /** 一覧の左端に出しているサムネイルの番号（0 から）。多い画像は横に流して見る */
      stripOffset: number
      /** 「一覧から外す」を一度押した画像のパス。コメントのある画像は二度押しで外す */
      confirmRemove: string
    }
  }
}
