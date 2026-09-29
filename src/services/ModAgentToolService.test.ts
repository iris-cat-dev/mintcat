import {afterEach, describe, expect, it, vi} from 'vitest';
import {StorageAPI} from '@/storage';
import {ProfileService} from '@/services/ProfileService';
import {ModAgentToolService} from '@/services/ModAgentToolService';

const profile = {
    id: 7,
    name: 'default',
    displayName: 'Default',
    gameId: 2,
    userId: 1,
    isActive: true,
};

const profileMods = [
    {id: 21, profileId: 7, modId: 102, parentFolderId: undefined, sortOrder: 0, isEnabled: true},
    {id: 22, profileId: 7, modId: 101, parentFolderId: undefined, sortOrder: 1, isEnabled: true},
];

const completeMods = [
    {
        modId: 101,
        platformId: 1001,
        gameId: 2,
        nameId: 'current-mod',
        displayName: 'Current Mod',
        sourceType: 'Local',
        status: {modId: 101, lastUpdateDate: 20, onlineUpdateDate: 20},
    },
    {
        modId: 102,
        platformId: 1002,
        gameId: 2,
        nameId: 'outdated-mod',
        displayName: 'Outdated Mod',
        sourceType: 'Local',
        status: {modId: 102, lastUpdateDate: 10, onlineUpdateDate: 20},
    },
];

describe('ModAgentToolService list_mods', () => {
    afterEach(() => vi.restoreAllMocks());

    it('returns the populated Home list when the ID-batched query is empty', async () => {
        vi.spyOn(ProfileService.prototype, 'ensureActiveProfile').mockResolvedValue(profile);
        vi.spyOn(StorageAPI, 'getProfiles').mockResolvedValue({
            getProfileMods: vi.fn(async () => profileMods),
            getProfileFolders: vi.fn(async () => []),
        } as never);
        vi.spyOn(StorageAPI, 'getMods').mockResolvedValue({
            getAllCompleteModData: vi.fn(async () => completeMods),
            getBatchCompleteModDataOptimized: vi.fn(async () => []),
        } as never);

        const result = await new ModAgentToolService().executeRead({name: 'list_mods', args: {limit: 50}});

        expect(result).toEqual({
            ok: true,
            data: {
                items: [
                    expect.objectContaining({modId: 102, name: 'Outdated Mod', enabled: true, updateAvailable: true}),
                    expect.objectContaining({modId: 101, name: 'Current Mod', enabled: true, updateAvailable: false}),
                ],
                returned: 2,
                totalMatching: 2,
                totalInProfile: 2,
            },
        });
    });
});
