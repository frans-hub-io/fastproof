import { AppShellLayout, type CommandItem, type NavItem } from '@franshub/ui-shell';
import { IconFlask2, IconKey, IconListDetails, IconShieldSearch } from '@tabler/icons-react';
import { useQueryClient, type InfiniteData } from '@tanstack/react-query';
import { useEffect, useMemo, useState, type ReactNode } from 'react';
import { matchPath, useLocation, useNavigate } from 'react-router';
import { useProjects } from './api/hooks';
import type { Launch, Page } from './api/types';
import { readRecent, type RecentLaunch } from './lib/recent';

function useRecent(): RecentLaunch[] {
  const [list, setList] = useState(readRecent);
  useEffect(() => {
    const update = () => setList(readRecent());
    window.addEventListener('fastproof-recent', update);
    window.addEventListener('storage', update);
    return () => {
      window.removeEventListener('fastproof-recent', update);
      window.removeEventListener('storage', update);
    };
  }, []);
  return list;
}

function Logo() {
  return (
    <svg viewBox="0 0 64 64" width="28" height="28" aria-hidden focusable="false">
      <rect width="64" height="64" rx="14" fill="#7C2ACB" />
      <g transform="skewX(-10) translate(6 0)">
        <path d="M18 46V18h22v7H26v5h12v7H26v9z" fill="#FFFFFF" />
      </g>
      <circle cx="48" cy="44" r="6" fill="#FFD028" />
    </svg>
  );
}

export function Shell({ children }: { children: ReactNode }) {
  const navigate = useNavigate();
  const location = useLocation();
  const projects = useProjects();
  const recent = useRecent();
  const qc = useQueryClient();
  const projectKey = matchPath('/p/:key/*', location.pathname)?.params.key ?? matchPath('/p/:key', location.pathname)?.params.key;
  const path = location.pathname;

  const navItems = useMemo<NavItem[]>(() => {
    const items: NavItem[] = [{ label: 'Projects', href: '/', icon: <IconFlask2 size={18} />, active: path === '/' }];
    if (projectKey) {
      const name = projects.data?.find((p) => p.key === projectKey)?.name ?? projectKey;
      items.push({ label: `${name}: launches`, href: `/p/${projectKey}`, icon: <IconListDetails size={18} />, active: path.startsWith(`/p/${projectKey}`) });
    }
    items.push({ label: 'API tokens', href: '/settings/tokens', icon: <IconKey size={18} />, active: path === '/settings/tokens' });
    items.push({ label: 'Audit log', href: '/settings/audit', icon: <IconShieldSearch size={18} />, active: path === '/settings/audit', permission: 'audit:read' });
    return items;
  }, [path, projectKey, projects.data]);

  const commands = useMemo<CommandItem[]>(() => {
    const out: CommandItem[] = [];
    for (const p of projects.data ?? []) {
      out.push({ id: `project:${p.key}`, label: p.name, group: 'Projects', keywords: [p.key, p.description ?? ''], onSelect: () => void navigate(`/p/${p.key}`) });
    }
    const seen = new Set<string>();
    for (const r of recent) {
      seen.add(r.id);
      out.push({
        id: `recent:${r.id}`,
        label: `#${String(r.number)} ${r.name}`,
        group: 'Recent launches',
        keywords: [r.project, String(r.number)],
        onSelect: () => void navigate(`/p/${r.project}/launches/${r.id}`),
      });
    }
    if (projectKey) {
      const cached = qc.getQueriesData<InfiniteData<Page<Launch>>>({ queryKey: ['launches', projectKey] });
      const launches = cached.flatMap(([, d]) => (d && 'pages' in d ? d.pages.flatMap((pg) => pg.items) : []));
      for (const l of launches.slice(0, 15)) {
        if (seen.has(l.id)) continue;
        seen.add(l.id);
        out.push({
          id: `launch:${l.id}`,
          label: `#${String(l.number)} ${l.name}`,
          group: 'Launches in this project',
          keywords: [l.vcs?.branch ?? '', l.status],
          onSelect: () => void navigate(`/p/${projectKey}/launches/${l.id}`),
        });
      }
    }
    return out;
  }, [projects.data, recent, projectKey, qc, navigate]);

  return (
    <AppShellLayout productName="FastProof" logo={<Logo />} navItems={navItems} commands={commands} onNavigate={(href) => void navigate(href)}>
      {children}
    </AppShellLayout>
  );
}
