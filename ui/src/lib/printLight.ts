const ATTR = 'data-mantine-color-scheme';

/** Shows the light theme while the browser prints, so a dark theme never reaches paper or a PDF. */
export function installPrintLight(target: Document = document): () => void {
  let saved: string | null | undefined;
  const root = target.documentElement;
  const before = () => {
    if (saved !== undefined) return;
    saved = root.getAttribute(ATTR);
    root.setAttribute(ATTR, 'light');
  };
  const after = () => {
    if (saved === undefined) return;
    if (saved === null) root.removeAttribute(ATTR);
    else root.setAttribute(ATTR, saved);
    saved = undefined;
  };
  window.addEventListener('beforeprint', before);
  window.addEventListener('afterprint', after);
  return () => {
    window.removeEventListener('beforeprint', before);
    window.removeEventListener('afterprint', after);
    after();
  };
}
