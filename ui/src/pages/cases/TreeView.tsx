import { Loader, Text } from '@mantine/core';
import { IconCheck, IconChevronDown, IconChevronRight, IconMinus } from '@tabler/icons-react';
import { useEffect, useRef, type DragEvent, type KeyboardEvent, type ReactNode } from 'react';

export interface TreeRow {
  id: string;
  level: number;
  /** Plain text for type-ahead; the accessible name comes from the visible name (counts are aria-hidden). */
  text: string;
  hasChildren: boolean;
  expanded: boolean;
  loading?: boolean;
  checked?: boolean | 'mixed';
  setSize: number;
  posInSet: number;
  /** Shown after the name (counts, badges). */
  extra?: ReactNode;
  icon?: ReactNode;
  testId?: string;
  /** The row can be dragged (with `onDragStartRow`). */
  draggable?: boolean;
}

export interface TreeViewProps {
  label: string;
  rows: TreeRow[];
  activeId: string | null;
  selectedId?: string | null;
  onActiveChange: (id: string) => void;
  onToggle: (id: string, expand: boolean) => void;
  onActivate?: (id: string) => void;
  /** With checkboxes: Space toggles the row's check state. */
  onCheck?: (id: string) => void;
  onDropOn?: (id: string, e: DragEvent<HTMLDivElement>) => void;
  onDragStartRow?: (id: string, e: DragEvent<HTMLDivElement>) => void;
  acceptsDrop?: (e: DragEvent<HTMLDivElement>) => boolean;
  testId?: string;
  empty?: ReactNode;
}

/**
 * An ARIA treeview over a flat list of visible rows (level, set size and position carry the hierarchy) with a
 * roving tabindex: one row is in the tab order, arrow keys move it, Right/Left expand and collapse, Home/End jump,
 * typing a letter finds the next row starting with it, Enter activates and Space checks.
 */
export function TreeView({ label, rows, activeId, selectedId, onActiveChange, onToggle, onActivate, onCheck, onDropOn, onDragStartRow, acceptsDrop, testId, empty }: TreeViewProps) {
  const ref = useRef<HTMLDivElement>(null);
  const focusWanted = useRef(false);
  const current = rows.find((r) => r.id === activeId) ?? rows[0];

  useEffect(() => {
    if (!focusWanted.current || !current) return;
    focusWanted.current = false;
    const el = ref.current?.querySelector<HTMLElement>(`[data-tree-id="${CSS.escape(current.id)}"]`);
    el?.focus();
    el?.scrollIntoView({ block: 'nearest' });
  }, [current]);

  const move = (id: string | undefined) => {
    if (!id) return;
    focusWanted.current = true;
    onActiveChange(id);
  };

  const onKeyDown = (e: KeyboardEvent<HTMLDivElement>) => {
    if (!current || e.altKey || e.ctrlKey || e.metaKey) return;
    const i = rows.findIndex((r) => r.id === current.id);
    const parentOf = (k: number) => {
      const lvl = rows[k]?.level ?? 0;
      for (let j = k - 1; j >= 0; j--) if ((rows[j]?.level ?? 0) < lvl) return rows[j];
      return undefined;
    };
    switch (e.key) {
      case 'ArrowDown':
        e.preventDefault();
        move(rows[Math.min(rows.length - 1, i + 1)]?.id);
        break;
      case 'ArrowUp':
        e.preventDefault();
        move(rows[Math.max(0, i - 1)]?.id);
        break;
      case 'Home':
        e.preventDefault();
        move(rows[0]?.id);
        break;
      case 'End':
        e.preventDefault();
        move(rows[rows.length - 1]?.id);
        break;
      case 'ArrowRight':
        e.preventDefault();
        if (current.hasChildren && !current.expanded) onToggle(current.id, true);
        else if (current.hasChildren && current.expanded) move(rows[i + 1]?.id);
        break;
      case 'ArrowLeft':
        e.preventDefault();
        if (current.hasChildren && current.expanded) onToggle(current.id, false);
        else move(parentOf(i)?.id);
        break;
      case 'Enter':
        e.preventDefault();
        onActivate?.(current.id);
        break;
      case ' ':
        e.preventDefault();
        if (onCheck) onCheck(current.id);
        else onActivate?.(current.id);
        break;
      case '*': {
        e.preventDefault();
        const lvl = current.level;
        for (const r of rows) if (r.level === lvl && r.hasChildren && !r.expanded) onToggle(r.id, true);
        break;
      }
      default:
        if (e.key.length === 1 && /\S/.test(e.key)) {
          const k = e.key.toLowerCase();
          const order = [...rows.slice(i + 1), ...rows.slice(0, i + 1)];
          const hit = order.find((r) => r.text.toLowerCase().startsWith(k));
          if (hit) move(hit.id);
        }
    }
  };

  if (!rows.length) return <div data-testid={testId}>{empty}</div>;

  return (
    <div role="tree" aria-label={label} ref={ref} onKeyDown={onKeyDown} className="fp-tm-tree" data-testid={testId} aria-multiselectable={onCheck ? true : undefined}>
      {rows.map((r) => {
        const active = r.id === current?.id;
        return (
          <div
            key={r.id}
            role="treeitem"
            aria-level={r.level}
            aria-setsize={r.setSize}
            aria-posinset={r.posInSet}
            aria-expanded={r.hasChildren ? r.expanded : undefined}
            aria-selected={onCheck ? r.checked === true : r.id === selectedId}
            aria-checked={onCheck ? (r.checked === 'mixed' ? 'mixed' : !!r.checked) : undefined}
            tabIndex={active ? 0 : -1}
            data-tree-id={r.id}
            data-testid={r.testId ?? 'tree-row'}
            data-selected={r.id === selectedId}
            data-active={active}
            className="fp-tm-tree-row"
            style={{ paddingLeft: 6 + (r.level - 1) * 16 }}
            onClick={() => {
              onActiveChange(r.id);
              if (onCheck) onCheck(r.id);
              else onActivate?.(r.id);
            }}
            onFocus={() => {
              if (!active) onActiveChange(r.id);
            }}
            draggable={r.draggable && !!onDragStartRow ? true : undefined}
            onDragStart={r.draggable && onDragStartRow ? (e) => onDragStartRow(r.id, e) : undefined}
            onDragOver={(e) => {
              if (onDropOn && (!acceptsDrop || acceptsDrop(e))) {
                e.preventDefault();
                e.currentTarget.dataset.dropping = 'true';
              }
            }}
            onDragLeave={(e) => {
              delete e.currentTarget.dataset.dropping;
            }}
            onDrop={(e) => {
              delete e.currentTarget.dataset.dropping;
              if (!onDropOn) return;
              e.preventDefault();
              onDropOn(r.id, e);
            }}
          >
            <span
              className="fp-tm-tree-toggle"
              aria-hidden
              onClick={(e) => {
                if (!r.hasChildren) return;
                e.stopPropagation();
                onActiveChange(r.id);
                onToggle(r.id, !r.expanded);
              }}
              style={{ visibility: r.hasChildren ? 'visible' : 'hidden' }}
            >
              {r.loading ? <Loader size={10} /> : r.expanded ? <IconChevronDown size={14} /> : <IconChevronRight size={14} />}
            </span>
            {onCheck && (
              <span className="fp-tm-check" data-state={r.checked === 'mixed' ? 'mixed' : r.checked ? 'on' : 'off'} aria-hidden>
                {r.checked === 'mixed' ? <IconMinus size={11} /> : r.checked ? <IconCheck size={11} /> : null}
              </span>
            )}
            {r.icon}
            <Text size="sm" className="fh-truncate" style={{ flex: 1, minWidth: 0 }} component="span">
              {r.text}
            </Text>
            {r.extra}
          </div>
        );
      })}
    </div>
  );
}
