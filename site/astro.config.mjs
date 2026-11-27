// @ts-check
import { defineConfig } from 'astro/config';
import starlight from '@astrojs/starlight';

// GitHub Pages, on the project's own domain, from its root. The old address,
// agentstatelabs.github.io/oxtel/, redirects here.
const site = 'https://oxtel.agentstatelabs.com';
const base = '/';

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
						{ label: 'A command-line tool', slug: 'scenarios/cli' },
						{ label: 'A background worker', slug: 'scenarios/worker' },
						{ label: 'An MCP server over stdio', slug: 'scenarios/mcp-stdio' },
						{ label: 'An MCP server over HTTP', slug: 'scenarios/mcp-http' },
						{ label: 'Database access', slug: 'scenarios/database' },
						{ label: 'Two services', slug: 'scenarios/two-services' },
						{ label: 'Prometheus scraping only', slug: 'scenarios/prometheus' },
						{ label: 'A library crate', slug: 'scenarios/library' },
						{ label: 'Testing your telemetry', slug: 'scenarios/testing' },
					],
				},
				{
					label: 'Guide',
					items: [
						{ label: 'Overview', slug: 'guide' },
						{ label: 'The words worth knowing', slug: 'guide/words' },
						{ label: 'Recipes', slug: 'guide/recipes' },
						{ label: 'Sending to a collector', slug: 'guide/sending' },
						{ label: 'Metrics for your backend', slug: 'guide/metrics' },
						{ label: 'Libraries oxtel instruments', slug: 'guide/libraries' },
						{ label: 'Databases', slug: 'guide/databases' },
						{ label: "When something's wrong", slug: 'guide/troubleshooting' },
						{ label: 'eBPF and oxtel', slug: 'guide/ebpf' },
					],
				},
				{
					label: 'Reference',
					items: [
						{ label: 'What works with what', slug: 'reference/integrations' },
						{ label: 'Every setting', slug: 'reference/settings' },
						{ label: 'Every span and metric', slug: 'reference/telemetry' },
					],
				},
				{
					label: 'More',
					items: [
						{ label: 'API documentation', link: 'https://docs.rs/oxtel' },
						{ label: 'Design and roadmap', link: 'https://github.com/agentstatelabs/oxtel/blob/main/DESIGN.md' },
						{ label: 'Changelog', link: 'https://github.com/agentstatelabs/oxtel/blob/main/CHANGELOG.md' },
					],
				},
			],
		}),
	],
});
