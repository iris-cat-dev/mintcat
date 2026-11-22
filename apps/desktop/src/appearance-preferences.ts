import { isTauri } from '@tauri-apps/api/core';
import { getSharedRuntime, getUiLanguage, saveUiLanguage, tr, type UiLanguage } from '@mintcat/sdk-ui';
import { getWindowGlassEnabled, setWindowGlassEnabled } from './window-surface';

export function LanguagePreference() {
  const { React, Antd } = getSharedRuntime();
  const h = React.createElement;
  const current = getUiLanguage();
  const [language, setLanguage] = React.useState(current);
  const { message } = Antd.App.useApp();
  const label = tr('界面语言', 'Interface language');
  return h('div', { className: 'settings-flat-row' },
    h('div', { className: 'settings-flat-row-info' },
      h('div', { className: 'settings-flat-row-title' }, label),
      h('div', { className: 'settings-flat-row-desc' }, tr('应用后重新加载当前页面，并记住所选语言。', 'Apply to reload this page in the selected language. Your choice is saved.'))),
    h(Antd.Space, { wrap: true },
      h(Antd.Select<UiLanguage>, {
        'aria-label': label, value: language, onChange: setLanguage,
        options: [{ value: 'zh-CN', label: '简体中文' }, { value: 'en-US', label: 'English' }],
      }),
      h(Antd.Button, { disabled: language === current, onClick: () => {
        try { saveUiLanguage(language); }
        catch { void message.error(tr('无法保存语言偏好。', 'Could not save the language preference.')); return; }
        window.location.reload();
      } }, tr('应用语言', 'Apply language'))));
}

export function WindowGlassPreference() {
  const { React, Antd } = getSharedRuntime();
  const h = React.createElement;
  const [enabled, setEnabled] = React.useState(getWindowGlassEnabled);
  const { message } = Antd.App.useApp();
  const label = tr('原生窗口毛玻璃材质', 'Native window blur');
  const descriptionId = React.useId();
  const supported = isTauri() && document.documentElement.dataset.windowPlatform !== 'linux';
  return h('div', { className: 'settings-flat-row' },
    h('div', { className: 'settings-flat-row-info' },
      h('div', { className: 'settings-flat-row-title' }, label),
      h('div', { id: descriptionId, className: 'settings-flat-row-desc' }, supported
        ? tr('开启时跟随系统「减弱透明度」设置。关闭开关或系统不支持毛玻璃时，使用纯色背景。', 'When on, follows the system’s Reduce Transparency setting. Uses a solid background when off or when native blur is unsupported.')
        : tr('原生毛玻璃仅在受支持的桌面环境中可用。', 'Native blur is only available in supported desktop environments.'))),
    h(Antd.Switch, { ...{ 'aria-label': label, 'aria-describedby': descriptionId },
      checked: supported && enabled, disabled: !supported, onChange: checked => {
        try { setWindowGlassEnabled(checked); setEnabled(checked); }
        catch { void message.error(tr('无法保存窗口材质偏好。', 'Could not save the window material preference.')); }
      } }));
}
