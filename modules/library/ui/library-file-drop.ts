import type { HTMLAttributes, ReactNode, RefObject } from 'react';
import { getSharedRuntime, subscribeFileDrag, tr } from '@mintcat/sdk-ui';
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
  banner: ReactNode;
  tools: ReactNode;
  scrollKey: string;
}

export function FileListArea({ targetRef, scanning, newRowId, banner, tools, scrollKey, children, ...areaProps }: FileListProps) {
  const { React } = getSharedRuntime();
  const scrollRef = React.useRef<HTMLDivElement>(null);
  const toolsRef = React.useRef<HTMLDivElement>(null);
  React.useLayoutEffect(() => {
    const scroll = scrollRef.current, controls = toolsRef.current;
    if (!scroll || !controls) return;
    const measure = () => scroll.style.setProperty('--library-tools-height', `${controls.getBoundingClientRect().height}px`);
    measure();
    const observer = new ResizeObserver(measure);
    observer.observe(controls);
    return () => observer.disconnect();
  }, []);
  React.useLayoutEffect(() => {
    if (scrollRef.current) scrollRef.current.scrollTop = 0;
  }, [scrollKey]);
  React.useLayoutEffect(() => {
    if (!newRowId) return;
    const area = targetRef.current;
    const row = area?.querySelector<HTMLElement>('tr[data-import-new]');
    if (!area || !row) return; // Honor the user's current filters and collapsed groups.
    const scroll = scrollRef.current;
    if (!scroll) return;
    const bounds = scroll.getBoundingClientRect(), item = row.getBoundingClientRect();
    const top = Math.max(bounds.top, toolsRef.current?.getBoundingClientRect().bottom ?? bounds.top)
      + (area.querySelector('thead')?.clientHeight ?? 0);
    if (item.top < top) scroll.scrollTop += item.top - top;
    else if (item.bottom > bounds.bottom) scroll.scrollTop += item.bottom - bounds.bottom;
  }, [newRowId, targetRef]);
  return React.createElement('div', { className: 'library-file-stage', 'data-file-scan': scanning || undefined },
    React.createElement('div', { className: 'library-mod-scroll', ref: scrollRef },
      React.createElement('div', { className: 'library-scroll-banner' }, banner),
      React.createElement('div', { className: 'library-scroll-tools', ref: toolsRef }, tools),
      React.createElement('div', { ...areaProps, ref: targetRef }, children)),
    scanning ? React.createElement('span', { className: 'library-file-caption', role: 'status' }, tr('松开以导入本地模组', 'Drop to import local mods')) : null);
}
