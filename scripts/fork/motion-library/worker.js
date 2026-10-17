// Motion Library bucket gate: a private R2 bucket behind a bearer token.
//
// The bucket holds paid, licensed assets: nothing is public. Two secrets:
// - READ_TOKEN: every machine that runs EffectCraft (the Mac, rented GPUs). GET/HEAD only.
// - PUBLISH_TOKEN: the Mac that publishes. GET/HEAD/PUT.
// Keys are only `head.json` and `blobs/<sha256>[.gz]`; anything else is 404.

const KEY = /^(head\.json|blobs\/[0-9a-f]{64}(\.gz)?)$/;
// Workers on the free plan accept request bodies up to 100 MB.
const MAX_PUT = 100 * 1000 * 1000;

async function same(a, b) {
  const enc = new TextEncoder();
  const [x, y] = [enc.encode(a), enc.encode(b)];
  if (x.byteLength !== y.byteLength) return false;
  return crypto.subtle.timingSafeEqual(x, y);
}

// The browser build fetches from its own origin with a bearer token (no cookies), so any origin
// may ask; the token is what is checked.
const CORS = {
  'access-control-allow-origin': '*',
  'access-control-allow-methods': 'GET, HEAD, PUT, OPTIONS',
  'access-control-allow-headers': 'authorization, content-type',
  'access-control-max-age': '86400',
};

function reply(status, text) {
  return new Response(text, { status, headers: { 'content-type': 'text/plain', 'cache-control': 'no-store', ...CORS } });
}

export default {
  async fetch(request, env) {
    if (request.method === 'OPTIONS') return new Response(null, { status: 204, headers: CORS });
    const auth = request.headers.get('authorization') || '';
    const token = auth.startsWith('Bearer ') ? auth.slice(7) : '';
    const publisher = !!env.PUBLISH_TOKEN && token !== '' && (await same(token, env.PUBLISH_TOKEN));
    const reader = publisher || (!!env.READ_TOKEN && token !== '' && (await same(token, env.READ_TOKEN)));
    if (!reader) return reply(401, 'unauthorized');

    const key = decodeURIComponent(new URL(request.url).pathname.slice(1));
    if (!KEY.test(key)) return reply(404, 'not found');

    if (request.method === 'GET' || request.method === 'HEAD') {
      const obj = request.method === 'HEAD' ? await env.LIBRARY.head(key) : await env.LIBRARY.get(key);
      if (!obj) return reply(404, 'not found');
      const headers = new Headers({
        'content-type': key === 'head.json' ? 'application/json' : 'application/octet-stream',
        // Blobs never change (named by content); the head does.
        'cache-control': key === 'head.json' ? 'no-store' : 'private, max-age=31536000, immutable',
        'content-length': String(obj.size),
        etag: obj.httpEtag,
        ...CORS,
      });
      return new Response(request.method === 'HEAD' ? null : obj.body, { headers });
    }

    if (request.method === 'PUT') {
      if (!publisher) return reply(403, 'read-only token');
      const size = Number(request.headers.get('content-length') || '0');
      if (size > MAX_PUT) return reply(413, 'too large');
      await env.LIBRARY.put(key, request.body);
      return reply(201, 'stored');
    }

    return reply(405, 'method not allowed');
  },
};
