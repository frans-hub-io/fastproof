import type { ReactNode } from 'react';
import type { ResultDetail } from '../../api/types';

/** Suggestions for the cluster of a failed result (owner: suggestions). Renders nothing until that branch fills it. */
export const SuggestionsPanel: (props: { project: string; r: ResultDetail }) => ReactNode = () => null;
