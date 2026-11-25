import type { ReactNode } from 'react';
import type { ResultDetail } from '../../api/types';

/** Signals of grids, CI and Kubernetes about a result's attempts (owner: signals). Renders nothing until that branch fills it. */
export const SignalsBlock: (props: { project: string; r: ResultDetail }) => ReactNode = () => null;
