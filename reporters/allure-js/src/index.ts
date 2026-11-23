import { FastProofAllure, type AllureFastProofOptions } from './core';

export { FastProofAllure, TRACE_LABEL, type AllureFastProofOptions, type Hint } from './core';

/**
 * For any adapter built on `allure-js-commons` that takes a `listeners` array:
 *
 *     const fastproof = createFastProofListener({ resultsDir: 'allure-results' });
 *     // adapter options: { listeners: [fastproof.listener] }
 *     fastproof.start();            // when the run begins
 *     await fastproof.close();      // when it ends
 */
export function createFastProofListener(options: AllureFastProofOptions = {}): FastProofAllure {
  return new FastProofAllure(options);
}
