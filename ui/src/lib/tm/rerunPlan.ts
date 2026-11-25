import type { RerunSettings } from '../../api/types-next';

export const MAX_REFS = 20;
export const MAX_VARIABLES = 20;
export const MAX_VALUES = 50;
const NAME = /^[A-Z][A-Z0-9_]{0,63}$/;
const GLOB = /^[A-Za-z0-9_./*-]{1,200}$/;
const RESERVED_PREFIXES = ['CI_', 'GITLAB_', 'GITHUB_', 'RUNNER_', 'ACTIONS_', 'LD_', 'DYLD_', 'BASH_', 'NODE_', 'PYTHON', 'JAVA_', 'JDK_'];
const RESERVED = ['CI', 'PATH', 'ENV', 'IFS', 'HOME', 'SHELL', 'USER', 'PS4', 'PROMPT_COMMAND', 'SHELLOPTS', 'TMPDIR', 'OPTIONS', 'JAVA_TOOL_OPTIONS', '_JAVA_OPTIONS'];
const encoder = new TextEncoder();

export type Variable = NonNullable<RerunSettings['variables']>[number];

function valueError(v: string): boolean {
  // eslint-disable-next-line no-control-regex
  return v.length === 0 || encoder.encode(v).length > 256 || /[\u0000-\u001f\u007f]/.test(v);
}

/** What is wrong with the plan fields of the rerun settings, by input (empty when valid). */
export function rerunPlanErrors(rr: RerunSettings | null): { refs: string | null; variables: (string | null)[] } {
  const refs = rr?.refs ?? [];
  const vars = rr?.variables ?? [];
  const names = new Set<string>();
  return {
    refs: refs.length > MAX_REFS ? `At most ${String(MAX_REFS)} patterns` : refs.some((r) => !GLOB.test(r) || r.includes('..')) ? 'Letters, digits, - _ . / and * only' : null,
    variables: vars.map((v) => {
      const dup = names.has(v.name);
      names.add(v.name);
      if (!NAME.test(v.name) || v.name.startsWith('FASTPROOF_')) return 'Name: capital letters, digits and _, starting with a letter, not FASTPROOF_…';
      if (RESERVED.includes(v.name) || RESERVED_PREFIXES.some((p) => v.name.startsWith(p))) return 'Reserved: the name changes how the CI or the shell behaves';
      if (dup) return 'Names must be unique';
      if ((v.values ?? []).length > MAX_VALUES || (v.values ?? []).some(valueError)) return `Values: at most ${String(MAX_VALUES)}, 1 to 256 bytes, no control characters`;
      if (v.default !== undefined && v.default !== null && (v.values?.length ? !v.values.includes(v.default) : valueError(v.default))) return 'The default must be one of the values';
      if ((v.description ?? '').length > 200) return 'The description has at most 200 characters';
      return null;
    }),
  };
}

/** The rerun settings as sent: empty `refs` and `variables` are left out, so a project that does not use them keeps its document unchanged. */
export function cleanRerunPlanFields(rr: RerunSettings): RerunSettings {
  const { refs, variables, ...rest } = rr;
  const out: RerunSettings = { ...rest };
  if (refs && refs.length > 0) out.refs = refs;
  if (variables && variables.length > 0) {
    out.variables = variables.map((v) => ({
      name: v.name,
      ...(v.values && v.values.length > 0 ? { values: v.values } : {}),
      ...(v.default ? { default: v.default } : {}),
      ...(v.description ? { description: v.description } : {}),
    }));
  }
  return out;
}
