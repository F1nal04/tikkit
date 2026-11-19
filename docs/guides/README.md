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
| Understanding how the parts fit, or developing | [Architecture at a glance](architecture.md) |
| Fixing a problem on a running install | [Troubleshooting](troubleshooting.md), then the [operations runbook](../operations.md) |
| An Admin configuring the organization | [Administrator guide](admin.md) |
| Deciding whether to use AI, or setting up a provider key | [AI assistance guide](ai.md) |
| An Operator, Specialist or Team Lead working tickets | [Help desk guide](operator.md) |
| Anyone who submits tickets (every role) | [Requester guide](requester.md) |
| Checking that every documented promise is covered | [Coverage map](coverage.md); the [workflow test coverage](../workflow-coverage.md) lists the tests behind each workflow |
| Checking accessibility, or planning a screen-reader review | [Accessibility](../accessibility.md) (what is scanned, keyboard and failure-recovery tests, the screen-reader checklist) |
| Sizing the host or judging performance | [Performance baseline and capacity](../performance-baseline.md) |
| Deciding whether this release is ready, or what is still open | [Release validation](../release-validation.md): what was run against the release candidate, the results per acceptance criterion, remaining risks and the repository settings still to apply |

Reference pages behind the guides: [deployment](../deployment.md),
[backup and restore](../backup-restore.md), [operations](../operations.md),
[configuration](../configuration.md), [security](../security.md),
[retention](../retention.md), [audit](../audit.md), [testing](../testing.md),
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
| Images | **No release has been published yet**: build both images from your checkout ([install, step 1](install.md#1-get-the-files-and-the-images)). Once releases exist: `ghcr.io/f1nal04/tikkit-web:web-vX.Y.Z` and `ghcr.io/f1nal04/tikkit-api:api-vX.Y.Z`, versioned independently; use the pair listed together in a release |
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
pages. Commands, build output, development proxy and the facts about `ORIGIN`:
[architecture at a glance](architecture.md).

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

- **No release has been published yet**: there is no release tag and no image in
  the container registry. Build both images from your checkout
  ([install, step 1](install.md#1-get-the-files-and-the-images)); the registry path
  applies once releases exist.
- There is no Admin-side password reset. Password recovery needs outbound email.
  Without it, an Admin can deactivate the account and issue a new invitation,
  which creates a new account without the person's ticket history.
- The **My team** page for Team Leads is a placeholder. Team Leads work from the
  **Team queue** tab of the General queue page and from the ticket pages.
- The requester ticket page does not refresh by itself outside the AI step; use
  **Refresh**. Staff pages update automatically.
- The submission receipt and the Admin submission pages use the short status labels
  (for example "Queued", "Awaiting confirmation"); the requester ticket page uses the
  longer ones.
- Rate limits are counted per API process and reset on restart, so one API replica
  is supported.
- Email delivery is at least once across a crash: in the moment between the relay
  accepting a message and Tikkit recording it, a crash can send that message again.
- If two Admins edit the same team at the same moment, the later save wins; there
  is no conflict warning for team edits.
- A human screen-reader pass is still outstanding. Automated checks cover
  WCAG 2.2 A and AA; the [screen-reader checklist](../accessibility.md#screen-reader-pass)
  is what a person with VoiceOver or NVDA still has to walk.
- Notification outbox counts and the retry of failed email rows are available only
  through the Admin API, not as portal controls. The documented procedure needs bash,
  `curl` and `jq`
  ([operations](../operations.md#reading-the-outbox-and-retrying-failed-email)).
- Recovery objectives and the performance baseline are measurements on one
  machine, not guarantees
  ([backup and restore](../backup-restore.md#recovery-objectives),
  [performance baseline](../performance-baseline.md)).

## Conventions

- UI labels are written exactly as they appear in the English interface, in bold.
- Hostnames are placeholders such as `<your-domain>`. No guide contains real
  credentials; generate your own values.
- Roles are always spelled **User**, **Operator**, **Specialist**, **Team Lead**
  and **Admin**.
