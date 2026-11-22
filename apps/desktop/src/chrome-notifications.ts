import { getSharedRuntime, getUiLanguage, tr } from '@mintcat/sdk-ui';
import type { ReactElement } from 'react';
import './chrome-notifications.css';

interface Notice { id: number; title: string; time: Date; tone: 'info' | 'error' }
const notices = new EventTarget();
let sequence = 0;
const historyLimit = 40;
const timeFormat = new Intl.DateTimeFormat(getUiLanguage(), { month: 'numeric', day: 'numeric', hour: '2-digit', minute: '2-digit' });
const countFormat = new Intl.NumberFormat(getUiLanguage());

// Only user-facing summaries belong here; raw request parameters and diagnostics stay out of history.
export function publishNotification(title: string, tone: Notice['tone'] = 'info'): void {
  const notice: Notice = { id: ++sequence, title, time: new Date(), tone };
  notices.dispatchEvent(new CustomEvent('notice', { detail: notice }));
}

export function NotificationsButton(): ReactElement {
  const { React, Antd, Icons } = getSharedRuntime();
  const h = React.createElement;
  const [items, setItems] = React.useState<Notice[]>([]);
  const [open, setOpen] = React.useState(false);
  const [lastRead, setLastRead] = React.useState(0);
  const trigger = React.useRef<HTMLButtonElement>(null);
  const panelId = React.useId();
  React.useEffect(() => {
    const add = (event: Event): void => {
      const item = (event as CustomEvent<Notice>).detail;
      setItems(previous => [item, ...previous.slice(0, historyLimit - 1)]);
    };
    notices.addEventListener('notice', add);
    return () => notices.removeEventListener('notice', add);
  }, []);
  const newest = items[0]?.id ?? 0;
  React.useEffect(() => { if (open) setLastRead(newest); }, [open, newest]);
  React.useEffect(() => {
    if (!open) return;
    const escape = (event: KeyboardEvent): void => {
      if (event.key === 'Escape') { setOpen(false); trigger.current?.focus(); }
    };
    window.addEventListener('keydown', escape);
    return () => window.removeEventListener('keydown', escape);
  }, [open]);
  const unread = open ? 0 : items.filter(item => item.id > lastRead).length;
  const label = tr('通知消息', 'Notifications');
  const content = h('section', { className: 'chrome-inbox', id: panelId, 'aria-label': tr('本次会话通知', 'Session notifications') },
    h('div', { className: 'chrome-inbox-heading' },
      h('strong', null, label),
      items.length ? h(Antd.Button, { type: 'text', size: 'small', onClick: () => { setItems([]); trigger.current?.focus(); } }, tr('清空', 'Clear')) : null),
    items.length ? h('ul', { className: 'chrome-inbox-list' }, ...items.map(item =>
      h('li', { key: item.id },
        h(item.tone === 'error' ? Icons.ExclamationCircleOutlined : Icons.InfoCircleOutlined, { 'aria-label': item.tone === 'error' ? tr('异常', 'Error') : tr('提示', 'Information'), className: 'chrome-inbox-kind' }),
        h('div', null, h('div', { className: 'chrome-inbox-message' }, item.title),
          h('time', { dateTime: item.time.toISOString() }, timeFormat.format(item.time))))))
      : h(Antd.Empty, { image: Antd.Empty.PRESENTED_IMAGE_SIMPLE, description: tr('本次会话暂无通知', 'No notifications this session') }));
  return h(Antd.Popover, { trigger: 'click', placement: 'bottomRight', open, onOpenChange: setOpen, content },
    h(Antd.Button, { ref: trigger, type: 'text', title: label,
      'aria-label': unread ? tr(`通知消息，${countFormat.format(unread)} 条未读`, `Notifications, ${countFormat.format(unread)} unread`) : label,
      'aria-expanded': open, 'aria-controls': open ? panelId : undefined,
      icon: h(Antd.Badge, { count: unread, size: 'small', overflowCount: 9, offset: [1, -1] }, h(Icons.BellOutlined)) }));
}
