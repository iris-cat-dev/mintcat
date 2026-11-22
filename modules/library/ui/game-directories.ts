import type { GameInstallationCandidate } from '@mintcat/sdk-ts';
import type { Disposable, UiContext } from '@mintcat/sdk-ui';
import { getSharedRuntime, mountReactView, pickLocalPath } from '@mintcat/sdk-ui';
import type { ChangeEvent, ReactNode } from 'react';
import { librarySession, loadAvailableGames, saveTarget, selectedTarget } from './game-inventory';
import type { AvailableGame } from './game-inventory';

export async function mountGameDirectories(root: HTMLElement, context: UiContext, guided = false): Promise<Disposable> {
  const initialGames = await loadAvailableGames(context);
  const { React, Antd, Icons } = getSharedRuntime();
  const h = React.createElement;
  const row = (title: string, control: ReactNode) => h('label', { className: 'game-settings-field' },
    h('span', { className: 'game-settings-field-title' }, title), control);
  const option = (value: string, label: string) => h('option', { value, key: value }, label);

  function DirectoryForm({ game, refresh }: { game: AvailableGame; refresh(): Promise<void> }) {
    const [rootPath, setRootPath] = React.useState('');
    const [candidates, setCandidates] = React.useState<GameInstallationCandidate[]>();
    const [busy, setBusy] = React.useState(false);
    const [detecting, setDetecting] = React.useState(false);
    const manuallyEdited = React.useRef(false);
    const [, redraw] = React.useReducer(value => value + 1, 0);
    const installations = guided
      ? game.installations.filter(installation => installation.state === 'configured' && installation.rootPath)
      : game.installations;
    const target = selectedTarget(context, { ...game, installations });
    React.useEffect(() => {
      if (!guided || !game.variants.length || installations.length) return;
      let stale = false;
      setDetecting(true);
      void context.call('games.discover', { gameId: game.gameId, includeSteam: true, roots: [] })
        .then(result => {
          if (stale || context.signal.aborted) return;
          setCandidates(result.items);
          if (result.items.length === 1 && !manuallyEdited.current) {
            const [candidate] = result.items;
            saveTarget(context, game.gameId, { ...target, variantId: candidate.variantId });
            setRootPath(candidate.rootPath);
          }
        }).catch(() => {
          if (!stale && !context.signal.aborted) setCandidates([]);
        }).finally(() => {
          if (!stale && !context.signal.aborted) setDetecting(false);
        });
      return () => { stale = true; };
    }, [game.gameId, game.variants.length, installations.length]);
    const choose = (patch: Partial<typeof target>): void => {
      saveTarget(context, game.gameId, { ...target, ...patch }); redraw();
    };
    const run = (action: () => Promise<void>) => context.guard(async () => {
      setBusy(true);
      try { await action(); } finally { setBusy(false); }
    });
    const discover = run(async () => {
      const path = rootPath.trim();
      const result = await context.call('games.discover', { gameId: game.gameId, includeSteam: true,
        roots: path ? [{ rootPath: path, variantId: target.variantId }] : [] });
      setCandidates(result.items);
    });
    const register = run(async () => {
      const { installation } = await context.call('games.register', {
        gameId: game.gameId, variantId: target.variantId, rootPath: rootPath.trim(),
      }, crypto.randomUUID());
      if (context.signal.aborted) return;
      if (!installation || typeof installation !== 'object' || !('installationId' in installation) || typeof installation.installationId !== 'string') {
        throw new Error('注册结果缺少安装目录标识，请重新打开游戏安装目录。');
      }
      saveTarget(context, game.gameId, { ...target, installationId: installation.installationId });
      await refresh(); setRootPath(''); setCandidates(undefined); context.notify('安装目录已注册并选中。', '游戏安装目录已注册');
    });
    return h(React.Fragment, null,
      h('section', { className: 'settings-group-card' },
        h('h3', { className: 'settings-group-title' }, '模组管理使用的目标'),
        row('游戏版本变体', h('select', { className: 'game-settings-control', value: target.variantId, disabled: busy || !game.variants.length, 'aria-label': '游戏版本',
          onChange: (e: ChangeEvent<HTMLSelectElement>) => { manuallyEdited.current = true; choose({ variantId: e.target.value }); setCandidates(undefined); } },
        game.variants.length ? game.variants.map(v => option(v.variantId, v.displayName)) : option('', '无可用版本'))),
        row('已注册安装目录', h('select', { className: 'game-settings-control', value: target.installationId, disabled: busy || !installations.length, 'aria-label': '已注册安装目录',
          onChange: (e: ChangeEvent<HTMLSelectElement>) => choose({ installationId: e.target.value }) },
        installations.length ? installations.map(i => option(i.installationId, i.rootPath ?? i.installationId)) : option('', '尚未注册安装目录')))),
      h('section', { className: 'settings-group-card' },
        h('div', { className: 'settings-group-head' }, h('div', null,
          h('h3', { className: 'settings-group-title' }, '发现与注册'),
          h('p', { className: 'settings-group-subtitle' }, game.variants.length ? '自动查找 Steam 安装目录，或指定本地游戏目录。' : '请先安装或启用此游戏的支持插件，再发现或注册目录。'))),
        game.variants.length ? h(Antd.Space, { orientation: 'vertical', style: { width: '100%' } },
          h(Antd.Space.Compact, { style: { width: '100%' } },
            h(Antd.Input, { value: rootPath, disabled: busy, 'aria-label': '安装目录', placeholder: '游戏安装目录的绝对路径',
              onChange: (e: ChangeEvent<HTMLInputElement>) => { manuallyEdited.current = true; setRootPath(e.target.value); } }),
            h(Antd.Button, { icon: h(Icons.FolderOpenOutlined), disabled: busy, onClick: run(async () => {
              const picked = await pickLocalPath({ kind: 'directory', title: '选择游戏安装目录' });
              if (picked) { manuallyEdited.current = true; setRootPath(picked); }
            }) }, '浏览目录')),
          detecting ? h('p', { className: 'settings-group-subtitle', role: 'status' }, '正在查找 Steam 安装目录…')
            : guided && candidates?.length === 1 && candidates[0].rootPath === rootPath
              ? h('p', { className: 'settings-group-subtitle', role: 'status' }, '已自动填入 Steam 安装目录。')
              : candidates?.length ? row('发现的安装目录', h('select', { className: 'game-settings-control', 'aria-label': '发现的安装目录', disabled: busy,
            value: candidates.findIndex(c => c.rootPath === rootPath && c.variantId === target.variantId),
            onChange: (e: ChangeEvent<HTMLSelectElement>) => { const candidate = candidates[Number(e.target.value)]; choose({ variantId: candidate.variantId }); setRootPath(candidate.rootPath); } },
          h('option', { value: -1, disabled: true }, '选择发现的安装目录'),
          ...candidates.map((c, i) => option(String(i), `${c.rootPath} · ${game.variants.find(v => v.variantId === c.variantId)?.displayName ?? c.variantId}`))))
            : candidates ? h('p', { className: 'settings-group-subtitle', role: 'status' }, '未发现安装目录，请浏览或填写游戏目录。') : null,
          h(Antd.Space, { wrap: true }, h(Antd.Button, { disabled: busy || detecting, onClick: discover }, '发现安装目录'),
            h(Antd.Button, { type: 'primary', disabled: busy || !rootPath.trim(), onClick: register }, '验证并注册'))) : null));
  }

  function DirectorySettings() {
    const [games, setGames] = React.useState(initialGames);
    const [gameId, setGameId] = React.useState(() => librarySession(context).gameId);
    const game = games.find(item => item.gameId === gameId) ?? games[0];
    const generation = React.useRef(0);
    const refresh = React.useCallback(async () => {
      const current = ++generation.current;
      const next = await loadAvailableGames(context);
      if (current === generation.current) setGames(next);
    }, []);
    React.useEffect(() => {
      const sub = context.onInventoryChanged(refresh);
      return () => sub.dispose();
    }, [refresh]);
    return h('div', { className: 'settings-page-shell' },
      h('div', { className: 'settings-section-header' }, h('h2', { className: 'settings-section-title' }, '游戏安装目录')),
      game ? h(React.Fragment, null,
        h('section', { className: 'settings-group-card settings-row-list' }, row('游戏', h('select', {
          className: 'game-settings-control', 'aria-label': '游戏', value: game.gameId, onChange: (e: ChangeEvent<HTMLSelectElement>) => {
            const gameId = e.target.value;
            context.saveState('library', 1, { ...librarySession(context), gameId }); setGameId(gameId);
          },
        }, ...games.map(item => option(item.gameId, item.displayName))))),
        h(DirectoryForm, { key: game.gameId, game, refresh }))
        : h('section', { className: 'settings-group-card' }, h('h3', { className: 'settings-group-title' }, '尚未安装游戏支持插件'),
          h('p', { className: 'settings-section-desc' }, '安装并启用游戏支持插件后，即可发现或注册安装目录。')));
  }
  return mountReactView(root, h(DirectorySettings));
}
