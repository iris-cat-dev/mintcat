import { invoke } from '@tauri-apps/api/core';
import { listen, type UnlistenFn } from '@tauri-apps/api/event';
import { getUiTheme } from '@mintcat/sdk-ui';

interface WindowSurface {
  platform: 'macos' | 'windows' | 'linux';
  material: 'glass' | 'solid';
}

export function initializeWindowSurface(native: boolean): () => void {
  const root = document.documentElement;
  root.dataset.windowMaterial = 'solid';
  if (!native) {
    delete root.dataset.windowPlatform;
    return () => {};
  }
  // Reserve the traffic-light area before the first asynchronous native reply.
  root.dataset.windowPlatform = /Mac/i.test(navigator.platform) ? 'macos' : /Win/i.test(navigator.platform) ? 'windows' : 'linux';
  const transparency = window.matchMedia('(prefers-reduced-transparency: reduce)');
  let disposed = false;
  let revision = 0;
  let pending = false;
  let settled: Promise<void> = Promise.resolve();
  let unlisten: UnlistenFn | undefined;
  const synchronize = async (): Promise<void> => {
    if (pending || disposed) return;
    pending = true;
    try {
      let applied: number;
      do {
        applied = revision;
        try {
          const result = await invoke<WindowSurface>('desktop_window_surface', {
            dark: getUiTheme() === 'Dark',
            reduceTransparency: transparency.matches,
            observe: true,
          });
          if (!disposed && applied === revision) {
            root.dataset.windowPlatform = result.platform;
            root.dataset.windowMaterial = result.material;
          }
        } catch (error) {
          // The fully opaque CSS fallback remains readable when native effects fail.
          console.warn('Native window surface unavailable; keeping solid background', error);
        }
      } while (!disposed && applied !== revision);
    } finally { pending = false; }
  };
  const changed = (): void => {
    if (disposed) return;
    revision++;
    root.dataset.windowMaterial = 'solid';
    if (!pending) settled = synchronize();
  };
  window.addEventListener('mintcat-ui-theme', changed);
  transparency.addEventListener('change', changed);
  void listen('mintcat-window-transparency', changed).then(release => {
    if (disposed) release();
    else { unlisten = release; changed(); }
  }).catch(error => { console.warn('Native transparency listener unavailable', error); });
  changed();
  return (): void => {
    if (disposed) return;
    disposed = true;
    root.dataset.windowMaterial = 'solid';
    window.removeEventListener('mintcat-ui-theme', changed);
    transparency.removeEventListener('change', changed);
    unlisten?.();
    void settled.then(() => invoke('desktop_window_surface', { dark: getUiTheme() === 'Dark', reduceTransparency: true, observe: false })).catch(() => {
      // Closing the native window may release IPC before its final callback.
    });
  };
}
