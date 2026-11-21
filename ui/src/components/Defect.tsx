import { Badge, Tooltip } from '@mantine/core';
import { IconRobot, IconWand } from '@tabler/icons-react';
import type { DefectInfo, DefectType } from '../api/types-next';
import { defectColor, defectLabel, type DefectRef } from '../lib/defects';
import { formatDateTime } from '../lib/format';

export function DefectChip({ defect, types, size = 'sm' }: { defect: DefectRef & Partial<Pick<DefectInfo, 'source' | 'by' | 'at'>>; types?: Map<string, DefectType>; size?: 'xs' | 'sm' | 'md' }) {
  const color = defectColor(defect, types);
  const label = defectLabel(defect, types);
  const how = defect.source === 'rule' ? `by rule${defect.by?.startsWith('rule:') ? ` ${defect.by.slice(5)}` : ''}` : defect.source === 'ai' ? 'suggested by AI' : defect.by ? `by ${defect.by}` : '';
  const tip = [how, defect.at ? formatDateTime(defect.at) : ''].filter(Boolean).join(' · ');
  const chip = (
    <Badge
      size={size}
      radius="sm"
      variant="light"
      data-testid="defect-chip"
      data-defect={defect.typeId ?? defect.group ?? ''}
      leftSection={defect.source === 'rule' ? <IconWand size={11} /> : defect.source === 'ai' ? <IconRobot size={11} /> : <span aria-hidden style={{ width: 8, height: 8, borderRadius: 2, background: color, display: 'inline-block' }} />}
      styles={{ root: { color, background: `color-mix(in srgb, ${color} 16%, transparent)`, textTransform: 'none', flex: 'none' } }}
    >
      {label}
    </Badge>
  );
  return tip ? <Tooltip label={tip}>{chip}</Tooltip> : chip;
}
