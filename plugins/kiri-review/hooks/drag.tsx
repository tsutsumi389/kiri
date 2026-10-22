import type { ClientModule, ClientPointerEvent } from 'claude-code'

/**
 * path: どの画像の上に重ねているか。送るときに付けて返し、切り替えのあとに届いた選択を取り違えない。
 * aspect: 画像の縦 / 横。領域を画像の高さまで伸ばす行数を決める
 */
export type DragProps = { path: string; aspect: number }

/** 領域の中の位置（マス単位）。coarse はマスの中の位置が分からない面 */
type Point = { x: number; y: number; coarse: boolean }
type DragState = { from?: Point; to?: Point }
type Box01 = { x0: number; y0: number; x1: number; y1: number }

const clamp = (v: number) => Math.min(1, Math.max(0, v))

function point(e: ClientPointerEvent): Point {
  return e.fine === undefined ? { x: e.x, y: e.y, coarse: true } : { x: e.fine.x, y: e.fine.y, coarse: false }
}

/**
 * 2 点で囲む範囲を領域に対する 0–1 へ。マス単位の面では、両端のマスを丸ごと含める
 * （ドラッグ中に描く枠と同じ範囲になり、端のマスまで選べる）
 */
export function span(a: Point, b: Point, columns: number, rows: number): Box01 {
  const pad = a.coarse || b.coarse ? 1 : 0
  return {
    x0: clamp(Math.min(a.x, b.x) / Math.max(1, columns)),
    y0: clamp(Math.min(a.y, b.y) / Math.max(1, rows)),
    x1: clamp((Math.max(a.x, b.x) + pad) / Math.max(1, columns)),
    y1: clamp((Math.max(a.y, b.y) + pad) / Math.max(1, rows)),
  }
}

/**
 * 画像の上に重ねる透明な領域。ドラッグの間は枠を描き、離したら範囲（0–1）を
 * フックのモジュールへ送る。画像そのものはフック側の Svg が描く
 */
const Drag: ClientModule<DragProps, DragState> = (props, surface) => {
  const { Box, Text } = surface.elements
  const state = surface.state ?? {}
  // 毎回付け直して、いまの props と state を使う
  surface.onPointer(e => {
    const now = surface.state ?? {}
    const p = point(e)
    if (e.type === 'down' && e.button === 'left') surface.setState({ from: p, to: p })
    else if (e.type === 'move' && now.from !== undefined) {
      // ボタンを離したのが窓の外で up が来なかったときは、ボタン無しの move で終える
      surface.setState(e.button === 'left' ? { ...now, to: p } : {})
    } else if (e.type === 'up' && now.from !== undefined) {
      surface.post({ path: props.path, ...span(now.from, p, surface.columns, surface.rows) })
      surface.setState({})
    }
  })
  // デスクトップでは Client の高さは描いた行数で決まり、height の指定は効かない。
  // 画像より多めに空行を描いて伸ばすと、重ねた Box（画像の大きさ）で切られて画像とちょうど重なる。
  // 1 マスは横より縦が長いので、列数 × 縦横比の行数あれば足りる
  const lines = Math.max(1, Math.ceil(Math.max(surface.columns, 40) * Math.min(4, Math.max(0.1, props.aspect || 1))))
  const filler = Array.from({ length: lines }, () => <Text> </Text>)
  const { from, to } = state
  if (from === undefined || to === undefined) return <Box flexDirection="column" width="100%">{filler}</Box>
  const r = span(from, to, surface.columns, surface.rows)
  const left = Math.floor(r.x0 * surface.columns)
  const top = Math.floor(r.y0 * surface.rows)
  const right = Math.ceil(r.x1 * surface.columns)
  const bottom = Math.ceil(r.y1 * surface.rows)
  return (
    <Box flexDirection="column" width="100%">
      {filler}
      <Box position="absolute" left={left} top={top} width={Math.max(1, right - left)} height={Math.max(1, bottom - top)}
        borderStyle="single" borderColor="blue" />
    </Box>
  )
}

export default Drag
