import {NexusModsApi, type NexusModsParsedUrl} from '@/apis/nexusmods';
import {asyncPoolAll} from '@/utils/AsyncPool';
import {HomeService, type AddModFromUrlResult} from '@/services/HomeService';
import {ModUpdateService} from '@/services/ModUpdateService';

export interface OnlineModAddSummary {
    results: AddModFromUrlResult[];
    errorCount: number;
    errors: unknown[];
    downloadErrorCount: number;
}

export type NexusDownloadResolver = (url: string, parsed: NexusModsParsedUrl) => Promise<string>;
export interface OnlineModAdder {
    addModFromUrl(url: string, groupId: number): Promise<AddModFromUrlResult>;
}


/** Reusable online-add workflow. UI-specific Nexus authentication is injected by the caller. */
export class OnlineModAddService {
    public constructor(private readonly homeService: OnlineModAdder = new HomeService()) {}

    public async addUrls(
        urls: string[],
        groupId: number,
        resolveNexusDownload: NexusDownloadResolver,
    ): Promise<OnlineModAddSummary> {
        const uniqueUrls = [...new Set(urls.map((url) => url.trim()).filter(Boolean))];
        const directUrls: string[] = [];
        const nexusUrls: Array<{url: string; parsed: NexusModsParsedUrl}> = [];

        for (const url of uniqueUrls) {
            const parsed = NexusModsApi.parseModLinks(url);
            if (parsed && !NexusModsApi.hasReusableDownloadCredential(url)) {
                nexusUrls.push({url, parsed});
            } else {
                directUrls.push(url);
            }
        }

        const results: AddModFromUrlResult[] = [];
        const errors: unknown[] = [];
        if (directUrls.length > 0) {
            const directResult = await asyncPoolAll(
                directUrls,
                (url) => this.homeService.addModFromUrl(url, groupId),
                5,
            );
            results.push(...directResult.results.filter(Boolean));
            errors.push(...directResult.errors);
        }

        for (const item of nexusUrls) {
            try {
                const resolvedUrl = await resolveNexusDownload(item.url, item.parsed);
                results.push(await this.homeService.addModFromUrl(resolvedUrl, groupId));
            } catch (error) {
                errors.push(error);
            }
        }

        const nexusDownloads = results
            .filter((result) => result.downloadAfterAdd && result.mod)
            .map((result) => result.mod!);
        const downloadErrors: unknown[] = [];
        if (nexusDownloads.length > 0) {
            const downloadResult = await asyncPoolAll(
                nexusDownloads,
                async (mod) => {
                    if (await ModUpdateService.needsOnlineModDownload(mod)) {
                        await ModUpdateService.updateModFile(mod);
                    }
                },
                2,
            );
            downloadErrors.push(...downloadResult.errors);
        }

        return {
            results,
            errorCount: errors.length,
            errors,
            downloadErrorCount: downloadErrors.length,
        };
    }
}
