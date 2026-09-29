import {SearchSource} from '@/apis/search';
import type {AgentFunctionTool} from '@/apis/agent/types';

export type AgentToolName =
    | 'get_active_profile'
    | 'list_folders'
    | 'list_mods'
    | 'get_mod'
    | 'search_mods'
    | 'add_mod_from_search'
    | 'add_mod_from_url'
    | 'rename_mod'
    | 'set_mod_enabled'
    | 'set_mod_version'
    | 'move_mod'
    | 'remove_mod';

export type AgentToolRequest =
    | {name: 'get_active_profile'; args: Record<string, never>}
    | {name: 'list_folders'; args: Record<string, never>}
    | {name: 'list_mods'; args: {query?: string; source?: string; enabled?: boolean; folderId?: number; limit: number}}
    | {name: 'get_mod'; args: {modId: number}}
    | {name: 'search_mods'; args: {query: string; source: SearchSource | 'all'; pageSize: number}}
    | {name: 'add_mod_from_search'; args: {resultId: string; folderId: number}}
    | {name: 'add_mod_from_url'; args: {url: string; folderId: number}}
    | {name: 'rename_mod'; args: {modId: number; displayName: string}}
    | {name: 'set_mod_enabled'; args: {modId: number; enabled: boolean}}
    | {name: 'set_mod_version'; args: {modId: number; version: string}}
    | {name: 'move_mod'; args: {modId: number; folderId: number}}
    | {name: 'remove_mod'; args: {modId: number}};
export type ReadOnlyAgentToolRequest = Extract<AgentToolRequest, {
    name: 'get_active_profile' | 'list_folders' | 'list_mods' | 'get_mod' | 'search_mods';
}>;
export type MutationAgentToolRequest = Exclude<AgentToolRequest, ReadOnlyAgentToolRequest>;


const objectSchema = (properties: Record<string, unknown>, required: string[] = []): Record<string, unknown> => ({
    type: 'object',
    properties,
    required,
    additionalProperties: false,
});
const integer = {type: 'integer', minimum: 1};
const folderId = {type: 'integer', minimum: 0, description: 'Folder ID; use 0 for the profile root'};

export const AGENT_FUNCTION_TOOLS: AgentFunctionTool[] = [
    {type: 'function', function: {name: 'get_active_profile', description: 'Get the active game and mod profile.', parameters: objectSchema({})}},
    {type: 'function', function: {name: 'list_folders', description: 'List folders in the active profile with their IDs.', parameters: objectSchema({})}},
    {type: 'function', function: {name: 'list_mods', description: 'List mods in the active profile, including updateAvailable for outdated mods. Omit filters to inspect the whole current list. Use returned modId values for later actions.', parameters: objectSchema({
        query: {type: 'string'}, source: {type: 'string'}, enabled: {type: 'boolean'}, folderId,
        limit: {type: 'integer', minimum: 1, maximum: 100},
    })}},
    {type: 'function', function: {name: 'get_mod', description: 'Get safe details for one mod in the active profile.', parameters: objectSchema({modId: integer}, ['modId'])}},
    {type: 'function', function: {name: 'search_mods', description: 'Search online mod providers available for the active game.', parameters: objectSchema({
        query: {type: 'string', minLength: 1},
        source: {type: 'string', enum: ['all', SearchSource.MODIO, SearchSource.MODCAT, SearchSource.NEXUSMODS]},
        pageSize: {type: 'integer', minimum: 1, maximum: 20},
    }, ['query'])}},
    {type: 'function', function: {name: 'add_mod_from_search', description: 'Add a mod from a resultId returned by search_mods.', parameters: objectSchema({resultId: {type: 'string'}, folderId}, ['resultId'])}},
    {type: 'function', function: {name: 'add_mod_from_url', description: 'Add a mod.io, ModCat, or Nexus Mods public URL.', parameters: objectSchema({url: {type: 'string'}, folderId}, ['url'])}},
    {type: 'function', function: {name: 'rename_mod', description: 'Change a mod display name. This name is shared across profiles.', parameters: objectSchema({modId: integer, displayName: {type: 'string', minLength: 1, maxLength: 200}}, ['modId', 'displayName'])}},
    {type: 'function', function: {name: 'set_mod_enabled', description: 'Enable or disable a mod in the active profile.', parameters: objectSchema({modId: integer, enabled: {type: 'boolean'}}, ['modId', 'enabled'])}},
    {type: 'function', function: {name: 'set_mod_version', description: 'Pin a mod to an available version, or pass an empty version to use automatic selection.', parameters: objectSchema({modId: integer, version: {type: 'string', maxLength: 100}}, ['modId', 'version'])}},
    {type: 'function', function: {name: 'move_mod', description: 'Move a mod to a folder in the active profile. Folder 0 is the root.', parameters: objectSchema({modId: integer, folderId}, ['modId', 'folderId'])}},
    {type: 'function', function: {name: 'remove_mod', description: 'Remove a mod from the active profile only. Cached/global metadata is retained.', parameters: objectSchema({modId: integer}, ['modId'])}},
];

const READ_ONLY_TOOL_NAMES = new Set<AgentToolName>([
    'get_active_profile', 'list_folders', 'list_mods', 'get_mod', 'search_mods',
]);

export function isReadOnlyAgentToolRequest(request: AgentToolRequest): request is ReadOnlyAgentToolRequest {
    return READ_ONLY_TOOL_NAMES.has(request.name);
}

function parseObject(argumentsJson: string): object {
    let value: unknown;
    try {
        value = JSON.parse(argumentsJson || '{}');
    } catch {
        throw new Error('Tool arguments must be valid JSON');
    }
    if (!value || typeof value !== 'object' || Array.isArray(value)) {
        throw new Error('Tool arguments must be a JSON object');
    }
    return value;
}

function assertKeys(value: object, allowed: string[]): void {
    const unknown = Object.keys(value).filter((key) => !allowed.includes(key));
    if (unknown.length > 0) throw new Error(`Unknown tool argument: ${unknown[0]}`);
}

function optionalString(value: object, key: string, maxLength = 500): string | undefined {
    const field = Reflect.get(value, key);
    if (field === undefined) return undefined;
    if (typeof field !== 'string' || field.length > maxLength) throw new Error(`${key} must be a string no longer than ${maxLength} characters`);
    return field.trim();
}

function requiredString(value: object, key: string, maxLength = 500, allowEmpty = false): string {
    const field = optionalString(value, key, maxLength);
    if (field === undefined || (!allowEmpty && field.length === 0)) throw new Error(`${key} is required`);
    return field;
}

function optionalInteger(value: object, key: string, minimum: number, maximum: number): number | undefined {
    const field = Reflect.get(value, key);
    if (field === undefined) return undefined;
    if (!Number.isInteger(field) || field < minimum || field > maximum) throw new Error(`${key} must be an integer from ${minimum} to ${maximum}`);
    return field;
}

function requiredInteger(value: object, key: string, minimum = 1): number {
    const field = optionalInteger(value, key, minimum, Number.MAX_SAFE_INTEGER);
    if (field === undefined) throw new Error(`${key} is required`);
    return field;
}

function requiredBoolean(value: object, key: string): boolean {
    const field = Reflect.get(value, key);
    if (typeof field !== 'boolean') throw new Error(`${key} must be a boolean`);
    return field;
}

function parseSearchSource(value: string | undefined): SearchSource | 'all' {
    if (!value || value === 'all') return 'all';
    if (value === SearchSource.MODIO || value === SearchSource.MODCAT || value === SearchSource.NEXUSMODS) return value;
    throw new Error(`Unsupported search source: ${value}`);
}

export function parseAgentToolRequest(name: string, argumentsJson: string): AgentToolRequest {
    const value = parseObject(argumentsJson);
    switch (name) {
        case 'get_active_profile':
        case 'list_folders':
            assertKeys(value, []);
            return {name, args: {}};
        case 'list_mods':
            assertKeys(value, ['query', 'source', 'enabled', 'folderId', 'limit']);
            return {name, args: {
                query: optionalString(value, 'query', 200),
                source: optionalString(value, 'source', 50),
                enabled: Reflect.get(value, 'enabled') === undefined ? undefined : requiredBoolean(value, 'enabled'),
                folderId: optionalInteger(value, 'folderId', 0, Number.MAX_SAFE_INTEGER),
                limit: optionalInteger(value, 'limit', 1, 100) ?? 50,
            }};
        case 'get_mod':
        case 'remove_mod':
            assertKeys(value, ['modId']);
            return {name, args: {modId: requiredInteger(value, 'modId')}};
        case 'search_mods':
            assertKeys(value, ['query', 'source', 'pageSize']);
            return {name, args: {
                query: requiredString(value, 'query', 200),
                source: parseSearchSource(optionalString(value, 'source', 20)),
                pageSize: optionalInteger(value, 'pageSize', 1, 20) ?? 10,
            }};
        case 'add_mod_from_search':
            assertKeys(value, ['resultId', 'folderId']);
            return {name, args: {resultId: requiredString(value, 'resultId', 200), folderId: optionalInteger(value, 'folderId', 0, Number.MAX_SAFE_INTEGER) ?? 0}};
        case 'add_mod_from_url':
            assertKeys(value, ['url', 'folderId']);
            return {name, args: {url: requiredString(value, 'url', 2000), folderId: optionalInteger(value, 'folderId', 0, Number.MAX_SAFE_INTEGER) ?? 0}};
        case 'rename_mod':
            assertKeys(value, ['modId', 'displayName']);
            return {name, args: {modId: requiredInteger(value, 'modId'), displayName: requiredString(value, 'displayName', 200)}};
        case 'set_mod_enabled':
            assertKeys(value, ['modId', 'enabled']);
            return {name, args: {modId: requiredInteger(value, 'modId'), enabled: requiredBoolean(value, 'enabled')}};
        case 'set_mod_version':
            assertKeys(value, ['modId', 'version']);
            return {name, args: {modId: requiredInteger(value, 'modId'), version: requiredString(value, 'version', 100, true)}};
        case 'move_mod':
            assertKeys(value, ['modId', 'folderId']);
            return {name, args: {modId: requiredInteger(value, 'modId'), folderId: requiredInteger(value, 'folderId', 0)}};
        default:
            throw new Error(`Unknown tool: ${name}`);
    }
}
