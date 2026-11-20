FROM oven/bun:1.4.3@sha256:ec06c3b6cea04192ae6770c434f668ca41d343ad19fa6472216c7b48be39c598 AS bun
FROM node:24-bookworm-slim@sha256:dc26ae92c9fb15cf6b1d63e20f0944b2efe0f115b20e00be48fb23fceb061c54 AS build
COPY --from=bun /usr/local/bin/bun /usr/local/bin/bun
WORKDIR /src
COPY package.json bun.lock ./
COPY apps/api/package.json apps/api/
COPY apps/web/package.json apps/web/
COPY packages/api-client/package.json packages/api-client/
RUN bun install --frozen-lockfile
COPY packages/api-client/src/ packages/api-client/src/
COPY apps/web/ apps/web/
RUN bun run --cwd apps/web build

FROM node:24-bookworm-slim@sha256:dc26ae92c9fb15cf6b1d63e20f0944b2efe0f115b20e00be48fb23fceb061c54
ENV NODE_ENV=production
WORKDIR /app
# Adapter-node bundles this app's workspace client and framework code.
COPY --from=build /src/apps/web/package.json apps/web/
COPY --from=build /src/apps/web/build/ apps/web/build/
USER node
CMD ["node", "apps/web/build/index.js"]
