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

describe('dual-mode documentation service', () => {
  it('serves HTML to a normal browser request', async () => {
    const response = await run('/agent-quickstart/');

    expect(response.status).toBe(200);
    expect(response.headers.get('Content-Type')).toContain('text/html');
    expect(response.headers.get('Vary')).toBe('Accept');
    expect(await response.text()).toContain('<h1>智能体快速入门</h1>');
  });

  it('serves Markdown when the client requests text/markdown', async () => {
    const response = await run('/agent-quickstart/', {
      headers: { Accept: 'text/markdown' },
    });

    expect(response.status).toBe(200);
    expect(response.headers.get('Content-Type')).toBe('text/markdown; charset=utf-8');
    expect(response.headers.get('Vary')).toBe('Accept');
    expect(await response.text()).toMatch(/^# 智能体快速入门/);
  });

  it('serves the independent English locale under /en', async () => {
    const html = await run('/en/agent-quickstart/');
    const markdown = await run('/en/agent-quickstart/', {
      headers: { Accept: 'text/markdown' },
    });

    expect(await html.text()).toContain('<h1>Agent Quickstart</h1>');
    expect(await markdown.text()).toMatch(/^# Agent Quickstart/);
  });

  it('serves direct .md routes as Markdown', async () => {
    const response = await run('/agent-quickstart.md');

    expect(response.status).toBe(200);
    expect(response.headers.get('Content-Type')).toBe('text/markdown; charset=utf-8');
  });

  it('maps the root to HTML or Markdown according to the request', async () => {
    const html = await run('/');
    const markdown = await run('/', { headers: { Accept: 'text/markdown' } });

    expect(html.headers.get('Content-Type')).toContain('text/html');
    expect(markdown.headers.get('Content-Type')).toBe('text/markdown; charset=utf-8');
    expect(await markdown.text()).toMatch(/^# Aether/);
  });

  it('serves generated agent indexes as text/plain', async () => {
    const response = await run('/llms.txt');
    const english = await run('/en/llms.txt');

    expect(response.status).toBe(200);
    expect(response.headers.get('Content-Type')).toBe('text/plain; charset=utf-8');
    expect(await response.text()).toContain('## 概览');
    expect(await english.text()).toContain('## Overview');
  });

  it('returns a plain-text 404 when a requested Markdown twin does not exist', async () => {
    const response = await run('/missing-document', {
      headers: { Accept: 'text/markdown' },
    });

    expect(response.status).toBe(404);
    expect(response.headers.get('Content-Type')).toBe('text/plain; charset=utf-8');
    expect(response.headers.get('Cache-Control')).toBe('no-store');
  });

  it('returns a plain-text 405 for unsupported methods', async () => {
    const response = await run('/agent-quickstart', { method: 'POST' });

    expect(response.status).toBe(405);
    expect(response.headers.get('Allow')).toBe('GET, HEAD');
  });

  it('returns the selected representation without a body for HEAD', async () => {
    const html = await run('/agent-quickstart/', { method: 'HEAD' });
    const markdown = await run('/agent-quickstart/', {
      method: 'HEAD',
      headers: { Accept: 'text/markdown' },
    });

    expect(html.headers.get('Content-Type')).toContain('text/html');
    expect(markdown.headers.get('Content-Type')).toBe('text/markdown; charset=utf-8');
    expect(await html.text()).toBe('');
    expect(await markdown.text()).toBe('');
  });

  it('returns a plain-text 503 when the Markdown asset lookup fails', async () => {
    vi.spyOn(env.ASSETS, 'fetch').mockRejectedValueOnce(new Error('binding unavailable'));

    const response = await run('/agent-quickstart', {
      headers: { Accept: 'text/markdown' },
    });

    expect(response.status).toBe(503);
    expect(response.headers.get('Content-Type')).toBe('text/plain; charset=utf-8');
  });
});
