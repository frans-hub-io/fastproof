import { Anchor, Breadcrumbs, Group, Stack, Text, Title } from '@mantine/core';
import type { ReactNode } from 'react';
import { Link } from 'react-router';

export interface Crumb {
  label: string;
  to?: string;
}

export function PageHeader({ crumbs, title, subtitle, actions, badges }: { crumbs?: Crumb[]; title: ReactNode; subtitle?: ReactNode; actions?: ReactNode; badges?: ReactNode }) {
  return (
    <Stack gap={6} mb="md">
      {crumbs && crumbs.length > 0 && (
        <Breadcrumbs separator="/" separatorMargin={6} styles={{ root: { flexWrap: 'wrap' } }}>
          {crumbs.map((c) =>
            c.to ? (
              <Anchor key={c.label} component={Link} to={c.to} size="sm">
                {c.label}
              </Anchor>
            ) : (
              <Text key={c.label} size="sm" c="dimmed">
                {c.label}
              </Text>
            ),
          )}
        </Breadcrumbs>
      )}
      <Group justify="space-between" align="flex-start" gap="sm" wrap="wrap">
        <Stack gap={4} style={{ minWidth: 0, flex: '1 1 320px' }}>
          <Group gap="sm" wrap="wrap" align="center">
            <Title order={2} fz={{ base: 22, sm: 26 }} style={{ wordBreak: 'break-word' }}>
              {title}
            </Title>
            {badges}
          </Group>
          {subtitle && (
            <Text size="sm" c="dimmed" component="div">
              {subtitle}
            </Text>
          )}
        </Stack>
        {actions && (
          <Group gap="xs" wrap="wrap">
            {actions}
          </Group>
        )}
      </Group>
    </Stack>
  );
}
