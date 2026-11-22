import { describe, expect, it } from 'vitest';
import type { CaseStep, CaseWrite } from '../../api/tm';
import { bothChanged, mergeCaseWrite, mergeSteps } from './merge';

function step(id: string, action: string, steps: CaseStep[] = []): CaseStep {
  return { id, action, expected: '', data: '', attachments: [], steps };
}

function write(over: Partial<CaseWrite> & { steps?: CaseStep[]; description?: string } = {}): CaseWrite {
  const { steps, description, ...rest } = over;
  return {
    name: 'Pay with a saved card',
    status: 'active',
    folderId: 'f1',
    tags: ['smoke'],
    fields: { priority: 'High', layer: 'UI' },
    body: { description: description ?? 'Pays', preconditions: '', links: [], steps: steps ?? [step('a', 'Open the cart'), step('b', 'Pay')] },
    ...rest,
  };
}

describe('three-way merge of a case edit (412 "Save my changes")', () => {
  it('keeps a tag added meanwhile when the user changed only the name', () => {
    const base = write();
    const mine = write({ name: 'Pay with a saved card (edited)' });
    const theirs = write({ tags: ['smoke', 'concurrent'] });
    const out = mergeCaseWrite(base, mine, theirs);
    expect(out.name).toBe('Pay with a saved card (edited)');
    expect(out.tags).toEqual(['smoke', 'concurrent']);
    expect(bothChanged(base, mine, theirs)).toEqual([]);
  });

  it('merges custom fields by key and the folder, status and description separately', () => {
    const base = write();
    const mine = write({ fields: { priority: 'Critical', layer: 'UI' }, status: 'outdated' });
    const theirs = write({ fields: { priority: 'High', layer: 'API', owner: 'maria' }, folderId: 'f2', description: 'Pays (from code)' });
    const out = mergeCaseWrite(base, mine, theirs);
    expect(out.fields).toEqual({ priority: 'Critical', layer: 'API', owner: 'maria' });
    expect(out.status).toBe('outdated');
    expect(out.folderId).toBe('f2');
    expect(out.body.description).toBe('Pays (from code)');
  });

  it('the user wins where both changed the same value, and that is reported', () => {
    const base = write();
    const mine = write({ name: 'Mine' });
    const theirs = write({ name: 'Theirs' });
    expect(mergeCaseWrite(base, mine, theirs).name).toBe('Mine');
    expect(bothChanged(base, mine, theirs)).toEqual(['name']);
  });

  it('merges steps by id: edits of different steps, added and removed steps', () => {
    const base = [step('a', 'Open the cart'), step('b', 'Pay', [step('b1', 'Enter the CVC')]), step('c', 'See the receipt')];
    const mine = [step('a', 'Open the cart page'), step('b', 'Pay', [step('b1', 'Enter the CVC')]), step('c', 'See the receipt'), step('m', 'Mine at the end')];
    const theirs = [step('a', 'Open the cart'), step('b', 'Pay now', [step('b1', 'Enter the CVC'), step('t1', 'Confirm 3-D Secure')]), step('x', 'Inserted by them')];
    const out = mergeSteps(base, mine, theirs);
    expect(out.map((s) => s.action)).toEqual(['Open the cart page', 'Pay now', 'Inserted by them', 'Mine at the end']);
    expect(out[1]?.steps.map((s) => s.action)).toEqual(['Enter the CVC', 'Confirm 3-D Secure']);
  });

  it('a step removed meanwhile stays when the user edited it', () => {
    const base = [step('a', 'One'), step('b', 'Two')];
    const mine = [step('a', 'One'), step('b', 'Two, edited')];
    const theirs = [step('a', 'One')];
    expect(mergeSteps(base, mine, theirs).map((s) => s.action)).toEqual(['One', 'Two, edited']);
  });

  it('without concurrent step changes the user steps are taken as they are', () => {
    const base = [step('a', 'One')];
    const mine = [step('b', 'New'), step('a', 'One')];
    expect(mergeSteps(base, mine, base)).toEqual(mine);
    expect(mergeSteps(base, base, mine)).toEqual(mine);
  });
});

describe('markdown as plain text for the print view', () => {
  it('drops marks and keeps link targets', async () => {
    const { markdownText } = await import('./ui');
    expect(markdownText('Press **Add to cart** on *Trail shoes* at `https://shop.example.com`')).toBe('Press Add to cart on Trail shoes at https://shop.example.com');
    expect(markdownText('See [spec](https://example.com/spec) ![shot](attachment:1) 2 * 3')).toBe('See spec (https://example.com/spec) shot 2 * 3');
  });
});
