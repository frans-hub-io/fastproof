import type { DefectInfo } from '../api/types-next';

export interface TriageTarget {
  /** What is being triaged, e.g. a test name or "12 results with this error". */
  label: string;
  count: number;
  resultIds?: string[];
  launchId?: string;
  fingerprint?: string;
  defect?: DefectInfo | null;
  muted?: boolean;
  muteUntil?: string | null;
}

export function resultTarget(r: { id: string; fullName: string; defect?: DefectInfo | null; muted: boolean; muteUntil?: string | null }): TriageTarget {
  return { label: r.fullName, count: 1, resultIds: [r.id], defect: r.defect, muted: r.muted, muteUntil: r.muteUntil };
}
