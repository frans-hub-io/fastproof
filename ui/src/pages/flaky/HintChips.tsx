import type { ReactNode } from 'react';
import type { FlakyTest } from '../../api/types-next';

/** Likely causes of a flaky test (owner: flaky causes). Renders nothing until that branch fills it. */
export const HintChips: (props: { project: string; item: FlakyTest }) => ReactNode = () => null;
