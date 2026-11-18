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

/** kiri の警告が指した範囲を、出力画像の画素へ写したもの（左上が原点、右と下の端は含まない） */
export type KiriRegion = {
  /** 警告のコード（HALO_REMAINS / CONTOUR_ROUGH / RIM_CONTAMINATED） */
  code: string
  score: number
  x0: number
  y0: number
  x1: number
  y1: number
  /** kiri が出した入力画像での範囲（両端を含む）。写す前のもの */
  bbox?: [number, number, number, number]
}

/** 出力画像と入力画像の対応（kiri の outputs[].source_rect / content_rect。両端を含む） */
export type KiriMapping = {
  /** 入力画像のパス（絶対にできなければ kiri に渡したまま） */
  input: string
  /** 入力画像の寸法（EXIF を当てたあと） */
  inputWidth: number
  inputHeight: number
  source: [number, number, number, number]
  content: [number, number, number, number]
}

/** --edge-repair auto の結果 */
export type KiriRepair = {
  repaired: number
  gatedBy: string | null
  before: Record<string, number | null>
  after: Record<string, number | null>
}

/** kiri の --json の結果から拾った、出力画像 1 枚ぶんの情報 */
export type KiriInfo = {
  /** この画像を書いた kiri のコマンド（&& などでつないだ前後と、頭の VAR=... は落とす）。ラベルの書き出しに残す */
  command?: string
  /** kiri を走らせた作業ディレクトリ。分からなければ無い */
  cwd?: string
  regions: KiriRegion[]
  /** 警告ごとの上限で、範囲を落としたか */
  truncated?: boolean
  mapping?: KiriMapping
  repair?: KiriRepair
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
  /** この版を書いた kiri の結果から拾ったもの。--json の結果が無ければ無い */
  kiri?: KiriInfo
  /** 入力画像の隣にある正解アルファ（<名前>.alpha.png、255 = 商品）のパス。無ければ無い */
  truth?: string
}

/** 正解アルファとの差分を色分けして重ねた画像（欠けは赤、漏れは青） */
export type ReviewDiff = {
  path: string
  /** 作ったときの画像の版。差し替わったら使わない */
  version: number
  /** 原寸の合成画像（.cache の下）。拡大はここから切り出す */
  file: string
  shot: ReviewShot
  /** 欠け・漏れの面積（正解の商品の面積に対する割合） */
  missing: number
  leak: number
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

/**
 * 指摘の種類。halo: 縁残り（背景が縁に残った）、missing: 欠け（商品が削れた）、extra: 残り（背景が塊で残った）、
 * rough: ギザギザ（輪郭が粗い）、tint: 混色（縁に背景の色が混ざった）、other: その他
 */
export type ReviewKind = 'halo' | 'missing' | 'extra' | 'rough' | 'tint' | 'other'

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
  /** 付けたときの画像の寸法。差し替えで寸法が変わったら、範囲を同じ割合の位置へ移す */
  width?: number
  height?: number
  /** 指摘の種類。無ければ未分類 */
  kind?: ReviewKind
  /** kiri の指摘から作ったコメントなら、その警告のコード */
  code?: string
  /** 付けたときの画像の版と、その版での範囲。差し替わって範囲が動いても、ラベルはこの版のものとして書き出す */
  seen?: ReviewSeen
}

/** コメントを付けたときの画像の版と、その版での範囲・kiri の結果 */
export type ReviewSeen = {
  mtimeMs: number
  version: number
  width: number
  height: number
  x0: number
  y0: number
  x1: number
  y1: number
  kiri?: KiriInfo
}

/** kiri の指摘 1 件への人の判定。hit: 当たり（そこに欠陥がある）、miss: 外れ（欠陥は無い） */
export type ReviewVerdict = 'hit' | 'miss'

/** 画像の 1 版ぶんの判定。差し替わったあとも、その版の kiri の結果といっしょにラベルへ書き出す */
export type ReviewJudged = {
  path: string
  /** 判定したときの版（範囲は画像全体）と、その版の kiri の結果 */
  seen: ReviewSeen
  /** 指摘の鍵（コードと出力画像での範囲）ごとの判定 */
  verdicts: Record<string, ReviewVerdict>
  /** いまの判定をラベルに書き出してあるか。付け直すと外れる。書き出していない判定のある画像は、黙って一覧から外さない */
  saved?: boolean
}

/** 指摘を順に確かめている途中の位置 */
export type ReviewTriage = {
  /** いま確かめている指摘のある画像 */
  path: string
  /** いま確かめている指摘（画像の版と指摘の鍵） */
  id: string
  /** 飛ばした指摘。一巡するまで出さない */
  skipped: string[]
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
  /** 正解との差分の合成画像から切り出したか */
  diff?: boolean
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
      /** 次に付けるコメントの種類。無ければ未分類 */
      kind: ReviewKind | null
      /** kiri の指摘への判定（画像の版ごと） */
      judged: ReviewJudged[]
      /** 指摘を順に確かめている途中なら、その位置 */
      triage: ReviewTriage | null
      /** 正解との差分を出しているなら、その合成画像 */
      diff: ReviewDiff | null
      /** このセッションで、中身（ラベルか判定）のある行を書き出した画像の版。中身が無くなったら、取り消しの行を書く */
      exported: string[]
    }
  }
}
