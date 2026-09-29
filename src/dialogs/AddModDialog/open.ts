import {t} from "i18next";
import {emitVoidEvent, onceEvent} from "@/events";
import {ModioApi} from "@/apis/modio";
import {ModcatApi} from "@/apis/modcat";
import {NexusModsApi} from "@/apis/nexusmods";
import {captureNexusDownloadUrl} from "@/apis/nexusmods/downloadWebview";
import {HomeViewModel} from "@/pages/HomePage/HomeViewModel.ts";
import { IoC } from "@/core/IoC.ts";
import {emitTo} from "@tauri-apps/api/event";
import {WebviewWindow} from "@tauri-apps/api/webviewWindow";
import {ClipboardApi} from "@/apis/ClipboardApi.ts";
import {AddModType} from "@/dialogs/AddModDialog/index.tsx";
import StatusBar from "@/components/StatusBar.tsx";
import {StorageAPI} from "@/storage";
import {message} from "antd";
import {frontendRouteUrl} from "@/utils/FrontendUpdateRuntime.ts";
import {OnlineModAddService} from "@/services/OnlineModAddService";
import type {AddModFromPathResult} from "@/services/HomeService";
let windowInstance: WebviewWindow;

type AddModDialogInitData = {
    text: string;
    groupId: number;
    addModType: string;
};

export async function openWindow(addModType: string = AddModType.LOCAL,
                                 groupId: number = 0,
                                 text: string = "",
                                 onUpdated?: () => Promise<void>): Promise<void> {

    const vm = await IoC.get(HomeViewModel);

    const initData: AddModDialogInitData = {
        text,
        groupId,
        addModType,
    };

    const setInitData = (data: AddModDialogInitData) => {
        localStorage.setItem('add-mod-dialog-init-data', JSON.stringify(data));
    };

    const sendInitData = async () => {
        await emitTo('add-mod-dialog', "add-mod-dialog-init-data", initData);
    }

    setInitData(initData);

    if (windowInstance) {
        await sendInitData();
        await windowInstance.show();
        return;
    }

    windowInstance = new WebviewWindow('add-mod-dialog', {
        url: frontendRouteUrl(`/add_mod_dialog?init=${encodeURIComponent(JSON.stringify(initData))}`),
        width: 400,
        height: 580,
        title: t("Add Mod"),
        dragDropEnabled: true,
    });

    windowInstance.once('tauri://destroyed', () => {
        windowInstance = null;
    }).then();

    // ✅ 使用 onceEvent 自动管理类型安全
    await onceEvent('add-mod-dialog-ok', async (result) => {
        // result.list 去重
        result.list = [...new Set(result.list)];

        switch (result.addModType) {
            case AddModType.SUBSCRIBED: {
                if (result.modInfoList && result.modInfoList.length > 0) {
                    const batchResult = await vm.addModsFromSubscribed(result.modInfoList, result.groupId);
                    if (batchResult.existsCount > 0) {
                        message.warning(t("Mod Already Exists") + ` (${batchResult.existsCount} ${t("in current list")})`);
                    }
                    if (batchResult.errorCount > 0) {
                        message.warning(t("mod.someFailedToAdd") + `: ${batchResult.errorCount}/${result.modInfoList.length}`);
                    }
                }
                break;
            }
            case AddModType.ONLINE: {
                await addOnlineMods(vm, result.list, result.groupId);
            }
                break;
            case AddModType.LOCAL: {
                const list = result.list;
                const pathResults: AddModFromPathResult[] = [];
                for (const item of list) {
                    pathResults.push(await vm.addModFromPath(item, result.groupId));
                }
                const existsCount = pathResults.filter((r) => r.status === "exists").length;
                if (existsCount > 0) {
                    message.warning(t("Mod Already Exists") + ` (${existsCount} ${t("in current list")})`);
                }
            }
                break;
            default:
                break;
        }

        await StatusBar.success(t("Add Complete"));
        await emitVoidEvent('mods-added');
        if (onUpdated) {
            await onUpdated();
        }

        await windowInstance.close();
        windowInstance = null;
    });

    await onceEvent('add-mod-dialog-close', async () => {
        await windowInstance.close();
        windowInstance = null;
    });

}

async function addOnlineMods(vm: HomeViewModel, list: string[], groupId: number): Promise<void> {
    const service = new OnlineModAddService(vm);
    const summary = await service.addUrls(
        list,
        groupId,
        (url, parsed) => captureNexusDownloadUrl(url, {parsed}),
    );

    const existsCount = summary.results.filter((result) => result.status === "exists").length;
    if (existsCount > 0) {
        message.warning(t("Mod Already Exists") + ` (${existsCount} ${t("in current list")})`);
    }
    const totalErrors = summary.errorCount + summary.downloadErrorCount;
    if (totalErrors > 0) {
        message.warning(t("mod.someFailedToAdd") + `: ${totalErrors}/${list.length}`);
        const firstError = summary.errors[0];
        if (firstError) {
            console.error('[AddModDialog] Failed to add online mod:', firstError);
        }
    }
}

async function onClipboardChange(text: string) {
    if (!text) {
        return;
    }
    const links = text.split("\n").map(l => l.trim()).filter(l => l);
    
    // 检查是否所有链接都是有效的 mod 链接（mod.io、ModCat 或 Nexus Mods）
    let hasModioLinks = false;
    let hasModcatLinks = false;
    let hasNexusModsLinks = false;
    
    for (const link of links) {
        const isModio = !!ModioApi.parseModLinks(link);
        const isModcat = ModcatApi.isModcatLink(link);
        const isNexusMods = NexusModsApi.isNexusModsLink(link);
        
        if (!isModio && !isModcat && !isNexusMods) {
            return; // 存在无效链接，不处理
        }
        
        if (isModio) hasModioLinks = true;
        if (isModcat) hasModcatLinks = true;
        if (isNexusMods) hasNexusModsLinks = true;
    }

    // 如果没有任何有效链接，不处理
    if (!hasModioLinks && !hasModcatLinks && !hasNexusModsLinks) {
        return;
    }

    openWindow(AddModType.ONLINE, 0, text).then();
}


export async function initClipboardWatcher() {
    const settings = await StorageAPI.getSettings();
    const enabled = await settings.getClipboardMonitorEnabled();
    if (enabled) {
        await ClipboardApi.setClipboardWatcher(onClipboardChange);
    } else {
        // 仍然注册回调，但不启动监控
        ClipboardApi.setClipboardWatcher(onClipboardChange).then(() => {
            ClipboardApi.stopClipboardWatcher();
        });
    }
}
