export type Part = 'node' | 'supervisor' | 'ffmpeg' | 'pot' | 'ytdlp-plugin';
export const PARTS: Part[];
export const PART_DIRS: Record<Part, string>;
export const SWAP_FOLDERS: string[];
export const PART_KEY_FILES: Record<string, string>;
export function foldersFor(parts: Part[]): string[];
export function hashPart(root: string, part: Part): Promise<string | null>;
export function hashParts(root: string): Promise<Record<Part, string | null>>;
export function diffParts(install: Record<Part, string | null>, release: Partial<Record<Part, string>>): Part[];
