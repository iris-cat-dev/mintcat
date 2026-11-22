import type { InstallationSummary } from '@mintcat/sdk-ts';
import type { UiContext } from '@mintcat/sdk-ui';
import type { DeployTarget } from './library-model';

export interface AvailableGame {
  gameId: string;
  displayName: string;
  variants: Array<{ variantId: string; displayName: string; store?: string; storeAppId?: string }>;
  installations: InstallationSummary[];
  localRemoteGameKey?: string;
  sourceIds: string[];
}

export function librarySession(context: UiContext) {
  return context.state('library', 1, { gameId: '', profileId: '', query: '', jobs: [] as Array<{ jobId: string; profileId: string; deployTarget?: DeployTarget }> });
}

type GameTarget = { variantId: string; installationId: string };

export function selectedTarget(context: UiContext, game: AvailableGame): GameTarget {
  const saved = context.state<GameTarget>(`game-target/${game.gameId}`, 1, { variantId: '', installationId: '' });
  return {
    variantId: game.variants.find(item => item.variantId === saved.variantId)?.variantId ?? game.variants[0]?.variantId ?? '',
    installationId: game.installations.find(item => item.installationId === saved.installationId)?.installationId ?? game.installations[0]?.installationId ?? '',
  };
}

export function saveTarget(context: UiContext, gameId: string, target: GameTarget): void {
  context.saveState(`game-target/${gameId}`, 1, target);
}

export async function loadAvailableGames(context: UiContext): Promise<AvailableGame[]> {
  const [inventory, first] = await Promise.all([
    context.call('modules.list', {}), context.call('games.list', { pageSize: 100 }),
  ]);
  const registered = [...first.items];
  let cursor = first.nextCursor;
  while (cursor) {
    const page = await context.call('games.list', { pageSize: 100, cursor });
    registered.push(...page.items); cursor = page.nextCursor;
  }
  const games = new Map<string, AvailableGame>(registered.map(game => [game.gameId, {
    gameId: game.gameId, displayName: game.displayName, installations: game.installations, variants: [], sourceIds: [],
  }]));
  for (const { enabled, game } of inventory.modules) {
    if (!enabled || !game || !Array.isArray(game.descriptor.variants)) continue;
    const variants: AvailableGame['variants'] = game.descriptor.variants.flatMap(value => {
      if (!value || typeof value !== 'object' || Array.isArray(value)) return [];
      const { variantId, displayName, store, storeAppId } = value as Record<string, unknown>;
      return typeof variantId === 'string' && typeof displayName === 'string'
        ? [{ variantId, displayName, ...(typeof store === 'string' ? { store } : {}), ...(typeof storeAppId === 'string' ? { storeAppId } : {}) }]
        : [];
    });
    const sourceIds: string[] = [];
    let localRemoteGameKey: string | undefined;
    if (Array.isArray(game.descriptor.sourceBindings)) {
      for (const value of game.descriptor.sourceBindings) {
        if (!value || typeof value !== 'object' || Array.isArray(value)) continue;
        const { sourceId, remoteGameKey } = value as Record<string, unknown>;
        if (typeof sourceId !== 'string') continue;
        sourceIds.push(sourceId);
        if (sourceId === 'local' && typeof remoteGameKey === 'string') localRemoteGameKey = remoteGameKey;
      }
    }
    games.set(game.gameId, { gameId: game.gameId, displayName: game.displayName, variants, sourceIds,
      installations: games.get(game.gameId)?.installations ?? [], ...(localRemoteGameKey ? { localRemoteGameKey } : {}),
    });
  }
  return [...games.values()];
}
