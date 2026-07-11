import { slug as githubSlug } from 'github-slugger';

// Slugifies each path segment independently with github-slugger's stateless
// `slug()` function. The result is stable across content sync, generated
// output paths, llms.txt links, and Worker routes.

export function computeSlug(destRelPath) {
  const withoutExt = destRelPath.replace(/\.(md|mdx)$/i, '');
  const segments = withoutExt.split('/').filter((s) => s.length > 0);
  const slugged = segments.map((s) => githubSlug(s));
  let slug = slugged.join('/');
  if (slug.endsWith('/index')) slug = slug.slice(0, -'/index'.length);
  if (slug.toLowerCase() === 'index' || slug === '') slug = '';
  return slug.toLowerCase();
}

export function slugToSitePath(slug) {
  return slug === '' ? '/' : `/${slug}`;
}
