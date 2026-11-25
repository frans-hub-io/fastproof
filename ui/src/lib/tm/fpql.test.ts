import { describe, expect, it } from 'vitest';
import { readListState } from './filters';
import { analyze, chipsToQuery, completionsAt, countLabel, errorText, quoteValue, SHORT_TEXT_NOTE, tokenize, valueText, type CompletionSchema } from './fpql';

const state = (extra: Record<string, string | string[]> = {}) => {
  const sp = new URLSearchParams();
  for (const [k, v] of Object.entries(extra)) for (const x of Array.isArray(v) ? v : [v]) sp.append(k, x);
  return readListState(sp);
};

const schema: CompletionSchema = {
  attributes: [
    { name: 'status', type: 'enum', operators: ['=', '!=', 'in'], values: ['draft', 'active', 'outdated'], description: 'Lifecycle status' },
    { name: 'automated', type: 'bool', operators: ['=', '!='], description: 'Automated' },
    { name: 'tag', type: 'string', operators: ['=', 'in', 'is empty'], description: 'Tag' },
    { name: 'field', type: 'field', operators: ['=', 'in'], description: 'Custom field' },
    { name: 'lastRunAt', type: 'date', operators: ['<', '>'], description: 'Last run' },
  ],
  functions: [
    { name: 'now', signature: 'now() - 7d', description: 'now' },
    { name: 'me', signature: 'me()', description: 'me' },
    { name: 'subtree', signature: 'subtree("path" | id)', description: 'subtree' },
  ],
  fields: [{ key: 'priority', name: 'Priority', type: 'select', values: ['High', 'Low risk'] }],
  keywords: ['and', 'or', 'not'],
};

describe('chipsToQuery', () => {
  it('is empty without chips', () => {
    expect(chipsToQuery(state())).toEqual({ query: '', note: null });
  });

  it('serializes every chip kind', () => {
    const s = state({ status: 'draft,active', automation: 'manual', aiUnreviewed: 'true', staleDays: '30', tag: ['smoke', 'a "b"'], field: ['priority:High', 'priority:Low risk', 'layer:UI'], q: 'refund' });
    expect(chipsToQuery(s).query).toBe(
      'status in [draft, active] and automated = false and aiUnreviewed = true and staleDays > 30 and tag = "smoke" and tag = "a \\"b\\"" and field["priority"] in ["High", "Low risk"] and field["layer"] in ["UI"] and (text ~ "refund" or name ~ "refund")',
    );
  });

  it('uses = for one status and true for automated', () => {
    expect(chipsToQuery(state({ status: 'active', automation: 'automated' })).query).toBe('status = active and automated = true');
  });

  it('turns TC-N and digits into a number condition', () => {
    expect(chipsToQuery(state({ q: 'TC-12' })).query).toBe('number = 12');
    expect(chipsToQuery(state({ q: ' 7 ' })).query).toBe('number = 7');
  });

  it('drops a search text shorter than three characters and says so', () => {
    expect(chipsToQuery(state({ q: 'ab', status: 'active' }))).toEqual({ query: 'status = active', note: SHORT_TEXT_NOTE });
  });

  it('escapes backslashes and quotes', () => {
    expect(quoteValue('a\\b"c')).toBe('"a\\\\b\\"c"');
    expect(chipsToQuery(state({ tag: 'C:\\temp' })).query).toBe('tag = "C:\\\\temp"');
  });
});

describe('valueText', () => {
  it('keeps safe words bare and quotes the rest', () => {
    expect(valueText('active')).toBe('active');
    expect(valueText('TC-12')).toBe('TC-12');
    expect(valueText('two words')).toBe('"two words"');
    expect(valueText('and')).toBe('"and"');
  });
});

describe('tokenize and analyze', () => {
  it('classifies the whole grammar', () => {
    const q = 'status = active and field["Priority"] in ["high", "critical"] and folder in subtree("Payments/Cards") and not tag = legacy and lastRunAt < now() - 14d and x is not empty';
    const roles = analyze(q).tokens.map((t) => `${t.text}:${t.role}`);
    expect(roles).toContain('status:attribute');
    expect(roles).toContain('active:value');
    expect(roles).toContain('and:keyword');
    expect(roles).toContain('field:attribute');
    expect(roles).toContain('"Priority":fieldkey');
    expect(roles).toContain('in:keyword');
    expect(roles).toContain('"high":string');
    expect(roles).toContain('subtree:function');
    expect(roles).toContain('now:function');
    expect(roles).toContain('14d:number');
    expect(roles).toContain('not:keyword');
    expect(roles).toContain('empty:keyword');
    expect(roles).toContain('<:operator');
  });

  it('tracks unterminated strings by offset', () => {
    const toks = tokenize('name = "abc');
    expect(toks[2]).toMatchObject({ role: 'string', from: 7, to: 11, closed: false });
  });

  it('handles escapes inside strings', () => {
    const toks = tokenize('name = "a\\"b" and');
    expect(toks[2]?.text).toBe('"a\\"b"');
    expect(toks[2]?.closed).toBe(true);
  });

  it('reports the grammar state at the end', () => {
    expect(analyze('').state).toBe('attr');
    expect(analyze('status').state).toBe('op');
    expect(analyze('status =').state).toBe('value');
    expect(analyze('status = active').state).toBe('logic');
    expect(analyze('status in [draft, ').state).toBe('listvalue');
    expect(analyze('status in [draft').state).toBe('listsep');
    expect(analyze('status in [draft] ').state).toBe('logic');
    expect(analyze('tag is ').state).toBe('isrest');
    expect(analyze('field[').state).toBe('fieldkey');
    expect(analyze('field["Priority"]').state).toBe('op');
    expect(analyze('(status = active) and ').state).toBe('attr');
    expect(analyze('lastRunAt < now() - ').state).toBe('value');
  });
});

describe('completionsAt', () => {
  const labels = (text: string) => completionsAt(text, schema)?.options.map((o) => o.label) ?? [];

  it('offers attributes at the start and after and', () => {
    expect(labels('')).toEqual(['status', 'automated', 'tag', 'field', 'lastRunAt', 'not']);
    expect(completionsAt('sta', schema)?.from).toBe(0);
    expect(labels('status = active and ')).toContain('tag');
  });

  it('offers the operators of the attribute, with not in beside in', () => {
    expect(labels('status ')).toEqual(['=', '!=', 'in', 'not in']);
    expect(labels('lastRunAt ')).toEqual(['<', '>']);
  });

  it('offers enum values, bare, after an operator', () => {
    const r = completionsAt('status = ', schema);
    expect(r?.options.map((o) => o.insert)).toEqual(['draft', 'active', 'outdated']);
    expect(r?.from).toBe(9);
  });

  it('offers values inside a list and a bool as true or false', () => {
    expect(labels('status in [draft, ')).toContain('outdated');
    expect(labels('automated = ')).toEqual(expect.arrayContaining(['true', 'false']));
  });

  it('offers field keys and names after field[ and closes the bracket', () => {
    const r = completionsAt('field[', schema);
    expect(r?.options.map((o) => o.insert)).toEqual(['priority]', '"Priority"]']);
  });

  it('offers the values of the chosen field, quoting those with spaces', () => {
    const r = completionsAt('field["Priority"] = ', schema);
    expect(r?.options.filter((o) => o.kind === 'value').map((o) => o.insert)).toEqual(['High', '"Low risk"']);
  });

  it('completes inside an open string and closes it', () => {
    const r = completionsAt('status = "ac', schema);
    expect(r?.from).toBe(10);
    expect(r?.options.map((o) => o.insert)).toContain('active"');
  });

  it('offers functions for values, with the cursor placed inside the argument', () => {
    const fns = completionsAt('lastRunAt < ', schema)?.options.filter((o) => o.kind === 'function');
    expect(fns?.map((f) => f.insert)).toEqual(['now()', 'me()', 'subtree("")']);
    expect(fns?.find((f) => f.label === 'subtree')?.cursorBack).toBe(2);
  });

  it('offers and or after a complete condition and nothing after is', () => {
    expect(labels('status = active ')).toEqual(['and', 'or']);
    expect(labels('tag is ')).toEqual(['empty', 'not empty']);
  });
});

describe('labels', () => {
  it('formats counts', () => {
    expect(countLabel(12, false)).toBe('12');
    expect(countLabel(1234, false)).toBe('1 234');
    expect(countLabel(10000, true)).toBe('10 000+');
  });

  it('adds what was expected to the message', () => {
    expect(errorText({ message: 'unknown attribute', expected: [] })).toBe('unknown attribute');
    expect(errorText({ message: 'expected an operator', expected: ['=', 'in'] })).toBe('expected an operator. Expected: =, in.');
  });
});
