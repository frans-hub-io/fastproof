import type { LaunchStatus, ResultStatus, UploadStatus } from '../api/types';

export const STATUS_LABEL: Record<ResultStatus, string> = {
  passed: 'Passed',
  failed: 'Failed',
  broken: 'Broken',
  skipped: 'Skipped',
  unknown: 'Unknown',
};

export const STATUS_ORDER: readonly ResultStatus[] = ['failed', 'broken', 'passed', 'skipped', 'unknown'];

export function statusVar(status: string): string {
  switch (status) {
    case 'passed':
    case 'failed':
    case 'broken':
    case 'skipped':
    case 'unknown':
    case 'flaky':
      return `var(--fp-status-${status})`;
    default:
      return 'var(--fp-status-unknown)';
  }
}

export function statusMantineColor(status: string): string {
  switch (status) {
    case 'passed':
      return 'green';
    case 'failed':
      return 'red';
    case 'broken':
      return 'orange';
    case 'skipped':
      return 'gray';
    case 'flaky':
      return 'grape';
    default:
      return 'violet';
  }
}

export const LAUNCH_STATUS_LABEL: Record<LaunchStatus, string> = {
  running: 'Running',
  finished: 'Finished',
  interrupted: 'Interrupted',
};

export function launchStatusColor(status: LaunchStatus): string {
  return status === 'running' ? 'blue' : status === 'interrupted' ? 'orange' : 'gray';
}

export function uploadStatusColor(status: UploadStatus): string {
  switch (status) {
    case 'done':
      return 'green';
    case 'failed':
      return 'red';
    case 'processing':
      return 'blue';
    default:
      return 'gray';
  }
}
