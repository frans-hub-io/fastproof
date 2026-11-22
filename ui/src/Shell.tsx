import { AppShellLayout, type CommandItem, type NavItem } from '@franshub/ui-shell';
import { IconAdjustments, IconBell, IconChartLine, IconClockCog, IconFlask2, IconGitCompare, IconKey, IconListDetails, IconShieldCheck, IconShieldSearch, IconWand, IconWaveSine } from '@tabler/icons-react';
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
    <>
      <img src="/brand/monogram-light.svg" alt="" width={46} height={26} className="fp-on-light" />
      <img src="/brand/monogram-dark.svg" alt="" width={46} height={26} className="fp-on-dark" />
    </>
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
      const base = `/p/${projectKey}`;
      const section = (s: string) => path === `${base}/${s}` || path.startsWith(`${base}/${s}/`);
      const sections = ['dashboard', 'flaky', 'compare', 'rules', 'gates', 'notifications', 'settings'];
      items.push({ label: `${name}: launches`, href: base, icon: <IconListDetails size={18} />, active: path.startsWith(base) && !sections.some(section) });
      items.push({ label: 'Dashboard', href: `${base}/dashboard`, icon: <IconChartLine size={18} />, active: section('dashboard') });
      items.push({ label: 'Flaky tests', href: `${base}/flaky`, icon: <IconWaveSine size={18} />, active: section('flaky') });
      items.push({ label: 'Compare launches', href: `${base}/compare`, icon: <IconGitCompare size={18} />, active: section('compare') });
      items.push({ label: 'Rules', href: `${base}/rules`, icon: <IconWand size={18} />, active: section('rules') });
      items.push({ label: 'Quality gates', href: `${base}/gates`, icon: <IconShieldCheck size={18} />, active: section('gates') });
      items.push({ label: 'Notifications', href: `${base}/notifications`, icon: <IconBell size={18} />, active: section('notifications') });
      items.push({ label: 'Project settings', href: `${base}/settings`, icon: <IconAdjustments size={18} />, active: section('settings') });
    }
    items.push({ label: 'API tokens', href: '/settings/tokens', icon: <IconKey size={18} />, active: path === '/settings/tokens' });
    items.push({ label: 'Audit log', href: '/settings/audit', icon: <IconShieldSearch size={18} />, active: path === '/settings/audit', permission: 'audit:read' });
    items.push({ label: 'Scheduler jobs', href: '/admin/jobs', icon: <IconClockCog size={18} />, active: path === '/admin/jobs', permission: 'settings:write' });
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
