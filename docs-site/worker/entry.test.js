import { createExecutionContext, waitOnExecutionContext } from 'cloudflare:test';
import { env } from 'cloudflare:workers';
import { afterEach, describe, expect, it, vi } from 'vitest';
import worker from './entry.js';

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

describe('plain-text documentation service', () => {
  it('serves the root as Markdown without content negotiation', async () => {
    const response = await run('/');

    expect(response.status).toBe(200);
    expect(response.headers.get('Content-Type')).toBe('text/markdown; charset=utf-8');
    expect(response.headers.get('X-Content-Type-Options')).toBe('nosniff');
    expect(await response.text()).toMatch(/^# Aether Documentation/);
  });

  it('serves extensionless and trailing-slash routes as Markdown', async () => {
    const extensionless = await run('/agent-quickstart');
    const trailingSlash = await run('/agent-quickstart/');

    expect(extensionless.headers.get('Content-Type')).toBe('text/markdown; charset=utf-8');
    expect(trailingSlash.headers.get('Content-Type')).toBe('text/markdown; charset=utf-8');
    expect(await extensionless.text()).toMatch(/^# Agent Quickstart/);
    expect(await trailingSlash.text()).toMatch(/^# Agent Quickstart/);
  });

  it('serves direct .md routes as Markdown', async () => {
    const response = await run('/agent-quickstart.md');

    expect(response.status).toBe(200);
    expect(response.headers.get('Content-Type')).toBe('text/markdown; charset=utf-8');
  });

  it('never serves HTML even when the client explicitly requests it', async () => {
    const response = await run('/agent-quickstart/', {
      headers: { Accept: 'text/html' },
    });

    expect(response.status).toBe(200);
    expect(response.headers.get('Content-Type')).toBe('text/markdown; charset=utf-8');
    expect(await response.text()).not.toContain('<html');
  });

  it('serves generated text indexes as text/plain', async () => {
    const response = await run('/llms.txt');

    expect(response.status).toBe(200);
    expect(response.headers.get('Content-Type')).toBe('text/plain; charset=utf-8');
    expect(await response.text()).toContain('# Aether Documentation');
  });

  it('returns a plain-text 404 when a document does not exist', async () => {
    const response = await run('/missing-document');

    expect(response.status).toBe(404);
    expect(response.headers.get('Content-Type')).toBe('text/plain; charset=utf-8');
    expect(response.headers.get('Cache-Control')).toBe('no-store');
    expect(await response.text()).toBe('Document not found. See /llms.txt for the index.\n');
  });

  it('returns a plain-text 405 for unsupported methods', async () => {
    const response = await run('/agent-quickstart', { method: 'POST' });

    expect(response.status).toBe(405);
    expect(response.headers.get('Allow')).toBe('GET, HEAD');
    expect(response.headers.get('Content-Type')).toBe('text/plain; charset=utf-8');
  });

  it('returns headers without a body for HEAD requests', async () => {
    const response = await run('/agent-quickstart', { method: 'HEAD' });

    expect(response.status).toBe(200);
    expect(response.headers.get('Content-Type')).toBe('text/markdown; charset=utf-8');
    expect(await response.text()).toBe('');
  });

  it('returns a plain-text 503 when the asset binding fails', async () => {
    vi.spyOn(env.ASSETS, 'fetch').mockRejectedValueOnce(new Error('binding unavailable'));

    const response = await run('/agent-quickstart');

    expect(response.status).toBe(503);
    expect(response.headers.get('Content-Type')).toBe('text/plain; charset=utf-8');
    expect(await response.text()).toBe('Documentation temporarily unavailable.\n');
  });
});
