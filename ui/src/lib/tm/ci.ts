import type { PlanCi } from '../../api/planDispatches';

export const ENV_NAME = /^[A-Za-z0-9][A-Za-z0-9_.-]{0,63}$/;
export const MAX_ENVIRONMENTS = 10;

/** The CI part of a start: the variables the user set (the server fills the defaults), the ref and the environments. */
export function ciBody(values: Record<string, string>, ref: string, environments: string[]): PlanCi {
  const variables = Object.fromEntries(Object.entries(values).filter(([, v]) => v !== ''));
  return {
    dispatch: true,
    ...(ref ? { ref } : {}),
    ...(Object.keys(variables).length > 0 ? { variables } : {}),
    ...(environments.length > 0 ? { environments: environments.map((name) => ({ name })) } : {}),
  };
}
