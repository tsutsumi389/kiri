import { describe, expect, test } from 'claude-code/testing'

import { copyImageArgv } from '../hooks/register'

const PANE = { component: 'Pane', requestId: 'kiri-review' } as const
const PROPS = { title: 'kiri review', isFocused: false, bodyColumns: 80 }

describe('クリップボードに入れる形式', () => {
  test('JPEG は JPEG、PNG は透過を残して PNGf、パスは引数で渡す', async () => {
    expect(copyImageArgv('/w/a b.JPG').join(' ')).toContain('as JPEG picture')
    expect(copyImageArgv('/w/a.png').join(' ')).toContain('«class PNGf»')
    expect(copyImageArgv('/w/a.png').at(-1)).toBe('/w/a.png')
    expect(copyImageArgv('/w/a.webp', '/c/clipboard.png').at(-1)).toBe('/c/clipboard.png')
  })
})

test('画像をコピーを押すと、表示中の元画像をクリップボードへ入れる', async ($, on) => {
  const ran: string[][] = []
  on('fs.stat', () => ({ value: { kind: 'file', size: 1, mtimeMs: 1, isLink: false, realPath: '/w/out.png' } }) as never)
  on('fs.read', () => ({ value: { base64: 'QUJD' } }) as never)
  on('process.run', (_$, e) => {
    ran.push([...e.argv])
    const out = e.argv.at(-1) === '/w/out.png' ? 'pixelWidth: 800\n  pixelHeight: 600' : 'pixelWidth: 640\n  pixelHeight: 480'
    return { value: { exitCode: 0, stdout: out, stderr: '', isStdoutTruncated: false, isStderrTruncated: false } }
  })
  on('ui.open', () => ({ value: { isPlaced: true } }) as never)
  const toasts: string[] = []
  on('ui.toast', (_$, e) => {
    toasts.push(String((e as { text?: string }).text ?? ''))
    return { value: undefined } as never
  })
  await $.command.run({ command: 'kiri-review', args: 'out.png' } as never)
  const ui = await $.ui.mount({ plugin: 'kiri-review', surface: 'desktop', ...PANE, props: PROPS as never })
  await ui.press({ key: 'copy' })
  const osa = ran.filter(a => a[0] === 'osascript')
  expect(osa.length).toBe(1)
  expect(osa[0]?.at(-1)).toBe('/w/out.png')
  expect(toasts.some(t => t.includes('out.png をクリップボードにコピーしました'))).toBe(true)
  await ui.unmount()
})
