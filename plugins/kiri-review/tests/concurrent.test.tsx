import { expect, mock, test } from 'claude-code/testing'

type File = { w: number; h: number; b64: string }

const ok = (stdout = '') =>
  ({ value: { exitCode: 0, stdout, stderr: '', isStdoutTruncated: false, isStderrTruncated: false } })
const fail = (stderr: string) =>
  ({ value: { exitCode: 1, stdout: '', stderr, isStdoutTruncated: false, isStderrTruncated: false } })

test('読み込みが重なっても、縮小画像と寸法が別の画像と混ざらない', async ($, on) => {
  const clock = mock.clock(on)
  // qlmanage と sips が書く先を、本物のファイルのように持つ
  const files = new Map<string, File>([
    ['/w/a.png', { w: 800, h: 600, b64: 'QUFB' }],
    ['/w/b.png', { w: 600, h: 800, b64: 'QkJC' }],
  ])
  let delayed = false

  on('fs.stat', (_$, e) =>
    ({ value: { kind: 'file', size: 1, mtimeMs: 1, isLink: false, realPath: `/w/${(e as { path: string }).path.split('/').at(-1)}` } }) as never)
  on('fs.read', (_$, e) => {
    const f = files.get((e as { path: string }).path)
    return { value: { base64: f?.b64 ?? '' } } as never
  })
  on('process.run', async (_$, e) => {
    const [cmd, ...args] = e.argv
    if (cmd === 'mkdir') return ok()
    if (cmd === 'rm') {
      files.delete(args.at(-1) ?? '')
      return ok()
    }
    if (cmd === 'qlmanage') {
      const src = args.at(-1) ?? ''
      const dir = args[args.indexOf('-o') + 1]
      const f = files.get(src)
      if (f === undefined) return fail(`no ${src}`)
      const s = Number(args[args.indexOf('-s') + 1]) / Math.max(f.w, f.h)
      files.set(`${dir}/${src.split('/').at(-1)}.png`, { w: Math.round(f.w * s), h: Math.round(f.h * s), b64: f.b64 })
      return ok()
    }
    if (cmd === 'sips' && args.includes('--out')) {
      const thumb = args[args.indexOf('--out') - 1] ?? ''
      const f = files.get(thumb)
      if (f === undefined) return fail(`${thumb} がありません`)
      files.set(args[args.indexOf('--out') + 1] ?? '', f)
      return ok()
    }
    if (cmd === 'sips' && args[0] === '-g') {
      const path = args.at(-1) ?? ''
      // 最初の画像の、縮小画像の寸法を読むところで待たせ、その間に次の読み込みを走らせる
      if (path.endsWith('.jpg') && !delayed) {
        delayed = true
        await clock.sleep(100)
      }
      const f = files.get(path)
      if (f === undefined) return fail(`${path} がありません`)
      return ok(`pixelWidth: ${f.w}\n  pixelHeight: ${f.h}`)
    }
    return fail(`想定外: ${e.argv.join(' ')}`)
  })
  on('ui.open', () => ({ value: { isPlaced: true } }) as never)

  const pa = $.command.run({ command: 'kiri-review', args: 'a.png' } as never)
  await clock.settle()
  const pb = $.command.run({ command: 'kiri-review', args: 'b.png' } as never)
  await clock.settle()
  await clock.advance(100)
  const [ra, rb] = await Promise.all([pa, pb])

  expect(ra.text).toContain('/w/a.png（800×600）')
  expect(rb.text).toContain('/w/b.png（600×800）')

  // あとから頼んだ b.png が、b.png 自身の縮小画像と寸法で残る
  const ui = await $.ui.mount({ plugin: 'kiri-review', surface: 'desktop', component: 'Pane', requestId: 'kiri-review',
    props: { title: 'kiri review', isFocused: false, bodyColumns: 80 } as never })
  expect(await ui.find({ type: 'Text', text: /^b\.png · 600×800/ })).toBeDefined()
  const svg = await ui.find({ type: 'Svg' })
  const source = String((svg as { props?: { source?: unknown } } | undefined)?.props?.source ?? '')
  expect(source).toContain('viewBox="0 0 480 640"')
  expect(source).toContain('base64,QkJC')
  await ui.unmount()
})
