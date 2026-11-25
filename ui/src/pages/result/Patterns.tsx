import type { ReactNode } from 'react';
import type { ResultDetail } from '../../api/types';

/** Log pattern marks of a result (owner: log patterns). Renders nothing until that branch fills it. */
export const Patterns: (props: { project: string; r: ResultDetail }) => ReactNode = () => null;
