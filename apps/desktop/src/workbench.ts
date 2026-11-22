import { COMMAND_SPECS } from '@mintcat/sdk-ts';
import type { CommandName, CommandParams, CommandResult } from '@mintcat/sdk-ts';
import { Disposables, QueryCache, SessionStore, createOnlineModJobs, getSharedRuntime, kit, mountGameSettings, mountReactView, presentUiError, renderUiError } from '@mintcat/sdk-ui';
import type { Disposable, GameModuleList, PageMount, UiContext, UiNavigationIcon, UiOnlineModJobs } from '@mintcat/sdk-ui';
import { mountLibrary, mountDownloads, mountGameDirectories } from '../../../modules/library/ui';
import { mountSearch } from '../../../modules/ui-search/ui';
import { mountInstall } from '../../../modules/install/ui';
import { mountBackupPage } from '../../../modules/feature-backup/ui';
import { mountAgentPage, type AgentPanel } from '../../../modules/feature-agent/ui';
import '../../../modules/library/ui/library.css';
import '../../../modules/ui-search/ui/styles.css';
import '../../../modules/install/ui/styles.css';
import '../../../modules/feature-backup/ui/styles.css';
import '../../../modules/feature-agent/ui/styles.css';
import { requestApplication } from './daemon';
import { publishNotification } from './chrome-notifications';
import { mountAppearanceSettings, mountRuntimeSettings } from './settings-overview';

interface Page {
  moduleId: string; route: string; title: string; subtitle?: string; icon: UiNavigationIcon;
  placement: 'sidebar' | 'titlebar' | 'settings'; settingsGroup?: 'system' | 'game'; mount: PageMount;
  setupStep?: 'import' | 'directory';
}
interface MountedPage { page: Page; disposable?: Disposable; controller: AbortController; lifetime: Disposables; dirty: boolean }
type MountedAgent = MountedPage & { disposable?: AgentPanel };
type SetupWindow = { mode: 'main'; open(): Promise<void> } | { mode: 'guide'; legacyPath?: string; close(): Promise<void> };

const agentPage = { moduleId: 'mintcat.feature.agent', route: '/mintcat.feature.agent/agent', title: 'Agent 助手', icon: 'robot', placement: 'titlebar', mount: mountAgentPage } satisfies Page;

const libraryRoute = '/mintcat.library/library';
const setupImportRoute = '/mintcat.desktop/setup';
const setupDirectoryRoute = '/mintcat.desktop/setup/game-directory';
const setupDismissedKey = 'workbench/setup-dismissed';

const builtinPages: readonly Page[] = [
  { moduleId: 'mintcat.library', route: libraryRoute, title: '模组管理', icon: 'home', placement: 'sidebar', mount: mountLibrary },
  { moduleId: 'mintcat.ui.search', route: '/mintcat.ui.search/search', title: '在线模组', icon: 'search', placement: 'sidebar', mount: mountSearch },
  { moduleId: 'mintcat.library', route: '/mintcat.library/downloads', title: '下载管理', icon: 'downloads', placement: 'sidebar', mount: mountDownloads },
  agentPage,
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
    moduleId: 'mintcat.library',
    route: '/mintcat.library/game-directories',
    title: '游戏安装目录',
    icon: 'settings',
    placement: 'settings',
    settingsGroup: 'game',
    mount: mountGameDirectories,
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

const setupPages: readonly Page[] = [
  {
    moduleId: 'mintcat.feature.backup', route: setupImportRoute, title: '导入旧配置', icon: 'local',
    placement: 'settings', setupStep: 'import', mount: mountBackupPage,
  },
  {
    moduleId: 'mintcat.library', route: setupDirectoryRoute, title: '设置游戏路径', icon: 'local',
    placement: 'settings', setupStep: 'directory', mount: mountGameDirectories,
  },
];

export class Workbench implements Disposable {
  private readonly session: SessionStore;
  private readonly onlineModJobs: UiOnlineModJobs;
  private readonly queries = new QueryCache();
  private readonly inventoryEvents = new EventTarget();
  private readonly navigation = kit.element('nav');
  private readonly content = kit.element('section');
  private readonly workspace = kit.element('div');
  private readonly bodyRow = kit.element('div');
  private readonly settingsNav = kit.element('nav');
  private readonly auxiliaryRoot = kit.element('section');
  private readonly gamesPanel = kit.element('details');
  private readonly notice = kit.element('div');
  private sidebarView?: Disposable;
  private settingsView?: Disposable;
  private renderedSettingsPages?: readonly Page[];
  private mounted?: MountedPage;
  private auxiliary?: MountedAgent;
  private pages: readonly Page[] = builtinPages;
  private inventory?: GameModuleList;
  private inventoryFailureTitle?: string;
  private route: string;
  private closed = false;
  private pageOpened = false;
  private refreshing?: Promise<void>;
  private noticeTimer?: number;
  private readonly inventoryTimer?: number;
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
    if (icon === 'local') {
      void this.navigate('/mintcat.library/library').then(() => {
        window.dispatchEvent(new CustomEvent('mintcat-open-import-modal', { detail: { tab: 'local' } }));
      });
      return;
    }
    const page = icon === 'settings' ? this.settingsPages()[0] : this.pages.find(page => page.icon === icon);
    if (!page) return;
    void this.navigate(page.route);
  };
  private readonly openCloudSync = async (): Promise<void> => {
    const route = '/mintcat.feature.backup/backups';
    await this.navigate(route);
    if (this.mounted?.page.route === route) {
      this.content.querySelector<HTMLElement>('#cloud-snapshot-sync')?.focus();
      this.content.querySelector('#cloud-snapshot-sync')?.scrollIntoView({ block: 'start' });
    }
  };

  constructor(private root: HTMLElement, private dataDir: string, private rescue: (error: unknown) => void, gamesContainer?: HTMLElement, navigationRoot?: HTMLElement, private toolbarRoot?: HTMLElement, private readonly setup?: SetupWindow) {
    this.session = new SessionStore(window.localStorage, `mintcat.workbench.session.v1/${encodeURIComponent(dataDir)}`);
    this.onlineModJobs = createOnlineModJobs(this.session);
    this.route = setup?.mode === 'guide' ? setupImportRoute : this.session.get('workbench/route', 1, '');
    this.navigation.ariaLabel = '工作台导航'; this.navigation.className = 'app-sidebar';
    this.workspace.className = 'workbench-content';
    this.bodyRow.className = 'workbench-body';
    this.settingsNav.className = 'settings-sidebar';
    this.settingsNav.ariaLabel = '设置分类导航';
    this.settingsNav.hidden = true;
    this.content.className = 'module-page'; this.auxiliaryRoot.className = 'module-panel-host';
    this.notice.role = 'status'; this.notice.className = 'module-notice';
    this.gamesPanel.className = 'game-plugins';
    this.bodyRow.append(this.settingsNav, this.content);
    this.workspace.append(this.notice, this.bodyRow);
    if (setup?.mode === 'guide') {
      this.pages = setupPages;
      root.classList.add('mintcat-setup-window');
      root.append(this.workspace);
      window.addEventListener('mintcat-ui-render-error', this.renderErrorHandler);
      return;
    }
    (navigationRoot ?? root).append(this.navigation);
    root.append(this.workspace, this.auxiliaryRoot);
    (gamesContainer ?? this.workspace).append(this.gamesPanel);
    window.addEventListener('hashchange', this.hashHandler);
    window.addEventListener('beforeunload', this.beforeUnload);
    window.addEventListener('mintcat-ui-navigate', this.navigationHandler);
    window.addEventListener('mintcat-open-cloud-sync', this.openCloudSync);
    window.addEventListener('mintcat-ui-render-error', this.renderErrorHandler);
    this.inventoryTimer = window.setInterval(() => { void this.refresh(); }, 2_000);
  }

  refresh(): Promise<void> {
    if (this.closed) return Promise.resolve();
    if (this.setup?.mode === 'guide') return this.pageOpened ? Promise.resolve() : this.navigate(setupImportRoute, true);
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
      if (!this.pageOpened) {
        const route = location.hash.slice(1) || this.route;
        const openSetup = this.setup?.mode === 'main' && await this.shouldOpenSetup(inventory, route);
        if (!this.pageOpened) {
          await this.navigate(route, true);
          if (!this.closed && openSetup && this.setup?.mode === 'main') void this.setup.open().catch(error => this.showFailure(error));
        }
      }
      this.inventoryFailureTitle = undefined;
      if (!this.closed && !this.notice.textContent) window.dispatchEvent(new CustomEvent('mintcat-ui-notice', { detail: '已连接独立后台' }));
    } catch (error) {
      if (!this.closed) { this.showFailure(error, 'inventory'); if (!this.inventory) this.rescue(error); }
    }
  }
  private async shouldOpenSetup(inventory: GameModuleList, route: string): Promise<boolean> {
    if (location.hash || (route && route !== libraryRoute) || this.session.get(setupDismissedKey, 1, false)) return false;
    if (!inventory.modules.some(module => module.enabled && module.game?.descriptor)) return false;
    // Existing installations should open directly, even if they predate the guide's completion marker.
    try {
      let cursor: string | undefined;
      do {
        const result: CommandResult['games.list'] = await requestApplication(this.dataDir, 'games.list', { pageSize: 100, ...(cursor ? { cursor } : {}) });
        if (result.items.some(game => game.installations.some(installation => installation.state === 'configured' && installation.rootPath))) return false;
        cursor = result.nextCursor;
      } while (cursor && !this.closed);
      return true;
    } catch {
      return false;
    }
  }
  refreshAfterSetup(): void {
    if (this.closed) return;
    this.session.reload();
    this.inventoryEvents.dispatchEvent(new Event('inventory'));
  }
  private allowDiscard(): boolean {
    return !this.mounted?.dirty || window.confirm('离开未保存的页面？草稿将保留，后台任务继续运行。');
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
      moduleId: page.moduleId, signal, kit, onlineModJobs: this.onlineModJobs,
      get toolbar() { return active() && mounted === workbench.mounted ? workbench.toolbarRoot : undefined; },
      call: <M extends CommandName>(method: M, params: CommandParams[M], key?: string): Promise<CommandResult[M]> => request(method, params, key),
      request,
      query: async (key, load, ttl) => { assertActive(); const result = await this.queries.get(`${page.moduleId}/${key}`, load, ttl); assertActive(); return result; },
      invalidate: prefix => this.queries.invalidate(`${page.moduleId}/${prefix}`),
      state: (key, version, fallback) => this.session.get(`${page.moduleId}/${key}`, version, fallback),
      saveState: (key, version, value) => this.session.set(`${page.moduleId}/${key}`, version, value),
      dirty: value => { if (active()) mounted.dirty = value; },
      notify: (text, inboxSummary) => {
        if (!active()) return;
        this.showNotice(text);
        if (inboxSummary) publishNotification(inboxSummary);
      },
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
  private clearNotice(): void {
    if (this.noticeTimer !== undefined) {
      clearTimeout(this.noticeTimer);
      this.noticeTimer = undefined;
    }
    this.notice.replaceChildren();
    delete this.notice.dataset.tone;
  }
  private showNotice(text: string): void {
    this.clearNotice();
    this.notice.dataset.tone = 'info';
    this.notice.classList.remove('mc-error-box');
    const content = kit.element('span', text);
    content.className = 'module-notice-text';
    const dismiss = kit.button('×', () => { this.clearNotice(); });
    dismiss.className = 'module-notice-dismiss';
    dismiss.ariaLabel = '关闭提示';
    this.notice.replaceChildren(content, dismiss);
    this.noticeTimer = window.setTimeout(() => { this.clearNotice(); }, 5_000);
    window.dispatchEvent(new CustomEvent('mintcat-ui-notice', { detail: text }));
  }
  private showFailure(cause: unknown, source?: 'inventory'): void {
    this.clearNotice();
    this.notice.dataset.tone = 'error';
    const error = presentUiError(cause);
    if (source !== 'inventory' || this.inventoryFailureTitle !== error.title) publishNotification(error.title, 'error');
    if (source === 'inventory') this.inventoryFailureTitle = error.title;
    const body = kit.element('div');
    body.className = 'module-notice-error-body';
    renderUiError(body, error);
    const dismiss = kit.button('×', () => { this.clearNotice(); });
    dismiss.className = 'module-notice-dismiss';
    dismiss.ariaLabel = '关闭提示';
    this.notice.classList.remove('mc-error-box');
    this.notice.replaceChildren(body, dismiss);
    window.dispatchEvent(new CustomEvent('mintcat-ui-notice', { detail: `${error.title}：${error.description}` }));
  }
  private settingsPages(): Page[] { return this.pages.filter(page => page.placement === 'settings'); }
  private renderSlots(): void {
    if (this.closed || this.setup?.mode === 'guide') return;
    this.sidebarView?.dispose();
    const { React, Antd, Icons } = getSharedRuntime(); const h = React.createElement;
    const icons = { home: Icons.AppstoreOutlined, search: Icons.CompassOutlined, settings: Icons.BgColorsOutlined, robot: Icons.RobotOutlined, cloud: Icons.CloudSyncOutlined, install: Icons.DeploymentUnitOutlined, proxy: Icons.ApiOutlined, local: Icons.ImportOutlined, downloads: Icons.CloudDownloadOutlined };
    const settings = this.settingsPages();
    const currentPage = this.pages.find(page => page.route === this.route);
    const isSettings = currentPage?.placement === 'settings';
    const items = this.pages.filter(page => page.placement === 'sidebar').map(page => ({ key: page.route, title: page.title, 'aria-label': page.title, className: 'sidebar-route-item', label: h('span', { className: 'sidebar-link' }, h(icons[page.icon]), h('span', { className: 'sidebar-label' }, page.title)) }));
    items.push({ key: '@settings', title: '设置', 'aria-label': '设置', className: 'sidebar-settings-item', label: h('span', { className: 'sidebar-link' }, h(Icons.SettingOutlined), h('span', { className: 'sidebar-label' }, '设置')) });
    this.sidebarView = mountReactView(this.navigation,
      h(Antd.Menu, { mode: 'inline', selectedKeys: [isSettings ? '@settings' : this.route], items, onClick: ({ key }) => { void this.navigate(key === '@settings' ? isSettings ? this.route : settings[0].route : key); } }));
    this.settingsNav.hidden = !isSettings;
    if (isSettings) {
      // Keep the links mounted between settings routes so focus and scroll stay in the rail.
      if (this.renderedSettingsPages !== this.pages) {
        this.settingsView?.dispose();
        this.settingsView = mountReactView(this.settingsNav,
          h('ul', { className: 'settings-sidebar-list' }, ...settings.map(page => {
            const Icon = page.settingsGroup === 'game' ? Icons.ControlOutlined : icons[page.icon];
            return h('li', { key: page.route },
              h('a', { className: 'settings-sidebar-link', href: `#${page.route}` },
                h(Icon, { 'aria-hidden': true }), h('span', null, page.title)));
          })));
        this.renderedSettingsPages = this.pages;
      }
      for (const link of this.settingsNav.querySelectorAll('a')) {
        link.ariaCurrent = link.hash === `#${this.route}` ? 'page' : null;
      }
    }
    window.dispatchEvent(new CustomEvent('mintcat-ui-active-page', {
      detail: {
        route: this.route,
        title: currentPage?.title ?? '',
        placement: currentPage?.placement ?? 'sidebar',
      },
    }));
    window.dispatchEvent(new CustomEvent('mintcat-ui-contributions', { detail: { icons: [...new Set([...this.pages.map(page => page.icon), 'settings'])], games: this.inventory?.modules.flatMap(module => module.enabled && module.game ? [{ gameId: module.game.gameId, displayName: module.game.displayName }] : []) ?? [] } }));
  }
  private openAgent(): void {
    if (this.closed) return;
    if (this.auxiliary) { this.auxiliary.disposable?.setOpen(true); return; }
    const mounted: MountedAgent = { page: agentPage, controller: new AbortController(), lifetime: new Disposables(), dirty: false };
    this.auxiliary = mounted;
    const pageRoot = kit.element('section'); pageRoot.dataset.module = agentPage.moduleId; this.auxiliaryRoot.replaceChildren(pageRoot);
    try {
      const panel = agentPage.mount(pageRoot, this.context(mounted));
      if (mounted.controller.signal.aborted) panel.dispose(); else mounted.disposable = panel;
    } catch (error) { if (!mounted.controller.signal.aborted) { this.unmountAuxiliary(); this.showFailure(error); } }
  }
  private unmountAuxiliary(): void {
    const mounted = this.auxiliary; this.auxiliary = undefined;
    if (mounted) this.disposePage(mounted);
    this.auxiliaryRoot.replaceChildren();
  }
  private renderSetup(root: HTMLElement, mounted: MountedPage): HTMLElement {
    const importing = mounted.page.setupStep === 'import';
    const { React, Antd } = getSharedRuntime();
    const h = React.createElement;
    const host = kit.element('div'); host.className = 'mintcat-setup-header';
    const body = kit.element('div'); body.className = 'mintcat-setup-content'; root.append(host, body);
    const finish = (): void => {
      this.session.set(setupDismissedKey, 1, true);
      if (this.setup?.mode === 'guide') void this.setup.close().catch(error => this.showFailure(error));
    };
    const header = h('section', { className: 'settings-page-shell', 'aria-label': '首次启动引导' },
      h(Antd.Steps, { size: 'small', current: importing ? 0 : 1, items: [{ title: '导入旧配置' }, { title: '设置游戏路径' }] }),
      h('div', { className: 'settings-group-card' },
        h('div', { className: 'settings-section-header' },
          h('div', null,
            h('h2', { className: 'settings-section-title' }, importing ? '欢迎使用 MintCat' : '设置游戏路径'),
            h('p', { className: 'settings-section-desc' }, importing
              ? '会自动查找默认位置的旧版配置。请审阅并批准导入，等迁移显示「已完成」后继续；也可以直接下一步。'
              : '会自动查找所选游戏的 Steam 安装。路径找到后直接验证并注册；没找到时可手动浏览。'))),
        h(Antd.Space, { wrap: true, style: { marginTop: 12 } },
          h(Antd.Button, { onClick: importing ? finish : () => void this.navigate(setupImportRoute) }, importing ? '跳过引导' : '上一步'),
          h(Antd.Button, { type: 'primary', className: 'glass-primary-action', onClick: importing ? () => void this.navigate(setupDirectoryRoute) : finish },
            importing ? '下一步：设置游戏路径' : '进入模组管理'))));
    mounted.lifetime.add(mountReactView(host, header));
    return body;
  }
  async navigate(route: string, force = false): Promise<void> {
    if (this.closed) return;
    if (route === agentPage.route) {
      if (!this.mounted) await this.navigate(builtinPages[0].route, true);
      else if (location.hash === `#${route}`) history.replaceState(null, '', `#${this.route}`);
      this.openAgent();
      return;
    }
    if (!force && route === this.route && this.mounted) return;
    if (!force && !this.allowDiscard()) { history.replaceState(null, '', `#${this.route}`); return; }
    this.clearNotice();
    this.auxiliary?.disposable?.setOpen(false);
    this.unmountPage();
    const page = this.pages.find(page => page.route === route) ?? this.pages[0];
    this.pageOpened = true;
    this.route = page.route;
    if (this.setup?.mode !== 'guide') {
      this.session.set('workbench/route', 1, this.route);
      this.renderSlots(); history.replaceState(null, '', `#${this.route}`);
    }
    const pageRoot = kit.element('section'); pageRoot.dataset.module = page.moduleId; this.content.replaceChildren(pageRoot);
    const mounted: MountedPage = { page, controller: new AbortController(), lifetime: new Disposables(), dirty: false }; this.mounted = mounted;
    try {
      const contentRoot = page.setupStep ? this.renderSetup(pageRoot, mounted) : pageRoot;
      const context = this.context(mounted);
      const disposable = await (page.setupStep === 'import' && this.setup?.mode === 'guide'
        ? mountBackupPage(contentRoot, context, { legacyPath: this.setup.legacyPath })
        : page.setupStep === 'directory' ? mountGameDirectories(contentRoot, context, true) : page.mount(contentRoot, context));
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
    const mounted = this.mounted; this.mounted = undefined;
    if (mounted) this.disposePage(mounted);
  }
  dispose(): void {
    if (this.closed) return;
    this.closed = true; this.clearNotice(); this.unmountAuxiliary(); this.unmountPage(); clearInterval(this.inventoryTimer);
    window.removeEventListener('hashchange', this.hashHandler); window.removeEventListener('beforeunload', this.beforeUnload);
    window.removeEventListener('mintcat-ui-navigate', this.navigationHandler); window.removeEventListener('mintcat-ui-render-error', this.renderErrorHandler);
    window.removeEventListener('mintcat-open-cloud-sync', this.openCloudSync);
    this.sidebarView?.dispose(); this.settingsView?.dispose(); this.gamesPanel.remove(); this.navigation.remove();
    window.dispatchEvent(new CustomEvent('mintcat-ui-contributions', { detail: { icons: [], games: [] } }));
    this.root.replaceChildren();
  }
}
