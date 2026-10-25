import { describe, expect, test } from 'claude-code/testing'

import { isSendable, markReplaced, markSent, openCounts, promptFor } from '../hooks/register'

const PANE = { component: 'Pane', requestId: 'kiri-review' } as const
const PROPS = { title: 'kiri review', isFocused: false, bodyColumns: 80 }
const ok = (stdout = '') =>
  ({ value: { exitCode: 0, stdout, stderr: '', isStdoutTruncated: false, isStderrTruncated: false } })

const c = (id: number, path: string, status?: 'draft' | 'sent' | 'check' | 'fixed' | 'again') =>
  ({ id, path, x0: 0, y0: 0, x1: 10, y1: 10, text: `c${id}`, ...(status === undefined ? {} : { status }) })

describe('指摘の状態', () => {
  test('送るのは未送信と「まだ」だけ。一覧の件数は直った以外を数える', async () => {
    expect([c(1, 'a'), c(2, 'a', 'sent'), c(3, 'a', 'check'), c(4, 'a', 'fixed'), c(5, 'a', 'again')].map(isSendable))
      .toEqual([true, false, false, false, true])
    const counts = openCounts([c(1, 'a', 'fixed'), c(2, 'a', 'sent'), c(3, 'b'), c(4, 'b', 'again'), c(5, 'c', 'fixed')])
    expect([...counts]).toEqual([['a', 1], ['b', 2]])
  })
  test('送ったら送信済みと版を付け、差し替わったら送信済みだけ確認待ちにする', async () => {
    const sent = markSent([c(1, 'a'), c(2, 'a', 'again'), c(3, 'b'), c(4, 'a', 'fixed')], new Set([1, 2, 4]))
    expect(sent.map(x => x.status)).toEqual(['sent', 'sent', undefined, 'fixed'])
    const replaced = markReplaced([...sent, c(5, 'b', 'sent')], new Set(['a']))
    expect(replaced.map(x => x.status)).toEqual(['check', 'check', undefined, 'fixed', 'sent'])
  })
  test('「まだ」のコメントは再指摘として送る', async () => {
    const img = { path: 'a', width: 100, height: 100, preview: '', previewWidth: 1, previewHeight: 1, thumb: '', thumbWidth: 1, thumbHeight: 1, mtimeMs: 0 }
    const text = promptFor([img], [c(1, 'a', 'again'), c(2, 'a')])
    expect(text).toContain('1. x 0–10, y 0–10 px（横 0–10%、縦 0–10%）: c1（再指摘: 前回の修正のあとも直っていません）')
    expect(text).toContain('2. x 0–10, y 0–10 px（横 0–10%、縦 0–10%）: c2\n')
  })
})

test('送ったコメントは残り、差し替わったら確認待ちになって、「まだ」のものだけ次に送る', async ($, on) => {
  let mtime = 1
  const toasts: string[] = []
  const sent: string[] = []
  on('fs.stat', (_$, e) =>
    ({ value: { kind: 'file', size: 1, mtimeMs: mtime, isLink: false, realPath: `/w/${(e as { path: string }).path.split('/').at(-1)}` } }) as never)
  on('fs.read', () => ({ value: { base64: 'QUJD' } }) as never)
  on('process.run', (_$, e) => {
    const p = e.argv.at(-1) ?? ''
    return ok(p.startsWith('/w/') || p.includes('/versions/') ? 'pixelWidth: 800\n  pixelHeight: 600' : 'pixelWidth: 640\n  pixelHeight: 480')
  })
  on('ui.open', () => ({ value: { isPlaced: true } }) as never)
  on('ui.toast', (_$, e) => {
    toasts.push(e.text)
    return { value: undefined } as never
  })
  on('prompt.submit', (_$, e) => {
    sent.push(e.text)
    return { text: e.text } as never
  })
  on('tool.call', { tool: 'Bash' }, () => ({ result: { stdout: 'done', stderr: '', interrupted: false } }) as never)

  await $.command.run({ command: 'kiri-review', args: 'a.png b.png' } as never)
  const ui = await $.ui.mount({ plugin: 'kiri-review', surface: 'desktop', ...PANE, props: PROPS as never })
  const strip = async () => {
    const svg = (await ui.findAll({ type: 'Svg' }))[0] as { props?: { source?: unknown } } | undefined
    return String(svg?.props?.source ?? '')
  }
  const sendLabel = async () => ((await ui.find({ type: 'Button', key: 'send' })) as { props?: { label?: unknown } } | undefined)?.props?.label
  // 件数の赤丸（一覧の番号と違い text-anchor が付く）
  const badges = async () => [...(await strip()).matchAll(/text-anchor="middle">(\d+)<\/text>/g)].map(m => m[1])

  await ui.resize({ in: 'drag', columns: 80, rows: 30 })
  await ui.pointer({ in: 'drag', type: 'down', x: 0, y: 0, button: 'left' })
  await ui.pointer({ in: 'drag', type: 'up', x: 39, y: 14, button: 'left' })
  await ui.input({ key: 'comment', text: '縁にトゲ' })
  await ui.input({ key: 'comment', text: '全体に暗い' })
  await ui.press({ key: 'send' })
  expect(sent).toHaveLength(1)
  // 送っても消さず、送信済みとして残す。一覧の件数も残る
  expect(await ui.find({ type: 'Text', text: /^1\. 縁にトゲ（送信済み）/ })).toBeDefined()
  expect(await ui.find({ type: 'Text', text: /^2\. 全体に暗い（画像全体）（送信済み）/ })).toBeDefined()
  expect(await ui.find({ type: 'Button', key: 'send' })).toBeUndefined()
  expect(await badges()).toEqual(['2'])

  // kiri が a.png を書き直すと確認待ちになる
  mtime = 2
  await $.tool.call({ tool: 'Bash', command: 'kiri cutout x.jpg -o /w/a.png' } as never)
  expect(toasts.at(-1)).toBe('kiri-review: a.png を読み直しました（確認待ち 2 件）')
  expect(await ui.find({ type: 'Text', text: /^1\. 縁にトゲ（確認待ち）/ })).toBeDefined()
  expect(await ui.find({ type: 'Text', text: /^確認待ち 2$/ })).toBeDefined()

  await ui.press({ key: 'fix1' })
  await ui.press({ key: 'again2' })
  expect(await ui.find({ type: 'Text', text: /^1\. 縁にトゲ（直った）/ })).toBeDefined()
  expect(await ui.find({ type: 'Text', text: /^2\. 全体に暗い（画像全体）（まだ・次に送ります）/ })).toBeDefined()
  expect(await ui.find({ type: 'Text', text: /^まだ 1 · 直った 1$/ })).toBeDefined()
  // 直ったものは一覧の件数から外れる
  expect(await badges()).toEqual(['1'])
  expect(await sendLabel()).toBe('Claude に送る（1 件）')
  await ui.press({ key: 'send' })
  expect(sent).toHaveLength(2)
  expect(sent[1]).toContain('1. 画像全体: 全体に暗い（再指摘: 前回の修正のあとも直っていません）')
  expect(sent[1]).not.toContain('縁にトゲ')
  expect(await ui.find({ type: 'Text', text: /^2\. 全体に暗い（画像全体）（送信済み）/ })).toBeDefined()

  // 直ったを取り消して「まだ」にもできる
  await ui.press({ key: 'again1' })
  expect(await sendLabel()).toBe('Claude に送る（1 件）')

  // 別の画像を開き直しても、未解決のコメントのある画像は一覧に残す
  const r = await $.command.run({ command: 'kiri-review', args: 'b.png' } as never)
  expect(r.text).toContain('未解決のコメントのある画像 1 枚は一覧に残しました。')
  await ui.unmount()
})
