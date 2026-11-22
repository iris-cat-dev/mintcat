import { invoke } from '@tauri-apps/api/core';
import { getCurrentWindow } from '@tauri-apps/api/window';
import './ui-runtime';
import { presentUiError, renderUiError } from '@mintcat/sdk-ui';
import { mountChrome } from './chrome';
import { callDaemon, hasNativeDaemonConnector, readDaemonStatus, requestApplication, runDaemonDoctor } from './daemon';
import type { Workbench } from './workbench';
import { initializeWindowSurface } from './window-surface';

export async function mountDesktop(app: HTMLElement, configuredDataDir?: string): Promise<void> {
const native = hasNativeDaemonConnector();
if (native && getCurrentWindow().label === 'first-run-setup') {
  try {
    const [{ Workbench }, { dataDir }, legacyPath] = await Promise.all([
      import('./workbench'),
      configuredDataDir ? Promise.resolve({ dataDir: configuredDataDir }) : invoke<{ dataDir: string }>('desktop_environment'),
      invoke<string | null>('detect_legacy_setup_database').catch(() => null),
    ]);
    const guide = new Workbench(app, dataDir, error => renderUiError(app, presentUiError(error)), undefined, undefined, undefined, {
      mode: 'guide', legacyPath: legacyPath ?? undefined, close: () => invoke('close_setup_window'),
    });
    window.addEventListener('pagehide', () => guide.dispose(), { once: true });
    await guide.refresh();
  } catch (error) {
    renderUiError(app, presentUiError(error));
  }
  return;
}
const disposeWindowSurface = initializeWindowSurface(native);
window.addEventListener('pagehide', disposeWindowSurface, { once: true });
const shell = document.createElement('section'); shell.className = 'app';
const header = document.createElement('header'); header.className = 'app-header';
const sidebar = document.createElement('aside'); sidebar.className = 'app-sidebar-host';
const brand = document.createElement('div'); brand.className = 'sidebar-brand-host';
const navigation = document.createElement('div'); navigation.className = 'sidebar-navigation-host';
navigation.id = 'mc-sidebar-navigation';
sidebar.append(brand, navigation);
const surface = document.createElement('main'); surface.className = 'app-workspace';
const footer = document.createElement('footer'); footer.className = 'app-footer';
const rescue = document.createElement('details'); rescue.className = 'rescue';
const summary = document.createElement('summary'); summary.textContent = '后台状态与恢复';
const description = document.createElement('p'); description.className = 'rescue-directory';
description.textContent = `Isolated vNext data: ${configuredDataDir || 'Not configured'}. No legacy desktop database is opened.`;
const status = document.createElement('div'); status.role = 'status'; status.className = 'rescue-status';
const recoveryActions = document.createElement('div'); recoveryActions.className = 'rescue-recovery-actions';
const output = document.createElement('pre'); output.className = 'rescue-output';
const controls = document.createElement('div'); controls.className = 'controls';
const modules = document.createElement('div'); modules.className = 'rescue-modules';
rescue.append(summary, description, status, controls, modules, recoveryActions, output);
shell.append(sidebar, header, surface, footer, rescue); app.append(shell);
const chrome = mountChrome(header, footer, brand, () => { rescue.open = !rescue.open; }, native);
const openRescueHandler = (): void => { rescue.open = true; };
window.addEventListener('mintcat-open-rescue', openRescueHandler);
const placeholder = document.createElement('div'); placeholder.className = 'workbench-loading';
placeholder.textContent = native ? '正在加载工作台…' : '浏览器预览只显示桌面框架；游戏、模组与任务需要原生后台。';
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
    renderUiError(status, presentUiError(error));
    rescue.open = true;
    return;
  }
}
if (!native || !configuredDataDir) {
  status.textContent = native ? 'Set VITE_MINTCAT_VNEXT_DATA_DIR to the isolated vNext directory before native use.' : 'Browser preview: shell and rescue layout only. Daemon data and writes require the native desktop.';
  placeholder.textContent = status.textContent;
  window.dispatchEvent(new CustomEvent('mintcat-ui-notice', { detail: status.textContent }));
} else {
  const dataDir = configuredDataDir;
  const showError = (cause: unknown): void => {
    rescue.open = true;
    const error = presentUiError(cause);
    renderUiError(status, error);
    window.dispatchEvent(new CustomEvent('mintcat-ui-notice', { detail: `${error.title}：${error.description}` }));
  };
  const button = (label: string, action: () => Promise<unknown>, variant?: 'primary' | 'warn'): HTMLButtonElement => {
    const element = document.createElement('button'); element.type = 'button'; element.textContent = label;
    if (variant) element.className = `rescue-btn--${variant}`;
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
        workbench = new framework.Workbench(surface, dataDir, showError, modules, navigation, header.querySelector<HTMLElement>('#mc-page-toolbar') ?? undefined, {
          mode: 'main', open: () => invoke('open_setup_window'),
        });
      }
      await workbench.refresh();
      if (epoch === shellGeneration) status.textContent = 'Workbench loaded. Persistent daemon jobs continue independently.';
    })().finally(() => { if (starting === pending) starting = undefined; });
    starting = pending;
    return pending;
  };
  button('Status', async () => { recoveryActions.replaceChildren(); const result = await readDaemonStatus(dataDir); status.textContent = `Daemon ready · ${result.generationId}`; output.textContent = JSON.stringify(result, null, 2); });
  button('Start and diagnose', async () => { recoveryActions.replaceChildren(); const result = await runDaemonDoctor(dataDir); output.textContent = JSON.stringify(result, null, 2); status.textContent = result.healthy ? 'Daemon diagnostics passed' : 'Daemon needs recovery'; });
  button('View daemon log', async () => {
    recoveryActions.replaceChildren();
    const log = await invoke<{ text: string; truncated: boolean }>('rescue_log', { dataDir });
    output.textContent = `${log.truncated ? '[Last 64 KiB]\\n' : ''}${log.text}`;
  });
  button('Local confirmed recovery sets', async () => {
    recoveryActions.replaceChildren();
    const result = await callDaemon(dataDir, 'updates.operations', {});
    output.replaceChildren(document.createTextNode(JSON.stringify(result, null, 2)));
    for (const operation of result.operations) {
      const { operationId, scopeId, previous } = operation;
      if (typeof operationId !== 'string' || typeof scopeId !== 'string') throw new Error('Invalid recovery operation identity');
      if (previous == null) continue;
      if (typeof previous !== 'object' || !('confirmed' in previous) || typeof previous.confirmed !== 'boolean') throw new Error('Invalid previous recovery snapshot');
      if (!previous.confirmed) continue;
      if (!('generationId' in previous) || typeof previous.generationId !== 'string') throw new Error('Missing confirmed recovery generation');
      const restore = document.createElement('button'); restore.type = 'button';
      restore.className = 'rescue-restore-btn';
      restore.textContent = `Restore local confirmed ${scopeId}: ${previous.generationId}`;
      restore.addEventListener('click', () => {
        if (!window.confirm(`Restore the confirmed local set for ${scopeId}? Runtime revision and data rollback gates will be enforced.`)) return;
        restore.disabled = true;
        callDaemon(dataDir, 'updates.rollback', { operationId, reason: 'Human rescue UI selected confirmed local recovery set' }, crypto.randomUUID())
          .then(job => { status.textContent = `Recovery task ${job.jobId} started in daemon. Reload workbench after it completes.`; })
          .catch(showError).finally(() => { restore.disabled = false; });
      });
      recoveryActions.append(restore);
    }
  });
  button('Load workbench', startWorkbench, 'primary');
  button('Safe mode', async () => { shellGeneration++; starting = undefined; workbench?.dispose(); workbench = undefined; status.textContent = 'Safe mode: workbench closed. Persistent daemon jobs are unchanged.'; }, 'warn');
  button('Game plugin inventory', async () => {
    recoveryActions.replaceChildren();
    const list = await requestApplication(dataDir, 'modules.list', {});
    output.replaceChildren(document.createTextNode(JSON.stringify(list, null, 2)));
  });
  const setupListener = getCurrentWindow().listen('mintcat-setup-closed', () => workbench?.refreshAfterSetup());
  window.addEventListener('pagehide', () => { void setupListener.then(release => release()).catch(() => {}); }, { once: true });
  void setupListener.catch(showError);
  void startWorkbench().catch(showError);
}
window.addEventListener('pagehide', () => { window.removeEventListener('mintcat-open-rescue', openRescueHandler); shellGeneration++; starting = undefined; workbench?.dispose(); chrome.dispose(); }, { once: true });
}
