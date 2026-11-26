# Baton documentation site

The Baton site is two things deployed together on Cloudflare Pages.

- `site/index.html` is the landing page at https://baton.openbrowser.me.
- `docs-site/` is the Mintlify source for https://baton.openbrowser.me/docs.

Mintlify's hosted product serves a site at the root of a domain, so a page at `/docs` cannot be hosted there. The docs are written in the Mintlify format and rendered with `mint export --base-path /docs`, then served as static files by Cloudflare Pages. The same source can still be published to Mintlify if it later gets its own domain.

## Preview

```sh
cd docs-site
npx -y mint@latest dev
```

## Validate

```sh
cd docs-site
npx -y mint@latest validate
```

The validator checks the navigation, every page, and the build. It must pass before a deploy.

## Build

```sh
docs-site/build.sh
```

The script validates the source, exports it with the `/docs` base path, and copies the result into `site/docs/`. That directory is generated and is not committed.

## Deploy

```sh
npx wrangler pages deploy site --project-name baton --branch main
```

`wrangler.jsonc` names `site` as the Pages output directory. The custom domain `baton.openbrowser.me` is attached to the `baton` Pages project.

## Writing

- One page, one mode: tutorial, how-to, reference, or explanation.
- Sentence-case headings. No em dashes. No emojis.
- Every command and flag must exist in the repository at the commit that lands the page.
- Do not name other agent tools or their design systems in the docs. Describe what Baton does.

## Demo media

`site/media/` holds the recordings embedded in the landing page and the docs.

```sh
cargo build --locked
uv run --with pyte --with pillow python scripts/record_demo.py \
  --demo commands --binary target/debug/baton --out site/media --font-size 16
uv run --with pyte --with pillow python scripts/record_demo.py \
  --demo review --binary target/debug/baton --out site/media --font-size 16
```

Each run writes an MP4, a GIF, and a poster image. The recorder replays the bytes the TUI wrote through a terminal emulator, so the frames match what a user sees. The harness in both demos is a script, and the pages that embed the media say so.
