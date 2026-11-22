import type { PointerEvent, ReactElement } from 'react';
import { getSharedRuntime } from '@mintcat/sdk-ui';
import './library-atlas.css';

type Scene = { gameId: string; displayName: string };
export interface GameChange { from: Scene; to: string; revealing: boolean }

export function GameBanner({ game, change }: { game?: Scene; change?: GameChange }): ReactElement {
  const { React } = getSharedRuntime();
  const h = React.createElement;
  const switching = change && (game?.gameId === change.to || game?.gameId === change.from.gameId) ? change : undefined;
  const scene = switching && !switching.revealing ? switching.from : game;
  const outgoing = switching?.revealing && scene?.gameId === switching.to ? switching.from : undefined;
  function plate(item: Scene | undefined, leaving = false): ReactElement {
    const drg = item?.gameId === 'game:drg';
    return h(leaving ? 'div' : 'header', { key: item?.gameId ?? 'empty', 'aria-hidden': leaving || undefined,
      className: `library-atlas${drg ? ' library-atlas--drg' : ''}${leaving ? ' atlas-leaving' : ''}` },
      h('span', { className: 'library-atlas-art', 'aria-hidden': true }),
      h('div', { className: 'library-survey', 'aria-hidden': true },
        h('span', { className: 'survey-dust' }), h('span', { className: 'survey-dust survey-dust--far' }), h('span', { className: 'survey-lamp' })),
      h('span', { className: 'library-atlas-kicker', 'aria-hidden': true }, drg ? 'HOXXES IV' : 'MINERAL ATLAS'),
      h('h2', { title: item?.displayName }, item?.displayName ?? '选择游戏'),
      drg ? null : h('span', { className: 'library-atlas-caption', 'aria-hidden': true }, 'FIG. 01  /  QUARTZ'));
  }
  return h('div', { className: `atlas-stage${outgoing ? ' atlas-stage--switch' : ''}`,
    onPointerMove: (event: PointerEvent<HTMLElement>) => {
      const stage = event.currentTarget, box = stage.getBoundingClientRect();
      stage.style.setProperty('--lamp-x', `${event.clientX - box.left}px`);
      stage.style.setProperty('--lamp-y', `${event.clientY - box.top}px`);
    } }, plate(scene), outgoing && plate(outgoing, true));
}
