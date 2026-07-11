import { createExecutionContext, waitOnExecutionContext } from 'cloudflare:test';
import { env } from 'cloudflare:workers';
import { afterEach, describe, expect, it, vi } from 'vitest';
import worker from './entry.js';

/** Runs the Worker's fetch handler against the fixture ASSETS binding. */
async function run(path, init) {
  const request = new Request(`https://example.com${path}`, init);
  const ctx = createExecutionContext();
  const response = await worker.fetch(request, env, ctx);
  await waitOnExecutionContext(ctx);
  return response;
}

afterEach(() => {
  vi.restoreAllMocks();
});

describe('content negotiation', () => {
  it('passes a normal request straight through to ASSETS unchanged', async () => {
    const response = await run('/agent-quickstart/');

    expect(response.status).toBe(200);
    expect(response.headers.get('Content-Type')).toContain('text/html');
    expect(response.headers.get('Vary')).toBe('Accept');
    expect(await response.text()).toContain('Agent Quickstart');
  });

  it('rewrites Accept: text/markdown on a page URL to its .md sibling', async () => {
    const response = await run('/agent-quickstart/', {
      headers: { Accept: 'text/markdown' },
    });

    expect(response.status).toBe(200);
    expect(response.headers.get('Content-Type')).toBe('text/markdown; charset=utf-8');
    expect(response.headers.get('Vary')).toBe('Accept');
    expect(await response.text()).toMatch(/^---\ntitle: Agent Quickstart/);
  });

  it('does not double-rewrite a direct .md URL request', async () => {
    const response = await run('/agent-quickstart.md');

    expect(response.status).toBe(200);
    expect(await response.text()).toMatch(/^---\ntitle: Agent Quickstart/);
  });

  it('falls through to normal asset serving when no .md sibling exists', async () => {
    const response = await run('/no-markdown-page/', {
      headers: { Accept: 'text/markdown' },
    });

    expect(response.status).toBe(200);
    expect(response.headers.get('Content-Type')).toContain('text/html');
    expect(await response.text()).toContain('No Markdown Page');
  });

  it('maps the root path with Accept: text/markdown to /index.md', async () => {
    const response = await run('/', { headers: { Accept: 'text/markdown' } });

    expect(response.status).toBe(200);
    expect(response.headers.get('Content-Type')).toBe('text/markdown; charset=utf-8');
    expect(await response.text()).toMatch(/^---\ntitle: Aether/);
  });

  it('falls back gracefully when ASSETS.fetch throws on the markdown lookup', async () => {
    vi.spyOn(env.ASSETS, 'fetch').mockRejectedValueOnce(new Error('binding hiccup'));

    const response = await run('/agent-quickstart/', {
      headers: { Accept: 'text/markdown' },
    });

    expect(response.status).toBe(200);
    expect(response.headers.get('Content-Type')).toContain('text/html');
    expect(await response.text()).toContain('Agent Quickstart');
  });
});
