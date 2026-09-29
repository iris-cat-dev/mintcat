import {
    initializeSearchProviders,
    SearchProviderRegistry,
    SearchSource,
    type SearchParams,
    type SearchResult,
} from '@/apis/search';
import {NEXUSMODS_DRG_DOMAIN, NEXUSMODS_ROGUE_CORE_DOMAIN} from '@/apis/nexusmods';
import {StorageAPI} from '@/storage';

export interface SearchContext {
    availableSources: SearchSource[];
    modcatGameId?: string;
    nexusmodsGameDomain?: string;
}

/** Shared, UI-independent orchestration for the registered mod search providers. */
export class ModSearchService {
    public constructor() {
        initializeSearchProviders();
    }

    public async getContext(): Promise<SearchContext> {
        const registry = SearchProviderRegistry.getInstance();
        const allSources = registry.getSources();
        const games = await StorageAPI.getGames();
        const activeGame = await games.getActiveGame();
        const gameName = activeGame?.name?.toLowerCase();

        if (gameName === 'rc') {
            return {
                availableSources: [SearchSource.NEXUSMODS, SearchSource.MODCAT]
                    .filter((source) => allSources.includes(source)),
                modcatGameId: 'drgrc',
                nexusmodsGameDomain: NEXUSMODS_ROGUE_CORE_DOMAIN,
            };
        }

        if (gameName === 'drg') {
            return {
                availableSources: allSources.filter((source) =>
                    source === SearchSource.MODIO
                    || source === SearchSource.MODCAT
                    || source === SearchSource.NEXUSMODS),
                modcatGameId: 'drg',
                nexusmodsGameDomain: NEXUSMODS_DRG_DOMAIN,
            };
        }

        return {availableSources: allSources};
    }
    public getSearchPlaceholder(source: SearchSource): string {
        return SearchProviderRegistry.getInstance().get(source)?.getSearchPlaceholder() || 'Search...';
    }


    public async search(source: SearchSource, params: Omit<SearchParams, 'modcatGameId' | 'nexusmodsGameDomain'>): Promise<SearchResult> {
        const context = await this.getContext();
        if (!context.availableSources.includes(source)) {
            throw new Error(`Search source is unavailable for the active game: ${source}`);
        }

        const provider = SearchProviderRegistry.getInstance().get(source);
        if (!provider) {
            throw new Error(`Search provider is not registered: ${source}`);
        }

        return await provider.search({
            ...params,
            modcatGameId: context.modcatGameId,
            nexusmodsGameDomain: context.nexusmodsGameDomain,
        });
    }
}
