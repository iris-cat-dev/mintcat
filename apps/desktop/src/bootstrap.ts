import { invoke } from '@tauri-apps/api/core';
import './ui-runtime';
import { mountChrome } from './chrome';
import { hasNativeDaemonConnector, readDaemonStatus, requestApplication, runDaemonDoctor } from './daemon';
import type { Workbench, ModuleList } from './workbench';
import { initializeWindowSurface } from './window-surface';

export async function mountDesktop(app: HTMLElement, configuredDataDir?: string): Promise<void> {
const native = hasNativeDaemonConnector();
const disposeWindowSurface = initializeWindowSurface(native);
window.addEventListener('pagehide', disposeWindowSurface, { once: true });
const shell = document.createElement('section'); shell.className = 'app';
const header = document.createElement('header'); header.className = 'app-header';
const sidebar = document.createElement('aside'); sidebar.className = 'app-sidebar-host';
const brand = document.createElement('div'); brand.className = 'sidebar-brand-host';
const navigation = document.createElement('div'); navigation.className = 'sidebar-navigation-host';
sidebar.append(brand, navigation);
const surface = document.createElement('main'); surface.className = 'app-workspace';
const footer = document.createElement('footer'); footer.className = 'app-footer';
const rescue = document.createElement('details'); rescue.className = 'rescue';
const summary = document.createElement('summary'); summary.textContent = '后台状态与恢复';
const description = document.createElement('p'); description.className = 'rescue-directory';
description.textContent = `Isolated vNext data: ${configuredDataDir || 'Not configured'}. No legacy desktop database is opened.`;
const status = document.createElement('p'); status.role = 'status';
const output = document.createElement('pre'); output.className = 'rescue-output';
const controls = document.createElement('div'); controls.className = 'controls';
const modules = document.createElement('div');
rescue.append(summary, description, status, controls, modules, output);
shell.append(sidebar, header, surface, footer, rescue); app.append(shell);
const chrome = mountChrome(header, footer, brand, () => { rescue.open = !rescue.open; }, native);
const placeholder = document.createElement('div'); placeholder.className = 'workbench-loading';
placeholder.textContent = native ? '正在加载已签名模块…' : '浏览器预览只显示桌面框架；游戏、模组与任务需要原生后台。';
surface.append(placeholder);
let workbench: Workbench | undefined;
let shellGeneration = 0;
let starting: Promise<void> | undefined;
if (native && !configuredDataDir) {
  try {
    const environment = await invoke<{ dataDir: string }>('desktop_environment');
    configuredDataDir = environment.dataDir;
    description.textContent = `Isolated vNext data: ${configuredDataDir}. No legacy desktop database is opened.`;
  } catch (error) {
    status.textContent = `Native environment unavailable: ${error instanceof Error ? error.message : String(error)}`;
    rescue.open = true;
    return;
  }
}
if (!native || !configuredDataDir) {
  status.textContent = native ? 'Set VITE_MINTCAT_VNEXT_DATA_DIR to the isolated vNext directory before native use.' : 'Browser preview: shell and rescue layout only. Native signed modules, daemon data and writes are unavailable.';
  placeholder.textContent = status.textContent;
  window.dispatchEvent(new CustomEvent('mintcat-ui-notice', { detail: status.textContent }));
} else {
  const dataDir = configuredDataDir;
  const showError = (error: unknown): void => { rescue.open = true; status.textContent = error instanceof Error ? error.message : String(error); window.dispatchEvent(new CustomEvent('mintcat-ui-notice', { detail: status.textContent })); };
  const button = (label: string, action: () => Promise<unknown>): HTMLButtonElement => {
    const element = document.createElement('button'); element.type = 'button'; element.textContent = label;
    element.addEventListener('click', () => {
      element.disabled = true;
      action().catch(showError).finally(() => { element.disabled = false; });
    });
    controls.append(element); return element;
  };
  const startWorkbench = (): Promise<void> => {
    if (starting) return starting;
    const epoch = shellGeneration;
    let pending: Promise<void>;
    pending = (async () => {
      if (!workbench) {
        // Rescue must survive failure to fetch or initialize the workbench chunk.
        const framework = await import('./workbench');
        if (epoch !== shellGeneration) return;
        surface.replaceChildren();
        workbench = new framework.Workbench(surface, dataDir, showError, modules, navigation, header.querySelector<HTMLElement>('#mc-page-toolbar') ?? undefined);
      }
      await workbench.refresh();
    })().finally(() => { if (starting === pending) starting = undefined; });
    starting = pending;
    return pending;
  };
  button('Status', async () => { const result = await readDaemonStatus(dataDir); status.textContent = `Daemon ready · ${result.generationId}`; output.textContent = JSON.stringify(result, null, 2); });
  button('Start and diagnose', async () => { const result = await runDaemonDoctor(dataDir); output.textContent = JSON.stringify(result, null, 2); status.textContent = result.healthy ? 'Daemon diagnostics passed' : 'Daemon needs recovery'; });
  button('View daemon log', async () => {
    const log = await invoke<{ text: string; truncated: boolean }>('rescue_log', { dataDir });
    output.textContent = `${log.truncated ? '[Last 64 KiB]\\n' : ''}${log.text}`;
  });
  button('Local confirmed recovery sets', async () => {
    const operations = await requestApplication<{ items: Array<{ operationId: string; scopeId: string; phase: string; previous?: { generationId: string; confirmed: boolean } }> }>(dataDir, 'updates.operations', {});
    output.replaceChildren(document.createTextNode(JSON.stringify(operations, null, 2)));
    for (const operation of operations.items.filter(item => item.previous?.confirmed)) {
      const restore = document.createElement('button'); restore.type = 'button';
      restore.textContent = `Restore local confirmed ${operation.scopeId}: ${operation.previous!.generationId}`;
      restore.addEventListener('click', () => {
        if (!window.confirm(`Restore the confirmed local set for ${operation.scopeId}? Runtime revision and data rollback gates will be enforced.`)) return;
        restore.disabled = true;
        requestApplication<{ jobId: string }>(dataDir, 'updates.rollback', { operationId: operation.operationId, reason: 'Human rescue UI selected confirmed local recovery set' }, crypto.randomUUID())
          .then(job => { status.textContent = `Recovery task ${job.jobId} started in daemon. Reload signed workbench after it completes.`; })
          .catch(showError).finally(() => { restore.disabled = false; });
      });
      output.append(restore);
    }
  });
  button('Load signed workbench', startWorkbench);
  button('Safe mode', async () => { shellGeneration++; starting = undefined; workbench?.dispose(); workbench = undefined; status.textContent = 'Safe mode: UI modules unloaded. Persistent daemon jobs are unchanged.'; });
  button('Signed module inventory', async () => {
    const list = await requestApplication<ModuleList>(dataDir, 'modules.list', {});
    output.replaceChildren(document.createTextNode(JSON.stringify(list, null, 2)));
    for (const module of list.modules) {
      const enable = document.createElement('button'); enable.type = 'button'; enable.textContent = `Enable UI: ${module.moduleId}`;
      enable.addEventListener('click', () => {
        enable.disabled = true;
        requestApplication(dataDir, 'modules.setEnabled', { moduleId: module.moduleId, enabled: true, expectedGenerationId: list.generationId }, crypto.randomUUID())
          .then(startWorkbench).catch(showError).finally(() => { enable.disabled = false; });
      });
      output.append(enable);
    }
  });
  void startWorkbench().catch(showError);
}
window.addEventListener('pagehide', () => { shellGeneration++; starting = undefined; workbench?.dispose(); chrome.dispose(); }, { once: true });
}
