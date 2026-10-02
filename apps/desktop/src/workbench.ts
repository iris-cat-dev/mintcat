import { convertFileSrc, invoke } from '@tauri-apps/api/core';
import { COMMAND_SPECS } from '@mintcat/sdk-ts';
import type { CommandName, CommandParams, CommandResult } from '@mintcat/sdk-ts';
import { Disposables, QueryCache, SessionStore, getSharedRuntime, kit, mountGameSettings, mountReactView, validateUiManifest } from '@mintcat/sdk-ui';
import type { Disposable, PageMount, UiContext, UiManifest, UiModule, UiNavigationIcon } from '@mintcat/sdk-ui';
import { requestApplication, requestUiApplication } from './daemon';

export interface ModuleBinding { moduleId: string; digest: string; enabled?: boolean; game?: { gameId: string; displayName: string } }
export interface ModuleList { generationId: string; modules: ModuleBinding[] }
interface NativeGeneration { token: string; generationId: string; modules: Array<{ digest: string; manifest: unknown; files: string[] }> }
interface LoadedModule { sourceToken: string; digest: string; manifest: UiManifest; exports: UiModule; files: string[]; lifetime: Disposables; controller: AbortController; context: UiContext; dirty: boolean }
interface Generation { native: NativeGeneration; modules: LoadedModule[] }
interface UiBarrier { barrierId: string; scopeId: string; expectedGenerationId: string; newGenerationId: string; runtimeChanged: boolean; affectedModuleIds: string[] }
interface MountedPage { owner: LoadedModule; disposable?: Disposable; controller: AbortController; lifetime: Disposables }

export class Workbench implements Disposable {
  private readonly session: SessionStore;
  private readonly realm: SessionStore;
  private readonly knownSessions: Set<string>;
  private readonly nativeGenerations = new Map<string, NativeGeneration>();
  private readonly queries = new QueryCache();
  private readonly generationEvents = new EventTarget();
  private readonly dirtyContexts = new WeakMap<object, Set<AbortSignal>>();
  private readonly navigation = kit.element('nav');
  private readonly commands = kit.element('div');
  private readonly content = kit.element('section');
  private readonly workspace = kit.element('div');
  private readonly settingsTabs = kit.element('div');
  private readonly auxiliaryRoot = kit.element('section');
  private auxiliary?: MountedPage;
  private sidebarView?: Disposable;
  private settingsView?: Disposable;
  private readonly modulesPanel = kit.element('details');
  private readonly notice = kit.element('p');
  private generation?: Generation;
  private preparing?: Generation;
  private mounted?: MountedPage;
  private route = '';
  private closed = false;
  private switching = false;
  private refreshPending = false;
  private loadedGenerations = 0;
  private readonly pausedModules = new Set<string>();
  private barrier?: UiBarrier & { sessionIds: string[] };
  private barrierPolling = false;
  private readonly disposedSessionIds = new Set<string>();
  private pageFailed = false;
  private cleanup?: Promise<void>;
  private readonly barrierTimer: number;
  private readonly hashHandler = (): void => { void this.navigate(location.hash.slice(1)); };
  private readonly renderErrorHandler = (event: Event): void => {
    const { root, error } = (event as CustomEvent<{ root: HTMLElement; error: Error }>).detail;
    if (this.closed) return;
    if (this.mounted && this.content.contains(root)) this.pageError(error);
    else if (this.auxiliary && this.auxiliaryRoot.contains(root)) { this.unmountAuxiliary(); this.showNotice(`面板渲染失败：${message(error)}`); }
  };
  private readonly navigationHandler = (event: Event): void => {
    const icon = (event as CustomEvent<UiNavigationIcon>).detail;
    if (icon === 'settings') {
      const route = this.settingsRoutes()[0]?.route;
      if (route) void this.navigate(route); else this.showNotice('没有启用的设置模块');
      return;
    }
    for (const module of this.generation?.modules ?? []) {
      const item = module.manifest.navigation.find(item => item.icon === icon);
      const page = item && module.manifest.pages.find(page => page.id === item.pageId);
      if (!page || module.controller.signal.aborted || this.pausedModules.has(module.manifest.moduleId)) continue;
      if (icon === 'robot') void this.openAuxiliary(module, page.route);
      else void this.navigate(page.route);
      return;
    }
    this.showNotice('对应的已签名 UI 模块未启用');
  };
  private readonly beforeUnload = (event: BeforeUnloadEvent): void => {
    if (this.generation?.modules.some(module => module.dirty)) { event.preventDefault(); event.returnValue = ''; }
  };
  constructor(private root: HTMLElement, private dataDir: string, private rescue: (error: unknown) => void, private modulesContainer?: HTMLElement, private navigationRoot?: HTMLElement, private toolbarRoot?: HTMLElement) {
    this.session = new SessionStore(window.localStorage, `mintcat.workbench.session.v1/${encodeURIComponent(dataDir)}`);
    this.realm = new SessionStore(window.sessionStorage, `mintcat.workbench.realm.v1/${encodeURIComponent(dataDir)}`);
    this.knownSessions = new Set(this.realm.get('workbench/ui-sessions', 1, [] as string[]));
    this.route = this.session.get('workbench/route', 1, '');
    this.navigation.ariaLabel = '模块导航'; this.navigation.className = 'app-sidebar';
    this.workspace.className = 'workbench-content'; this.commands.className = 'app-page-actions';
    this.settingsTabs.className = 'settings-tabs'; this.content.className = 'module-page';
    this.auxiliaryRoot.className = 'module-panel-host';
    this.notice.role = 'status'; this.notice.className = 'module-notice';
    this.modulesPanel.className = 'signed-modules'; this.modulesPanel.append(kit.element('summary', '已签名模块'));
    this.workspace.append(this.commands, this.settingsTabs, this.notice, this.content);
    (navigationRoot ?? root).append(this.navigation);
    root.append(this.workspace, this.auxiliaryRoot);
    (modulesContainer ?? this.workspace).append(this.modulesPanel);
    window.addEventListener('hashchange', this.hashHandler);
    window.addEventListener('beforeunload', this.beforeUnload);
    window.addEventListener('mintcat-ui-navigate', this.navigationHandler);
    window.addEventListener('mintcat-ui-render-error', this.renderErrorHandler);
    this.barrierTimer = window.setInterval(() => { void this.pollBarriers(); }, 1_000);
  }
  async refresh(approvedTransition = false): Promise<void> {
    if (this.closed) return;
    if (this.switching) { this.refreshPending = true; return; }
    this.switching = true;
    let candidate: Generation | undefined;
    try {
      const list = await requestApplication<ModuleList>(this.dataDir, 'modules.list', {});
      if (this.closed) return;
      this.renderModules(list);
      if (this.barrier && this.barrier.newGenerationId !== list.generationId) return;
      if (this.generation?.native.generationId === list.generationId && this.generation.modules.every(module => !module.controller.signal.aborted) && !this.barrier) return;
      const changed = new Set((this.generation?.modules ?? []).filter(module => !list.modules.some(binding => binding.moduleId === module.manifest.moduleId && binding.digest === module.digest && binding.enabled !== false)).map(module => module.manifest.moduleId));
      if (!approvedTransition && !this.allowDiscard(changed)) { this.notice.textContent = 'New UI generation is available; save affected module edits before refreshing.'; return; }
      if (this.loadedGenerations >= 8) {
        if (!this.allowDiscard()) { this.notice.textContent = 'Save remaining module drafts before rebuilding the shared WebView.'; return; }
        if (this.barrier) this.realm.set('workbench/rebuild', 1, { barrierId: this.barrier.barrierId, sessionIds: this.barrier.sessionIds, newGenerationId: list.generationId });
        await this.rebuildWebView(); return;
      }
      const native = await invoke<NativeGeneration>('ui_open_generation', { dataDir: this.dataDir, generationId: list.generationId });
      candidate = { native, modules: [] }; this.preparing = candidate;
      this.nativeGenerations.set(native.token, native);
      this.knownSessions.add(native.token); this.realm.set('workbench/ui-sessions', 1, [...this.knownSessions]);
      this.loadedGenerations++;
      for (const descriptor of native.modules) {
        if (this.closed) throw new DOMException('Workbench closed', 'AbortError');
        const manifest = validateUiManifest(descriptor.manifest, descriptor.files);
        if (candidate.modules.some(module => module.manifest.moduleId === manifest.moduleId)) throw new Error('Duplicate module binding');
        const previous = this.generation?.modules.find(module => module.manifest.moduleId === manifest.moduleId);
        if (previous?.digest === descriptor.digest && !previous.controller.signal.aborted) { candidate.modules.push(previous); continue; }
        if (previous && previous.digest !== descriptor.digest) this.queries.invalidate(`${manifest.moduleId}/`);
        const moduleBase = { sourceToken: native.token, digest: descriptor.digest, manifest, exports: {} as UiModule, files: descriptor.files, lifetime: new Disposables(), controller: new AbortController(), dirty: false };
        const module: LoadedModule = Object.assign(moduleBase, { context: this.context(moduleBase, moduleBase.controller.signal, moduleBase.lifetime) });
        candidate.modules.push(module);
        // The native generation contains immutable verified bytes, not paths into a mutable directory.
        module.exports = await import(/* @vite-ignore */ module.context.resource(manifest.entry)) as UiModule;
        if (this.closed || module.controller.signal.aborted) throw new DOMException('UI module unloaded', 'AbortError');
        if (module.exports.activate) module.lifetime.add(await module.exports.activate(module.context));
        if (this.closed || module.controller.signal.aborted) throw new DOMException('UI module unloaded', 'AbortError');
        for (const page of manifest.pages) if (typeof module.exports[page.export] !== 'function') throw new Error(`Page export missing: ${page.export}`);
        await this.loadStyles(module, false);
      }
      if (this.closed) { await this.release(candidate); candidate = undefined; return; }
      // Validate the entire candidate before replacing contributions. Old routes remain recoverable.
      const routeOwners = new Set<string>(); const gameOwners = new Set<string>();
      for (const module of candidate.modules) {
        for (const page of module.manifest.pages) { if (routeOwners.has(page.route)) throw new Error('Duplicate route'); routeOwners.add(page.route); }
        for (const game of module.manifest.games) { if (gameOwners.has(game.gameId)) throw new Error('Duplicate signed game descriptor'); gameOwners.add(game.gameId); }
      }
      const previous = this.generation;
      const retainPage = !!this.mounted && candidate.modules.includes(this.mounted.owner) && !this.mounted.controller.signal.aborted;
      if (this.auxiliary && (!candidate.modules.includes(this.auxiliary.owner) || this.auxiliary.controller.signal.aborted)) this.unmountAuxiliary();
      if (!retainPage) this.unmountPage();
      this.generation = candidate; candidate = undefined;
      for (const module of this.generation.modules) {
        for (const css of module.manifest.css) {
          const href = module.context.resource(css);
          for (const link of document.querySelectorAll<HTMLLinkElement>('link[rel="stylesheet"]')) if (link.href === href) link.media = 'all';
        }
      }
      this.renderSlots();
      if (!retainPage) await this.navigate(location.hash.slice(1) || this.route, true);
      else this.pageFailed = false;
      if (this.closed) { if (previous) await this.release(previous); return; }
      if (this.barrier && this.barrier.newGenerationId === list.generationId) {
        await this.ack(this.barrier, this.pageFailed ? 'failed' : 'ready', this.pageFailed ? { error: 'Signed page failed to mount' } : {});
        if (this.pageFailed && previous) {
          const failed = this.generation;
          this.generation = previous;
          this.barrier = undefined; this.pausedModules.clear();
          await this.release(failed); await this.resume(previous); this.renderSlots(); await this.navigate(this.route, true);
          return;
        }
        this.barrier = undefined; this.pausedModules.clear();
        this.renderSlots();
      }
      this.generationEvents.dispatchEvent(new CustomEvent('generation', { detail: list.generationId }));
      const rebuilt = this.realm.get('workbench/rebuild', 1, { barrierId: '', sessionIds: [] as string[], newGenerationId: '' });
      if (rebuilt.barrierId && rebuilt.newGenerationId === list.generationId) {
        for (const sessionId of rebuilt.sessionIds) await requestApplication(this.dataDir, 'ui.ack', { barrierId: rebuilt.barrierId, sessionId, stage: this.pageFailed ? 'failed' : 'ready', rebuilt: true }, crypto.randomUUID());
        this.realm.set('workbench/rebuild', 1, { barrierId: '', sessionIds: [], newGenerationId: '' });
      }
      if (previous) await this.release(previous);
      this.notice.textContent = '';
      window.dispatchEvent(new CustomEvent('mintcat-ui-notice', { detail: `已连接独立后台 · ${list.generationId}` }));
    } catch (error) {
      if (candidate) await this.release(candidate);
      if (this.closed) return;
      if (this.barrier) {
        await this.ack(this.barrier, 'failed', { error: message(error) }).catch(ackError => this.rescue(ackError));
        this.barrier = undefined; this.pausedModules.clear();
        if (this.generation) {
          await this.resume(this.generation).catch(resumeError => this.rescue(resumeError));
          this.renderSlots(); await this.navigate(this.route, true);
        }
      }
      this.notice.textContent = `Module activation refused: ${message(error)}`;
      if (!this.generation) this.rescue(error);
    } finally {
      this.preparing = undefined; this.switching = false;
      if (this.refreshPending && !this.closed) { this.refreshPending = false; await this.refresh(); }
    }
  }
  private allowDiscard(affected?: ReadonlySet<string>): boolean {
    return !this.generation?.modules.some(module => module.dirty && (!affected || affected.has(module.manifest.moduleId))) || window.confirm('Leave this module with unsaved edits? Serializable drafts will be retained; daemon jobs continue.');
  }
  private async ack(barrier: UiBarrier & { sessionIds: string[] }, stage: string, extra: Record<string, unknown> = {}): Promise<void> {
    for (const sessionId of stage === 'failed' ? barrier.sessionIds.slice(0, 1) : barrier.sessionIds) {
      if (stage === 'disposed' && this.disposedSessionIds.has(sessionId)) continue;
      await requestApplication(this.dataDir, 'ui.ack', { barrierId: barrier.barrierId, sessionId, stage, ...extra }, crypto.randomUUID());
      if (stage === 'disposed') this.disposedSessionIds.add(sessionId);
    }
  }
  private async pollBarriers(): Promise<void> {
    if (this.closed || this.switching || this.barrierPolling || !this.generation) return;
    const method = 'ui.prepare';
    if (!COMMAND_SPECS.some(spec => spec.method === method)) return;
    this.barrierPolling = true;
    const generation = this.generation;
    try {
      const pendingById = new Map<string, UiBarrier & { sessionIds: string[] }>();
      for (const sessionId of this.knownSessions) {
        const result = await requestApplication<{ barriers: UiBarrier[] }>(this.dataDir, method, { sessionId });
        if (this.closed || this.generation !== generation || this.switching) return;
        for (const item of result.barriers) {
          const pending = pendingById.get(item.barrierId);
          if (pending) pending.sessionIds.push(sessionId); else pendingById.set(item.barrierId, { ...item, sessionIds: [sessionId] });
        }
        if (!result.barriers.length && !this.nativeGenerations.has(sessionId)) this.knownSessions.delete(sessionId);
      }
      this.realm.set('workbench/ui-sessions', 1, [...this.knownSessions]);
      const pending = this.barrier ? pendingById.get(this.barrier.barrierId) : pendingById.values().next().value as (UiBarrier & { sessionIds: string[] }) | undefined;
      if (!pending) {
        if (this.barrier) {
          this.barrier = undefined; this.pausedModules.clear();
          const active = await requestApplication<ModuleList>(this.dataDir, 'modules.list', {});
          if (this.closed || this.generation !== generation || this.switching) return;
          if (active.generationId !== this.generation.native.generationId) await this.refresh(true);
          else {
            await this.resume(this.generation); this.renderSlots();
            if (!this.mounted) await this.navigate(this.route, true);
          }
        }
        return;
      }
      if (!this.barrier) {
        const affected = new Set(pending.runtimeChanged ? this.generation.modules.map(module => module.manifest.moduleId) : pending.affectedModuleIds);
        if (this.closed) return;
        if (this.generation.modules.some(module => module.dirty && affected.has(module.manifest.moduleId))) {
          await this.ack(pending, 'deferred'); this.notice.textContent = 'Update deferred: save affected module edits first.'; return;
        }
        if (pending.runtimeChanged) await this.ack(pending, 'rebuild-required');
        if (this.closed || this.generation !== generation || this.switching) return;
        if (this.generation.modules.some(module => module.dirty && affected.has(module.manifest.moduleId))) {
          await this.ack(pending, 'deferred'); this.notice.textContent = 'Update deferred: save affected module edits first.'; return;
        }
        for (const moduleId of affected) this.pausedModules.add(moduleId);
        if (this.mounted && affected.has(this.mounted.owner.manifest.moduleId)) this.unmountPage();
        if (this.auxiliary && affected.has(this.auxiliary.owner.manifest.moduleId)) this.unmountAuxiliary();
        for (const module of this.generation.modules) if (affected.has(module.manifest.moduleId)) { module.controller.abort(); this.queries.releasePending(`${module.manifest.moduleId}/`); module.lifetime.dispose(); }
        this.renderSlots();
        this.barrier = pending;
        this.disposedSessionIds.clear();
        await this.ack(pending, 'disposed', { state: this.session.snapshot() });
        this.notice.textContent = 'Affected UI state saved. Other scopes and daemon jobs remain available.';
      } else { this.barrier = pending; await this.ack(pending, 'disposed', { state: this.session.snapshot() }); }
      if (this.closed) return;
      const list = await requestApplication<ModuleList>(this.dataDir, 'modules.list', {});
      if (this.closed || this.generation !== generation || this.switching) return;
      if (list.generationId !== pending.newGenerationId) return;
      if (pending.runtimeChanged) {
        this.realm.set('workbench/rebuild', 1, { barrierId: pending.barrierId, sessionIds: pending.sessionIds, newGenerationId: list.generationId });
        await this.rebuildWebView();
      } else await this.refresh(true);
    } catch (error) {
      if (!this.closed) this.notice.textContent = `UI update barrier unavailable: ${message(error)}`;
    } finally { this.barrierPolling = false; }
  }
  private context(owner: Omit<LoadedModule, 'context'>, signal: AbortSignal, lifetime: Disposables): UiContext {
    const dirtySignals = this.dirtyContexts.get(owner) ?? new Set<AbortSignal>();
    this.dirtyContexts.set(owner, dirtySignals);
    lifetime.add({ dispose: () => { dirtySignals.delete(signal); owner.dirty = dirtySignals.size > 0; } });
    const active = (): boolean => !signal.aborted && !owner.controller.signal.aborted && !this.closed;
    const toolbar = (): HTMLElement | undefined => active() && this.mounted?.controller.signal === signal ? this.toolbarRoot : undefined;
    const assertActive = (): void => { if (!active()) throw new DOMException('UI module unloaded', 'AbortError'); };
    const check = (method: string): string => {
      assertActive();
      const spec = COMMAND_SPECS.find(spec => spec.method === method);
      if (!spec) throw new Error(`Unknown Application command: ${method}`);
      if (spec.classification !== 'read' && (this.pausedModules.has(owner.manifest.moduleId) || !this.generation?.modules.includes(owner as LoadedModule))) throw new Error('This module is paused or not yet active');
      return spec.classification;
    };
    return {
      moduleId: owner.manifest.moduleId, signal, kit,
      get toolbar() { return toolbar(); },
      // Once accepted, a mutation remains daemon-owned; its real JobRef may still be
      // persisted by a disposing module. Only obsolete read results are discarded.
      call: async <M extends CommandName>(method: M, params: CommandParams[M], key?: string): Promise<CommandResult[M]> => { const classification = check(method); const value = await requestUiApplication<CommandResult[M]>(this.authorityToken(owner), owner.manifest.moduleId, method, params, key); if (classification === 'read') assertActive(); return value; },
      request: async <T>(method: string, params: Record<string, unknown>, key?: string): Promise<T> => { const classification = check(method); const value = await requestUiApplication<T>(this.authorityToken(owner), owner.manifest.moduleId, method, params, key); if (classification === 'read') assertActive(); return value; },
      query: async (key, load, ttl) => { assertActive(); const value = await this.queries.get(`${owner.manifest.moduleId}/${key}`, load, ttl); assertActive(); return value; },
      invalidate: prefix => this.queries.invalidate(`${owner.manifest.moduleId}/${prefix}`),
      state: (key, version, fallback) => this.session.get(`${owner.manifest.moduleId}/${key}`, version, fallback),
      saveState: (key, version, value) => this.session.set(`${owner.manifest.moduleId}/${key}`, version, value),
      dirty: value => { if (!active()) return; if (value) dirtySignals.add(signal); else dirtySignals.delete(signal); owner.dirty = dirtySignals.size > 0; },
      notify: text => { if (active()) this.showNotice(text); },
      resource: path => {
        if (!owner.files.includes(path) || !path.startsWith('ui/') || path.includes('..')) throw new Error('Resource is not in module signed file list');
        return `${convertFileSrc(owner.sourceToken, 'mintcat-ui')}/${owner.manifest.moduleId}/${owner.digest.slice(7)}/${path}`;
      },
      guard: action => (...args) => { if (active()) Promise.resolve().then(() => { if (active()) return action(...args); }).catch(error => { if (active()) this.showNotice(`${owner.manifest.moduleId}: ${message(error)}`); }); },
      own: disposable => lifetime.add(disposable),
      onGenerationChanged: listener => {
        const handler = (event: Event): void => {
          if (!active() || !this.generation?.modules.includes(owner as LoadedModule)) return;
          Promise.resolve().then(() => { if (active() && this.generation?.modules.includes(owner as LoadedModule)) return listener((event as CustomEvent<string>).detail); }).catch(error => {
            if (active()) this.notice.textContent = `${owner.manifest.moduleId}: ${message(error)}`;
          });
        };
        this.generationEvents.addEventListener('generation', handler);
        return lifetime.add({ dispose: () => this.generationEvents.removeEventListener('generation', handler) });
      },
      interval: (action, milliseconds) => {
        let pending = false;
        const timer = window.setInterval(() => {
          if (pending || !active()) return;
          pending = true;
          Promise.resolve().then(() => { if (active()) return action(); }).catch(error => { if (active()) this.notice.textContent = message(error); }).finally(() => { pending = false; });
        }, milliseconds);
        return lifetime.add({ dispose: () => clearInterval(timer) });
      },
    };
  }
  private authorityToken(owner: Omit<LoadedModule, 'context'>): string {
    // A retained module keeps its ESM object and resource URLs, but new business
    // calls bind the latest active environment that contains that exact object.
    return this.generation?.modules.includes(owner as LoadedModule) ? this.generation.native.token : owner.sourceToken;
  }
  private renderModules(list: ModuleList): void {
    this.modulesPanel.replaceChildren(kit.element('summary', 'Signed modules'));
    for (const module of list.modules) {
      const row = kit.element('div', `${module.moduleId} · ${module.digest}`);
      const button = kit.button(module.enabled === false ? 'Enable UI' : 'Disable UI', () => {
        void (async () => {
          if (this.closed || this.barrier || !this.allowDiscard(new Set([module.moduleId]))) return;
          button.disabled = true;
          try {
            await requestApplication(this.dataDir, 'modules.setEnabled', { moduleId: module.moduleId, enabled: module.enabled === false, expectedGenerationId: list.generationId }, crypto.randomUUID());
            await this.refresh(true);
          } catch (error) { if (!this.closed) this.notice.textContent = message(error); }
          finally { if (!this.closed) button.disabled = false; }
        })();
      });
      row.append(button); this.modulesPanel.append(row);
    }
  }
  private showNotice(text: string): void {
    this.notice.textContent = text;
    window.dispatchEvent(new CustomEvent('mintcat-ui-notice', { detail: text }));
  }
  private settingsRoutes(): Array<{ title: string; route: string }> {
    const routes: Array<{ title: string; route: string }> = [];
    for (const module of this.generation?.modules ?? []) {
      if (this.pausedModules.has(module.manifest.moduleId) || module.controller.signal.aborted) continue;
      for (const settings of module.manifest.settings) routes.push({ title: settings.title, route: `/${module.manifest.moduleId}/settings/${encodeURIComponent(settings.id)}` });
      for (const navigation of module.manifest.navigation.filter(item => item.placement === 'settings')) {
        const page = module.manifest.pages.find(page => page.id === navigation.pageId)!;
        routes.push({ title: page.title, route: page.route });
      }
    }
    return routes;
  }
  private renderSlots(): void {
    if (this.closed) return;
    this.sidebarView?.dispose(); this.settingsView?.dispose(); this.commands.replaceChildren();
    const { React, Antd, Icons } = getSharedRuntime();
    const h = React.createElement;
    const modules = this.generation?.modules.filter(module => !this.pausedModules.has(module.manifest.moduleId) && !module.controller.signal.aborted) ?? [];
    const gameIds = new Set(modules.flatMap(module => module.manifest.games.map(game => game.gameId)));
    const icons = { home: Icons.AppstoreOutlined, search: Icons.CodeSandboxOutlined, settings: Icons.SettingOutlined, robot: Icons.RobotOutlined, cloud: Icons.CloudOutlined, install: Icons.InboxOutlined, proxy: Icons.ThunderboltOutlined, local: Icons.ImportOutlined, downloads: Icons.DownloadOutlined };
    const nav = modules.flatMap(module => module.manifest.navigation.map(item => ({ module, item }))).sort((a, b) => a.item.order - b.item.order);
    const settings = this.settingsRoutes(); const isSettings = settings.some(item => item.route === this.route);
    const items = nav.filter(({ item }) => !item.placement || item.placement === 'sidebar').map(({ module, item }) => {
      const page = module.manifest.pages.find(page => page.id === item.pageId)!;
      return { key: page.route, className: 'sidebar-route-item', label: h('span', { 'aria-label': page.title, className: 'sidebar-link' }, h(icons[item.icon ?? 'home']), h('span', { className: 'sidebar-label' }, page.title)) };
    });
    if (settings.length) items.push({ key: '@settings', className: 'sidebar-settings-item', label: h('span', { 'aria-label': '设置', className: 'sidebar-link' }, h(Icons.SettingOutlined), h('span', { className: 'sidebar-label' }, '设置')) });
    this.sidebarView = mountReactView(this.navigation, h(Antd.Menu, { mode: 'inline', selectedKeys: [isSettings ? '@settings' : this.route], items, onClick: ({ key }) => { void this.navigate(key === '@settings' ? isSettings ? this.route : settings[0].route : key); } }));
    this.settingsTabs.hidden = !isSettings;
    this.settingsView = mountReactView(this.settingsTabs, h(Antd.Tabs, { size: 'small', activeKey: this.route, items: settings.map(item => ({ key: item.route, label: item.title })), onChange: route => { void this.navigate(route); } }));
    for (const module of modules) for (const command of module.manifest.commands) {
      const button = kit.button(command.title, module.context.guard(async () => {
        const result = await module.context.request(command.method, command.params, crypto.randomUUID()); module.context.notify(JSON.stringify(result));
      }));
      button.disabled = !!command.enableWhen && !gameIds.has(command.enableWhen.gameId); this.commands.append(button);
    }
    window.dispatchEvent(new CustomEvent('mintcat-ui-contributions', { detail: { icons: [...new Set([...nav.map(({ item }) => item.icon).filter(Boolean), ...(settings.length ? ['settings'] : [])])], games: modules.flatMap(module => module.manifest.games.map(game => ({ gameId: game.gameId, displayName: game.displayName }))) } }));
  }
  private async openAuxiliary(owner: LoadedModule, route: string): Promise<void> {
    if (this.closed || this.pausedModules.has(owner.manifest.moduleId) || owner.controller.signal.aborted) return;
    if (this.auxiliary && !this.allowDiscard(new Set([this.auxiliary.owner.manifest.moduleId]))) return;
    this.unmountAuxiliary();
    const page = owner.manifest.pages.find(page => page.route === route);
    if (!page) return;
    const controller = new AbortController(); const lifetime = new Disposables();
    const mounted: MountedPage = { owner, controller, lifetime }; this.auxiliary = mounted;
    const pageRoot = kit.element('section'); pageRoot.dataset.module = owner.manifest.moduleId; this.auxiliaryRoot.replaceChildren(pageRoot);
    try {
      const disposable = await (owner.exports[page.export] as PageMount)(pageRoot, this.context(owner, controller.signal, lifetime));
      if (!disposable || typeof disposable.dispose !== 'function') throw new Error('Signed UI panel mount must return a Disposable');
      if (this.auxiliary !== mounted || controller.signal.aborted) disposable.dispose(); else mounted.disposable = disposable;
    } catch (error) {
      if (!controller.signal.aborted) { this.unmountAuxiliary(); this.showNotice(`Agent 面板加载失败：${message(error)}`); }
    }
  }
  private unmountAuxiliary(): void {
    const mounted = this.auxiliary; this.auxiliary = undefined;
    if (!mounted) return;
    mounted.controller.abort();
    try { mounted.disposable?.dispose(); } catch (error) { this.showNotice(`Panel cleanup failed: ${message(error)}`); }
    mounted.lifetime.dispose(); this.auxiliaryRoot.replaceChildren();
  }
  async navigate(route: string, force = false): Promise<void> {
    if (this.closed || !this.generation) return;
    if (!force && !this.allowDiscard(new Set([this.mounted, this.auxiliary].filter((page): page is MountedPage => !!page).map(page => page.owner.manifest.moduleId)))) { history.replaceState(null, '', `#${this.route}`); return; }
    this.pageFailed = false;
    this.unmountPage();
    const generation = this.generation;
    let owner = generation.modules.find(module => module.manifest.pages.some(page => page.route === route) || module.manifest.settings.some(section => `/${module.manifest.moduleId}/settings/${encodeURIComponent(section.id)}` === route));
    if (!owner) {
      const navigation = generation.modules.filter(module => !this.pausedModules.has(module.manifest.moduleId) && !module.controller.signal.aborted).flatMap(module => module.manifest.navigation.filter(item => !item.placement || item.placement === 'sidebar').map(item => ({ module, item }))).sort((a, b) => a.item.order - b.item.order);
      const fallback = navigation.find(entry => entry.item.icon === 'home') ?? navigation[0];
      owner = fallback?.module;
      route = owner?.manifest.pages.find(page => page.id === fallback!.item.pageId)?.route ?? '';
    }
    this.route = route; this.session.set('workbench/route', 1, route);
    this.renderSlots();
    history.replaceState(null, '', route ? `#${route}` : location.pathname);
    if (!owner) { this.content.textContent = 'No signed UI modules enabled. Use Signed modules or rescue controls.'; return; }
    // Mount only the verified published candidate while quiesced; context.check
    // still fences its mutations until the page has mounted and READY is accepted.
    const awaitingGeneration = this.pausedModules.has(owner.manifest.moduleId) && this.barrier?.newGenerationId !== generation.native.generationId;
    if (awaitingGeneration || owner.controller.signal.aborted) { this.content.textContent = 'This module is waiting for its signed update. Other scopes remain available.'; return; }
    const controller = new AbortController(); const lifetime = new Disposables();
    const pageRoot = kit.element('section'); pageRoot.dataset.module = owner.manifest.moduleId; this.content.replaceChildren(pageRoot);
    const mounted: MountedPage = { owner, controller, lifetime };
    this.mounted = mounted;
    const context = this.context(owner, controller.signal, lifetime);
    try {
      const page = owner.manifest.pages.find(page => page.route === route);
      const section = owner.manifest.settings.find(section => `/${owner!.manifest.moduleId}/settings/${encodeURIComponent(section.id)}` === route);
      const disposable = page ? await (owner.exports[page.export] as PageMount)(pageRoot, context) : await mountGameSettings(pageRoot, context, section!.gameId, section!.fields);
      if (!disposable || typeof disposable.dispose !== 'function') throw new Error('Signed UI page mount must return a Disposable');
      if (this.mounted !== mounted || controller.signal.aborted) disposable.dispose(); else mounted.disposable = disposable;
    } catch (error) { if (!controller.signal.aborted) this.pageError(error); }
  }
  private pageError(error: unknown): void {
    this.pageFailed = true;
    this.unmountPage();
    this.content.replaceChildren(kit.element('h2', 'This module page failed'), kit.element('p', message(error)), kit.button('Retry page', () => { void this.navigate(this.route, true); }));
  }
  private unmountPage(): void {
    this.unmountAuxiliary();
    const mounted = this.mounted; this.mounted = undefined;
    if (!mounted) return;
    mounted.controller.abort();
    this.queries.releasePending(`${mounted.owner.manifest.moduleId}/`);
    try { mounted.disposable?.dispose(); } catch (error) { this.notice.textContent = `Page cleanup failed: ${message(error)}`; }
    mounted.lifetime.dispose();
  }
  private async loadStyles(module: LoadedModule, active: boolean): Promise<void> {
    for (const css of module.manifest.css) {
      if (this.closed || module.controller.signal.aborted) throw new DOMException('UI module unloaded', 'AbortError');
      const link = kit.element('link'); link.rel = 'stylesheet'; link.media = active ? 'all' : 'not all'; link.href = module.context.resource(css);
      await new Promise<void>((resolve, reject) => {
        const signal = module.controller.signal;
        const finish = (error?: Error): void => {
          link.onload = link.onerror = null;
          signal.removeEventListener('abort', abort);
          if (error) reject(error); else resolve();
        };
        const abort = (): void => finish(new DOMException('UI module unloaded', 'AbortError'));
        link.onload = () => finish(); link.onerror = () => finish(new Error(`Signed stylesheet failed: ${css}`));
        signal.addEventListener('abort', abort, { once: true });
        module.lifetime.add({ dispose: () => link.remove() });
        document.head.append(link);
      });
    }
  }
  private async resume(generation: Generation): Promise<void> {
    for (const module of generation.modules) {
      if (this.closed) return;
      if (!module.controller.signal.aborted) continue;
      module.controller = new AbortController(); module.lifetime = new Disposables();
      module.context = this.context(module, module.controller.signal, module.lifetime);
      if (module.exports.activate) module.lifetime.add(await module.exports.activate(module.context));
      await this.loadStyles(module, true);
    }
  }
  private async release(generation: Generation): Promise<void> {
    for (const module of generation.modules.slice().reverse()) {
      if (this.generation?.modules.includes(module)) continue;
      module.controller.abort(); this.queries.releasePending(`${module.manifest.moduleId}/`); module.lifetime.dispose();
    }
    // ESM has no unload primitive: disposed exports and lazy imports may still
    // reference immutable resources. Their native/UI leases remain until realm
    // destruction, not merely until their DOM contribution is removed.
  }
  private async rebuildWebView(): Promise<void> {
    this.dispose(); await this.cleanup;
    try {
      for (const [token] of this.nativeGenerations) await invoke('ui_close_generation', { token });
      this.nativeGenerations.clear();
    } catch (error) { this.rescue(error); throw error; }
    location.reload();
  }
  dispose(): void {
    if (this.closed) return;
    this.closed = true; this.unmountPage();
    if (this.preparing) for (const module of this.preparing.modules) { module.controller.abort(); module.lifetime.dispose(); }
    clearInterval(this.barrierTimer);
    window.removeEventListener('hashchange', this.hashHandler); window.removeEventListener('beforeunload', this.beforeUnload);
    window.removeEventListener('mintcat-ui-navigate', this.navigationHandler);
    window.removeEventListener('mintcat-ui-render-error', this.renderErrorHandler);
    this.sidebarView?.dispose(); this.settingsView?.dispose(); this.modulesPanel.remove();
    this.navigation.remove();
    window.dispatchEvent(new CustomEvent('mintcat-ui-contributions', { detail: { icons: [], games: [] } }));
    const generation = this.generation; this.generation = undefined;
    if (generation) this.cleanup = this.release(generation);
    this.root.replaceChildren();
  }
}
function message(error: unknown): string { return error instanceof Error ? error.message : String(error); }
