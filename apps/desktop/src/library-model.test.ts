import { describe, expect, it } from 'vitest';
import type { ProfilesContentStateResult } from '@mintcat/sdk-ts';

import {
  canEditProfileContent,
  groupDropTarget,
  modDropTarget,
  modMetadataLabels,
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
