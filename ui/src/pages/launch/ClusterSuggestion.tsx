import type { ReactNode } from 'react';
import type { Launch } from '../../api/types';
import type { Cluster } from '../../api/types-next';

/** Triage suggestions of a cluster (owner: suggestions). Renders nothing until that branch fills it. */
export const ClusterSuggestion: (props: { project: string; launch: Launch; cluster: Cluster }) => ReactNode = () => null;
