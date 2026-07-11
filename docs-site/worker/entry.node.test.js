import { describe, expect, it } from 'vitest';
import worker from './entry.js';

const files = new Map([
  ['/index.md', '# Aether Documentation\n'],
  ['/agent-quickstart.md', '# Agent Quickstart\n'],
  ['/llms.txt', '# Aether Documentation\n'],
]);

function environment(options = {}) {
  return {
    ASSETS: {
      async fetch(request) {
        if (options.throwOnFetch) throw new Error('asset binding unavailable');
        const content = files.get(new URL(request.url).pathname);
        return content === undefined
          ? new Response('missing', { status: 404 })
          : new Response(request.method === 'HEAD' ? null : content);
      },
    },
  };
}

function run(path, init, options) {
  return worker.fetch(new Request(`https://example.com${path}`, init), environment(options));
}

describe('plain-text Worker in the Node unit-test runtime', () => {
  it('maps root and extensionless paths to Markdown assets', async () => {
    const root = await run('/');
    const document = await run('/agent-quickstart/');

    expect(root.headers.get('Content-Type')).toBe('text/markdown; charset=utf-8');
    expect(await root.text()).toContain('# Aether Documentation');
    expect(await document.text()).toContain('# Agent Quickstart');
  });

  it('serves direct Markdown and text-index paths with distinct content types', async () => {
    const markdown = await run('/agent-quickstart.md');
    const index = await run('/llms.txt');

    expect(markdown.headers.get('Content-Type')).toBe('text/markdown; charset=utf-8');
    expect(index.headers.get('Content-Type')).toBe('text/plain; charset=utf-8');
  });

  it('returns plain-text protocol and routing errors', async () => {
    const unsupported = await run('/agent-quickstart', { method: 'POST' });
    const invalidExtension = await run('/index.html');
    const missing = await run('/missing');

    expect(unsupported.status).toBe(405);
    expect(unsupported.headers.get('Allow')).toBe('GET, HEAD');
    expect(invalidExtension.status).toBe(404);
    expect(missing.status).toBe(404);
    expect(missing.headers.get('Cache-Control')).toBe('no-store');
    expect(await missing.text()).toContain('Document not found');
  });

  it('returns a bodyless Markdown response to HEAD', async () => {
    const response = await run('/agent-quickstart', { method: 'HEAD' });

    expect(response.status).toBe(200);
    expect(response.headers.get('Content-Type')).toBe('text/markdown; charset=utf-8');
    expect(await response.text()).toBe('');
  });

  it('converts asset-binding failures into plain-text 503 responses', async () => {
    const response = await run('/agent-quickstart', undefined, { throwOnFetch: true });

    expect(response.status).toBe(503);
    expect(response.headers.get('Content-Type')).toBe('text/plain; charset=utf-8');
  });
});
