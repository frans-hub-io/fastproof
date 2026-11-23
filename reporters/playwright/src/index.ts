import { FastProofReporter } from './reporter';

export default FastProofReporter;
export { FastProofReporter, statusOf, stripAnsi } from './reporter';
export type { FastProofOptions, Config } from './config';
export { resolveConfig } from './config';
export { Masker, DEFAULT_SECRET_ENV, MASK } from './mask';
export { loadTestPlan, testPlanGrep, inTestPlan, fullNameOf, type TestPlan } from './testplan';
export {
  traceFor,
  parseTraceparent,
  formatTraceparent,
  randomSpanId,
  SEED_ENV,
  LAUNCH_ENV,
  TRACE_ANNOTATION,
  type TraceContext,
} from './trace';
export { flushSpool, listSessions } from './spool';
export { Api } from './api';
export { Client, type ClientSummary } from './client';
export type * from './types';
