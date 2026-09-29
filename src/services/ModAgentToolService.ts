import type {SearchResultItem} from '@/apis/search';
import {ModioApi} from '@/apis/modio';
import {ModcatApi} from '@/apis/modcat';
import {NexusModsApi} from '@/apis/nexusmods';
import {StorageAPI} from '@/storage';
import type {CompleteModData} from '@/storage/dao/ModDAO';
import type {ProfileData, ProfileFolderData, ProfileModData} from '@/storage/dao/ProfileDAO';
import {ProfileService} from '@/services/ProfileService';
import {HomeService} from '@/services/HomeService';
import {ModSearchService} from '@/services/ModSearchService';
import {OnlineModAddService, type NexusDownloadResolver} from '@/services/OnlineModAddService';
import {emitVoidEvent} from '@/events';
import type {MutationAgentToolRequest, ReadOnlyAgentToolRequest} from '@/services/AgentToolRegistry';

export interface AgentToolResult {
    ok: boolean;
    data?: unknown;
    error?: {code: string; message: string};
}

export interface PreparedAgentAction {
    id: string;
    profileId: number;
    request: MutationAgentToolRequest;
    title: string;
    details: string[];
    createdAt: number;
}

interface ActiveModContext {
    profile: ProfileData;
    profileMod: ProfileModData;
    mod: CompleteModData;
    folders: ProfileFolderData[];
}

function successful(data: unknown): AgentToolResult {
    return {ok: true, data};
}

function failed(code: string, message: string): AgentToolResult {
    return {ok: false, error: {code, message}};
}

function safeMod(mod: CompleteModData, profileMod: ProfileModData, folderName?: string) {
    return {
        modId: mod.modId,
        profileModId: profileMod.id,
        name: mod.displayName,
        originalName: mod.originalName,
        source: mod.sourceType,
        sourcePage: mod.url,
        tags: mod.tags ?? [],
        enabled: profileMod.isEnabled ?? true,
        folderId: profileMod.parentFolderId ?? 0,
        folderName: folderName ?? 'Root',
        currentVersion: mod.version?.currentVersion ?? '',
        selectedVersion: profileMod.usedVersion ?? '',
        availableVersions: mod.version?.availableVersions ?? [],
        approvalStatus: mod.approvalStatus,
        localMissing: mod.status?.isLocalNotFound ?? false,
        updateAvailable: (mod.status?.onlineUpdateDate ?? 0) > (mod.status?.lastUpdateDate ?? 0),
    };
}

export class ModAgentToolService {
    private readonly profileService = new ProfileService();
    private readonly homeService = new HomeService();
    private readonly searchService = new ModSearchService();
    private readonly onlineAddService = new OnlineModAddService(this.homeService);
    private readonly searchResults = new Map<string, SearchResultItem>();

    private async activeProfile(): Promise<ProfileData> {
        return await this.profileService.ensureActiveProfile();
    }

    private async requireFolder(profileId: number, requestedFolderId: number): Promise<ProfileFolderData | null> {
        if (requestedFolderId === 0) return null;
        const profiles = await StorageAPI.getProfiles();
        const folders = await profiles.getProfileFolders(profileId);
        const folder = folders.find((item) => item.id === requestedFolderId);
        if (!folder) throw new Error(`Folder ${requestedFolderId} is not in the active profile`);
        return folder;
    }

    private async requireActiveMod(modId: number): Promise<ActiveModContext> {
        const profile = await this.activeProfile();
        const profiles = await StorageAPI.getProfiles();
        const profileMods = await profiles.getProfileMods(profile.id!);
        const profileMod = profileMods.find((item) => item.modId === modId);
        if (!profileMod?.id) throw new Error(`Mod ${modId} is not in the active profile`);
        const mods = await StorageAPI.getMods();
        const mod = await mods.getCompleteModData(modId);
        if (!mod) throw new Error(`Mod ${modId} does not exist`);
        const folders = await profiles.getProfileFolders(profile.id!);
        return {profile, profileMod, mod, folders};
    }

    public async executeRead(request: ReadOnlyAgentToolRequest): Promise<AgentToolResult> {
        try {
            switch (request.name) {
                case 'get_active_profile': {
                    const profile = await this.activeProfile();
                    const games = await StorageAPI.getGames();
                    const game = await games.getActiveGame();
                    return successful({profileId: profile.id, profileName: profile.displayName || profile.name, game: game?.name ?? ''});
                }
                case 'list_folders': {
                    const profile = await this.activeProfile();
                    const profiles = await StorageAPI.getProfiles();
                    const folders = await profiles.getProfileFolders(profile.id!);
                    return successful([{id: 0, name: 'Root', parentFolderId: null}, ...folders.map((folder) => ({
                        id: folder.id,
                        name: folder.name,
                        parentFolderId: folder.parentFolderId ?? 0,
                    }))]);
                }
                case 'list_mods': {
                    const profile = await this.activeProfile();
                    const profiles = await StorageAPI.getProfiles();
                    const profileMods = await profiles.getProfileMods(profile.id!);
                    const mods = await StorageAPI.getMods();
                    // Use the same complete-data source as the Home list. The batched ID
                    // query can return an empty array after an internal query failure,
                    // making a populated profile look empty to the Agent.
                    const completeMods = await mods.getAllCompleteModData();
                    const completeModById = new Map(completeMods.map((mod) => [mod.modId, mod]));
                    const folders = await profiles.getProfileFolders(profile.id!);
                    const folderNameById = new Map(folders.filter((folder) => folder.id != null).map((folder) => [folder.id!, folder.name]));
                    const query = request.args.query?.toLocaleLowerCase();
                    const matchingItems = profileMods.flatMap((profileMod) => {
                        const mod = completeModById.get(profileMod.modId);
                        if (!mod) return [];
                        if (query && !`${mod.displayName} ${mod.originalName ?? ''} ${mod.nameId}`.toLocaleLowerCase().includes(query)) return [];
                        if (request.args.source && mod.sourceType?.toLocaleLowerCase() !== request.args.source.toLocaleLowerCase()) return [];
                        if (request.args.enabled !== undefined && (profileMod.isEnabled ?? true) !== request.args.enabled) return [];
                        if (request.args.folderId !== undefined && (profileMod.parentFolderId ?? 0) !== request.args.folderId) return [];
                        return [safeMod(mod, profileMod, folderNameById.get(profileMod.parentFolderId ?? 0))];
                    });
                    const items = matchingItems.slice(0, request.args.limit);
                    return successful({
                        items,
                        returned: items.length,
                        totalMatching: matchingItems.length,
                        totalInProfile: profileMods.length,
                    });
                }
                case 'get_mod': {
                    const context = await this.requireActiveMod(request.args.modId);
                    const folder = context.folders.find((item) => item.id === context.profileMod.parentFolderId);
                    return successful(safeMod(context.mod, context.profileMod, folder?.name));
                }
                case 'search_mods': {
                    const context = await this.searchService.getContext();
                    const sources = request.args.source === 'all'
                        ? context.availableSources
                        : [request.args.source];
                    const settled = await Promise.allSettled(sources.map(async (source) => ({
                        source,
                        result: await this.searchService.search(source, {
                            query: request.args.query,
                            page: 0,
                            pageSize: request.args.pageSize,
                            sortBy: 'downloads',
                            sortOrder: 'desc',
                        }),
                    })));
                    const items: Array<Record<string, unknown>> = [];
                    const errors: Array<{source: string; message: string}> = [];
                    for (let index = 0; index < settled.length; index++) {
                        const outcome = settled[index];
                        if (outcome.status === 'rejected') {
                            errors.push({source: sources[index], message: outcome.reason instanceof Error ? outcome.reason.message : String(outcome.reason)});
                            continue;
                        }
                        for (const item of outcome.value.result.items) {
                            this.searchResults.set(item.id, item);
                            items.push({
                                resultId: item.id,
                                source: item.source,
                                name: item.nameTrans || item.name,
                                summary: (item.summaryTrans || item.summary || '').slice(0, 500),
                                author: item.author.name,
                                downloads: item.stats.downloads,
                                rating: item.stats.rating,
                                tags: item.tags?.slice(0, 10) ?? [],
                                sourcePage: item.profileUrl,
                            });
                        }
                    }
                    return successful({items, errors});
                }
                default:
                    return failed('mutation_requires_confirmation', 'This action requires user confirmation');
            }
        } catch (error) {
            return failed('tool_failed', error instanceof Error ? error.message : String(error));
        }
    }

    public async prepareMutation(request: PreparedAgentAction['request']): Promise<PreparedAgentAction> {
        const profile = await this.activeProfile();
        const profileName = profile.displayName || profile.name;
        let title: string;
        let details: string[];

        switch (request.name) {
            case 'add_mod_from_search': {
                const result = this.searchResults.get(request.args.resultId);
                if (!result) throw new Error('Search result is missing or belongs to an expired conversation');
                const folder = await this.requireFolder(profile.id!, request.args.folderId);
                title = `Add ${result.nameTrans || result.name}`;
                details = [`Profile: ${profileName}`, `Source: ${result.source}`, `Folder: ${folder?.name ?? 'Root'}`];
                break;
            }
            case 'add_mod_from_url': {
                const nexus = NexusModsApi.parseModLinks(request.args.url);
                if (!nexus && !ModcatApi.isModcatLink(request.args.url) && !ModioApi.parseModLinks(request.args.url)) {
                    throw new Error('Only valid mod.io, ModCat, and Nexus Mods URLs can be added');
                }
                const folder = await this.requireFolder(profile.id!, request.args.folderId);
                const displayUrl = nexus ? NexusModsApi.getModUrl(nexus.domain, nexus.modId) : request.args.url;
                title = 'Add mod from URL';
                details = [`Profile: ${profileName}`, `URL: ${displayUrl}`, `Folder: ${folder?.name ?? 'Root'}`];
                break;
            }
            case 'rename_mod': {
                const context = await this.requireActiveMod(request.args.modId);
                title = `Rename ${context.mod.displayName}`;
                details = [`Old name: ${context.mod.displayName}`, `New name: ${request.args.displayName}`, 'This display name is shared across profiles.'];
                break;
            }
            case 'set_mod_enabled': {
                const context = await this.requireActiveMod(request.args.modId);
                title = `${request.args.enabled ? 'Enable' : 'Disable'} ${context.mod.displayName}`;
                details = [`Profile: ${profileName}`, `Current: ${(context.profileMod.isEnabled ?? true) ? 'enabled' : 'disabled'}`, `New: ${request.args.enabled ? 'enabled' : 'disabled'}`];
                break;
            }
            case 'set_mod_version': {
                const context = await this.requireActiveMod(request.args.modId);
                const versions = context.mod.version?.availableVersions ?? [];
                if (request.args.version && !versions.includes(request.args.version)) throw new Error(`Version is not available: ${request.args.version}`);
                title = `Change version for ${context.mod.displayName}`;
                details = [`Current selection: ${context.profileMod.usedVersion || 'Automatic'}`, `New selection: ${request.args.version || 'Automatic'}`];
                break;
            }
            case 'move_mod': {
                const context = await this.requireActiveMod(request.args.modId);
                const target = await this.requireFolder(profile.id!, request.args.folderId);
                const current = context.folders.find((item) => item.id === context.profileMod.parentFolderId);
                title = `Move ${context.mod.displayName}`;
                details = [`From: ${current?.name ?? 'Root'}`, `To: ${target?.name ?? 'Root'}`];
                break;
            }
            case 'remove_mod': {
                const context = await this.requireActiveMod(request.args.modId);
                title = `Remove ${context.mod.displayName}`;
                details = [`Profile: ${profileName}`, 'Only the active profile association is removed. Cached files and global metadata are retained.'];
                break;
            }
        }

        return {id: crypto.randomUUID(), profileId: profile.id!, request, title, details, createdAt: Date.now()};
    }

    public async executeMutation(action: PreparedAgentAction, resolveNexusDownload: NexusDownloadResolver): Promise<AgentToolResult> {
        try {
            const profile = await this.activeProfile();
            if (profile.id !== action.profileId) return failed('profile_changed', 'The active profile changed; request confirmation again');
            if (Date.now() - action.createdAt > 5 * 60_000) return failed('confirmation_expired', 'The confirmation expired');

            const request = action.request;
            switch (request.name) {
                case 'add_mod_from_search': {
                    const result = this.searchResults.get(request.args.resultId);
                    if (!result) return failed('search_result_expired', 'The search result expired');
                    await this.requireFolder(profile.id!, request.args.folderId);
                    const summary = await this.onlineAddService.addUrls([result.profileUrl], request.args.folderId, resolveNexusDownload);
                    if (summary.errorCount || summary.downloadErrorCount) {
                        const firstError = summary.errors[0];
                        return failed('add_failed', firstError instanceof Error ? firstError.message : 'Failed to add or download the mod');
                    }
                    await emitVoidEvent('mods-added');
                    return successful({status: summary.results[0]?.status ?? 'invalid'});
                }
                case 'add_mod_from_url': {
                    await this.requireFolder(profile.id!, request.args.folderId);
                    const summary = await this.onlineAddService.addUrls([request.args.url], request.args.folderId, resolveNexusDownload);
                    if (summary.errorCount || summary.downloadErrorCount) {
                        const firstError = summary.errors[0];
                        return failed('add_failed', firstError instanceof Error ? firstError.message : 'Failed to add or download the mod');
                    }
                    await emitVoidEvent('mods-added');
                    return successful({status: summary.results[0]?.status ?? 'invalid'});
                }
                case 'rename_mod': {
                    await this.requireActiveMod(request.args.modId);
                    await this.homeService.updateModDisplayName(request.args.modId, request.args.displayName);
                    return successful({renamed: true});
                }
                case 'set_mod_enabled': {
                    const context = await this.requireActiveMod(request.args.modId);
                    await this.homeService.setModEnabled(request.args.modId, request.args.enabled, context.profileMod.id);
                    return successful({enabled: request.args.enabled});
                }
                case 'set_mod_version': {
                    const context = await this.requireActiveMod(request.args.modId);
                    const versions = context.mod.version?.availableVersions ?? [];
                    if (request.args.version && !versions.includes(request.args.version)) return failed('version_unavailable', 'The requested version is no longer available');
                    await this.homeService.setModUsedVersion(context.profileMod.id!, request.args.version);
                    return successful({version: request.args.version});
                }
                case 'move_mod': {
                    const context = await this.requireActiveMod(request.args.modId);
                    await this.requireFolder(profile.id!, request.args.folderId);
                    await this.homeService.moveMod(context.profileMod.id!, request.args.folderId);
                    return successful({folderId: request.args.folderId});
                }
                case 'remove_mod':
                    await this.requireActiveMod(request.args.modId);
                    await this.homeService.removeModFromActiveProfile(request.args.modId);
                    return successful({removedFromProfile: true});
            }
        } catch (error) {
            return failed('action_failed', error instanceof Error ? error.message : String(error));
        }
    }
}
