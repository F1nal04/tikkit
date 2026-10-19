//! Append-only public replies and private notes. See docs/conversations.md.
use axum::{extract::State, http::StatusCode};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::{Digest, Sha256};
use sqlx::PgConnection;
use time::OffsetDateTime;
use utoipa::ToSchema;
use uuid::Uuid;

use crate::{
    AppState,
    api::{ApiError, FieldErrors, Json, Page, Pagination, Path, Query, Role, TicketStatus},
    auth::{CurrentUser, ident},
    authz::{self, Actor, Permission, TicketScope, TicketView},
    lifecycle::{self, By, Transition},
    screenshots,
};

pub const BODY_MAX: usize = 10_000;

#[derive(Clone, Copy, Debug, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum MessageKind {
    PublicReply,
    InternalNote,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum AuthorKind {
    Requester,
    Staff,
}

#[derive(Deserialize, Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct SendMessage {
    /// New UUID per send. Reuse the UUID and exact input to retry safely.
    pub idempotency_key: Uuid,
    /// Plain text, 1–10,000 characters, nonblank. HTML and Markdown are not interpreted.
    pub body: String,
    /// Own pending screenshot uploads (at most 10, distinct).
    #[serde(default)]
    pub screenshot_ids: Vec<Uuid>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct Message {
    pub id: Uuid,
    /// Stable ascending order; gaps are valid. Never order by transaction timestamps.
    pub sequence: i64,
    pub kind: MessageKind,
    pub author_kind: AuthorKind,
    /// Immutable account identity for staff consumers; null in requester responses.
    #[schema(required = true)]
    pub author_account_id: Option<Uuid>,
    /// User-authored plain text; render as text, never HTML.
    pub author_display_name: String,
    /// Original plain text; render as text, never HTML or Markdown.
    pub body: String,
    /// HTML-escaped text for renderers; no tags, links, or embedded attachments.
    /// Preserve newlines with CSS `white-space: pre-wrap`.
    pub body_html: String,
    pub screenshot_ids: Vec<Uuid>,
    #[serde(with = "time::serde::rfc3339")]
    #[schema(value_type = String, format = DateTime)]
    pub created_at: OffsetDateTime,
}

#[derive(sqlx::FromRow)]
struct MessageRow {
    id: Uuid,
    sequence: i64,
    kind: String,
    author_kind: String,
    author_account_id: Uuid,
    author_display_name: String,
    body: String,
    screenshot_ids: Vec<Uuid>,
    created_at: OffsetDateTime,
}

impl MessageRow {
    fn visible(self, view: TicketView) -> Result<Message, ApiError> {
        Ok(Message {
            id: self.id,
            sequence: self.sequence,
            kind: ident(self.kind)?,
            author_kind: ident(self.author_kind)?,
            author_account_id: (view == TicketView::Staff).then_some(self.author_account_id),
            author_display_name: self.author_display_name,
            body_html: escape(&self.body),
            body: self.body,
            screenshot_ids: self.screenshot_ids,
            created_at: self.created_at,
        })
    }
}

const SELECT: &str = "SELECT m.id, m.sequence, m.kind, m.author_kind, m.author_account_id,
    a.display_name AS author_display_name, m.body, m.created_at,
    ARRAY(SELECT s.id FROM screenshots s WHERE s.message_id = m.id ORDER BY s.id) AS screenshot_ids
    FROM ticket_messages m JOIN accounts a ON a.id = m.author_account_id";

fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(c),
        }
    }
    out
}

/// Private notes require the staff representation AND a destination team (or
/// Admin). Returning a ticket to general intake must not publish its notes.
pub(crate) fn can_read_notes(actor: &Actor, scope: &TicketScope) -> bool {
    authz::ticket_view(actor, scope) == Some(TicketView::Staff)
        && (scope.team_id.is_some() || actor.role == Role::Admin)
}

async fn scope(db: &mut PgConnection, id: Uuid, lock: bool) -> Result<TicketScope, ApiError> {
    let sql = format!(
        "SELECT requester_account_id, team_id, assignee_account_id, status::text FROM tickets WHERE id = $1{}",
        if lock { " FOR UPDATE" } else { "" }
    );
    let (requester_account_id, team_id, assignee_account_id, status): (
        Uuid,
        Option<Uuid>,
        Option<Uuid>,
        String,
    ) = sqlx::query_as(&sql)
        .bind(id)
        .fetch_optional(db)
        .await?
        .ok_or(ApiError::NotFound)?;
    Ok(TicketScope {
        requester_account_id,
        team_id,
        assignee_account_id,
        status: ident(status)?,
    })
}

/// Conversation oldest first, with private rows excluded in SQL before pagination.
/// `cursor` is an opaque sequence position, not the default timestamp cursor.
#[utoipa::path(get, path = "/api/tickets/{id}/messages", tag = "tickets",
    params(("id" = Uuid, Path), Pagination),
    responses((status = 200, body = Page<Message>),
        (status = 401, body = crate::api::ErrorEnvelope),
        (status = 404, description = "Missing or invisible ticket", body = crate::api::ErrorEnvelope),
        (status = 422, description = "Invalid cursor", body = crate::api::ErrorEnvelope)))]
pub async fn list(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(id): Path<Uuid>,
    Query(page): Query<Pagination>,
) -> Result<Json<Page<Message>>, ApiError> {
    let after = match &page.cursor {
        None => 0,
        Some(raw) => URL_SAFE_NO_PAD
            .decode(raw)
            .ok()
            .and_then(|b| String::from_utf8(b).ok())
            .and_then(|s| s.parse::<i64>().ok())
            .filter(|n| *n > 0)
            .ok_or_else(|| {
                ApiError::Validation(FieldErrors::from([(
                    "cursor".into(),
                    vec!["invalid_cursor"],
                )]))
            })?,
    };
    let mut tx = state.pool.begin().await?;
    sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ READ ONLY")
        .execute(&mut *tx)
        .await?;
    let actor = Actor::load(&mut *tx, &user).await?;
    let scope = scope(&mut tx, id, false).await?;
    let view = authz::ticket_view(&actor, &scope).ok_or(ApiError::NotFound)?;
    let mut rows: Vec<MessageRow> = sqlx::query_as(&format!(
        "{SELECT}
        WHERE m.ticket_id = $1 AND m.sequence > $2 AND (m.kind = 'public_reply' OR $3)
        ORDER BY m.sequence LIMIT $4"
    ))
    .bind(id)
    .bind(after)
    .bind(can_read_notes(&actor, &scope))
    .bind(page.limit() + 1)
    .fetch_all(&mut *tx)
    .await?;
    let more = rows.len() as i64 > page.limit();
    rows.truncate(page.limit() as usize);
    let next_cursor = rows
        .last()
        .filter(|_| more)
        .map(|r| URL_SAFE_NO_PAD.encode(r.sequence.to_string()));
    let items = rows
        .into_iter()
        .map(|r| r.visible(view))
        .collect::<Result<_, _>>()?;
    Ok(Json(Page { items, next_cursor }))
}

/// Send a public reply. Ownership takes precedence over staff role: the ticket's
/// submitting account always speaks as requester. No AI work is scheduled.
#[utoipa::path(post, path = "/api/tickets/{id}/replies", tag = "tickets",
    params(("id" = Uuid, Path)), request_body = SendMessage,
    responses((status = 201, description = "Committed message (same receipt on identical retry)", body = Message),
        (status = 401, body = crate::api::ErrorEnvelope),
        (status = 403, description = "Staff writer denied", body = crate::api::ErrorEnvelope),
        (status = 404, description = "Missing or invisible ticket", body = crate::api::ErrorEnvelope),
        (status = 409, description = "Requester ticket not open, or key reused with different input", body = crate::api::ErrorEnvelope),
        (status = 422, description = "Invalid body or screenshots", body = crate::api::ErrorEnvelope)))]
pub async fn reply(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(id): Path<Uuid>,
    Json(input): Json<SendMessage>,
) -> Result<(StatusCode, Json<Message>), ApiError> {
    send(state, user, id, input, MessageKind::PublicReply).await
}

/// Add a private internal note. Author, staff role, and visibility come from
/// the server's current ticket policy, never client-supplied fields.
#[utoipa::path(post, path = "/api/tickets/{id}/notes", tag = "tickets",
    params(("id" = Uuid, Path)), request_body = SendMessage,
    responses((status = 201, description = "Committed private note (same receipt on identical retry)", body = Message),
        (status = 401, body = crate::api::ErrorEnvelope),
        (status = 403, description = "Staff writer denied (including self-handling)", body = crate::api::ErrorEnvelope),
        (status = 404, description = "Missing or invisible ticket", body = crate::api::ErrorEnvelope),
        (status = 409, description = "Key reused with different input", body = crate::api::ErrorEnvelope),
        (status = 422, description = "Invalid body or screenshots", body = crate::api::ErrorEnvelope)))]
pub async fn note(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(id): Path<Uuid>,
    Json(input): Json<SendMessage>,
) -> Result<(StatusCode, Json<Message>), ApiError> {
    send(state, user, id, input, MessageKind::InternalNote).await
}

async fn send(
    state: AppState,
    user: CurrentUser,
    ticket: Uuid,
    input: SendMessage,
    kind: MessageKind,
) -> Result<(StatusCode, Json<Message>), ApiError> {
    let mut errors = FieldErrors::new();
    if input.body.trim().is_empty() {
        errors.insert("body".into(), vec!["required"]);
    } else if input.body.chars().count() > BODY_MAX {
        errors.insert("body".into(), vec!["too_long"]);
    } else if input.body.contains('\0') {
        errors.insert("body".into(), vec!["invalid"]);
    }
    screenshots::check_ids(&mut errors, &input.screenshot_ids);
    if !errors.is_empty() {
        return Err(ApiError::Validation(errors));
    }
    let hash = Sha256::digest(serde_json::to_vec(&(kind, &input)).map_err(|_| ApiError::Internal)?)
        .to_vec();
    let mut tx = state.pool.begin().await?;
    // Same lock order as lifecycle: account/membership/leadership, then ticket.
    let actor = lifecycle::lock_actor(&mut tx, user.account_id()).await?;
    let scope = scope(&mut tx, ticket, true).await?;
    let view = authz::ticket_view(&actor, &scope).ok_or(ApiError::NotFound)?;
    let private = matches!(kind, MessageKind::InternalNote);
    if private && !can_read_notes(&actor, &scope) {
        return Err(ApiError::Forbidden);
    }
    // Under the ticket lock duplicate sends serialize, even across API processes.
    // Recheck current visibility, but a committed receipt survives closure/reassignment.
    let existing: Option<(Uuid, Vec<u8>)> = sqlx::query_as("SELECT id, request_hash FROM ticket_messages WHERE ticket_id=$1 AND author_account_id=$2 AND idempotency_key=$3")
        .bind(ticket).bind(actor.id).bind(input.idempotency_key).fetch_optional(&mut *tx).await?;
    let id = if let Some((id, stored)) = existing {
        if stored != hash {
            return Err(ApiError::Conflict);
        }
        id
    } else {
        let requester = view == TicketView::Requester;
        if requester && !private {
            if !matches!(
                scope.status,
                TicketStatus::Queued
                    | TicketStatus::InProgress
                    | TicketStatus::WaitingForRequester
                    | TicketStatus::AwaitingConfirmation
            ) {
                return Err(ApiError::Conflict);
            }
        } else {
            authz::authorize_ticket(
                &actor,
                &scope,
                if private {
                    Permission::AddInternalNote
                } else {
                    Permission::ReplyAsStaff
                },
            )?;
        }
        let id = Uuid::new_v4();
        let kind = if private {
            "internal_note"
        } else {
            "public_reply"
        };
        sqlx::query("INSERT INTO ticket_messages (id,ticket_id,author_account_id,kind,body,author_kind,idempotency_key,request_hash,created_at)
            VALUES ($1,$2,$3,$4,$5,$6,$7,$8,clock_timestamp())")
            .bind(id).bind(ticket).bind(actor.id).bind(kind).bind(&input.body)
            .bind(if requester { "requester" } else { "staff" }).bind(input.idempotency_key).bind(&hash).execute(&mut *tx).await?;
        screenshots::attach_message(&mut tx, actor.id, &input.screenshot_ids, ticket, id).await?;
        if requester && scope.status == TicketStatus::WaitingForRequester {
            lifecycle::transition(
                &mut tx,
                ticket,
                By::Account(actor.id),
                &Transition::RequesterResponded,
            )
            .await?;
        }
        // No content in events: consumers must reload the message AND recheck
        // current visibility at delivery, including team changes and self-submission.
        sqlx::query("INSERT INTO ticket_events (ticket_id,actor_account_id,event_type,payload) VALUES ($1,$2,$3,$4)")
            .bind(ticket).bind(actor.id).bind(if private { "internal_note_added" } else { "public_reply_added" })
            .bind(json!({"message_id":id})).execute(&mut *tx).await?;
        if !private {
            sqlx::query("UPDATE tickets SET updated_at=clock_timestamp() WHERE id=$1")
                .bind(ticket)
                .execute(&mut *tx)
                .await?;
        }
        id
    };
    let row: MessageRow = sqlx::query_as(&format!("{SELECT} WHERE m.id=$1"))
        .bind(id)
        .fetch_one(&mut *tx)
        .await?;
    let message = row.visible(view)?;
    tx.commit().await?;
    Ok((StatusCode::CREATED, Json(message)))
}
