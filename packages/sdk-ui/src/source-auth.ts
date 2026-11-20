export type RemoteSourceId = 'modio' | 'modcat' | 'nexusmods';

export interface SourceAuthGuide {
  readonly sourceId: RemoteSourceId;
  readonly sourceName: string;
  readonly description: string;
  readonly credentialLabel: string;
  readonly placeholder: string;
  readonly portalName: string;
  readonly portalUrl: string;
  readonly actionLabel: string;
  readonly steps: readonly string[];
}

export const SOURCE_NAMES: Readonly<Record<string, string>> = Object.freeze({
  modio: 'mod.io',
  modcat: 'ModCat',
  nexusmods: 'Nexus Mods',
});

const MAX_SOURCE_CREDENTIAL_LENGTH = 32768;

export const SOURCE_AUTH_GUIDES: Readonly<Record<RemoteSourceId, SourceAuthGuide>> = Object.freeze({
  modio: Object.freeze({
    sourceId: 'modio',
    sourceName: 'mod.io',
    description: '官方合作模组仓库连接器',
    credentialLabel: '个人访问令牌（Personal Access Token）',
    placeholder: '粘贴 mod.io Personal Access Token',
    portalName: 'mod.io Personal Access Tokens',
    portalUrl: 'https://mod.io/me/access#tokens',
    actionLabel: '前往 mod.io 获取凭据',
    steps: Object.freeze([
      '打开 mod.io 个人访问令牌页面并登录你的账号。',
      '点击 Generate Token，按需勾选 Read（浏览）或 Read + Write 权限，生成后复制完整令牌。',
      '将完整的 Personal Access Token 粘贴到凭据输入框，点击保存即可加密存入本机系统钥匙串并立即生效。',
    ]),
  }),
  nexusmods: Object.freeze({
    sourceId: 'nexusmods',
    sourceName: 'Nexus Mods',
    description: 'Nexus 开放模组生态接入',
    credentialLabel: '个人 API 密钥（Personal API Key）',
    placeholder: '粘贴 Nexus Mods Personal API Key',
    portalName: 'Nexus Mods API 访问设置',
    portalUrl: 'https://www.nexusmods.com/users/myaccount?tab=api%20access',
    actionLabel: '前往 Nexus Mods 获取密钥',
    steps: Object.freeze([
      '打开 Nexus Mods 账号的 API Access 设置页面并登录你的账号。',
      '在页面底部的 Personal API Key 区域生成或复制你的个人 API Key。',
      '将 API Key 粘贴到凭据输入框，点击保存即可加密存入本机系统钥匙串并立即生效。',
    ]),
  }),
  modcat: Object.freeze({
    sourceId: 'modcat',
    sourceName: 'ModCat',
    description: '中文社区镜像与索引服务',
    credentialLabel: 'ModCat OAuth 访问令牌',
    placeholder: '粘贴 ModCat 访问令牌',
    portalName: 'ModCat 官网',
    portalUrl: 'https://modcat.top',
    actionLabel: '前往 ModCat 获取凭据',
    steps: Object.freeze([
      '打开 ModCat 官网并登录你的账号。',
      '在个人中心或账号授权页面获取并复制你的 ModCat OAuth 访问令牌。',
      '将令牌粘贴到凭据输入框，点击保存即可加密存入本机系统钥匙串并立即生效。',
    ]),
  }),
});

export function isRemoteSourceId(sourceId: string): sourceId is RemoteSourceId {
  return sourceId === 'modio' || sourceId === 'modcat' || sourceId === 'nexusmods';
}

export function getSourceDisplayName(sourceId: string): string {
  return SOURCE_NAMES[sourceId] ?? sourceId;
}

export function getSourceAuthGuide(sourceId: string): SourceAuthGuide | undefined {
  return isRemoteSourceId(sourceId) ? SOURCE_AUTH_GUIDES[sourceId] : undefined;
}

export function validateSourceCredentialInput(raw: string): { ok: true; value: string } | { ok: false; message: string } {
  const trimmed = raw.trim();
  if (!trimmed) {
    return { ok: false, message: '请输入有效的凭据后再保存。' };
  }
  if (trimmed.length > MAX_SOURCE_CREDENTIAL_LENGTH) {
    return { ok: false, message: '凭据长度超出限制，请确认仅粘贴令牌本身。' };
  }
  if (/[^\x21-\x7E]/.test(trimmed)) {
    return { ok: false, message: '凭据不能包含空格、换行或非 ASCII 字符，请仅粘贴官方生成的令牌或 API Key 本身。' };
  }
  return { ok: true, value: trimmed };
}

export async function copyTextToClipboard(text: string): Promise<boolean> {
  if (typeof navigator !== 'undefined' && navigator.clipboard && typeof navigator.clipboard.writeText === 'function') {
    try {
      await navigator.clipboard.writeText(text);
      return true;
    } catch { /* Fall through to selection copy when Clipboard API is restricted. */ }
  }
  if (typeof document !== 'undefined' && typeof document.execCommand === 'function') {
    const area = document.createElement('textarea');
    area.value = text;
    area.setAttribute('readonly', '');
    area.style.position = 'fixed';
    area.style.top = '0';
    area.style.left = '0';
    area.style.opacity = '0';
    document.body.append(area);
    area.focus();
    area.select();
    area.setSelectionRange(0, text.length);
    try {
      return document.execCommand('copy');
    } catch {
      return false;
    } finally {
      area.remove();
    }
  }
  return false;
}
