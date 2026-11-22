import { describe, expect, it } from 'vitest';
import type { ProfilesContentStateResult } from '@mintcat/sdk-ts';

import {
  canEditProfileContent,
  compareModsForSort,
  deploymentStatus,
  deployJobProgress,
  extractGameVersionTags,
  groupDropTarget,
  matchesModFacets,
  modDisplayVersion,
  modDropTarget,
  modHasUpdate,
  modMetadataLabels,
  modSourceUrl,
  overlayProgressPercent,
  parseModUrlLines,
  shouldRefreshLibrary,
} from '../../../modules/library/ui/library-model';

const busyState: ProfilesContentStateResult = {
  profileId: 'profile-a',
  contentRevision: 4,
  writable: false,
  busyOperationId: 'operation-a',
  recoveryRequired: false,
  operations: [{
    operationId: 'operation-a',
    jobId: 'job-a',
    mode: 'prepare-and-apply',
    state: 'running',
    phase: 'download-running',
    completedUnits: 1,
    totalUnits: 2,
    items: [{
      profileModId: 'profile-mod-a',
      incarnationId: 'incarnation-a',
      resolvedVersion: 'v2',
      pendingSelection: { kind: 'pinned', versionId: 'v2' },
      transferId: 'transfer-a',
      attemptId: 'attempt-b',
      transferSeq: 8,
      transferPhase: 'verifying',
      transferState: 'verifying',
      downloadedBytes: 25,
      totalBytes: 100,
      verified: true,
    }],
  }],
};

describe('download overlay model', () => {

  it('uses backend capability and content revision as authorities', () => {
    expect(canEditProfileContent(busyState)).toBe(false);
    expect(shouldRefreshLibrary(3, busyState)).toBe(true);
    expect(shouldRefreshLibrary(4, busyState)).toBe(false);
  });
});

describe('deployment status', () => {
  const profile = { profileId: 'p', incarnationId: 'p:1', gameId: 'game', name: 'A', displayName: 'A',
    active: true, description: '', revision: 12, contentRevision: 1 };
  const installation = { installationId: 'i', incarnationId: 'i:1', rootPath: '/game', state: 'registered' };
  const content = { profileId: 'p', contentRevision: 4, writable: true, recoveryRequired: false, operations: [] };
  const item = { receiptId: 'r', installationId: 'i', profileRevision: 11,
    planHash: 'sha256:head', fileCount: 2, committedAtMs: 1 };
  const deployedProfile = { profileId: 'p', profileIncarnationId: 'p:1', contentRevision: 4,
    installationIncarnationId: 'i:1', variantId: 'steam' };
  const receipts = { installationId: 'i', items: [item], deployedProfile };
  const state = { installationId: 'i', installations: [installation], profileId: 'p', profiles: [profile],
    variantId: 'steam', content, receipts };

  it('only shows units from the current job event, never borrowing progress from a previous phase', () => {
    const job = { jobId: 'apply', command: 'install.apply', generationId: 'g', lastSeq: 3, state: 'committing' as const, createdAt: '', updatedAt: '' };
    const event = { jobId: job.jobId, generationId: 'g', seq: 3, phase: 'commit-started', state: job.state, completedUnits: 2, totalUnits: 8 };
    expect(deployJobProgress(job, event)).toEqual({ label: '写入游戏目录', percent: 25 });
    for (const difference of [{ seq: 2 }, { generationId: 'old' }, { totalUnits: 0 }, { completedUnits: undefined }, { completedUnits: 9 }]) {
      expect(deployJobProgress(job, { ...event, ...difference })).toEqual({ label: '写入游戏目录' });
    }
    expect(deployJobProgress({ ...job, command: 'install.plan', state: 'awaiting-approval' }, event)).toEqual({ label: '等待人工批准' });
  });

  it('requires a receipt for the live content, even with a stale profile list or no enabled mods', () => {
    expect(deploymentStatus(state)).toBe('applied');
    expect(deploymentStatus({ ...state, content: { ...content, contentRevision: 5 } })).toBe('changed');
    expect(deploymentStatus({ ...state, receipts: { installationId: 'i', items: [] } })).toBe('not-deployed');
  });

  it.each([
    { profileId: 'other' }, { profileIncarnationId: 'restored' },
    { installationIncarnationId: 'replaced' }, { variantId: 'other' },
  ])('distinguishes deployment identity: %o', difference => {
    expect(deploymentStatus({ ...state, receipts: { ...receipts,
      deployedProfile: { ...deployedProfile, ...difference } } })).toBe('changed');
  });

  it('does not infer deployment while loading, during recovery, or from legacy receipts', () => {
    expect(deploymentStatus({ ...state, receipts: undefined })).toBe('unknown');
    expect(deploymentStatus({ ...state, content: { ...content, recoveryRequired: true } })).toBe('unknown');
    expect(deploymentStatus({ ...state, receipts: { ...receipts, installationId: 'other' } })).toBe('unknown');
    expect(deploymentStatus({ ...state, receipts: { installationId: 'i', items: [item] } })).toBe('unknown');
  });
});

describe('actual mod metadata labels', () => {
  it('does not infer multiplayer compatibility or certification from missing metadata', () => {
    expect(modMetadataLabels([], '')).toEqual({ category: '—', multiplayer: '—', approval: '—' });
    expect(modMetadataLabels(['Future category'], 'Custom review')).toEqual({
      category: 'Future category', multiplayer: '—', approval: 'Custom review',
    });
  });

  it('keeps categories separate and prioritizes the strict all-player requirement', () => {
    expect(modMetadataLabels(['Optional', 'QoL', 'RequiredByAll'], 'Sandbox')).toEqual({
      category: 'QoL', multiplayer: '全员必装', approval: '沙盒',
    });
  });
});

describe('list drag sorting', () => {
  const mod = (profileModId: string, sortOrder: number, groupId = 'group-a', sourceId = 'modio') => ({
    profileModId, sortOrder, groupId, mod: { sourceId },
  });
  const group = (groupId: string, sortOrder: number, parentGroupId?: string) => ({
    groupId, sortOrder, parentGroupId,
  });

  it('uses before/after insertion positions after removing the dragged row in both directions', () => {
    const items = ['a', 'b', 'c', 'd'].map((id, order) => mod(id, order));
    expect(modDropTarget(items, items[0], items[2], 'before', 'profile')).toEqual({ groupId: 'group-a', position: 1 });
    expect(modDropTarget(items, items[0], items[2], 'after', 'profile')).toEqual({ groupId: 'group-a', position: 2 });
    expect(modDropTarget(items, items[3], items[1], 'before', 'profile')).toEqual({ groupId: 'group-a', position: 1 });
    expect(modDropTarget(items, items[3], items[1], 'after', 'profile')).toEqual({ groupId: 'group-a', position: 2 });
  });

  it('does not change hidden profile groups or immutable sources when sorting the source view', () => {
    const items = [mod('a', 0), mod('b', 1), mod('other-source', 2, 'group-a', 'local'), mod('other-group', 0, 'group-b')];
    expect(modDropTarget(items, items[0], items[1], 'after', 'source')).toEqual({ groupId: 'group-a', position: 1 });
    expect(modDropTarget(items, items[0], items[2], 'after', 'source')).toBeUndefined();
    expect(modDropTarget(items, items[0], items[3], 'after', 'source')).toBeUndefined();
    expect(modDropTarget(items, items[0], items[3], 'after', 'profile')).toEqual({ groupId: 'group-b', position: 1 });
  });

  it('counts hidden siblings and deterministic tie ordering in the destination profile group', () => {
    const items = [mod('dragged', 0, 'source'), mod('hidden', 2, 'destination'), mod('a', 7, 'destination'), mod('b', 7, 'destination')];
    expect(modDropTarget(items, items[0], items[3], 'before', 'profile')).toEqual({ groupId: 'destination', position: 2 });
    expect(modDropTarget(items, items[0], items[3], 'after', 'profile')).toEqual({ groupId: 'destination', position: 3 });
  });

  it('normally reorders sibling groups and only explicitly nests a group', () => {
    const groups = [group('local', 0), group('modio', 1)];
    expect(groupDropTarget(groups, groups[0], groups[1], 'before', false)).toEqual({ parentGroupId: undefined, position: 0 });
    expect(groupDropTarget(groups, groups[0], groups[1], 'after', false)).toEqual({ parentGroupId: undefined, position: 1 });
    expect(groupDropTarget(groups, groups[0], groups[1], 'after', true)).toEqual({ parentGroupId: 'modio', position: 0 });
  });

  it('rejects self drops and moving a group into its own subtree', () => {
    const groups = [group('root', 0), group('child', 0, 'root'), group('grandchild', 0, 'child')];
    expect(groupDropTarget(groups, groups[0], groups[0], 'before', false)).toBeUndefined();
    expect(groupDropTarget(groups, groups[0], groups[2], 'after', false)).toBeUndefined();
    expect(groupDropTarget(groups, groups[0], groups[2], 'after', true)).toBeUndefined();
    const items = [mod('a', 0)];
    expect(modDropTarget(items, items[0], items[0], 'after', 'source')).toBeUndefined();
  });
});

describe('parity helpers for metadata, facets, sorting, URLs and progress', () => {
  it('maps unavailable approval to dash and separates game version tags from category tags', () => {
    expect(modMetadataLabels(['1.40', 'v1.39.2', 'QoL', 'Verified', 'Optional'], 'unavailable')).toEqual({
      category: 'QoL', multiplayer: '客户端可选', approval: '—',
    });
    expect(extractGameVersionTags(['1.40', 'QoL', 'v1.39.2', 'RequiredByAll'])).toEqual(['1.40', 'v1.39.2']);
  });

  it('prefers display versions and derives canonical source URLs', () => {
    expect(modDisplayVersion({
      appliedVersionId: 'file:123', appliedDisplayVersion: '2.1.0',
      currentVersionId: 'file:456', currentDisplayVersion: '2.2.0',
      versionSelection: { kind: 'auto' },
    })).toBe('2.1.0');
    expect(modDisplayVersion({
      currentVersionId: 'file:456', currentDisplayVersion: '2.2.0',
      versionSelection: { kind: 'auto' },
    })).toBe('2.2.0');
    expect(modSourceUrl({
      url: 'https://mod.io/g/drg/m/better-flashlight',
      mod: { sourceId: 'modio', remoteGameKey: 'drg', remoteModId: '100' },
    })).toBe('https://mod.io/g/drg/m/better-flashlight');
    expect(modSourceUrl({
      mod: { sourceId: 'modcat', remoteGameKey: 'drg', remoteModId: 'abc-123' },
    })).toBe('https://modcat.top/#/modDetail?ModId=abc-123');
    expect(parseModUrlLines(' https://mod.io/g/drg/m/a \n\nhttps://mod.io/g/drg/m/a\nhttps://mod.io/g/drg/m/b ')).toEqual([
      'https://mod.io/g/drg/m/a', 'https://mod.io/g/drg/m/b',
    ]);
  });

  it('filters multi-facet combinations, detects updates, sorts by time and approval, and computes progress', () => {
    const baseMod = {
      profileModId: 'pm-1', incarnationId: 'inc-1', modId: 'm-1',
      mod: { sourceId: 'modio', remoteGameKey: 'drg', remoteModId: '1' },
      displayName: 'Alpha', tags: ['QoL', 'RequiredByAll'], approvalStatus: 'Verified',
      entityRevision: 1, enabled: false, sortOrder: 0, versionSelection: { kind: 'auto' as const },
      currentVersionId: 'file:2', appliedVersionId: 'file:1', createdAtMs: 100,
    };
    const secondMod = {
      ...baseMod, profileModId: 'pm-2', modId: 'm-2', displayName: 'Beta',
      approvalStatus: 'Sandbox', enabled: true, createdAtMs: 200,
    };
    expect(modHasUpdate(baseMod)).toBe(true);
    expect(matchesModFacets(baseMod, {
      status: 'disabled', sourceId: 'modio', approval: 'Verified', multiplayer: '全员必装', category: 'QoL', updatesOnly: true,
    }, new Set(), new Set(['pm-1']))).toBe(true);
    expect(matchesModFacets(baseMod, {
      status: 'disabled', sourceId: '', approval: '', multiplayer: '', category: '', updatesOnly: true,
    }, new Set(), new Set())).toBe(false);
    expect(matchesModFacets(secondMod, {
      status: 'disabled', sourceId: '', approval: '', multiplayer: '', category: '',
    }, new Set(), new Set())).toBe(false);
    expect(compareModsForSort(baseMod, secondMod, 'approval-asc')).toBeLessThan(0);
    expect(compareModsForSort(baseMod, secondMod, 'time-desc')).toBeGreaterThan(0);
    expect(overlayProgressPercent({ operation: busyState.operations[0], item: busyState.operations[0].items[0] })).toBe(25);
  });
});
