// Checks that every link from one page of the built site to another, and
// every #fragment in such a link, leads somewhere. Links elsewhere aren't
// checked: they'd make the build depend on other sites being up.
//
//   node scripts/check-links.mjs [dist]

import { existsSync, readdirSync, readFileSync, statSync } from 'node:fs';
import { join, relative } from 'node:path';

const dist = process.argv[2] ?? 'dist';
const site = 'https://agentstatelabs.github.io';
const base = '/oxtel/';

const pages = [];
const walk = (dir) => {
	for (const name of readdirSync(dir)) {
		const path = join(dir, name);
		if (statSync(path).isDirectory()) walk(path);
		else if (name.endsWith('.html')) pages.push(path);
	}
};
walk(dist);

const ids = new Map();
const idsOf = (file) => {
	if (!ids.has(file)) {
		const html = readFileSync(file, 'utf8');
		ids.set(file, new Set([...html.matchAll(/\sid="([^"]+)"/g)].map((m) => m[1])));
	}
	return ids.get(file);
};

/** The built file a site path is served from, if there is one. */
const target = (pathname) => {
	const path = decodeURIComponent(pathname.slice(base.length));
	for (const candidate of [path, join(path, 'index.html'), path.replace(/\/$/, '') + '.html']) {
		const file = join(dist, candidate);
		if (existsSync(file) && statSync(file).isFile()) return file;
	}
	return undefined;
};

const broken = [];
let checked = 0;
for (const page of pages) {
	const html = readFileSync(page, 'utf8');
	const at = new URL(base + relative(dist, page).replace(/index\.html$/, ''), site);
	for (const [, attribute, value] of html.matchAll(/\s(href|src)="([^"]*)"/g)) {
		const url = new URL(value.replaceAll('&amp;', '&'), at);
		if (url.origin !== site || !url.pathname.startsWith(base)) continue;
		checked++;
		const file = target(url.pathname);
		const fragment = decodeURIComponent(url.hash.slice(1));
		if (!file) broken.push(`${relative(dist, page)}: ${attribute}="${value}" leads nowhere`);
		else if (fragment && file.endsWith('.html') && !idsOf(file).has(fragment)) {
			broken.push(`${relative(dist, page)}: ${attribute}="${value}" has no #${fragment}`);
		}
	}
}

if (broken.length) {
	console.error(broken.join('\n'));
	process.exit(1);
}
console.log(`${checked} links between ${pages.length} pages lead somewhere`);
