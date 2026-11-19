# Tikkit guides

Task-oriented guides for people who install, run and use Tikkit and did not
build it. Tikkit is a self-hosted IT ticketing system for **one organization per
deployment**: a requester portal, a shared help-desk queue, team-based support,
administration, and optional AI assistance with your own provider key.

The product interface is available in **English and German**. These guides are
in English. The deeper reference pages in [`docs/`](..) are the source for
detail; the guides give the working steps and link to them.

## Who should read what

| You are… | Start here |
| --- | --- |
| Installing or upgrading Tikkit on a host | [Install with Docker Compose](install.md) |
| Fixing a problem on a running install | [Troubleshooting](troubleshooting.md), then the [operations runbook](../operations.md) |
| An Admin configuring the organization | [Administrator guide](admin.md) |
| Deciding whether to use AI, or setting up a provider key | [AI assistance guide](ai.md) |
| An Operator, Specialist or Team Lead working tickets | [Help desk guide](operator.md) |
| Anyone who submits tickets (every role) | [Requester guide](requester.md) |
| Checking that every documented promise is covered | [Coverage map](coverage.md) |

Reference pages behind the guides: [deployment](../deployment.md),
[backup and restore](../backup-restore.md), [operations](../operations.md),
[configuration](../configuration.md), [security](../security.md),
[retention](../retention.md), [audit](../audit.md),
[admin reports](../admin-reports.md), [lifecycle](../lifecycle.md),
[queues](../queues.md), [authorization](../authorization.md). Developers
start with the repository [README](../../README.md) and [AGENTS.md](../../AGENTS.md).

## Supported versions

| Component | Supported |
| --- | --- |
| Host | Linux x86-64. Published images are `linux/amd64`; the same Dockerfiles build locally on Linux ARM64. Docker Desktop is fine for testing. |
| Docker Engine / Compose | Engine 28 or newer; Compose v2.33.1 or newer |
| PostgreSQL | 17 (the reference Compose file pins a 17.12 image). A major-version upgrade needs its own dump-and-restore procedure. |
| Reverse proxy | Caddy, pinned in the reference Compose file (TLS certificates and HTTP→HTTPS redirect included) |
| Images | `ghcr.io/f1nal04/tikkit-web:web-vX.Y.Z` and `ghcr.io/f1nal04/tikkit-api:api-vX.Y.Z`, versioned independently; use the pair listed together in a release |
| Build from source (optional) | Bun 1.4.3+, Node 22.17+ (images use Node 24), Rust 1.88+ with rustfmt and Clippy |
| Browsers | Automated browser tests run on Chromium. The portal serves standard HTML pages; use a current browser. |
| AI providers | OpenAI, Google Gemini, Anthropic (official APIs, bring your own key) |
| Email | Any SMTP relay that offers STARTTLS or implicit TLS with a valid certificate, for **outbound** mail |

## How Tikkit is arranged

Pick one host, run the Compose stack, put one public hostname in front of it.
The stack has a TLS reverse proxy (Caddy), the web server (SvelteKit on Node),
the API and background workers (Rust), and PostgreSQL. The proxy sends `/api/*` to
the API and everything else to the web server. The API owns authentication,
authorization, validation and all business rules; the web server only renders
pages. Details for developers: the repository [README](../../README.md) and
[frontend structure](../frontend.md).

## Release limitations

These are **not in this release**. Do not plan around them:

- SLA targets, response or resolution deadlines, business calendars, breach
  reporting, reminders and escalation automation. Basic timing statistics and the
  specific **resolution-confirmation timeout** (default 168 hours, auto-close of an
  unanswered staff proposal) *are* included and are separate from SLAs.
- A knowledge base: authoring, requester search or browsing, and AI grounding or
  citations. AI works from the ticket alone.
- AI providers other than OpenAI, Google Gemini and Anthropic, and self-hosted
  models.
- Enterprise single sign-on (OIDC or similar). Accounts are username and password,
  created through Admin invitations.
- Inbound email: tickets and replies are created in the portal only. Replying to a
  notification email changes nothing.
- Arbitrary file attachments. Only static PNG, JPEG and WebP screenshots.
- Several organizations in one deployment (multi-tenancy).
- Hosted-platform or Kubernetes deployment guides. The supported deployment is
  single-host Docker Compose.
- Also not built, by design: automatic routing of tickets to teams by category,
  priority or AI result; automatic or round-robin assignment; AI chat, follow-up
  questions or regenerated suggestions; a form builder.

## Known limitations of this release

- The first hosted release and the first image publication to the container
  registry are still pending. If the image tag you need is not published, build
  both images from your checkout ([install, step 1](install.md#1-get-the-files-and-the-images)).
- There is no Admin-side password reset. Password recovery needs outbound email.
- The **My team** page for Team Leads is a placeholder; Team Leads manage their team
  from the General queue page's **Team queue** tab and the ticket pages.
- Notification outbox counts and the retry of failed email rows are available only
  through the Admin API, not as portal controls.
- Recovery objectives are measured numbers on one machine, not guarantees
  ([backup and restore](../backup-restore.md#recovery-objectives)).

## Conventions

- UI labels are written exactly as they appear in the English interface, in bold.
- Hostnames are placeholders such as `<your-domain>`. No guide contains real
  credentials; generate your own values.
- Roles are always spelled **User**, **Operator**, **Specialist**, **Team Lead**
  and **Admin**.
