# The oxtel site

The source of https://agentstatelabs.github.io/oxtel/, built with
[Astro](https://astro.build) and [Starlight](https://starlight.astro.build).
Pull requests that change it are built and link-checked by
`.github/workflows/site.yml`, and main is published to GitHub Pages.

```bash
npm ci
npm run dev        # http://localhost:4321/oxtel/
npm run build && node scripts/check-links.mjs
```

## Code on pages comes from the repository

Pages never contain Rust code or dependency lists of their own. They show
parts of files in the repository, which CI compiles and checks:

```mdx
import Example from '../../components/Example.astro';

<Example file="examples/web.rs" />                     {/* the file, without its //! documentation */}
<Example file="examples/web.rs" region="serve" />      {/* between // ANCHOR: serve and // ANCHOR_END: serve */}
<Example file="examples/logs.rs" doc="toml" />         {/* the first ```toml block in its //! documentation */}
<Example file="examples/logs.rs" doc="toml" nth={2} /> {/* the second */}
```

`cargo run -p xtask -- check-examples` builds each example once for every
`[dependencies]` block in its documentation, with only those dependencies,
so the lists the site shows are known to be enough. A region or block that
doesn't exist fails the site's build.

## Links

Write links between pages from the site's root, as `/start/`; the base path
(`/oxtel` while the site is on GitHub Pages) is added when the site is built.
