import { expect, test } from 'claude-code/testing'

const PANE = { component: 'Pane', requestId: 'kiri-review' } as const
const PROPS = { title: 'kiri review', isFocused: false, bodyColumns: 80 }
const ok = (stdout = '') =>
  ({ value: { exitCode: 0, stdout, stderr: '', isStdoutTruncated: false, isStderrTruncated: false } })

test('離したのが窓の外でも、ボタン無しの move で枠を消す', async ($, on) => {
  on('fs.stat', () => ({ value: { kind: 'file', size: 1, mtimeMs: 1, isLink: false, realPath: '/w/a.png' } }) as never)
  on('fs.read', () => ({ value: { base64: 'QUJD' } }) as never)
  on('process.run', (_$, e) => ok(e.argv.at(-1) === '/w/a.png' ? 'pixelWidth: 800\n  pixelHeight: 600' : 'pixelWidth: 640\n  pixelHeight: 480'))
  on('ui.open', () => ({ value: { isPlaced: true } }) as never)
  await $.command.run({ command: 'kiri-review', args: 'a.png' } as never)
  const ui = await $.ui.mount({ plugin: 'kiri-review', surface: 'desktop', ...PANE, props: PROPS as never })
  await ui.resize({ columns: 80, rows: 30 })
  await ui.pointer({ type: 'down', x: 1, y: 1, button: 'left' })
  await ui.pointer({ type: 'move', x: 5, y: 5, button: 'left' })
  expect(await ui.findAll({ type: 'Box', in: 'drag' })).toHaveLength(2)
  await ui.pointer({ type: 'move', x: 9, y: 9 })
  expect(await ui.findAll({ type: 'Box', in: 'drag' })).toHaveLength(1)
  await ui.pointer({ type: 'up', x: 9, y: 9, button: 'left' })
  expect(await ui.find({ type: 'Text', text: /^範囲: 画像全体/ })).toBeDefined()
  await ui.unmount()
})

test('読み直して寸法が変わった画像や、一覧から外した画像の選択は外す', async ($, on) => {
  let size = 'pixelWidth: 800\n  pixelHeight: 600'
  on('fs.stat', (_$, e) => ({ value: { kind: 'file', size: 1, mtimeMs: 1, isLink: false, realPath: `/w/${(e as { path: string }).path.split('/').at(-1)}` } }) as never)
  on('fs.read', () => ({ value: { base64: 'QUJD' } }) as never)
  on('process.run', (_$, e) => ok(e.argv.at(-1)?.startsWith('/w/') === true ? size : 'pixelWidth: 640\n  pixelHeight: 480'))
  on('ui.open', () => ({ value: { isPlaced: true } }) as never)
  on('ui.toast', () => ({ value: undefined }) as never)
  await $.command.run({ command: 'kiri-review', args: 'a.png b.png' } as never)
  const ui = await $.ui.mount({ plugin: 'kiri-review', surface: 'desktop', ...PANE, props: PROPS as never })
  await ui.resize({ columns: 80, rows: 30 })
  const drag = async () => {
    await ui.pointer({ type: 'down', x: 0, y: 0, button: 'left' })
    await ui.pointer({ type: 'up', x: 39, y: 14, button: 'left' })
  }
  await drag()
  expect(await ui.find({ type: 'Text', text: /^範囲: x 0–400/ })).toBeDefined()
  // 同じ寸法で読み直したなら残す
  await ui.press({ key: 'reload' })
  expect(await ui.find({ type: 'Text', text: /^範囲: x 0–400/ })).toBeDefined()
  size = 'pixelWidth: 1600\n  pixelHeight: 1200'
  await ui.press({ key: 'reload' })
  expect(await ui.find({ type: 'Text', text: /^範囲: 画像全体/ })).toBeDefined()
  // 外して kiri がまた書いても、前の選択は戻らない
  await drag()
  await ui.press({ key: 'remove' })
  await $.command.run({ command: 'kiri-review', args: 'a.png b.png' } as never)
  expect(await ui.find({ type: 'Text', text: /^範囲: 画像全体/ })).toBeDefined()
  await ui.unmount()
})
