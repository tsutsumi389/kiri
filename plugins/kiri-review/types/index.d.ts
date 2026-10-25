/** ペインに出す縮小 JPEG（base64）とその寸法 */
export type ReviewShot = {
  preview: string
  previewWidth: number
  previewHeight: number
}

/** 差し替えられる前の版。縮小画像と、元画像の写し（拡大して比べるため） */
export type ReviewVersion = ReviewShot & {
  /** 元画像の写しのパス（.cache/versions の下）。写せなかったら '' */
  snapshot: string
  width: number
  height: number
  mtimeMs: number
  /** 何版目か（1 から） */
  version: number
}

export type ReviewImage = ReviewShot & {
  /** 元画像の絶対パス */
  path: string
  /** 元画像の画素寸法 */
  width: number
  height: number
  /** 一覧に並べる小さな JPEG（base64）とその寸法 */
  thumb: string
  thumbWidth: number
  thumbHeight: number
  mtimeMs: number
  /** 透過のある画像。縮小と拡大は市松模様の上に重ねる */
  hasAlpha?: boolean
  /** 読んだときの元画像の写し（.cache/versions の下）。差し替えられたら前の版になる。写せなかったら '' */
  snapshot?: string
  /** 何版目か（1 から）。同じパスの画像が差し替わるたびに増える */
  version?: number
  /** 一つ前の版。差し替わったことが無ければ無い */
  before?: ReviewVersion
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

/**
 * コメントの状態。draft: まだ送っていない、sent: 送った、check: 送ったあと画像が差し替わった（確認待ち）、
 * fixed: 人が「直った」を付けた、again: 人が「まだ」を付けた（次の送信に入れる）
 */
export type ReviewStatus = 'draft' | 'sent' | 'check' | 'fixed' | 'again'

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
  /** 無ければ draft（追跡を足す前のコメント） */
  status?: ReviewStatus
  /** 送ったときの画像の版 */
  sentVersion?: number
}

/** 元画像から原寸で切り出して大きく出している範囲 */
export type ReviewZoom = {
  path: string
  /** 切り出したときの画像の版。差し替わったら作り直す */
  version: number
  /** 切り出した範囲（元画像の画素、左上が原点） */
  x: number
  y: number
  w: number
  h: number
  after: ReviewShot
  /** 前の版の同じ範囲。前の版が無いか、切り出せなかったら無い */
  before?: ReviewShot
  /** 前の版を切り出せなかった理由 */
  beforeError?: string
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
      /** 拡大して出している範囲。無ければ画像全体 */
      zoom: ReviewZoom | null
      /** 前の版を出しているか */
      compare: boolean
      /** 拡大を作っている間の案内 */
      busy: string
    }
  }
}
