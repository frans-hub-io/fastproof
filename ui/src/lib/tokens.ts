import type { CreateToken } from '../api/types';

export type ScopeMode = 'scopes' | 'all' | 'global';

export function buildCreateBody(form: { name: string; role: string; mode: ScopeMode; scopes: string[]; days: number | string; description: string }): CreateToken {
  const body: CreateToken = { name: form.name.trim(), role: form.role };
  if (form.mode === 'global') body.global = true;
  else if (form.mode === 'all') body.scopes = ['*'];
  else body.scopes = form.scopes;
  if (typeof form.days === 'number') body.expiresInDays = form.days;
  if (form.description.trim()) body.description = form.description.trim();
  return body;
}
