const MARKDOWN_CONTENT_TYPE = 'text/markdown; charset=utf-8';
const TEXT_CONTENT_TYPE = 'text/plain; charset=utf-8';

function responseHeaders(contentType, sourceHeaders) {
  const headers = new Headers(sourceHeaders);
  headers.set('Content-Type', contentType);
  headers.set('X-Content-Type-Options', 'nosniff');
  headers.set('Cache-Control', 'public, max-age=300');
  headers.delete('Vary');
  return headers;
}

function plainResponse(message, status, requestMethod, extraHeaders) {
  const headers = responseHeaders(TEXT_CONTENT_TYPE, extraHeaders);
  headers.set('Cache-Control', 'no-store');
  return new Response(requestMethod === 'HEAD' ? null : message, { status, headers });
}

function documentAssetPath(pathname) {
  if (pathname === '/') return '/index.md';
  if (pathname.endsWith('.md') || pathname.endsWith('.txt')) return pathname;

  const trimmedPath = pathname.endsWith('/') ? pathname.slice(0, -1) : pathname;
  const lastSegment = trimmedPath.slice(trimmedPath.lastIndexOf('/') + 1);
  if (lastSegment.includes('.')) return null;
  return `${trimmedPath}.md`;
}

export default {
  async fetch(request, env) {
    if (request.method !== 'GET' && request.method !== 'HEAD') {
      return plainResponse('Method not allowed.\n', 405, request.method, {
        Allow: 'GET, HEAD',
      });
    }

    const url = new URL(request.url);
    const assetPath = documentAssetPath(url.pathname);
    if (assetPath === null) {
      return plainResponse(
        'Document not found. See /llms.txt for the index.\n',
        404,
        request.method
      );
    }

    const assetUrl = new URL(assetPath, url);
    let assetResponse;
    try {
      assetResponse = await env.ASSETS.fetch(new Request(assetUrl, request));
    } catch {
      return plainResponse(
        'Documentation temporarily unavailable.\n',
        503,
        request.method
      );
    }

    if (!assetResponse.ok) {
      return plainResponse(
        'Document not found. See /llms.txt for the index.\n',
        404,
        request.method
      );
    }

    const contentType = assetPath.endsWith('.txt') ? TEXT_CONTENT_TYPE : MARKDOWN_CONTENT_TYPE;
    const headers = responseHeaders(contentType, assetResponse.headers);
    return new Response(request.method === 'HEAD' ? null : assetResponse.body, {
      status: assetResponse.status,
      headers,
    });
  },
};
