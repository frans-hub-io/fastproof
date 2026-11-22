// Types of the analysis, integrations and logs endpoints, from `schema.gen.ts` (generated from
// `schemas/openapi.json` with `npm run gen:api`). The aliases only narrow string fields that the
// spec documents as fixed sets and add the shapes the UI derives (normalized log lines, filters).
import type { components } from './schema.gen';
import type { IsoDateTime, ResultStatus, ResultSummary } from './types';

type S = components['schemas'];

export type DefectGroup = 'productBug' | 'automationBug' | 'systemIssue' | 'noDefect' | 'toInvestigate';

export const DEFECT_GROUPS: readonly DefectGroup[] = ['productBug', 'automationBug', 'systemIssue', 'noDefect', 'toInvestigate'];

export type DefectType = Omit<S['DefectType'], 'group'> & { group: DefectGroup };

export type DefectTypeInput = Omit<S['NewDefectType'], 'group'> & { group: DefectGroup };

export type DefectTypePatch = S['DefectTypePatch'];

export type DefectSource = 'user' | 'ai' | 'rule';

/** `defect` of a result or cluster. */
export type DefectInfo = Omit<S['Defect'], 'group' | 'source'> & {
  group: DefectGroup;
  source: DefectSource;
};

export type Transition = 'new' | 'fixed' | 'regressed' | 'malfunctioned';

export type TriageRequest = Omit<S['TriageRequest'], 'source'> & { source?: 'user' | 'ai' | null };

export type TriageResponse = S['TriageOutcome'];

export type Cluster = Omit<S['Cluster'], 'defect'> & { defect?: DefectInfo | null };

export type SimilarFailure = Omit<S['SimilarResult'], 'defect'> & { defect?: DefectInfo | null };

export type RuleMatch = Omit<S['RuleMatch'], 'statuses'> & { statuses?: ResultStatus[] | null };

export type RuleAction = S['RuleAction'];

export type Rule = Omit<S['Rule'], 'match'> & { match: RuleMatch };

export type RuleInput = Omit<S['NewRule'], 'match'> & { match: RuleMatch };

export type RulePatch = S['RulePatch'];

export type RuleTestResponse = S['DryRun'];

export type Task = S['Task'];

export type FlakySettings = Required<S['FlakySettings']>;

export type RetentionSettings = Required<Omit<S['RetentionSettings'], 'launchesKeep' | 'manualDays'>> &
  Pick<S['RetentionSettings'], 'launchesKeep' | 'manualDays'>;

export type RollupSettings = Required<S['RollupSettings']>;

export type GateMetric = S['Metric'];
export type GateOp = S['Op'];
export type GateRule = S['GateRule'];
export type Gate = S['Gate'];

export type NotificationKind = 'webhook' | 'slack' | 'teams' | 'email' | 'telegram';
export type NotificationEvent = 'launchFinished' | 'gateFailed' | 'newFailures';

export type NotificationTarget = Omit<S['Notification'], 'kind' | 'on'> & {
  kind: NotificationKind;
  on: NotificationEvent[];
};

export type IssueTrackerType = 'jira' | 'gitlab' | 'github';

export type IssueTracker = Omit<S['IssueTracker'], 'type'> & { type: IssueTrackerType };

export type VcsSettings = Omit<S['VcsSettings'], 'provider'> & { provider: 'gitlab' | 'github' };

export type FastMockSettings = S['FastMockSettings'];

/** The settings document with every part filled in (see `withSettingsDefaults`). */
export interface ProjectSettings {
  flaky: FlakySettings;
  retention: RetentionSettings;
  rollup: RollupSettings;
  gates: Gate[];
  notifications: NotificationTarget[];
  issueTrackers: IssueTracker[];
  vcs: VcsSettings | null;
  fastmock: FastMockSettings | null;
}

export type SecretInfo = S['SecretMeta'];

export type SecretPurpose = S['SecretPurpose'];

export type FlakyTest = Omit<S['FlakyItem'], 'lastStatuses'> & { lastStatuses: ResultStatus[] };

export type DurationChange = S['DurationChange'];

export type LaunchComparison = Omit<S['Comparison'], 'newFailures' | 'fixed' | 'stillFailing' | 'newTests' | 'missing'> & {
  newFailures: ResultSummary[];
  fixed: ResultSummary[];
  stillFailing: ResultSummary[];
  newTests: ResultSummary[];
  missing: ResultSummary[];
};

export type DashboardBucket = 'day' | 'week';

export interface DashboardFilters {
  branch?: string;
  environment?: string;
  from?: string;
  to?: string;
  bucket?: DashboardBucket;
}

export type Dashboard = Omit<S['Dashboard'], 'flakyTop'> & { flakyTop: FlakyTest[] };

export type TestPlanCreate = Omit<S['NewTestPlan'], 'statuses'> & { statuses?: ResultStatus[] | null };

export type TestPlanCreated = S['TestPlanCreated'];

export type GateStatus = S['GateStatus'];

export type GateRuleResult = S['RuleResult'];

export type GateResult = S['GateReport'];

export type DeliveryStatus = 'pending' | 'sending' | 'delivered' | 'failed';

export type Delivery = Omit<S['DeliveryView'], 'status'> & { status: DeliveryStatus };

export type NotificationTestResult = S['NotificationTestResult'];

export type MockInteraction = S['MockInteraction'];

export type MockInteractions = Omit<S['MockInteractions'], 'source'> & { source: 'traceId' | 'timeWindow' };

export type LaunchPatch = S['LaunchPatch'];

export type LogLevel = S['LogLevel'];

export const LOG_LEVELS: readonly LogLevel[] = ['trace', 'debug', 'info', 'warn', 'error'];

export type LogStream = S['LogStream'];

/** A log line as the server sends it (`ts` in milliseconds since the epoch). */
export type LogLineWire = S['LogLine'];

/** A log line as the UI keeps it: `ts` normalized to an ISO 8601 string. */
export type LogLine = Omit<LogLineWire, 'ts'> & { ts: IsoDateTime };

export type LogStreamBatch = S['LogStreamBatch'];

/** `gap` SSE event: the server dropped lines because the client was too slow. */
export type LogGap = S['LogGap'];

export interface LogFilters {
  /** Client id of an attempt, or the id of a result (all its attempts). */
  resultId?: string;
  attempt?: number;
  level?: LogLevel;
  q?: string;
}
