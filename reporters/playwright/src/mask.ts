/** Secret masking at the source: values of matching environment variables, explicit values and patterns. */

export const MASK = '****';

/** Names whose values are secrets unless configured otherwise. */
export const DEFAULT_SECRET_ENV = [
  'FASTPROOF_TOKEN',
  '*TOKEN*',
  '*SECRET*',
  '*PASSWORD*',
  '*PASSWD*',
  '*_KEY',
  '*APIKEY*',
  '*API_KEY*',
  '*CREDENTIAL*',
];

const NOT_SECRET = new Set(['true', 'false', 'yes', 'no', 'on', 'off', 'null', 'none', 'undefined']);
/** Shorter values are not masked: they would mangle ordinary text. */
export const MIN_SECRET_LENGTH = 4;

function globToRegExp(glob: string): RegExp {
  const esc = glob.replace(/[.+^${}()|[\]\\]/g, '\\$&').replace(/\*/g, '.*').replace(/\?/g, '.');
  return new RegExp(`^${esc}$`, 'i');
}

function escapeRegExp(s: string): string {
  return s.replace(/[.*+?^${}()|[\]\\]/g, '\\$&');
}

export interface MaskOptions {
  /** Environment variable names or globs (`*TOKEN*`); their values are masked. */
  env?: string[];
  /** Literal secret values. */
  values?: string[];
  /** Regular expressions; every match is masked. */
  patterns?: (string | RegExp)[];
}

export class Masker {
  private readonly literal: RegExp | undefined;
  private readonly patterns: RegExp[];

  constructor(o: MaskOptions = {}, env: NodeJS.ProcessEnv = process.env) {
    const globs = (o.env ?? DEFAULT_SECRET_ENV).map(globToRegExp);
    const values = new Set<string>();
    const add = (v: string | undefined) => {
      if (!v) return;
      const t = v.trim();
      if (t.length < MIN_SECRET_LENGTH || NOT_SECRET.has(t.toLowerCase()) || /^\d{1,6}$/.test(t)) return;
      values.add(t);
      const enc = encodeURIComponent(t);
      if (enc !== t) values.add(enc);
    };
    for (const [k, v] of Object.entries(env)) {
      if (globs.some((g) => g.test(k))) add(v);
    }
    for (const v of o.values ?? []) add(v);
    const sorted = [...values].sort((a, b) => b.length - a.length).map(escapeRegExp);
    this.literal = sorted.length ? new RegExp(sorted.join('|'), 'g') : undefined;
    this.patterns = (o.patterns ?? []).map((p) =>
      typeof p === 'string' ? new RegExp(p, 'g') : new RegExp(p.source, p.flags.includes('g') ? p.flags : p.flags + 'g'),
    );
  }

  get active(): boolean {
    return this.literal !== undefined || this.patterns.length > 0;
  }

  mask(s: string): string;
  mask(s: string | null | undefined): string | null | undefined;
  mask(s: string | null | undefined): string | null | undefined {
    if (!s) return s;
    let out = s;
    if (this.literal) out = out.replace(this.literal, MASK);
    for (const p of this.patterns) out = out.replace(p, MASK);
    return out;
  }
}
