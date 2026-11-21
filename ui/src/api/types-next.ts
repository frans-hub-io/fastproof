// Hand-written types for endpoints that are specified (31-streaming, 33-stage-c) but not yet in
// schemas/openapi.json. Drop each one once the server publishes it and `npm run gen:api` picks it up.
import type { IsoDateTime, ResultStatus, ResultSummary, Uuid } from './types';

export type DefectGroup = 'productBug' | 'automationBug' | 'systemIssue' | 'noDefect' | 'toInvestigate';

export const DEFECT_GROUPS: readonly DefectGroup[] = ['productBug', 'automationBug', 'systemIssue', 'noDefect', 'toInvestigate'];

export interface DefectType {
  id: string;
  group: DefectGroup;
  name: string;
  shortName: string;
  color: string;
  builtIn: boolean;
}

export interface DefectTypeInput {
  group: DefectGroup;
  name: string;
  shortName: string;
  color: string;
}

export type DefectSource = 'user' | 'ai' | 'rule';

export interface DefectInfo {
  typeId?: string | null;
  group?: DefectGroup | null;
  name?: string | null;
  /** Stage B name of the type; kept for servers before stage C. */
  type?: string | null;
  comment?: string | null;
  issueUrl?: string | null;
  issueKey?: string | null;
  source?: DefectSource | null;
  ruleId?: string | null;
  at?: IsoDateTime | null;
  by?: string | null;
}

export type Transition = 'new' | 'fixed' | 'regressed' | 'malfunctioned';

/** Stage C fields added to `ResultSummary`. */
export interface ResultAnalysis {
  defect?: DefectInfo | null;
  muteUntil?: IsoDateTime | null;
  transition?: Transition | null;
  flaky?: boolean | null;
}

export interface TriageRequest {
  resultIds?: Uuid[];
  launchId?: Uuid;
  fingerprint?: string;
  defectTypeId?: string;
  comment?: string;
  issue?: { url: string; key?: string };
  mute?: { until?: IsoDateTime; reason: string };
  unmute?: boolean;
  source?: 'user' | 'ai';
}

export interface TriageResponse {
  updated: number;
}

export interface Cluster {
  fingerprint: string;
  message: string;
  count: number;
  statuses: { failed: number; broken: number };
  sampleResultId: Uuid;
  defect?: DefectInfo | null;
  knownSince?: string | null;
  isNew: boolean;
}

export interface SimilarFailure {
  resultId: Uuid;
  launchId: Uuid;
  launchNumber: number;
  name: string;
  message: string;
  score: number;
  defect?: DefectInfo | null;
}

export interface RuleMatch {
  messageRegex?: string;
  traceRegex?: string;
  testCaseId?: string;
  fingerprint?: string;
  environment?: string;
  statuses?: ResultStatus[];
}

export interface RuleAction {
  defectTypeId?: string;
  comment?: string;
  issue?: { url: string; key?: string };
  mute?: { days?: number };
  accept?: boolean;
}

export interface Rule {
  id: string;
  name: string;
  enabled: boolean;
  order: number;
  match: RuleMatch;
  action: RuleAction;
  createdAt: IsoDateTime;
  updatedAt: IsoDateTime;
  hits: number;
}

export type RuleInput = Pick<Rule, 'name' | 'enabled' | 'order' | 'match' | 'action'>;

export interface RuleTestResponse {
  matches: Uuid[];
}

export interface FlakySettings {
  historyDepth: number;
  stabilizationPeriod: number;
  includePassedTests: boolean;
}

export interface RetentionSettings {
  rawDays: number;
  launchesKeep?: number | null;
  keepImportant: boolean;
}

export type GateMetric = 'failed' | 'broken' | 'newFailures' | 'passRate' | 'flaky' | 'duration';
export type GateOp = 'gt' | 'gte' | 'lt' | 'lte';

export interface GateRule {
  metric: GateMetric;
  op: GateOp;
  value: number;
  baseline?: string | null;
}

export interface Gate {
  name: string;
  enabled: boolean;
  branches?: string[];
  rules: GateRule[];
}

export type NotificationKind = 'webhook' | 'slack' | 'email' | 'telegram';
export type NotificationEvent = 'launchFinished' | 'gateFailed' | 'newFailures';

export interface NotificationTarget {
  id: string;
  kind: NotificationKind;
  on: NotificationEvent[];
  target: string;
  secretRef?: string | null;
  filter?: { branches?: string[]; environments?: string[] } | null;
}

export type IssueTrackerType = 'jira' | 'gitlab' | 'github';

export interface IssueTracker {
  type: IssueTrackerType;
  urlTemplate: string;
  keyRegex: string;
}

export interface VcsSettings {
  provider: 'gitlab' | 'github';
  url: string;
  tokenSecretRef?: string | null;
  comment: boolean;
  status: boolean;
}

export interface FastMockSettings {
  url: string;
  tokenSecretRef?: string | null;
}

export interface ProjectSettings {
  flaky: FlakySettings;
  retention: RetentionSettings;
  rollup: { afterDays: number };
  gates: Gate[];
  notifications: NotificationTarget[];
  issueTrackers: IssueTracker[];
  vcs?: VcsSettings | null;
  fastmock?: FastMockSettings | null;
}

export interface SecretInfo {
  name: string;
  updatedAt?: IsoDateTime | null;
}

export interface FlakyTest {
  testCaseId: string;
  seriesKey: string;
  name: string;
  score: number;
  lastStatuses: ResultStatus[];
  lastFlakyAt: IsoDateTime | null;
}

export interface DurationChange {
  resultIdA: Uuid;
  resultIdB: Uuid;
  name: string;
  deltaMs: number;
  ratio: number;
}

export interface LaunchComparison {
  newFailures: ResultSummary[];
  fixed: ResultSummary[];
  stillFailing: ResultSummary[];
  newTests: ResultSummary[];
  missing: ResultSummary[];
  durationChanges: DurationChange[];
}

export type DashboardBucket = 'day' | 'week';

export interface DashboardFilters {
  branch?: string;
  environment?: string;
  from?: string;
  to?: string;
  bucket?: DashboardBucket;
}

export interface Dashboard {
  passRate: { t: string; passRate: number; total: number }[];
  durations: { t: string; p50Ms: number; p95Ms: number }[];
  flakyTop: FlakyTest[];
  mostFailing: { testCaseId: string; name: string; failures: number }[];
  defectGroups: ({ t: string } & Record<DefectGroup, number>)[];
  launches: { t: string; count: number }[];
}

export interface TestPlanCreate {
  name: string;
  fromLaunchId?: Uuid;
  statuses?: ResultStatus[];
  testCaseIds?: string[];
}

export interface TestPlanCreated {
  id: string;
}

export type GateStatus = 'passed' | 'failed' | 'pending';

export interface GateResult {
  status: GateStatus;
  results: { gate: string; rule: string; actual: number | null; expected: number; ok: boolean }[];
}

export interface Delivery {
  id: string;
  notificationId: string;
  kind: NotificationKind;
  event: NotificationEvent;
  target: string;
  status: 'pending' | 'delivered' | 'failed' | 'retrying';
  attempts: number;
  responseStatus?: number | null;
  lastError?: string | null;
  createdAt: IsoDateTime;
  deliveredAt?: IsoDateTime | null;
  launchId?: Uuid | null;
}

export interface MockInteraction {
  at: IsoDateTime;
  method: string;
  url: string;
  status: number;
  matchedStub?: string | null;
  nearMiss?: string | null;
  fault?: string | null;
  durationMs: number;
}

export interface MockInteractions {
  source: 'traceId' | 'timeWindow';
  confidence: number;
  items: MockInteraction[];
}

export interface LaunchPatch {
  important?: boolean;
  name?: string;
  attributes?: { key: string; value: string }[];
}

export type LogLevel = 'trace' | 'debug' | 'info' | 'warn' | 'error';

export const LOG_LEVELS: readonly LogLevel[] = ['trace', 'debug', 'info', 'warn', 'error'];

export type LogStream = 'stdout' | 'stderr' | 'log' | 'step';

export interface LogLine {
  resultId: string;
  attempt: number;
  seq: number;
  ts: IsoDateTime;
  level: LogLevel;
  stream: LogStream;
  message: string;
  step?: string | null;
}

export interface LogFilters {
  resultId?: string;
  attempt?: number;
  level?: LogLevel;
  q?: string;
}

/** `gap` SSE event: the server dropped lines because the client was too slow. */
export interface LogGap {
  skipped: number;
}
