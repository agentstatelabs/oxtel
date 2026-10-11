// Rust code and dependency lists on the site come from files in the
// repository, never from the pages themselves: the examples are compiled
// and run in CI, and each example's dependency lists are built by
// `cargo run -p xtask -- check-examples`. A page names a file and,
// optionally, a part of it; anything missing fails the build.

import { readFileSync } from 'node:fs';
import { relative, resolve } from 'node:path';

/** The repository root. The site is built from `site/`. */
const root = resolve(process.cwd(), '..');

export interface Selection {
	/** A region between `ANCHOR: name` and `ANCHOR_END: name` comments. */
	region?: string;
	/** A fenced block of this language in the file's `//!` documentation. */
	doc?: string;
	/** Which such block, counting from 1. */
	nth?: number;
}

export interface Included {
	code: string;
	lang: string;
}

const LANGUAGES: Record<string, string> = {
	rs: 'rust',
	toml: 'toml',
	yaml: 'yaml',
	yml: 'yaml',
	sh: 'bash',
	json: 'json',
};

const ANCHOR = /\bANCHOR(_END)?:\s*(\S+)\s*$/;

export function include(file: string, selection: Selection = {}): Included {
	const path = resolve(root, file);
	if (relative(root, path).startsWith('..')) {
		throw new Error(`${file} is outside the repository`);
	}
	const source = readFileSync(path, 'utf8');
	if (selection.doc) {
		return { code: docBlock(file, source, selection.doc, selection.nth ?? 1), lang: selection.doc };
	}
	const lang = LANGUAGES[file.split('.').pop() ?? ''] ?? 'text';
	const lines = selection.region
		? region(file, source, selection.region)
		: withoutModuleDocs(source.split('\n'));
	return { code: dedent(lines.filter((line) => !ANCHOR.test(line))).join('\n').trimEnd(), lang };
}

/** The lines between a region's markers. */
function region(file: string, source: string, name: string): string[] {
	const lines = source.split('\n');
	const at = (end: boolean, from = 0) =>
		lines.findIndex((line, i) => {
			const marker = line.match(ANCHOR);
			return i >= from && marker?.[2] === name && (marker[1] === '_END') === end;
		});
	const start = at(false);
	const end = start < 0 ? -1 : at(true, start + 1);
	if (start < 0 || end < 0) {
		throw new Error(`${file} has no region "${name}" (ANCHOR: ${name} ... ANCHOR_END: ${name})`);
	}
	return lines.slice(start + 1, end);
}

/** The file without its leading `//!` documentation, which pages restate. */
function withoutModuleDocs(lines: string[]): string[] {
	let i = 0;
	while (i < lines.length && lines[i].startsWith('//!')) i++;
	while (i < lines.length && lines[i].trim() === '') i++;
	return lines.slice(i);
}

/** The nth fenced block of a language in the file's `//!` documentation. */
function docBlock(file: string, source: string, lang: string, nth: number): string {
	const doc = [];
	for (const line of source.split('\n')) {
		if (!line.startsWith('//!')) break;
		doc.push(line.replace(/^\/\/! ?/, ''));
	}
	const blocks: string[] = [];
	let block: string[] | undefined;
	for (const line of doc) {
		if (block === undefined) {
			if (line.trim() === '```' + lang) block = [];
		} else if (line.trim() === '```') {
			blocks.push(block.join('\n'));
			block = undefined;
		} else {
			block.push(line);
		}
	}
	const found = blocks[nth - 1];
	if (found === undefined) {
		throw new Error(`${file} has no ${lang} block ${nth} in its documentation (it has ${blocks.length})`);
	}
	return found;
}

function dedent(lines: string[]): string[] {
	const indents = lines.filter((line) => line.trim() !== '').map((line) => line.match(/^ */)![0].length);
	const strip = indents.length ? Math.min(...indents) : 0;
	return lines.map((line) => line.slice(strip));
}
