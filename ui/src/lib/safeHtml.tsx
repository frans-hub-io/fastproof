import { createElement, type ReactNode } from 'react';
import { isHttpUrl } from './format';

const ALLOWED = new Set([
  'p', 'br', 'b', 'strong', 'i', 'em', 'u', 's', 'code', 'pre', 'blockquote', 'ul', 'ol', 'li',
  'h1', 'h2', 'h3', 'h4', 'h5', 'h6', 'a', 'table', 'thead', 'tbody', 'tr', 'th', 'td', 'hr', 'span', 'div', 'sub', 'sup',
]);

const MAX_NODES = 5000;

function convert(node: Node, key: string, budget: { left: number }): ReactNode {
  if (budget.left-- <= 0) return null;
  if (node.nodeType === Node.TEXT_NODE) return node.textContent;
  if (node.nodeType !== Node.ELEMENT_NODE) return null;
  const el = node as Element;
  const tag = el.tagName.toLowerCase();
  const children = Array.from(el.childNodes).map((c, i) => convert(c, `${key}.${String(i)}`, budget));
  if (tag === 'script' || tag === 'style' || tag === 'iframe' || tag === 'object' || tag === 'embed') return null;
  if (!ALLOWED.has(tag)) return createElement('span', { key }, ...children);
  if (tag === 'a') {
    const href = el.getAttribute('href');
    if (!isHttpUrl(href)) return createElement('span', { key }, ...children);
    return createElement('a', { key, href, target: '_blank', rel: 'noopener noreferrer nofollow' }, ...children);
  }
  if (tag === 'br' || tag === 'hr') return createElement(tag, { key });
  return createElement(tag, { key }, ...children);
}

export function renderSafeHtml(html: string): ReactNode[] {
  const doc = new DOMParser().parseFromString(html, 'text/html');
  const budget = { left: MAX_NODES };
  return Array.from(doc.body.childNodes).map((n, i) => convert(n, String(i), budget));
}
