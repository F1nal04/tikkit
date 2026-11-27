FROM oven/bun:1.4.5@sha256:3aa040e5445efa09d75bb428a64ba50db13e2b3ac9b6b0c7f145615f62b2ee18 AS bun
FROM node:26-bookworm-slim@sha256:5df79989ed685e09f0a98019db4cebfb22fb9df1a53c0161664a011f7ff6b0f1 AS build
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

FROM node:26-bookworm-slim@sha256:5df79989ed685e09f0a98019db4cebfb22fb9df1a53c0161664a011f7ff6b0f1
ENV NODE_ENV=production
WORKDIR /app
# Adapter-node bundles this app's workspace client and framework code.
COPY --from=build /src/apps/web/package.json apps/web/
COPY --from=build /src/apps/web/build/ apps/web/build/
USER node
CMD ["node", "apps/web/build/index.js"]
