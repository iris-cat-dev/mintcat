export type UiLanguage = 'zh-CN' | 'en-US';

const storageKey = 'mintcat.ui.language';

// Read once per window. Changing the preference requires a reload so mounted
// pages, non-React views, and startup labels always use the same language.
let current: UiLanguage | undefined;

export function getUiLanguage(): UiLanguage {
  if (current) return current;
  try { current = localStorage.getItem(storageKey) === 'en-US' ? 'en-US' : 'zh-CN'; }
  catch { current = 'zh-CN'; }
  return current;
}

export function saveUiLanguage(language: UiLanguage): void {
  localStorage.setItem(storageKey, language);
}

export function tr(zh: string, en: string): string {
  return getUiLanguage() === 'en-US' ? en : zh;
}
