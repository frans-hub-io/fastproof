import { createElement, type ReactNode } from 'react';
import { attachmentUrl } from '../../api/client';
import { isHttpUrl } from '../../lib/format';
import { attachmentIdOf } from '../../lib/tm/ui';
import { useAuthedUrl } from '../../viewers/authedUrl';

const ALLOWED = new Set([
  'p', 'br', 'b', 'strong', 'i', 'em', 'u', 's', 'del', 'code', 'pre', 'blockquote', 'ul', 'ol', 'li',
  'h1', 'h2', 'h3', 'h4', 'h5', 'h6', 'a', 'table', 'thead', 'tbody', 'tr', 'th', 'td', 'hr', 'span', 'div', 'sub', 'sup',
]);

const MAX_NODES = 5000;


function CaseImage({ project, id, alt }: { project: string; id: string; alt: string }) {
  const { src } = useAuthedUrl(attachmentUrl(project, id));
  return src ? <img src={src} alt={alt} className="fp-case-img" loading="lazy" /> : <span className="fp-case-img-missing">{alt || 'image'}</span>;
}

function convert(node: Node, key: string, budget: { left: number }, project: string): ReactNode {
  if (budget.left-- <= 0) return null;
  if (node.nodeType === Node.TEXT_NODE) return node.textContent;
  if (node.nodeType !== Node.ELEMENT_NODE) return null;
  const el = node as Element;
  const tag = el.tagName.toLowerCase();
  if (tag === 'script' || tag === 'style' || tag === 'iframe' || tag === 'object' || tag === 'embed') return null;
  if (tag === 'img') {
    const id = attachmentIdOf(el.getAttribute('src'), project);
    const alt = el.getAttribute('alt') ?? '';
    return id ? createElement(CaseImage, { key, project, id, alt }) : createElement('span', { key }, alt);
  }
  const children = Array.from(el.childNodes).map((c, i) => convert(c, `${key}.${String(i)}`, budget, project));
  if (!ALLOWED.has(tag)) return createElement('span', { key }, ...children);
  if (tag === 'a') {
    const href = el.getAttribute('href');
    if (!isHttpUrl(href)) return createElement('span', { key }, ...children);
    return createElement('a', { key, href, target: '_blank', rel: 'noopener noreferrer nofollow' }, ...children);
  }
  if (tag === 'br' || tag === 'hr') return createElement(tag, { key });
  return createElement(tag, { key }, ...children);
}

/** Server-rendered Markdown shown as React elements (never as raw HTML); images only from this project's attachments. */
export function CaseHtml({ html, project, empty, className }: { html: string; project: string; empty?: ReactNode; className?: string }) {
  if (!html.trim()) return empty ? <>{empty}</> : null;
  const doc = new DOMParser().parseFromString(html, 'text/html');
  const budget = { left: MAX_NODES };
  return <div className={`fp-md ${className ?? ''}`}>{Array.from(doc.body.childNodes).map((n, i) => convert(n, String(i), budget, project))}</div>;
}
