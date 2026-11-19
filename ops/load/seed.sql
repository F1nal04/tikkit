BEGIN;

INSERT INTO accounts (username, display_name, email, password_hash, role, language, notify_email, notify_in_app)
SELECT seed.username, seed.username, seed.username || '@example.test', admin.password_hash, seed.role,
       CASE WHEN i % 5 = 0 THEN 'de' ELSE 'en' END,
       seed.role = 'user', seed.role = 'user'
FROM accounts admin
CROSS JOIN (
    SELECT 'load-user-' || i AS username, 'user' AS role, i FROM generate_series(1, :accounts) i
    UNION ALL
    SELECT 'load-operator-' || i, 'operator', i FROM generate_series(1, :operators) i
    UNION ALL
    SELECT 'load-lead-' || i, 'team_lead', i FROM generate_series(1, 3) i
    UNION ALL
    SELECT 'load-export-' || i, 'admin', i FROM generate_series(1, 3) i
) seed
WHERE admin.username = 'load-admin';

INSERT INTO teams (name, lead_account_id)
SELECT 'Load desk ' || right(username, 1), id FROM accounts WHERE role = 'team_lead';

INSERT INTO team_memberships (account_id, role, team_id)
SELECT a.id, a.role, tm.id FROM accounts a JOIN teams tm
  ON tm.name = 'Load desk ' || (
     CASE WHEN a.role = 'team_lead' THEN right(a.username, 1)::int
          ELSE 1 + (split_part(a.username, '-', 3)::int - 1) % 3 END
  )
WHERE a.role IN ('operator', 'team_lead');

-- Keep the historical profile focused on tickets, search and reporting.
-- Historical messages are assumed delivered; only live events enqueue SMTP.
ALTER TABLE ticket_events DISABLE TRIGGER ticket_notification_outbox;
CREATE TEMP TABLE bulk ON COMMIT DROP AS
SELECT gen_random_uuid() AS id, n, owner.id AS owner, c.id AS category, c.name AS category_name,
       c.skip_ai, p.id AS priority, p.name AS priority_name, tm.id AS team, tm.lead_account_id AS lead,
       (ARRAY['Printer offline', 'VPN reconnects', 'Outlook profile', 'Wi-Fi disconnects',
              'Monitor flickers', 'Calendar sync', 'Password manager'])[1 + n % 7]
         || ' / history ' || n AS subject,
       statement_timestamp() - interval '1 hour' - n * interval '55 seconds' AS created,
       n <= 2000 AND n % 12 = 2 AS awaiting
FROM generate_series(1, :tickets) n
JOIN accounts owner ON owner.username = 'load-user-' || (1 + n % :accounts)
JOIN categories c ON c.name = CASE WHEN n % 3 = 0 THEN 'Load AI' ELSE 'Load desk' END
JOIN priorities p ON p.name = CASE WHEN n % 7 = 0 THEN 'Urgent' ELSE 'Normal' END
JOIN teams tm ON tm.name = 'Load desk ' || (1 + n % 3);

INSERT INTO tickets (id, requester_account_id, category_id, priority_id, team_id, assignee_account_id,
                     status, subject, description, intake_snapshot, created_at, updated_at,
                     resolution_source, closure_reason, proposal_version, confirmation_deadline)
SELECT id, owner, category, priority,
       CASE WHEN n % 4 > 0 THEN team END,
       CASE WHEN n % 4 > 1 THEN lead END,
       CASE WHEN awaiting THEN 'awaiting_confirmation'
            ELSE CASE n % 4 WHEN 3 THEN 'closed' WHEN 2 THEN 'in_progress' ELSE 'queued' END END,
       subject, 'Intermittent workstation fault. Restarting temporarily restores service.',
       jsonb_build_object('subject', subject,
         'description', 'Intermittent workstation fault. Restarting temporarily restores service.',
         'catalog', jsonb_build_object('category_id', category, 'category_name', category_name,
           'skip_ai', skip_ai, 'priority_id', priority, 'priority_name', priority_name, 'answers', '[]'::jsonb),
         'screenshot_ids', '[]'::jsonb),
       created, created + interval '10 minutes',
       CASE WHEN n % 4 = 3 THEN 'operator' END,
       CASE WHEN n % 4 = 3 THEN 'requester_confirmed' END,
       CASE WHEN n % 4 = 3 OR awaiting THEN 1 ELSE 0 END,
       CASE WHEN awaiting THEN created + interval '168 hours 7 minutes' END
FROM bulk;

INSERT INTO ticket_events (ticket_id, actor_account_id, event_type, from_status, to_status, payload, created_at)
SELECT id, owner, 'submit', 'draft', 'queued', '{"route":"ai_disabled"}', created FROM bulk;

INSERT INTO ticket_events (ticket_id, actor_account_id, event_type, from_status, to_status, payload, created_at)
SELECT id, lead, 'assignment_changed', 'queued', 'queued',
       jsonb_build_object('team_id', team), created + interval '2 minutes'
FROM bulk WHERE n % 4 > 0;

INSERT INTO ticket_events (ticket_id, actor_account_id, event_type, from_status, to_status, created_at)
SELECT id, lead, 'start_work', 'queued', 'in_progress', created + interval '3 minutes'
FROM bulk WHERE n % 4 > 1;

WITH messages AS (
    INSERT INTO ticket_messages (ticket_id, author_account_id, kind, body, created_at,
                                 author_kind, idempotency_key, request_hash)
    SELECT id, lead, kind, body, created + offset_time, 'staff', gen_random_uuid(),
           sha256(convert_to(body, 'UTF8'))
    FROM bulk CROSS JOIN (VALUES
        ('public_reply', 'Please try the replacement profile.', interval '4 minutes'),
        ('internal_note', 'Checked workstation asset history.', interval '5 minutes')
    ) AS msg(kind, body, offset_time)
    WHERE n % 4 > 1
    RETURNING *
)
INSERT INTO ticket_events (ticket_id, actor_account_id, event_type, payload, created_at)
SELECT ticket_id, author_account_id,
       CASE kind WHEN 'public_reply' THEN 'public_reply_added' ELSE 'internal_note_added' END,
       jsonb_build_object('message_id', id), created_at
FROM messages;

INSERT INTO ticket_events (ticket_id, actor_account_id, event_type, from_status, to_status, payload, created_at)
SELECT id, lead, 'propose_resolution', 'in_progress', 'awaiting_confirmation',
       jsonb_build_object('proposal_version', 1, 'summary', 'Replaced the profile.',
                          'timeout_hours', 168, 'deadline', created + interval '168 hours 7 minutes'),
       created + interval '7 minutes'
FROM bulk WHERE n % 4 = 3 OR awaiting;

INSERT INTO ticket_events (ticket_id, actor_account_id, event_type, from_status, to_status, payload, created_at)
SELECT id, owner, 'accept_resolution', 'awaiting_confirmation', 'closed',
       '{"proposal_version":1,"closure_reason":"requester_confirmed"}', created + interval '10 minutes'
FROM bulk WHERE n % 4 = 3;

INSERT INTO ticket_submission_keys (requester_account_id, idempotency_key, request_hash, ticket_id, submitted_status)
SELECT owner, gen_random_uuid(), sha256(convert_to(subject, 'UTF8')), id, 'queued' FROM bulk;

ALTER TABLE ticket_events ENABLE TRIGGER ticket_notification_outbox;
COMMIT;
ANALYZE;
