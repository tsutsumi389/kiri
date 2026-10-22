import type { ClientModule } from 'claude-code'

/** aspect: 一覧の Svg の縦 / 横。領域を一覧の高さまで伸ばす行数を決める */
export type StripProps = { aspect: number }

type StripState = { from?: number }

/**
 * サムネイルの一覧に重ねる透明な領域。クリックした位置（横の 0–1）か、
 * 横にドラッグした量（領域の幅に対する割合）をフックのモジュールへ送る。
 * デスクトップのペインはホイールを Client に渡さないので、流すのは端の ‹ › のクリックかドラッグ
 */
const Strip: ClientModule<StripProps, StripState> = (props, surface) => {
  const { Box, Text } = surface.elements
  surface.onPointer(e => {
    const now = surface.state ?? {}
    const columns = Math.max(1, surface.columns)
    if (e.type === 'down' && e.button === 'left') surface.setState({ from: e.x })
    else if (e.type === 'move' && now.from !== undefined && e.button !== 'left') surface.setState({})
    else if (e.type === 'up' && now.from !== undefined) {
      // 1 マスまでの動きはクリック。それより動いたら、動いた分だけ一覧を流す
      surface.post(Math.abs(e.x - now.from) <= 1 ? { pick: (now.from + 0.5) / columns } : { drag: (e.x - now.from) / columns })
      surface.setState({})
    }
  })
  // drag.tsx と同じく、高さは描いた行数で決まるので多めに空行を描き、重ねた Box で切る
  const lines = Math.max(1, Math.ceil(Math.max(surface.columns, 40) * Math.min(4, Math.max(0.05, props.aspect || 1))))
  return <Box flexDirection="column" width="100%">{Array.from({ length: lines }, () => <Text> </Text>)}</Box>
}

export default Strip
