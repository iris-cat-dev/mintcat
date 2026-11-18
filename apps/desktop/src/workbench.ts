import { COMMAND_SPECS } from '@mintcat/sdk-ts';
import type { CommandName, CommandParams, CommandResult } from '@mintcat/sdk-ts';
import { Disposables, QueryCache, SessionStore, getSharedRuntime, kit, mountGameSettings, mountReactView, presentUiError, renderUiError } from '@mintcat/sdk-ui';
import type { Disposable, GameModuleList, PageMount, UiContext, UiNavigationIcon } from '@mintcat/sdk-ui';
import { mountLibrary, mountLocalImport, mountDownloads } from '../../../modules/library/ui';
import { mountSearch } from '../../../modules/ui-search/ui';
import { mountInstall } from '../../../modules/install/ui';
import { mountBackupPage } from '../../../modules/feature-backup/ui';
import { mountAgentPage } from '../../../modules/feature-agent/ui';
import '../../../modules/library/ui/library.css';
import '../../../modules/ui-search/ui/styles.css';
import '../../../modules/install/ui/styles.css';
import '../../../modules/feature-backup/ui/styles.css';
import '../../../modules/feature-agent/ui/styles.css';
import { requestApplication } from './daemon';
import { mountAppearanceSettings, mountRuntimeSettings } from './settings-overview';

interface Page {
  moduleId: string; route: string; title: string; subtitle?: string; icon: UiNavigationIcon;
  placement: 'sidebar' | 'titlebar' | 'settings'; settingsGroup?: 'system' | 'game'; mount: PageMount;
}
interface MountedPage { page: Page; disposable?: Disposable; controller: AbortController; lifetime: Disposables; dirty: boolean }

const builtinPages: readonly Page[] = [
  { moduleId: 'mintcat.library', route: '/mintcat.library/library', title: '模组管理', icon: 'home', placement: 'sidebar', mount: mountLibrary },
  { moduleId: 'mintcat.ui.search', route: '/mintcat.ui.search/search', title: '在线模组', icon: 'search', placement: 'sidebar', mount: mountSearch },
  { moduleId: 'mintcat.library', route: '/mintcat.library/local', title: '本地导入', icon: 'local', placement: 'sidebar', mount: mountLocalImport },
  { moduleId: 'mintcat.library', route: '/mintcat.library/downloads', title: '下载管理', icon: 'downloads', placement: 'sidebar', mount: mountDownloads },
  { moduleId: 'mintcat.feature.agent', route: '/mintcat.feature.agent/agent', title: 'Agent 助手', icon: 'robot', placement: 'titlebar', mount: mountAgentPage },
  { moduleId: 'mintcat.install', route: '/mintcat.install/tasks', title: '安装与打包', icon: 'install', placement: 'titlebar', mount: mountInstall },
  {
    moduleId: 'mintcat.desktop.appearance',
    route: '/mintcat.desktop/settings',
    title: '外观偏好',
    icon: 'settings',
    placement: 'settings',
    settingsGroup: 'system',
    mount: root => mountAppearanceSettings(root),
  },
  {
    moduleId: 'mintcat.feature.backup',
    route: '/mintcat.feature.backup/backups',
    title: '备份与恢复',
    icon: 'cloud',
    placement: 'settings',
    settingsGroup: 'system',
    mount: mountBackupPage,
  },
  {
    moduleId: 'mintcat.desktop.runtime',
    route: '/mintcat.desktop/runtime',
    title: '扩展与服务',
    icon: 'proxy',
    placement: 'settings',
    settingsGroup: 'system',
    mount: (root, context) => mountRuntimeSettings(
      root,
      context,
      route => { window.location.hash = `#${route}`; },
      () => { window.dispatchEvent(new CustomEvent('mintcat-open-rescue')); },
    ),
  },
];

export class Workbench implements Disposable {
  private readonly session: SessionStore;
  private readonly queries = new QueryCache();
  private readonly inventoryEvents = new EventTarget();
  private readonly navigation = kit.element('nav');
  private readonly content = kit.element('section');
  private readonly workspace = kit.element('div');
  private readonly bodyRow = kit.element('div');
  private readonly settingsTabs = kit.element('aside');
  private readonly auxiliaryRoot = kit.element('section');
  private readonly gamesPanel = kit.element('details');
  private readonly notice = kit.element('div');
  private sidebarView?: Disposable;
  private settingsView?: Disposable;
  private mounted?: MountedPage;
  private auxiliary?: MountedPage;
  private pages: readonly Page[] = builtinPages;
  private inventory?: GameModuleList;
  private route: string;
  private closed = false;
  private pageOpened = false;
  private refreshing?: Promise<void>;
  private readonly inventoryTimer: number;
  private readonly hashHandler = (): void => { void this.navigate(location.hash.slice(1)); };
  private readonly beforeUnload = (event: BeforeUnloadEvent): void => {
    if (this.mounted?.dirty || this.auxiliary?.dirty) { event.preventDefault(); event.returnValue = ''; }
  };
  private readonly renderErrorHandler = (event: Event): void => {
    const { root, error } = (event as CustomEvent<{ root: HTMLElement; error: Error }>).detail;
    if (this.closed) return;
    if (this.mounted && this.content.contains(root)) this.pageError(error);
    else if (this.auxiliary && this.auxiliaryRoot.contains(root)) { this.unmountAuxiliary(); this.showFailure(error); }
  };
  private readonly navigationHandler = (event: Event): void => {
    const icon = (event as CustomEvent<UiNavigationIcon>).detail;
    const page = icon === 'settings' ? this.settingsPages()[0] : this.pages.find(page => page.icon === icon);
    if (!page) return;
    if (icon === 'robot') void this.openAuxiliary(page);
    else void this.navigate(page.route);
  };

  constructor(private root: HTMLElement, private dataDir: string, private rescue: (error: unknown) => void, gamesContainer?: HTMLElement, navigationRoot?: HTMLElement, private toolbarRoot?: HTMLElement) {
    this.session = new SessionStore(window.localStorage, `mintcat.workbench.session.v1/${encodeURIComponent(dataDir)}`);
    this.route = this.session.get('workbench/route', 1, '');
    this.navigation.ariaLabel = '工作台导航'; this.navigation.className = 'app-sidebar';
    this.workspace.className = 'workbench-content';
    this.bodyRow.className = 'workbench-body';
    this.settingsTabs.className = 'settings-sidebar';
    this.settingsTabs.ariaLabel = '设置分类导航';
    this.content.className = 'module-page'; this.auxiliaryRoot.className = 'module-panel-host';
    this.notice.role = 'status'; this.notice.className = 'module-notice';
    this.gamesPanel.className = 'game-plugins';
    this.bodyRow.append(this.settingsTabs, this.content);
    this.workspace.append(this.notice, this.bodyRow);
    (navigationRoot ?? root).append(this.navigation);
    root.append(this.workspace, this.auxiliaryRoot);
    (gamesContainer ?? this.workspace).append(this.gamesPanel);
    window.addEventListener('hashchange', this.hashHandler);
    window.addEventListener('beforeunload', this.beforeUnload);
    window.addEventListener('mintcat-ui-navigate', this.navigationHandler);
    window.addEventListener('mintcat-ui-render-error', this.renderErrorHandler);
    this.inventoryTimer = window.setInterval(() => { void this.refresh(); }, 2_000);
  }

  refresh(): Promise<void> {
    if (this.closed) return Promise.resolve();
    if (this.refreshing) return this.refreshing;
    const pending = this.refreshInventory().finally(() => { if (this.refreshing === pending) this.refreshing = undefined; });
    this.refreshing = pending;
    return pending;
  }
  private async refreshInventory(): Promise<void> {
    try {
      const inventory = await requestApplication<GameModuleList>(this.dataDir, 'modules.list', {});
      if (this.closed) return;
      const changed = JSON.stringify(this.inventory?.modules) !== JSON.stringify(inventory.modules);
      this.inventory = inventory;
      if (changed) {
        const games = inventory.modules.flatMap(module => module.enabled && module.game?.descriptor ? [{ moduleId: module.moduleId, game: module.game }] : []);
        this.pages = [...builtinPages, ...games.map(({ moduleId, game }): Page => ({
          moduleId, route: `/${moduleId}/settings`, title: `${game.displayName}`, subtitle: '游戏运行参数', icon: 'settings', placement: 'settings', settingsGroup: 'game',
          mount: (root, context) => mountGameSettings(root, context, game.gameId),
        }))];
        this.gamesPanel.replaceChildren(kit.element('summary', '游戏插件'));
        for (const module of inventory.modules) {
          if (!module.game) continue;
          const row = kit.element('div'); row.className = 'game-plugin-row';
          const name = kit.element('span', module.game.displayName); name.className = 'game-plugin-name';
          const badge = kit.element('span', module.enabled ? '可用' : '未启用');
          badge.className = `game-plugin-badge ${module.enabled ? 'game-plugin-badge--enabled' : 'game-plugin-badge--disabled'}`;
          row.append(name, badge);
          if (module.digest) {
            const digest = kit.element('code', `${module.digest.slice(0, 19)}…`);
            digest.className = 'game-plugin-digest';
            digest.title = module.digest;
            row.append(digest);
          }
          this.gamesPanel.append(row);
        }
        if (!games.length) {
          const empty = kit.element('p', '尚未安装游戏支持插件；模组管理、备份和 Agent 仍可使用。');
          empty.className = 'game-plugin-empty';
          this.gamesPanel.append(empty);
        }
        this.renderSlots();
        this.inventoryEvents.dispatchEvent(new Event('inventory'));
      }
      if (!this.pageOpened) await this.navigate(location.hash.slice(1) || this.route, true);
      if (!this.closed && !this.notice.textContent) window.dispatchEvent(new CustomEvent('mintcat-ui-notice', { detail: '已连接独立后台' }));
    } catch (error) {
      if (!this.closed) { this.showFailure(error); if (!this.inventory) this.rescue(error); }
    }
  }
  private allowDiscard(page?: MountedPage): boolean {
    return !(page ? page.dirty : this.mounted?.dirty || this.auxiliary?.dirty) || window.confirm('离开未保存的页面？草稿将保留，后台任务继续运行。');
  }
  private context(mounted: MountedPage): UiContext {
    const { page, controller, lifetime } = mounted; const signal = controller.signal;
    const active = (): boolean => !signal.aborted && !this.closed;
    const assertActive = (): void => { if (!active()) throw new DOMException('Page closed', 'AbortError'); };
    const check = (method: string): string => {
      assertActive();
      const spec = COMMAND_SPECS.find(spec => spec.method === method);
      if (!spec) throw new Error(`Unknown Application command: ${method}`);
      return spec.classification;
    };
    const request = async <T>(method: string, params: object, key?: string): Promise<T> => {
      const classification = check(method);
      const result = await requestApplication<T>(this.dataDir, method, params, key);
      // Accepted mutations and their JobRefs survive page disposal. Reads do not.
      if (classification === 'read') assertActive();
      return result;
    };
    const workbench = this;
    return {
      moduleId: page.moduleId, signal, kit,
      get toolbar() { return active() && mounted === workbench.mounted ? workbench.toolbarRoot : undefined; },
      call: <M extends CommandName>(method: M, params: CommandParams[M], key?: string): Promise<CommandResult[M]> => request(method, params, key),
      request,
      query: async (key, load, ttl) => { assertActive(); const result = await this.queries.get(`${page.moduleId}/${key}`, load, ttl); assertActive(); return result; },
      invalidate: prefix => this.queries.invalidate(`${page.moduleId}/${prefix}`),
      state: (key, version, fallback) => this.session.get(`${page.moduleId}/${key}`, version, fallback),
      saveState: (key, version, value) => this.session.set(`${page.moduleId}/${key}`, version, value),
      dirty: value => { if (active()) mounted.dirty = value; },
      notify: text => { if (active()) this.showNotice(text); },
      guard: action => (...args) => { if (active()) Promise.resolve().then(() => { if (active()) return action(...args); }).catch(error => { if (active()) this.showFailure(error); }); },
      own: disposable => lifetime.add(disposable),
      onInventoryChanged: listener => {
        const handler = (): void => { if (active()) Promise.resolve().then(() => { if (active()) return listener(); }).catch(error => { if (active()) this.showFailure(error); }); };
        this.inventoryEvents.addEventListener('inventory', handler);
        return lifetime.add({ dispose: () => this.inventoryEvents.removeEventListener('inventory', handler) });
      },
      interval: (action, milliseconds) => {
        let pending = false;
        const timer = window.setInterval(() => {
          if (pending || !active()) return;
          pending = true;
          Promise.resolve().then(() => { if (active()) return action(); }).catch(error => { if (active()) this.showFailure(error); }).finally(() => { pending = false; });
        }, milliseconds);
        return lifetime.add({ dispose: () => clearInterval(timer) });
      },
    };
  }
  private showNotice(text: string): void {
    this.notice.dataset.tone = 'info';
    this.notice.classList.remove('mc-error-box');
    const content = kit.element('span', text);
    content.className = 'module-notice-text';
    const dismiss = kit.button('×', () => { this.notice.replaceChildren(); delete this.notice.dataset.tone; });
    dismiss.className = 'module-notice-dismiss';
    dismiss.ariaLabel = '关闭提示';
    this.notice.replaceChildren(content, dismiss);
    window.dispatchEvent(new CustomEvent('mintcat-ui-notice', { detail: text }));
  }
  private showFailure(cause: unknown): void {
    this.notice.dataset.tone = 'error';
    const error = presentUiError(cause);
    const body = kit.element('div');
    body.className = 'module-notice-error-body';
    renderUiError(body, error);
    const dismiss = kit.button('×', () => { this.notice.replaceChildren(); delete this.notice.dataset.tone; });
    dismiss.className = 'module-notice-dismiss';
    dismiss.ariaLabel = '关闭提示';
    this.notice.classList.remove('mc-error-box');
    this.notice.replaceChildren(body, dismiss);
    window.dispatchEvent(new CustomEvent('mintcat-ui-notice', { detail: `${error.title}：${error.description}` }));
  }
  private settingsPages(): Page[] { return this.pages.filter(page => page.placement === 'settings'); }
  private renderSlots(): void {
    if (this.closed) return;
    this.sidebarView?.dispose(); this.settingsView?.dispose();
    const { React, Antd, Icons } = getSharedRuntime(); const h = React.createElement;
    const icons = { home: Icons.AppstoreOutlined, search: Icons.CompassOutlined, settings: Icons.BgColorsOutlined, robot: Icons.RobotOutlined, cloud: Icons.CloudSyncOutlined, install: Icons.DeploymentUnitOutlined, proxy: Icons.ApiOutlined, local: Icons.ImportOutlined, downloads: Icons.CloudDownloadOutlined };
    const settings = this.settingsPages(), isSettings = settings.some(page => page.route === this.route);
    const currentPage = this.pages.find(page => page.route === this.route);
    const items = this.pages.filter(page => page.placement === 'sidebar').map(page => ({ key: page.route, className: 'sidebar-route-item', label: h('span', { 'aria-label': page.title, className: 'sidebar-link' }, h(icons[page.icon]), h('span', { className: 'sidebar-label' }, page.title)) }));
    items.push({ key: '@settings', className: 'sidebar-settings-item', label: h('span', { 'aria-label': '设置', className: 'sidebar-link' }, h(Icons.SettingOutlined), h('span', { className: 'sidebar-label' }, '设置')) });
    this.sidebarView = mountReactView(this.navigation, h(Antd.Menu, { mode: 'inline', selectedKeys: [isSettings ? '@settings' : this.route], items, onClick: ({ key }) => { void this.navigate(key === '@settings' ? isSettings ? this.route : settings[0].route : key); } }));
    this.settingsTabs.hidden = true;
    window.dispatchEvent(new CustomEvent('mintcat-ui-active-page', {
      detail: {
        route: this.route,
        title: currentPage?.title ?? '',
        placement: currentPage?.placement ?? 'sidebar',
        settingsTabs: settings.map(page => ({
          route: page.route,
          title: page.title,
          icon: page.settingsGroup === 'game' ? 'game' : page.icon,
        })),
      },
    }));
    window.dispatchEvent(new CustomEvent('mintcat-ui-contributions', { detail: { icons: [...new Set([...this.pages.map(page => page.icon), 'settings'])], games: this.inventory?.modules.flatMap(module => module.enabled && module.game ? [{ gameId: module.game.gameId, displayName: module.game.displayName }] : []) ?? [] } }));
  }
  private async openAuxiliary(page: Page): Promise<void> {
    if (this.closed || (this.auxiliary && !this.allowDiscard(this.auxiliary))) return;
    this.unmountAuxiliary();
    const mounted: MountedPage = { page, controller: new AbortController(), lifetime: new Disposables(), dirty: false };
    this.auxiliary = mounted;
    const pageRoot = kit.element('section'); pageRoot.dataset.module = page.moduleId; this.auxiliaryRoot.replaceChildren(pageRoot);
    try {
      const disposable = await page.mount(pageRoot, this.context(mounted));
      if (this.auxiliary !== mounted || mounted.controller.signal.aborted) disposable.dispose(); else mounted.disposable = disposable;
    } catch (error) { if (!mounted.controller.signal.aborted) { this.unmountAuxiliary(); this.showFailure(error); } }
  }
  private unmountAuxiliary(): void {
    const mounted = this.auxiliary; this.auxiliary = undefined;
    if (mounted) this.disposePage(mounted);
    this.auxiliaryRoot.replaceChildren();
  }
  async navigate(route: string, force = false): Promise<void> {
    if (this.closed) return;
    if (!force && route === this.route && this.mounted) return;
    if (!force && !this.allowDiscard()) { history.replaceState(null, '', `#${this.route}`); return; }
    this.unmountPage();
    const page = this.pages.find(page => page.route === route) ?? builtinPages[0];
    this.pageOpened = true;
    this.route = page.route; this.session.set('workbench/route', 1, this.route);
    this.renderSlots(); history.replaceState(null, '', `#${this.route}`);
    const pageRoot = kit.element('section'); pageRoot.dataset.module = page.moduleId; this.content.replaceChildren(pageRoot);
    const mounted: MountedPage = { page, controller: new AbortController(), lifetime: new Disposables(), dirty: false }; this.mounted = mounted;
    try {
      const disposable = await page.mount(pageRoot, this.context(mounted));
      if (this.mounted !== mounted || mounted.controller.signal.aborted) disposable.dispose(); else mounted.disposable = disposable;
    } catch (error) { if (!mounted.controller.signal.aborted) this.pageError(error); }
  }
  private pageError(error: unknown): void {
    this.unmountPage();
    const card = kit.element('div'); card.className = 'page-error-card';
    const heading = kit.element('h2', '页面加载失败'); heading.className = 'page-error-title';
    const failure = kit.element('div'); renderUiError(failure, presentUiError(error));
    const retry = kit.button('重新加载页面', () => { void this.navigate(this.route, true); });
    retry.className = 'page-error-retry';
    card.append(heading, failure, retry);
    this.content.replaceChildren(card);
  }
  private disposePage(mounted: MountedPage): void {
    mounted.controller.abort(); this.queries.releasePending(`${mounted.page.moduleId}/`);
    try { mounted.disposable?.dispose(); } catch (error) { this.showFailure(error); }
    mounted.lifetime.dispose();
  }
  private unmountPage(): void {
    this.unmountAuxiliary(); const mounted = this.mounted; this.mounted = undefined;
    if (mounted) this.disposePage(mounted);
  }
  dispose(): void {
    if (this.closed) return;
    this.closed = true; this.unmountPage(); clearInterval(this.inventoryTimer);
    window.removeEventListener('hashchange', this.hashHandler); window.removeEventListener('beforeunload', this.beforeUnload);
    window.removeEventListener('mintcat-ui-navigate', this.navigationHandler); window.removeEventListener('mintcat-ui-render-error', this.renderErrorHandler);
    this.sidebarView?.dispose(); this.settingsView?.dispose(); this.gamesPanel.remove(); this.navigation.remove();
    window.dispatchEvent(new CustomEvent('mintcat-ui-contributions', { detail: { icons: [], games: [] } }));
    this.root.replaceChildren();
  }
}
