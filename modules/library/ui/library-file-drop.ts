import type { HTMLAttributes, RefObject } from 'react';
import { getSharedRuntime, subscribeFileDrag } from '@mintcat/sdk-ui';
import './library-file-drop.css';

type Target = 'list' | 'modal';

export function useLibraryFileDrop(enabled: boolean, modalOpen: boolean, onDrop: (paths: string[], target: Target) => void) {
  const { React } = getSharedRuntime();
  const listRef = React.useRef<HTMLDivElement>(null), modalRef = React.useRef<HTMLDivElement>(null);
  const [hover, setHover] = React.useState<Target>();
  const drop = React.useRef(onDrop);
  React.useLayoutEffect(() => { drop.current = onDrop; });
  React.useEffect(() => {
    setHover(undefined);
    if (!enabled) return;
    return subscribeFileDrag(event => {
      if (event.type === 'leave') { setHover(undefined); return; }
      const element = document.elementFromPoint(event.x, event.y);
      const target = modalOpen ? (modalRef.current?.contains(element) ? 'modal' : undefined)
        : listRef.current?.contains(element) ? 'list' : undefined;
      if (event.type === 'drop') {
        setHover(undefined);
        if (target) drop.current(event.paths, target);
      } else setHover(target);
    });
  }, [enabled, modalOpen]);
  return { listRef, modalRef, hover };
}

interface FileListProps extends HTMLAttributes<HTMLDivElement> {
  targetRef: RefObject<HTMLDivElement | null>;
  scanning: boolean;
  newRowId?: string;
}

export function FileListArea({ targetRef, scanning, newRowId, children, ...areaProps }: FileListProps) {
  const { React } = getSharedRuntime();
  React.useLayoutEffect(() => {
    if (!newRowId) return;
    const area = targetRef.current;
    const row = area?.querySelector<HTMLElement>('tr[data-import-new]');
    if (!area || !row) return; // Honor the user's current filters and collapsed groups.
    const bounds = area.getBoundingClientRect(), item = row.getBoundingClientRect();
    const top = bounds.top + (area.querySelector('thead')?.clientHeight ?? 0);
    if (item.top < top) area.scrollTop += item.top - top;
    else if (item.bottom > bounds.bottom) area.scrollTop += item.bottom - bounds.bottom;
  }, [newRowId, targetRef]);
  return React.createElement('div', { className: 'library-file-stage', 'data-file-scan': scanning || undefined },
    React.createElement('div', { ...areaProps, ref: targetRef }, children),
    scanning ? React.createElement('span', { className: 'library-file-caption', role: 'status' }, '松开以导入本地模组') : null);
}
