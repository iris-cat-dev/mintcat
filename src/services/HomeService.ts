import { exists, stat } from "@tauri-apps/plugin-fs";
import { path } from "@tauri-apps/api";
import { ModioApi } from "@/apis/modio";
import { ModcatApi, MODCAT_PLATFORM } from "@/apis/modcat";
import { NexusModsApi, NEXUSMODS_PLATFORM } from "@/apis/nexusmods";
import { StorageAPI } from "@/storage";
import { ModService } from "@/services/ModService.ts";
import { ProfileService } from "@/services/ProfileService.ts";
import { IoC } from "@/core/IoC";
import type { ProfileData, ProfileFolderData } from "@/storage/dao/ProfileDAO";
import type { CompleteModData } from "@/storage/dao/ModDAO";

/** 删除分组时若会清空配置下的全部分组则抛出 */
export class LastGroupCannotDeleteError extends Error {
    constructor() {
        super("LastGroupCannotDeleteError");
        this.name = "LastGroupCannotDeleteError";
    }
}
import type { ModInfo } from "@/apis/modio/ModInfo.ts";
import { asyncPoolAll } from "@/utils/AsyncPool";

export type AddModFromUrlResult = {
    status: "invalid" | "exists" | "added";
    modName?: string;
    mod?: CompleteModData;
    downloadAfterAdd?: boolean;
};

export type AddModFromPathResult = {
    status: "missing" | "exists" | "added";
    modPath?: string;
};

export type BatchAddModResult = {
    addedCount: number;
    existsCount: number;
    errorCount: number;
};

export class HomeService {
    private _profileService: ProfileService | null = null;

    private async getProfileService(): Promise<ProfileService> {
        if (!this._profileService) {
            this._profileService = await IoC.get(ProfileService);
        }
        return this._profileService;
    }

    private async getActiveProfile(): Promise<ProfileData> {
        const profileService = await this.getProfileService();
        return profileService.ensureActiveProfile();
    }

    private async addModDependencies(modId: number, groupId: number, profileId: number): Promise<void> {
        const depends = await ModioApi.getDependencies(modId);
        if (!depends) {
            return;
        }

        for (const depend of depends) {
            await ModService.addModFromModio(depend, profileId, groupId);
        }
    }

    private async addModToProfile(options: {
        profileId: number;
        modId: number;
        groupId: number;
        usedVersion: string;
    }): Promise<void> {
        const profiles = await StorageAPI.getProfiles();
        const profileMods = await profiles.getProfileMods(options.profileId);
        const maxSortOrder = profileMods.reduce((max, pm) => Math.max(max, pm.sortOrder ?? 0), -1);

        await profiles.addModToProfile({
            profileId: options.profileId,
            modId: options.modId,
            parentFolderId: options.groupId,
            sortOrder: maxSortOrder + 1,
            isEnabled: true,
            usedVersion: options.usedVersion,
        });

    }

    public async addModFromUrl(url: string, groupId: number): Promise<AddModFromUrlResult> {
        if (NexusModsApi.isNexusModsLink(url)) {
            return await this.addModFromNexusmodsUrl(url, groupId);
        }

        // 检查是否为 ModCat 链接
        if (ModcatApi.isModcatLink(url)) {
            return await this.addModFromModcatUrl(url, groupId);
        }

        // 处理 mod.io 链接
        return await this.addModFromModioUrl(url, groupId);
    }

    /**
     * 从 Nexus Mods 链接添加 Mod
     */
    private async addModFromNexusmodsUrl(url: string, groupId: number): Promise<AddModFromUrlResult> {
        const parsed = NexusModsApi.parseModLinks(url);
        if (!parsed) {
            return { status: "invalid" };
        }
        const downloadAfterAdd = NexusModsApi.hasReusableDownloadCredential(url);

        const profile = await this.getActiveProfile();
        const modsApi = await StorageAPI.getMods();
        const profiles = await StorageAPI.getProfiles();

        const existingByPlatform = await modsApi.getModByPlatformId(parsed.modId, NEXUSMODS_PLATFORM);
        if (existingByPlatform) {
            if (downloadAfterAdd) {
                await modsApi.upsertModDownload({
                    modId: existingByPlatform.modId!,
                    downloadUrl: url,
                    downloadStatus: "pending",
                    downloadProgress: 0,
                });
            }
            const mod = downloadAfterAdd
                ? await modsApi.getCompleteModData(existingByPlatform.modId!) ?? undefined
                : undefined;
            const existingProfileMod = await profiles.getProfileMod(profile.id!, existingByPlatform.modId!);
            if (existingProfileMod) {
                return { status: "exists", modName: existingByPlatform.displayName, mod, downloadAfterAdd };
            }

            await this.addModToProfile({
                profileId: profile.id!,
                modId: existingByPlatform.modId!,
                groupId,
                usedVersion: "",
            });

            return { status: "added", mod, downloadAfterAdd };
        }

        const modInfoResp = await NexusModsApi.getModInfoByLink(url);
        if (!modInfoResp) {
            return { status: "invalid" };
        }

        const canonicalUrl = NexusModsApi.getModUrl(modInfoResp.domain, modInfoResp.modId);
        const existingByUrl = await modsApi.getModByUrl(canonicalUrl);
        if (existingByUrl) {
            if (downloadAfterAdd) {
                await modsApi.upsertModDownload({
                    modId: existingByUrl.modId!,
                    downloadUrl: url,
                    downloadStatus: "pending",
                    downloadProgress: 0,
                });
            }
            const mod = downloadAfterAdd
                ? await modsApi.getCompleteModData(existingByUrl.modId!) ?? undefined
                : undefined;
            const existingProfileMod = await profiles.getProfileMod(profile.id!, existingByUrl.modId!);
            if (existingProfileMod) {
                return { status: "exists", modName: modInfoResp.mod.name || "", mod, downloadAfterAdd };
            }

            await this.addModToProfile({
                profileId: profile.id!,
                modId: existingByUrl.modId!,
                groupId,
                usedVersion: "",
            });

            return { status: "added", mod, downloadAfterAdd };
        }

        const mod = await ModService.addModFromNexusmods(modInfoResp, profile.id!, groupId);
        return { status: "added", mod, downloadAfterAdd };
    }

    /**
     * 从 ModCat 链接添加 Mod
     */
    private async addModFromModcatUrl(url: string, groupId: number): Promise<AddModFromUrlResult> {
        const nameId = ModcatApi.parseModLinks(url);
        if (!nameId) {
            return { status: "invalid" };
        }

        const profile = await this.getActiveProfile();
        const modsApi = await StorageAPI.getMods();
        const profiles = await StorageAPI.getProfiles();

        // 先通过 nameId 检查数据库是否已存在
        const existingModByName = await modsApi.getModByNameId(nameId, MODCAT_PLATFORM);
        if (existingModByName) {
            const existingProfileMod = await profiles.getProfileMod(profile.id!, existingModByName.modId!);
            if (existingProfileMod) {
                return { status: "exists", modName: existingModByName.displayName };
            }

            await this.addModToProfile({
                profileId: profile.id!,
                modId: existingModByName.modId!,
                groupId,
                usedVersion: "",
            });

            return { status: "added" };
        }

        // 数据库中不存在，调用 ModCat API 获取 mod 信息
        const modInfoResp = await ModcatApi.getModInfoByLink(url);
        if (!modInfoResp) {
            return { status: "invalid" };
        }

        // 通过 URL 再检查一次
        const existingMod = await modsApi.getModByUrl(url);
        if (existingMod) {
            const existingProfileMod = await profiles.getProfileMod(profile.id!, existingMod.modId!);
            if (existingProfileMod) {
                return { status: "exists", modName: modInfoResp.Name || "" };
            }

            await this.addModToProfile({
                profileId: profile.id!,
                modId: existingMod.modId!,
                groupId,
                usedVersion: "",
            });

            return { status: "added" };
        }

        // 添加新 mod（仅元数据，不下载；安装时再下载）
        await ModService.addModFromModcat(modInfoResp, profile.id!, groupId);

        return { status: "added" };
    }

    /**
     * 从 mod.io 链接添加 Mod
     */
    private async addModFromModioUrl(url: string, groupId: number): Promise<AddModFromUrlResult> {
        // 先从 URL 解析 nameId，检查数据库是否已存在该 mod
        const nameId = ModioApi.parseModLinks(url);
        if (!nameId) {
            return { status: "invalid" };
        }

        const profile = await this.getActiveProfile();
        const modsApi = await StorageAPI.getMods();
        const profiles = await StorageAPI.getProfiles();

        // 先通过 nameId 检查数据库是否已存在（避免不必要的网络请求）
        const existingModByName = await modsApi.getModByNameId(nameId, "Modio");
        if (existingModByName) {
            const existingProfileMod = await profiles.getProfileMod(profile.id!, existingModByName.modId!);
            if (existingProfileMod) {
                return { status: "exists", modName: existingModByName.displayName };
            }

            await this.addModToProfile({
                profileId: profile.id!,
                modId: existingModByName.modId!,
                groupId,
                usedVersion: "",
            });

            // 检查依赖：需要获取 platformId 来查询依赖
            if (existingModByName.platformId) {
                await this.addModDependencies(existingModByName.platformId, groupId, profile.id!);
            }

            return { status: "added" };
        }

        // 数据库中不存在，才调用网络 API 获取 mod 信息
        const modInfoResp = await ModioApi.getModInfoByLink(url);
        if (modInfoResp === undefined) {
            return { status: "invalid" };
        }

        // 再通过 platformId 检查一次（防止 nameId 不匹配但 platformId 匹配的情况）
        const existingMod = await modsApi.getModByPlatformId(modInfoResp.id, "Modio");
        if (existingMod) {
            const existingProfileMod = await profiles.getProfileMod(profile.id!, existingMod.modId!);
            if (existingProfileMod) {
                return { status: "exists", modName: modInfoResp.name };
            }

            await this.addModToProfile({
                profileId: profile.id!,
                modId: existingMod.modId!,
                groupId,
                usedVersion: "",
            });

            if (modInfoResp.dependencies) {
                await this.addModDependencies(modInfoResp.id, groupId, profile.id!);
            }

            return { status: "added" };
        }

        // 仅写入元数据到列表，不下载；安装时再下载
        await ModService.addModFromModio(modInfoResp, profile.id!, groupId);

        if (modInfoResp.dependencies) {
            await this.addModDependencies(modInfoResp.id, groupId, profile.id!);
        }

        return { status: "added" };
    }

    /**
     * 批量导入 mod.io 订阅的 mod（使用已获取的 ModInfo，避免逐个请求 API）
     */
    public async addModsFromSubscribed(modInfos: ModInfo[], groupId: number): Promise<BatchAddModResult> {
        const profile = await this.getActiveProfile();
        const modsApi = await StorageAPI.getMods();
        const profiles = await StorageAPI.getProfiles();

        let addedCount = 0;
        let existsCount = 0;
        let errorCount = 0;

        const depModIds: number[] = [];

        for (const modInfo of modInfos) {
            try {
                const existingMod = await modsApi.getModByPlatformId(modInfo.id, "Modio");
                if (existingMod) {
                    const existingProfileMod = await profiles.getProfileMod(profile.id!, existingMod.modId!);
                    if (existingProfileMod) {
                        existsCount++;
                    } else {
                        await this.addModToProfile({
                            profileId: profile.id!,
                            modId: existingMod.modId!,
                            groupId,
                            usedVersion: "",
                        });
                        addedCount++;
                    }
                } else {
                    await ModService.addModFromModio(modInfo, profile.id!, groupId);
                    addedCount++;
                }

                if (modInfo.dependencies) {
                    depModIds.push(modInfo.id);
                }
            } catch (e) {
                console.error(`[addModsFromSubscribed] Failed to add mod ${modInfo.name}:`, e);
                errorCount++;
            }
        }

        // 依赖去重后批量解析，低并发避免风控
        const uniqueDepIds = [...new Set(depModIds)];
        if (uniqueDepIds.length > 0) {
            await asyncPoolAll(
                uniqueDepIds,
                (depModId) => this.addModDependencies(depModId, groupId, profile.id!),
                2
            );
        }

        return { addedCount, existsCount, errorCount };
    }

    public async addModFromPath(modPath: string, groupId: number): Promise<AddModFromPathResult> {
        if (!await exists(modPath)) {
            return { status: "missing", modPath };
        }

        const profile = await this.getActiveProfile();
        const modsApi = await StorageAPI.getMods();
        const profiles = await StorageAPI.getProfiles();

        const existingMod = await modsApi.getModByUrl(modPath);
        if (existingMod) {
            const existingProfileMod = await profiles.getProfileMod(profile.id!, existingMod.modId!);
            if (existingProfileMod) {
                return { status: "exists", modPath };
            }

            await this.addModToProfile({
                profileId: profile.id!,
                modId: existingMod.modId!,
                groupId,
                usedVersion: "-",
            });

            return { status: "added" };
        }

        const fileName = await path.basename(modPath);
        const addedMod = await ModService.addModFromPath(modPath, fileName, profile.id!, groupId);

        const fileInfo = await stat(modPath);
        if (addedMod.status) {
            addedMod.status.lastUpdateDate = fileInfo.mtime.getTime();
            await modsApi.upsertModStatus(addedMod.status);
        }

        return { status: "added" };
    }

    public async removeModFromActiveProfile(modId: number): Promise<void> {
        const profile = await this.getActiveProfile();
        const profiles = await StorageAPI.getProfiles();
        if (!await profiles.removeModFromProfile(profile.id!, modId)) {
            throw new Error(`Failed to remove mod ${modId} from profile ${profile.id}`);
        }
    }

    public async updateModDisplayName(modId: number, name: string): Promise<void> {
        const modsApi = await StorageAPI.getMods();
        if (!await modsApi.updateMod(modId, { displayName: name })) {
            throw new Error(`Failed to rename mod ${modId}`);
        }
    }

    /**
     * 设置 mod 启用状态
     * @param modId mod 的 ID
     * @param enable 是否启用
     * @param profileModId 可选的 profile_mods 表主键 ID，用于避免切换 profile 时的竞态条件
     */
    public async setModEnabled(modId: number, enable: boolean, profileModId?: number): Promise<void> {
        const profiles = await StorageAPI.getProfiles();
        let updated: boolean;
        if (profileModId !== undefined) {
            updated = await profiles.setModEnabledById(profileModId, enable);
        } else {
            const profile = await this.getActiveProfile();
            updated = await profiles.setModEnabled(profile.id!, modId, enable);
        }
        if (!updated) {
            throw new Error(`Failed to change enabled state for mod ${modId}`);
        }
    }

    /**
     * 批量设置 mod 启用状态（单条 SQL）
     */
    public async batchSetModEnabled(modIds: number[], enable: boolean): Promise<void> {
        const profiles = await StorageAPI.getProfiles();
        const profile = await this.getActiveProfile();
        if (!await profiles.batchSetModEnabled(profile.id!, modIds, enable)) {
            throw new Error(`Failed to change enabled state for ${modIds.length} mods`);
        }
    }

    public async setModUsedVersion(profileModId: number, version: string): Promise<void> {
        const profiles = await StorageAPI.getProfiles();
        if (!await profiles.updateProfileMod(profileModId, { usedVersion: version })) {
            throw new Error(`Failed to change version for profile mod ${profileModId}`);
        }
    }

    public async moveMod(profileModId: number, folderId: number): Promise<void> {
        const profiles = await StorageAPI.getProfiles();
        if (!await profiles.updateProfileMod(profileModId, {parentFolderId: folderId})) {
            throw new Error(`Failed to move profile mod ${profileModId}`);
        }
    }

    public async addGroup(parentGroupId: number, groupName: string): Promise<void> {
        const profile = await this.getActiveProfile();
        const profiles = await StorageAPI.getProfiles();

        await profiles.createFolder({
            profileId: profile.id!,
            name: groupName,
            parentFolderId: parentGroupId || null,
            folderType: "custom",
        });
    }

    /**
     * 本次删除涉及的文件夹 id（含自身及所有子文件夹）
     */
    private collectFolderSubtreeIds(rootId: number, allFolders: ProfileFolderData[]): Set<number> {
        const ids = new Set<number>();
        const queue: number[] = [rootId];
        while (queue.length > 0) {
            const id = queue.shift()!;
            if (ids.has(id)) {
                continue;
            }
            ids.add(id);
            for (const f of allFolders) {
                if (f.parentFolderId === id && f.id != null) {
                    queue.push(f.id);
                }
            }
        }
        return ids;
    }

    public async removeGroup(groupId: number): Promise<void> {
        const profile = await this.getActiveProfile();
        const profiles = await StorageAPI.getProfiles();
        const allFolders = await profiles.getProfileFolders(profile.id!);
        const subtreeIds = this.collectFolderSubtreeIds(groupId, allFolders);
        if (subtreeIds.size >= allFolders.length) {
            throw new LastGroupCannotDeleteError();
        }
        await profiles.deleteFolder(groupId);
    }

    /**
     * 清理本地文件不存在的 Local 类型 mod
     * @returns 清理的 mod 数量
     */
    public async cleanMissingLocalMods(): Promise<number> {
        const profile = await this.getActiveProfile();
        const modsApi = await StorageAPI.getMods();
        const profiles = await StorageAPI.getProfiles();

        // 获取当前 profile 中的 mod 列表
        const profileMods = await profiles.getProfileMods(profile.id!);
        const modIds = profileMods.map(pm => pm.modId!);
        
        // 获取这些 mod 的完整数据
        const completeMods = await modsApi.getBatchCompleteModDataOptimized(modIds);

        // 筛选出 sourceType === "Local" 且 isLocalNotFound === true 的 mod
        const missingLocalMods = completeMods.filter(mod => 
            mod.sourceType === "Local" && mod.status?.isLocalNotFound === true
        );

        // 从当前 profile 中移除这些 mod
        for (const mod of missingLocalMods) {
            await profiles.removeModFromProfile(profile.id!, mod.modId!);
        }
        return missingLocalMods.length;
    }
}
