import type { DefectGroup, DefectInfo, DefectType } from '../api/types-next';

export const GROUP_LABEL: Record<DefectGroup, string> = {
  productBug: 'Product bug',
  automationBug: 'Automation bug',
  systemIssue: 'System issue',
  noDefect: 'No defect',
  toInvestigate: 'To investigate',
};

export const GROUP_COLOR: Record<DefectGroup, string> = {
  productBug: '#e03131',
  automationBug: '#f08c00',
  systemIssue: '#1c7ed6',
  noDefect: '#868e96',
  toInvestigate: '#ae3ec9',
};

export function safeColor(c: string | null | undefined): string | undefined {
  return c && /^#[0-9a-fA-F]{3,8}$/.test(c) ? c : undefined;
}

export function defectLabel(d: DefectInfo, types?: Map<string, DefectType>): string {
  const t = d.typeId ? types?.get(d.typeId) : undefined;
  return t?.name ?? d.name ?? (d.group ? GROUP_LABEL[d.group] : undefined) ?? (d.type ? d.type.replace(/_/g, ' ') : 'Defect');
}

export function defectColor(d: DefectInfo, types?: Map<string, DefectType>): string {
  const t = d.typeId ? types?.get(d.typeId) : undefined;
  return safeColor(t?.color) ?? (d.group ? GROUP_COLOR[d.group] : undefined) ?? GROUP_COLOR.toInvestigate;
}

/** Issue URL from a tracker template (`{key}` placeholder), or null when the key does not match. */
export function issueUrlFor(template: string, keyRegex: string, key: string): string | null {
  const k = key.trim();
  if (!k || k.length > 100) return null;
  try {
    if (keyRegex && !new RegExp(keyRegex).test(k)) return null;
  } catch {
    return null;
  }
  const url = template.replace('{key}', encodeURIComponent(k.replace(/^#/, '')));
  return /^https?:\/\//.test(url) ? url : null;
}
