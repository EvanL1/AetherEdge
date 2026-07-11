import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest'

describe('downloadCsv', () => {
  let createObjectURLMock: ReturnType<typeof vi.fn>
  let revokeObjectURLMock: ReturnType<typeof vi.fn>
  let appendChildMock: ReturnType<typeof vi.fn>
  let removeChildMock: ReturnType<typeof vi.fn>
  let clickMock: ReturnType<typeof vi.fn>
  let createdLink: HTMLAnchorElement
  let capturedBlobContent: string

  beforeEach(() => {
    capturedBlobContent = ''
    createObjectURLMock = vi.fn(() => 'blob:mock-url')
    revokeObjectURLMock = vi.fn()
    clickMock = vi.fn()

    // Capture the blob content when Blob is constructed
    const OriginalBlob = global.Blob
    vi.spyOn(global, 'Blob').mockImplementation((parts?: BlobPart[], options?: BlobPropertyBag) => {
      if (parts) {
        capturedBlobContent = parts.map((p) => String(p)).join('')
      }
      return new OriginalBlob(parts, options)
    })

    global.URL.createObjectURL = createObjectURLMock
    global.URL.revokeObjectURL = revokeObjectURLMock

    createdLink = document.createElement('a')
    vi.spyOn(createdLink, 'click').mockImplementation(clickMock)
    appendChildMock = vi.spyOn(document.body, 'appendChild').mockReturnValue(createdLink as any)
    removeChildMock = vi.spyOn(document.body, 'removeChild').mockReturnValue(createdLink as any)

    vi.spyOn(document, 'createElement').mockImplementation((tag: string) => {
      if (tag === 'a') return createdLink
      return document.createElement(tag)
    })
  })

  afterEach(() => {
    vi.restoreAllMocks()
  })

  it('triggers a file download with correct filename', async () => {
    const { downloadCsv } = await import('../csv')
    const rows = [
      ['name', 'value'],
      ['apple', 1],
      ['banana', 2],
    ]
    downloadCsv(rows, 'test.csv')

    expect(createObjectURLMock).toHaveBeenCalledOnce()
    expect(appendChildMock).toHaveBeenCalledWith(createdLink)
    expect(clickMock).toHaveBeenCalledOnce()
    expect(removeChildMock).toHaveBeenCalledWith(createdLink)
    expect(revokeObjectURLMock).toHaveBeenCalledWith('blob:mock-url')
    expect(createdLink.download).toBe('test.csv')
  })

  it('escapes cells containing commas', async () => {
    const { downloadCsv } = await import('../csv')
    downloadCsv([['hello, world', 42]], 'out.csv')
    expect(capturedBlobContent).toContain('"hello, world"')
  })

  it('escapes cells containing double quotes', async () => {
    const { downloadCsv } = await import('../csv')
    downloadCsv([['say "hi"', 1]], 'out.csv')
    expect(capturedBlobContent).toContain('"say ""hi"""')
  })

  it('handles empty rows array', async () => {
    const { downloadCsv } = await import('../csv')
    downloadCsv([], 'empty.csv')
    expect(createObjectURLMock).toHaveBeenCalledOnce()
    expect(clickMock).toHaveBeenCalledOnce()
  })

  it('separates rows with CRLF', async () => {
    const { downloadCsv } = await import('../csv')
    downloadCsv(
      [
        ['a', 'b'],
        ['1', '2'],
      ],
      'out.csv',
    )
    expect(capturedBlobContent).toContain('a,b\r\n1,2')
  })
})
