export type Uuid = string;
export type IsoDateTime = string;

export interface ErrorBody {
  code: string;
  message: string;
  details?: unknown;
}

export interface Page<T> {
  items: T[];
  next?: string | null;
}

export interface ItemList<T> {
  items: T[];
}

export interface LastLaunch {
  id: Uuid;
  number: number;
  name: string;
  status: LaunchStatus;
  startedAt: IsoDateTime;
  counts: Counts;
}

export interface Project {
  key: string;
  name: string;
  description: string | null;
  createdAt: IsoDateTime;
  lastLaunchAt: IsoDateTime | null;
  launchCount: number;
  lastLaunch?: LastLaunch | null;
}

export interface ProjectCreate {
  key: string;
  name: string;
  description?: string;
}

export interface ProjectPatch {
  name?: string;
  description?: string;
}

export type LaunchStatus = 'running' | 'finished' | 'interrupted';

export type ResultStatus = 'passed' | 'failed' | 'broken' | 'skipped' | 'unknown';

export const RESULT_STATUSES: readonly ResultStatus[] = ['passed', 'failed', 'broken', 'skipped', 'unknown'];

export interface Attribute {
  key: string;
  value: string;
}

export interface LaunchCi {
  provider: string;
  pipelineId: string | null;
  jobId: string | null;
  url: string | null;
  buildOrder: number | null;
}

export interface LaunchVcs {
  repository: string | null;
  branch: string | null;
  commit: string | null;
  commitUrl?: string | null;
}

export interface LaunchEnvironment {
  key: string;
  values: Record<string, string>;
}

export interface Counts {
  total: number;
  passed: number;
  failed: number;
  broken: number;
  skipped: number;
  unknown: number;
  flaky: number;
  muted: number;
}

export interface UploadCounters {
  queued: number;
  processing: number;
  failed: number;
}

export interface LaunchStart {
  externalId?: string;
  name: string;
  attributes?: Attribute[];
  ci?: LaunchCi;
  vcs?: LaunchVcs;
  environment?: LaunchEnvironment;
  debug?: boolean;
}

export interface Launch {
  id: Uuid;
  number: number;
  name: string;
  status: LaunchStatus;
  debug: boolean;
  startedAt: IsoDateTime;
  finishedAt: IsoDateTime | null;
  durationMs: number | null;
  ci: LaunchCi | null;
  vcs: LaunchVcs | null;
  environment: LaunchEnvironment | null;
  attributes: Attribute[];
  counts: Counts;
  uploads: UploadCounters;
}

export interface LaunchFinish {
  status?: LaunchStatus;
}

export interface LaunchFilters {
  branch?: string;
  environment?: string;
  status?: string;
  attr?: string;
  q?: string;
  debug?: boolean;
}

export type UploadFormat = 'auto' | 'allure' | 'junit' | 'ctrf' | 'playwright';

export type UploadStatus = 'queued' | 'processing' | 'done' | 'failed';

export interface UploadError {
  code: string;
  message: string;
  limit?: number | string | null;
}

export interface Upload {
  id: Uuid;
  format: string;
  status: UploadStatus;
  sizeBytes: number;
  counts: { results: number; attachments: number; ignored: number };
  warnings: string[];
  error?: UploadError | null;
  createdAt: IsoDateTime;
  finishedAt: IsoDateTime | null;
}

export interface ResultsAccepted {
  accepted: number;
  duplicates: number;
  rejected: { index: number; message: string }[];
}

export interface AttachmentCreated {
  id: Uuid;
  sha256: string;
  size: number;
}

export interface Label {
  name: string;
  value: string;
}

export interface Defect {
  type: string;
  comment: string | null;
  issueUrl: string | null;
}

export interface ResultSummary {
  id: Uuid;
  testCaseId: string;
  seriesKey: string;
  name: string;
  fullName: string;
  suitePath: string[];
  status: ResultStatus;
  flakyInRun: boolean;
  attemptCount: number;
  durationMs: number | null;
  labels: Label[];
  muted: boolean;
  fingerprint: string | null;
  defect?: Defect | null;
  message?: string | null;
}

export interface ResultFilters {
  status?: string;
  q?: string;
  suite?: string;
  label?: string;
  flaky?: boolean;
  fingerprint?: string;
}

export type TreeGroupBy = 'suite' | 'feature' | 'package';

export interface TreeNode {
  name: string;
  path: string[];
  counts: Counts;
  children: TreeNode[];
  resultIds?: Uuid[];
}

export interface Parameter {
  name: string;
  value: string;
}

export interface Link {
  name: string | null;
  url: string;
  type: string | null;
}

export interface StatusError {
  message: string | null;
  trace: string | null;
  actual: string | null;
  expected: string | null;
}

export type AttachmentKind = 'image' | 'video' | 'trace' | 'imageDiff' | 'httpExchange' | 'text' | 'log' | 'json' | 'other';

export interface Attachment {
  id: Uuid;
  name: string;
  contentType: string;
  kind: AttachmentKind;
  size: number;
}

export type StepKind = 'step' | 'fixture';

export interface Step {
  name: string;
  status: ResultStatus;
  kind: StepKind;
  startedAt: IsoDateTime | null;
  stoppedAt: IsoDateTime | null;
  durationMs: number | null;
  error?: StatusError | null;
  parameters: Parameter[];
  attachments: Attachment[];
  steps: Step[];
}

export interface Attempt {
  no: number;
  status: ResultStatus;
  startedAt: IsoDateTime | null;
  stoppedAt: IsoDateTime | null;
  durationMs: number | null;
  error?: StatusError | null;
  steps: Step[];
  attachments: Attachment[];
  stdout?: string | null;
  stderr?: string | null;
  traceId?: string | null;
}

export interface SeriesHistoryItem {
  launchId: Uuid;
  launchNumber: number;
  status: ResultStatus;
  startedAt: IsoDateTime;
  resultId?: Uuid;
}

export interface ResultDetail extends ResultSummary {
  description: string | null;
  parameters: Parameter[];
  links: Link[];
  attempts: Attempt[];
  history: SeriesHistoryItem[];
  launchId?: Uuid;
  launchNumber?: number;
}

export interface TestCaseHistoryItem {
  launchId: Uuid;
  launchNumber: number;
  resultId: Uuid;
  status: ResultStatus;
  startedAt: IsoDateTime;
  durationMs: number | null;
  branch: string | null;
  environment: string | null;
}

export interface TestCaseHistoryFilters {
  branch?: string;
  environment?: string;
  limit?: number;
}

export interface LaunchUpdatedEvent {
  id: Uuid;
}

export interface UploadUpdatedEvent {
  launchId: Uuid;
  id: Uuid;
}

export type TokenStatus = 'active' | 'expired' | 'revoked';

export interface TokenInfo {
  id: Uuid;
  name: string;
  role: string;
  scopes: string[];
  global: boolean;
  description?: string;
  createdBy: string;
  createdAt: IsoDateTime;
  expiresAt: IsoDateTime;
  lastUsedAt: IsoDateTime | null;
  revokedAt: IsoDateTime | null;
  status: TokenStatus;
  parentId?: Uuid;
}

export interface CreatedToken extends TokenInfo {
  token: string;
}

export interface TokenList {
  tokens: TokenInfo[];
}

export interface CreateToken {
  name: string;
  role: string;
  scopes?: string[];
  global?: boolean;
  expiresInDays?: number;
  description?: string;
}

export interface GrantableRole {
  name: string;
  builtin: boolean;
  description?: string;
  permissions: string[];
  global: boolean;
  scopes: string[];
  allScopes: boolean;
}

export interface TokenOptions {
  maxDays: number;
  defaultDays: number;
  scopes: string[];
  roles: GrantableRole[];
}

export interface AuditEvent {
  id: number;
  at: IsoDateTime;
  principal: string;
  principalKind?: string;
  roles: string;
  scope?: string;
  action: string;
  detail: unknown;
}

export interface AuditPage {
  items?: AuditEvent[];
  next?: string | null;
  events?: AuditEvent[];
  nextBefore?: number | null;
}

export interface AuditFilters {
  action?: string;
  principal?: string;
  scope?: string;
}

export interface HttpExchangeBody {
  encoding?: string;
  value?: string;
  size?: number;
  contentType?: string;
}

export interface HttpExchangeHeader {
  name: string;
  value: string;
}

export interface HttpExchange {
  schemaVersion?: number;
  request?: {
    method?: string;
    url?: string;
    headers?: HttpExchangeHeader[];
    cookies?: HttpExchangeHeader[];
    body?: HttpExchangeBody | null;
  };
  response?: {
    status?: number;
    statusText?: string;
    headers?: HttpExchangeHeader[];
    cookies?: HttpExchangeHeader[];
    body?: HttpExchangeBody | null;
  };
  start?: number;
  stop?: number;
}

export interface AllureImageDiff {
  expected?: string;
  actual?: string;
  diff?: string;
  name?: string;
}
