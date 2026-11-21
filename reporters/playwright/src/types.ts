/** Wire types of the FastProof API used by the reporter (see reporters/PROTOCOL.md). */

export type Status = 'passed' | 'failed' | 'broken' | 'skipped' | 'unknown';
export type WireStatus = Status | 'running';

export interface Label {
  name: string;
  value: string;
}

export interface Link {
  name?: string | null;
  type?: string | null;
  url: string;
}

export interface Parameter {
  name: string;
  value?: string | null;
  mode?: 'default' | 'masked' | 'hidden';
  excluded?: boolean;
}

export interface AttachmentRef {
  id: string;
  name?: string | null;
  contentType?: string | null;
}

export interface ReportError {
  message?: string | null;
  trace?: string | null;
  actual?: string | null;
  expected?: string | null;
}

export interface ReportStep {
  name: string;
  status: Status;
  start?: number | null;
  stop?: number | null;
  durationMs?: number | null;
  error?: ReportError | null;
  parameters?: Parameter[];
  attachments?: AttachmentRef[];
  steps?: ReportStep[];
}

/** One attempt of one test (schemas/fastproof-report-v1.json), plus `running` for live results. */
export interface ReportResult {
  id: string;
  name: string;
  status: WireStatus;
  fullName?: string | null;
  testCaseId?: string | null;
  description?: string | null;
  start?: number | null;
  stop?: number | null;
  durationMs?: number | null;
  retry?: number | null;
  error?: ReportError | null;
  labels?: Label[];
  links?: Link[];
  parameters?: Parameter[];
  steps?: ReportStep[];
  attachments?: AttachmentRef[];
  stdout?: string | null;
  stderr?: string | null;
  traceparent?: string | null;
  source?: { file?: string | null; line?: number | null } | null;
}

export type LogLevel = 'trace' | 'debug' | 'info' | 'warn' | 'error';
export type LogStream = 'stdout' | 'stderr' | 'log' | 'step';

/** One line of `POST .../launches/{id}/logs`. */
export interface LogLine {
  resultId: string;
  attempt: number;
  seq: number;
  ts: number;
  level: LogLevel;
  stream: LogStream;
  message: string;
  step?: string;
}

export interface LaunchStart {
  externalId: string;
  name: string;
  attributes?: { key: string; value: string }[];
  ci?: {
    provider?: string;
    pipelineId?: string;
    jobId?: string;
    url?: string;
    buildOrder?: number;
  };
  vcs?: { repository?: string; branch?: string; commit?: string };
  environment?: { key: string; values: Record<string, string> };
  debug?: boolean;
}

export interface Launch {
  id: string;
  number?: number;
  status?: string;
}

export interface IngestSummary {
  accepted: number;
  duplicates: number;
  rejected: { index: number; message: string }[];
}

/** A file to upload as an attachment of an attempt. */
export interface AttachmentFile {
  name: string;
  contentType: string;
  path?: string;
  body?: Buffer;
}
