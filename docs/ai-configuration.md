# AI configuration (BYOK)

AI assistance is optional and off by default. The service desk works fully
without it: with AI disabled, unconfigured, or unusable, every submitted ticket
goes straight to the shared operator queue and no provider is contacted.

An Admin configures it at runtime in the app (`/admin/ai`); nothing here is an
environment variable except the encryption key that protects the stored
provider keys. Code: `apps/api/src/ai_config.rs`. Tables: [schema.md](schema.md#ai-configuration).

## What is stored

- **Enablement**, the **selected provider** (`openai`, `gemini`, or `anthropic`;
  nothing else can be selected or stored) and its **model** identifier.
- **One API key per provider**, entered by an Admin. Changing the selected
  provider never moves a key: each provider only ever uses the key stored for
  it, and a provider without its own key cannot be enabled.
- The **result of the last connection test** as a stable code.

There is no custom endpoint or base URL setting. Requests go only to the
supported providers' fixed hosts, through the outbound client of the provider
adapters (#34), which owns SSRF and redirect protection.

## Secrets

- A key is encrypted with AES-256-GCM under `ENCRYPTION_KEY` before it is
  stored, with the provider name as associated data. A ciphertext copied to
  another provider's row does not decrypt, so a key cannot be sent to another
  vendor even by editing the database.
- Keys are write-only. No endpoint returns a key, its ciphertext, or its length.
  Admins see the last four characters (only for keys of 20+ characters), when it
  was set, and by which account.
- Keys never appear in logs, error responses, or audit metadata. In memory a key
  lives in `ai_config::ApiKey`, whose `Debug` output is `<redacted>`.
- `ENCRYPTION_KEY` lives outside the database (see
  [configuration.md](configuration.md)). A database dump alone does not reveal
  provider keys. Keep the encryption key's backup separate from database backups.

## Admin API

All of these require the Admin role (`ManageSettings`); every other role gets
`403 forbidden`. Every change writes an audit event without secrets.

| Request | Effect | Audit event |
|---|---|---|
| `GET /api/settings/ai` | Masked configuration, per-provider key metadata, effective `status`, last connection test | |
| `PUT /api/settings/ai` `{enabled, provider, model}` | Select provider/model, enable or disable | `ai_settings_updated` (`from`/`to`) |
| `PUT /api/settings/ai/credentials/{provider}` `{api_key}` | Store or replace that provider's key | `ai_credential_set` (`provider`, `replaced`) |
| `DELETE /api/settings/ai/credentials/{provider}` | Delete that provider's key | `ai_credential_removed` (`provider`, `disabled_ai`) |
| `POST /api/settings/ai/test` | Test the selected provider/model with its stored key | `ai_connection_tested` (`provider`, `model`, `result`) |

Rules:

- Enabling needs a provider, a model, and a readable stored key for that
  provider (`422` with `provider`/`model`: `required`, or `enabled`:
  `credential_required` / `credential_unreadable`).
- Deleting the selected provider's key also disables AI in the same
  transaction. Disabling or deleting takes effect for the next submission; Admin
  writes lock the settings row that submissions read `FOR SHARE`.
- A test result describes one provider, model, and key. Changing any of them
  resets it to `untested`.
- "Allow requester to skip AI" is the organization setting
  `allow_requester_ai_skip` (`PUT /api/admin/settings`, Admin only, audited as
  `organization_settings_updated`, default off). Category exemptions ("Skip AI
  automatically") are per category ([submission.md](submission.md)) and bypass AI
  regardless of that toggle.

`status` in the response is the effective state of the stored configuration:

| `status` | Meaning |
|---|---|
| `disabled` | Not enabled (default). |
| `credential_missing` | Enabled, but the selected provider has no key. |
| `credential_unreadable` | Enabled, but the key does not decrypt with the current `ENCRYPTION_KEY`. |
| `ready` | Enabled with a provider, a model, and a readable key. |

Anything but `ready` routes new tickets directly to operators. `active` says
whether new tickets are really routed through AI right now.

## Connection test and image capability

Screenshots are sent to the provider as image inputs, so the model must accept
images. The connection test succeeds only if the key authenticates and the model
is a supported, image-capable one. Failures are stored and returned as stable
codes, never as provider text: `ai_provider_auth_failed`,
`ai_provider_unreachable`, `ai_model_unsupported`, `ai_provider_failed`, plus
`ai_not_configured` and `ai_credential_unreadable` before any call is made.

The test goes through one function, `ai_config::check_provider`. Until it is
connected to the provider adapters (#34) it reports `ai_test_unavailable`
(`501`), which is stored like any other failure.

## Effective policy for requesters

`GET /api/ai/policy` (any signed-in account) returns
`{ai_assistance, requester_skip_allowed}`: whether new tickets in non-exempt
categories get an AI suggestion first, and whether the requester may skip it
(`false` whenever AI does not apply). It is read-only and reveals nothing about
the provider. The client cannot grant itself a skip: Rust re-reads the policy in
the submission transaction and answers a disallowed skip with `403`.

## How availability reaches the lifecycle

`ai_config::ai_available` is the single decision whether a new ticket is routed
through AI. `lifecycle::submit` passes the encryption key so
`lifecycle::policy` can ask it; the answer becomes `Policy::ai_available`
([lifecycle.md](lifecycle.md#routing-into-human-support)).

It requires a usable configuration (`ai_config::load_active`) **and** the
suggestion-attempt worker (#35). Until that worker exists nothing could move a
ticket out of `ai_processing`, so `ai_available` returns `false` and every
submission is queued for operators even when `status` is `ready`.

Code that needs the provider, model, and decrypted key calls
`ai_config::load_active(conn, &encryption_key)`. It returns the reason instead
when AI cannot run, and only ever decrypts the selected provider's key.

## Encryption key rotation and recovery

`ENCRYPTION_KEY` protects the stored AI keys and the SMTP credentials. There is
no re-encryption tool: stored secrets are re-entered after the key changes.

**Planned rotation**

1. Have the provider API key(s) and the SMTP username/password at hand (from
   your password manager or the provider consoles; the app cannot show them).
2. Generate a new key (`openssl rand -base64 32`), set `ENCRYPTION_KEY`, and
   restart the API.
3. From this moment the old ciphertexts are unreadable. Nothing breaks: AI shows
   `credential_unreadable` and new tickets go straight to operators; outbound
   email pauses.
4. As an Admin, enter the provider key again in `/admin/ai` (Replace key) and
   the SMTP credentials in `/admin/email`. AI becomes `ready` again once the
   selected provider's key is stored under the new key; re-enable it if it was
   disabled.
5. Store the new `ENCRYPTION_KEY` in your secret backup, separate from database
   backups, and destroy the old one.

**Lost or wrong key** (restore onto a host with a different `ENCRYPTION_KEY`,
or the key was lost): the API starts normally and ticketing works. `/admin/ai`
marks each affected key as unreadable. If the old key still exists, put it back
and restart. Otherwise follow steps 2 to 5; the old ciphertexts cannot be
recovered and are overwritten when the keys are entered again.

**Rotating a provider key** (leaked or expired): create a new key at the
provider, use Replace key in `/admin/ai`, then revoke the old key at the
provider. To stop provider calls immediately, disable AI or remove the key.

**Suspected exposure of `ENCRYPTION_KEY` together with a database copy**: treat
all stored provider keys and SMTP credentials as compromised. Revoke them at the
providers, then rotate as above.
