// A small CommonMark subset for the mock server (the real server renders with pulldown-cmark): raw HTML is
// escaped, links are http(s) only and `attachment:<id>` images point at the project's attachment content.

function esc(s: string): string {
  return s.replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;').replace(/"/g, '&quot;');
}

function inline(text: string, project: string): string {
  let s = esc(text);
  s = s.replace(/`([^`]+)`/g, (_m, c: string) => `<code>${c}</code>`);
  s = s.replace(/!\[([^\]]*)\]\(attachment:([0-9a-zA-Z-]{1,64})\)/g, (_m, alt: string, id: string) => `<img src="/api/v1/projects/${encodeURIComponent(project)}/attachments/${id}/content" alt="${alt}">`);
  s = s.replace(/\[([^\]]+)\]\((https?:\/\/[^\s)]+)\)/g, (_m, t: string, u: string) => `<a href="${u}">${t}</a>`);
  s = s.replace(/\*\*([^*]+)\*\*/g, '<strong>$1</strong>');
  s = s.replace(/(^|[^*])\*([^*]+)\*/g, '$1<em>$2</em>');
  return s;
}

function table(lines: string[], project: string): string {
  const cells = (l: string) =>
    l
      .trim()
      .replace(/^\||\|$/g, '')
      .split('|')
      .map((c) => c.trim());
  const [head, , ...rows] = lines;
  const th = cells(head ?? '')
    .map((c) => `<th>${inline(c, project)}</th>`)
    .join('');
  const tr = rows.map((r) => `<tr>${cells(r).map((c) => `<td>${inline(c, project)}</td>`).join('')}</tr>`).join('');
  return `<table><thead><tr>${th}</tr></thead><tbody>${tr}</tbody></table>`;
}

export function renderMarkdown(md: string, project: string): string {
  const lines = md.replace(/\r\n?/g, '\n').split('\n');
  const out: string[] = [];
  let i = 0;
  while (i < lines.length) {
    const line = lines[i] ?? '';
    if (!line.trim()) {
      i++;
      continue;
    }
    if (line.startsWith('```')) {
      const body: string[] = [];
      i++;
      while (i < lines.length && !(lines[i] ?? '').startsWith('```')) body.push(lines[i++] ?? '');
      i++;
      out.push(`<pre><code>${esc(body.join('\n'))}</code></pre>`);
      continue;
    }
    const h = /^(#{1,6})\s+(.*)$/.exec(line);
    if (h) {
      const level = String(h[1]?.length ?? 1);
      out.push(`<h${level}>${inline(h[2] ?? '', project)}</h${level}>`);
      i++;
      continue;
    }
    if (line.trim().startsWith('|') && /^\s*\|?\s*:?-{3,}/.test(lines[i + 1] ?? '')) {
      const block: string[] = [];
      while (i < lines.length && (lines[i] ?? '').trim().startsWith('|')) block.push(lines[i++] ?? '');
      out.push(table(block, project));
      continue;
    }
    const ul = /^\s*[-*]\s+/;
    const ol = /^\s*\d+[.)]\s+/;
    if (ul.test(line) || ol.test(line)) {
      const ordered = ol.test(line);
      const re = ordered ? ol : ul;
      const items: string[] = [];
      while (i < lines.length && re.test(lines[i] ?? '')) items.push((lines[i++] ?? '').replace(re, ''));
      const tag = ordered ? 'ol' : 'ul';
      out.push(`<${tag}>${items.map((t) => `<li>${inline(t, project)}</li>`).join('')}</${tag}>`);
      continue;
    }
    const para: string[] = [];
    while (i < lines.length && (lines[i] ?? '').trim() && !/^(#{1,6}\s|```|\s*[-*]\s+|\s*\d+[.)]\s+)/.test(lines[i] ?? '')) para.push(lines[i++] ?? '');
    out.push(`<p>${para.map((p) => inline(p, project)).join('<br>')}</p>`);
  }
  return out.join('\n');
}
