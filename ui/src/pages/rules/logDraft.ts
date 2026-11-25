import type { RuleMatch } from '../../api/types-next';

/** What the editor keeps of `match.log`, `match.signals` and `action.pattern`. */
export interface LogDraft {
  logMode: 'off' | 'regex' | 'contains';
  logText: string;
  logLevels: string[];
  logStreams: string[];
  /** `lastBytes` in KiB (1 to 1024); empty for the server default. */
  logLastKiB: number | '';
  pattern: string;
  /** Signal conditions are kept as they are (set through the API or an agent). */
  signals: RuleMatch['signals'];
}

export const LOG_LEVELS = ['trace', 'debug', 'info', 'warn', 'error'] as const;
export const LOG_STREAMS = ['stdout', 'stderr', 'log', 'step'] as const;
const MAX_CONTAINS = 1024;
const PATTERN_NAME = /^[a-z0-9][a-z0-9._-]{0,63}$/;

type LogLevelName = (typeof LOG_LEVELS)[number];
type LogStreamName = (typeof LOG_STREAMS)[number];

export function logDraftOf(match?: RuleMatch, pattern?: string | null): LogDraft {
  const l = match?.log;
  return {
    logMode: l?.regex ? 'regex' : l?.contains ? 'contains' : 'off',
    logText: l?.regex ?? l?.contains ?? '',
    logLevels: l?.levels ?? [],
    logStreams: l?.streams ?? [],
    logLastKiB: l?.lastBytes ? Math.max(1, Math.round(l.lastBytes / 1024)) : '',
    pattern: pattern ?? '',
    signals: match?.signals,
  };
}

/** The `match.log` and `match.signals` of a draft. */
export function logMatchOf(d: LogDraft): Pick<RuleMatch, 'log' | 'signals'> {
  const text = d.logText.trim();
  const log =
    d.logMode === 'off' || !text
      ? undefined
      : {
          regex: d.logMode === 'regex' ? text : undefined,
          contains: d.logMode === 'contains' ? text : undefined,
          levels: d.logLevels.length ? (d.logLevels as LogLevelName[]) : undefined,
          streams: d.logStreams.length ? (d.logStreams as LogStreamName[]) : undefined,
          lastBytes: d.logLastKiB === '' ? undefined : d.logLastKiB * 1024,
        };
  return { log, signals: d.signals?.length ? d.signals : undefined };
}

export function hasLogCondition(d: LogDraft): boolean {
  return (d.logMode !== 'off' && !!d.logText.trim()) || !!d.signals?.length;
}

export function patternError(p: string): string | null {
  const t = p.trim();
  if (!t) return null;
  return PATTERN_NAME.test(t) ? null : 'Lower-case letters, digits, dot, dash and underscore; up to 64 characters';
}

export function logTextError(d: LogDraft, regexError: (s: string) => string | null): string | null {
  if (d.logMode === 'off') return null;
  const t = d.logText.trim();
  if (!t) return null;
  if (d.logMode === 'regex') return regexError(t);
  return t.length > MAX_CONTAINS ? `At most ${String(MAX_CONTAINS)} characters` : null;
}

/** Whether a draft makes the rule run after the launch (by the analysis role). */
export function isAfterLaunch(d: LogDraft): boolean {
  return hasLogCondition(d) || !!d.pattern.trim();
}

