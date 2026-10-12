// @ts-check
import { defineConfig } from 'astro/config';
import starlight from '@astrojs/starlight';

// GitHub Pages for now. With a custom domain, `site` becomes the domain and
// `base` becomes '/'.
const site = 'https://agentstatelabs.github.io';
const base = '/oxtel';

/**
 * Pages link to each other from the site's root ("/start/"), and this adds
 * the base path, in Markdown links and in components' `href`s, so moving the
 * site changes only `base`.
 */
function linksWithBase() {
	const prefix = base.replace(/\/$/, '');
	const rebase = (url) =>
		typeof url === 'string' && url.startsWith('/') && !url.startsWith('//') && !url.startsWith(prefix + '/')
			? prefix + url
			: url;
	const walk = (node) => {
		if (node.type === 'link' || node.type === 'definition') node.url = rebase(node.url);
		if (node.type === 'mdxJsxFlowElement' || node.type === 'mdxJsxTextElement') {
			for (const attribute of node.attributes ?? []) {
				if (attribute.name === 'href') attribute.value = rebase(attribute.value);
			}
		}
		node.children?.forEach(walk);
	};
	return walk;
}

export default defineConfig({
	site,
	base,
	markdown: { remarkPlugins: [linksWithBase] },
	integrations: [
		starlight({
			title: 'oxtel',
			description:
				'Structured logs in one line, and OpenTelemetry traces, metrics and logs when you want them, for Rust.',
			logo: {
				light: './src/assets/mark-light.svg',
				dark: './src/assets/mark-dark.svg',
				alt: 'oxtel',
			},
			favicon: '/favicon.svg',
			social: [
				{ icon: 'github', label: 'GitHub', href: 'https://github.com/agentstatelabs/oxtel' },
			],
			editLink: { baseUrl: 'https://github.com/agentstatelabs/oxtel/edit/main/site/' },
			customCss: ['./src/styles/custom.css'],
			sidebar: [
				{ label: 'Get started', slug: 'start' },
				{
					label: 'Scenarios',
					items: [
						{ label: 'Structured logs only', slug: 'scenarios/logs' },
						{ label: 'A web service', slug: 'scenarios/web-service' },
					],
				},
				{
					label: 'Guide',
					items: [
						{ label: 'Overview', slug: 'guide' },
						{ label: 'The words worth knowing', slug: 'guide/words' },
						{ label: 'Recipes', slug: 'guide/recipes' },
						{ label: 'Databases', slug: 'guide/databases' },
						{ label: "When something's wrong", slug: 'guide/troubleshooting' },
						{ label: 'eBPF and oxtel', slug: 'guide/ebpf' },
					],
				},
				{
					label: 'More',
					items: [
						{ label: 'Every setting', link: 'https://github.com/agentstatelabs/oxtel#settings' },
						{ label: 'API documentation', link: 'https://docs.rs/oxtel' },
						{ label: 'Design and roadmap', link: 'https://github.com/agentstatelabs/oxtel/blob/main/DESIGN.md' },
						{ label: 'Changelog', link: 'https://github.com/agentstatelabs/oxtel/blob/main/CHANGELOG.md' },
					],
				},
			],
		}),
	],
});
