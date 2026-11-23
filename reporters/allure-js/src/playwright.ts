import type { FullResult, TestCase, TestResult as PwResult, TestStep } from '@playwright/test/reporter';
import AllureReporter from 'allure-playwright';
import type { LifecycleListener } from 'allure-js-commons/sdk/reporter';

import { FastProofAllure, type AllureFastProofOptions } from './core';

type AllureOptions = ConstructorParameters<typeof AllureReporter>[0];
export type FastProofAllureReporterOptions = AllureOptions & AllureFastProofOptions;

/**
 * `allure-playwright` with FastProof attached: use it in place of `allure-playwright` in `playwright.config`,
 * with the same options plus `fastproof: {...}`. allure-results are written exactly as before.
 */
export default class FastProofAllureReporter extends AllureReporter {
  private readonly fp: FastProofAllure;

  constructor(options: FastProofAllureReporterOptions = {} as FastProofAllureReporterOptions) {
    const fp = new FastProofAllure({
      ...options,
      resultsDir: options.resultsDir,
    });
    const listeners: LifecycleListener[] = [...(options.listeners ?? []), fp.listener];
    super({ ...options, listeners });
    this.fp = fp;
  }

  override onBegin(suite: Parameters<AllureReporter['onBegin']>[0]): void {
    this.fp.start();
    super.onBegin(suite);
  }

  override onTestBegin(test: TestCase, result: PwResult): void {
    this.fp.hint({ testId: test.id, retry: result.retry }, () => super.onTestBegin(test, result));
  }

  override onStepEnd(test: TestCase, result: PwResult, step: TestStep): void {
    this.fp.hint({ testId: test.id, retry: result.retry }, () => super.onStepEnd(test, result, step));
  }

  override async onEnd(result?: FullResult): Promise<void> {
    await super.onEnd();
    await this.fp.close(result?.status === 'interrupted' ? 'interrupted' : 'finished');
  }
}
