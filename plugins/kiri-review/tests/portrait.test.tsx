import { expect, test } from 'claude-code/testing'
import { MAX_B64 } from '../hooks/register'

const B64 = 'A'.repeat(MAX_B64)

test('縦長の画像でも描けて、右下をドラッグで選べる', async ($, on) => {
  on('fs.stat', () => ({ value: { kind: 'file', size: 1, mtimeMs: 1, isLink: false, realPath: '/w/v.heic' } }) as never)
  on('fs.read', () => ({ value: { base64: B64 } }) as never)
  on('process.run', (_$, e) => {
    const out = e.argv.at(-1) === '/w/v.heic' ? 'pixelWidth: 4032\n  pixelHeight: 3024' : 'pixelWidth: 480\n  pixelHeight: 640'
    return { value: { exitCode: 0, stdout: out, stderr: '', isStdoutTruncated: false, isStderrTruncated: false } }
  })
  on('ui.open', () => ({ value: { isPlaced: true } }) as never)
  await $.command.run({ command: 'kiri-review', args: 'v.heic' } as never)
  const ui = await $.ui.mount({ plugin: 'kiri-review', surface: 'desktop', component: 'Pane', requestId: 'kiri-review',
    props: { title: 'kiri review', isFocused: false, bodyColumns: 60 } as never })
  const svg = await ui.find({ type: 'Svg' })
  expect(svg).toBeDefined()
  await ui.resize({ columns: 40, rows: 40 })
  await ui.pointer({ type: 'down', x: 30, y: 30, button: 'left' })
  // 領域の外まで引いても画像の端で止まる
  await ui.pointer({ type: 'up', x: 50, y: 45, button: 'left' })
  await ui.input({ key: 'comment', text: '右下' })
  expect(await ui.find({ type: 'Text', text: /\[x 2268–3024, y 3024–4032 px.*\] 右下/ })).toBeDefined()
})
