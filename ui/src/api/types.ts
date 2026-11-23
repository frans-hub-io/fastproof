// API types. Everything served by FastProof comes from `schema.gen.ts`, generated from
// `schemas/openapi.json` with `npm run gen:api`; the aliases below only narrow string fields that
// the spec documents as fixed sets (statuses, kinds) and give the UI short names. Platform routes
// that are not part of the FastProof OpenAPI (`/me`, `/tokens`) and client-side formats
// (Allure HTTP exchanges and image diffs) are written by hand at the end. The analysis,
// integrations and logs aliases live in `types-next.ts`.
import type { components } from './schema.gen';
import type { DefectGroup, DefectInfo, DefectSource, Transition } from './types-next';

type S = components['schemas'];

export type Uuid = string;
export type IsoDateTime = string;

export type ErrorBody = S['ErrorBody'];

export type VersionInfo = S['VersionInfo'];

export interface Page<T> {
  items: T[];
  next?: string | null;
}

export interface ItemList<T> {
  items: T[];
}

export type LaunchStatus = 'running' | 'finished' | 'interrupted';

export type ResultStatus = S['Status'];

export const RESULT_STATUSES: readonly ResultStatus[] = ['passed', 'failed', 'broken', 'skipped', 'unknown'];

export type Counts = S['Counts'];
export type Attribute = S['Attribute'];
export type LaunchCi = S['LaunchCi'];
export type LaunchVcs = S['LaunchVcs'];
export type LaunchEnvironment = S['LaunchEnvironment'];
export type UploadCounters = S['UploadCounts'];
export type LaunchStart = S['LaunchStart'];
export type LaunchFinish = S['FinishLaunch'];
export type ResultsAccepted = S['IngestSummary'];
export type AttachmentCreated = S['AttachmentCreated'];
export type Label = S['LabelView'];
export type Parameter = S['ParameterView'];
export type Link = S['LinkView'];
export type StatusError = S['ErrorView'];
export type ProjectCreate = S['NewProject'];
export type ProjectPatch = S['PatchProject'];
export type ScheduledJob = S['ScheduledJob'];
export type AuditEvent = S['AuditItem'];
export type AuditPage = S['AuditList'];
export type TestCaseView = S['TestCaseView'];

export type LastLaunch = Omit<S['LastLaunch'], 'status'> & { status: LaunchStatus };

export type Project = Omit<S['Project'], 'lastLaunch' | 'description'> & {
  description: string | null;
  lastLaunch?: LastLaunch | null;
};

export type Launch = Omit<S['Launch'], 'status'> & { status: LaunchStatus };
export type LaunchExport = Omit<S['LaunchExport'], 'status'> & { status: 'queued' | 'running' | 'done' | 'failed' | 'expired' };

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

export type UploadError = S['UploadError'];

export type Upload = Omit<S['Upload'], 'status'> & { status: UploadStatus };

export type Defect = DefectInfo;

export type ResultSummary = Omit<S['ResultSummary'], 'status' | 'defect' | 'transition'> & {
  status: ResultStatus;
  defect?: Defect | null;
  transition?: Transition | null;
};

/**
 * Explorer filters shared by the tree, its pages and the result list; the names are those of
 * the API and of the page URL. Lists are comma-separated.
 */
export interface ExplorerParams {
  message?: string;
  retried?: boolean;
  muted?: boolean;
  newFailures?: boolean;
  severity?: string;
  owner?: string;
  tag?: string;
  minDurationMs?: number;
  maxDurationMs?: number;
  defect?: string;
  attachment?: string;
  /** An environment key, or `(none)`. */
  env?: string;
}

export interface ResultFilters extends ExplorerParams {
  status?: string;
  q?: string;
  suite?: string;
  label?: string;
  flaky?: boolean;
  fingerprint?: string;
  source?: string;
}

/** `suite`, `feature`, `package` or comma-separated label names. */
export type TreeGroupBy = string;

export type LabelName = S['LabelName'];
export type ValueCount = S['ValueCount'];
export type LabelFacet = S['LabelFacet'];
export type LaunchFacets = S['LaunchFacets'];
export type EnvironmentCounts = S['EnvironmentCounts'];

export type TreeDefect = Omit<S['TreeDefect'], 'group' | 'source'> & { group: DefectGroup; source: DefectSource };

/** A result as a leaf of the launch tree (`includeTests`). */
export type TreeTest = Omit<S['TreeTest'], 'status' | 'defect' | 'transition'> & {
  status: ResultStatus;
  defect?: TreeDefect | null;
  transition?: Transition | null;
};

export type TreeNode = Omit<S['TreeNode'], 'children' | 'tests'> & {
  children: TreeNode[];
  tests?: TreeTest[] | null;
};

/** Filters of the launch tree with leaves. */
export interface TreeFilters extends ExplorerParams {
  groupBy: TreeGroupBy;
  status?: string;
  q?: string;
  flaky?: boolean;
  source?: string;
  reveal?: string;
}

export type AttachmentKind = 'image' | 'video' | 'trace' | 'imageDiff' | 'httpExchange' | 'text' | 'log' | 'json' | 'other';

/** An attachment as the server references it; `id` is null when the file was not uploaded. */
export type AttachmentRef = S['AttachmentView'];

/** An attachment that can be fetched (see `toAttachment`). */
export interface Attachment {
  id: Uuid;
  name: string;
  contentType: string;
  kind: AttachmentKind;
  size: number;
}

export type StepKind = 'step' | 'fixture';

export type Step = Omit<S['StepView'], 'status' | 'kind' | 'steps'> & {
  status: ResultStatus;
  kind: StepKind;
  steps: Step[];
};

export type Attempt = Omit<S['AttemptView'], 'status' | 'steps'> & {
  status: ResultStatus;
  steps: Step[];
};

export type SeriesHistoryItem = Omit<S['SeriesPoint'], 'status'> & { status: ResultStatus };

export type ResultDetail = ResultSummary &
  Omit<S['ResultDetail'], keyof S['ResultSummary'] | 'attempts' | 'history'> & {
    attempts: Attempt[];
    history: SeriesHistoryItem[];
  };

export type TestCaseHistoryItem = Omit<S['HistoryEntry'], 'status'> & { status: ResultStatus };

export type TestCaseHistory = Omit<S['HistoryList'], 'items'> & { items: TestCaseHistoryItem[] };

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

export interface AuditFilters {
  action?: string;
  principal?: string;
  scope?: string;
}

// Platform routes (`@franshub` access layer), not part of the FastProof OpenAPI.

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

// Attachment formats parsed in the browser.

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
