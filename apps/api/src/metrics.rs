//! Process counters and live PostgreSQL backlog gauges for the private listener.
//! All label values come from fixed enums or numeric histogram boundaries.

use std::{
    fmt::{Display, Write},
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering::Relaxed},
    },
    time::Duration,
};

use axum::{
    Router,
    extract::State,
    http::{StatusCode, header},
    response::IntoResponse,
    routing::get,
};
use sqlx::PgPool;
use tokio::net::TcpListener;

use crate::{
    AppState,
    ai_attempts::{MAX_CLAIMS, SWEEP_DUE_AT, sweep_grace_seconds},
};

const BUCKETS_MS: [u32; 9] = [5, 10, 25, 50, 100, 250, 500, 1000, 5000];
const STATUS_CLASSES: [&str; 5] = ["1xx", "2xx", "3xx", "4xx", "5xx"];

macro_rules! labels {
    ($name:ident { $($variant:ident => $label:literal),+ $(,)? }) => {
        #[derive(Clone, Copy)]
        pub(crate) enum $name { $($variant),+ }
        impl $name {
            const ALL: &'static [Self] = &[$(Self::$variant),+];
            pub(crate) fn label(self) -> &'static str {
                match self { $(Self::$variant => $label),+ }
            }
        }
    };
}

labels!(Worker {
    AiAttempts => "ai_attempts",
    AiSweep => "ai_attempt_sweep",
    Notifications => "notifications",
    AutoClose => "auto_close",
    Retention => "retention",
});

labels!(Event {
    DirectIntake => "queue_intake_direct",
    AiHandoff => "queue_intake_after_ai",
    HandoffFailed => "ai_handoff_failed",
    GenerationSucceeded => "ai_generation_succeeded",
    GenerationFailed => "ai_generation_failed",
    GenerationRefused => "ai_generation_refused",
    AiConfirmed => "ai_requester_confirmed",
    HumanConfirmed => "human_requester_confirmed",
    ConfirmationTimeout => "confirmation_timeout_closed",
    MailFailed => "notification_email_failed",
});

#[derive(Clone, Copy, Default)]
struct Histogram {
    buckets: [u64; BUCKETS_MS.len()],
    count: u64,
    sum_seconds: f64,
}

#[derive(Default)]
struct WorkerStats {
    failures: AtomicU64,
    retries: AtomicU64,
    last_success: AtomicU64,
}

#[derive(Default)]
struct Inner {
    requests: Mutex<[Histogram; STATUS_CLASSES.len()]>,
    workers: [WorkerStats; Worker::ALL.len()],
    events: [AtomicU64; Event::ALL.len()],
}

/// Cheap clones share one process-local collector between HTTP and workers.
#[derive(Clone, Default)]
pub struct Metrics(Arc<Inner>);

impl Metrics {
    pub(crate) fn request(&self, status: StatusCode, elapsed: Duration) {
        let Some(index) = usize::from(status.as_u16() / 100).checked_sub(1) else {
            return;
        };
        let mut requests = self.0.requests.lock().expect("request metrics lock");
        let Some(series) = requests.get_mut(index) else {
            return;
        };
        let seconds = elapsed.as_secs_f64();
        // Convert to cumulative buckets on export so the request only updates one.
        if let Some(bucket) = BUCKETS_MS
            .iter()
            .position(|ms| seconds <= f64::from(*ms) / 1000.)
        {
            series.buckets[bucket] += 1;
        }
        series.count += 1;
        series.sum_seconds += seconds;
    }

    pub(crate) fn event(&self, event: Event, count: u64) {
        self.0.events[event as usize].fetch_add(count, Relaxed);
    }

    pub(crate) fn failure(&self, worker: Worker) {
        self.0.workers[worker as usize]
            .failures
            .fetch_add(1, Relaxed);
    }

    pub(crate) fn retry(&self, worker: Worker) {
        self.0.workers[worker as usize]
            .retries
            .fetch_add(1, Relaxed);
    }

    pub(crate) fn step(&self, worker: Worker, succeeded: bool) {
        if succeeded {
            self.0.workers[worker as usize].last_success.store(
                time::OffsetDateTime::now_utc().unix_timestamp().max(0) as u64,
                Relaxed,
            );
        } else {
            self.failure(worker);
        }
    }

    fn render(&self) -> String {
        let mut output = String::new();
        let requests = *self.0.requests.lock().expect("request metrics lock");
        let http = "tikkit_http_request_duration_seconds";
        metric_header(
            &mut output,
            http,
            "histogram",
            "API response latency by status class.",
        );
        for (status, series) in STATUS_CLASSES.iter().zip(requests.iter()) {
            let labels = format!(r#"status_class="{status}""#);
            let mut cumulative = 0;
            for (ms, bucket) in BUCKETS_MS.iter().zip(series.buckets) {
                cumulative += bucket;
                writeln!(
                    output,
                    r#"{http}_bucket{{{labels},le="{}"}} {cumulative}"#,
                    f64::from(*ms) / 1000.
                )
                .unwrap();
            }
            writeln!(
                output,
                r#"{http}_bucket{{{labels},le="+Inf"}} {}"#,
                series.count
            )
            .unwrap();
            writeln!(output, "{http}_sum{{{labels}}} {}", series.sum_seconds).unwrap();
            writeln!(output, "{http}_count{{{labels}}} {}", series.count).unwrap();
        }
        let workers = Worker::ALL.iter().copied().zip(&self.0.workers);
        worker_family(
            &mut output,
            "failures_total",
            "counter",
            "Failed worker processing steps.",
            workers
                .clone()
                .map(|(worker, stats)| (worker, stats.failures.load(Relaxed))),
        );
        worker_family(
            &mut output,
            "retries_total",
            "counter",
            "Worker retries scheduled or taken over.",
            workers
                .clone()
                .map(|(worker, stats)| (worker, stats.retries.load(Relaxed))),
        );
        worker_family(
            &mut output,
            "last_success_timestamp_seconds",
            "gauge",
            "Unix timestamp of the last successful worker poll.",
            workers.map(|(worker, stats)| (worker, stats.last_success.load(Relaxed))),
        );
        metric_header(
            &mut output,
            "tikkit_ticket_operations_total",
            "counter",
            "Observed ticket workflow and delivery outcomes.",
        );
        for event in Event::ALL {
            writeln!(
                output,
                r#"tikkit_ticket_operations_total{{operation="{}"}} {}"#,
                event.label(),
                self.0.events[*event as usize].load(Relaxed)
            )
            .unwrap();
        }
        output
    }
}

fn metric_header(output: &mut String, name: &str, kind: &str, help: &str) {
    writeln!(output, "# HELP {name} {help}\n# TYPE {name} {kind}").unwrap();
}

fn worker_family(
    output: &mut String,
    name: &str,
    kind: &str,
    help: &str,
    rows: impl Iterator<Item = (Worker, impl Display)>,
) {
    let metric = format!("tikkit_worker_{name}");
    metric_header(output, &metric, kind, help);
    for (worker, value) in rows {
        writeln!(output, r#"{metric}{{worker="{}"}} {value}"#, worker.label()).unwrap();
    }
}

/// The operations listener is disabled by default and can bind only to loopback.
/// Its router is never merged into the public API router.
pub async fn bind(port: u16) -> std::io::Result<TcpListener> {
    TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, port)).await
}

pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/metrics", get(scrape))
        .with_state(state)
}

#[derive(sqlx::FromRow)]
struct Backlogs {
    ai_pending: i64,
    ai_due: i64,
    ai_lag: f64,
    mail_pending: i64,
    mail_due: i64,
    mail_lag: f64,
    close_pending: i64,
    close_due: i64,
    close_lag: f64,
    retention_pending: i64,
    retention_due: i64,
    retention_lag: f64,
}

async fn backlogs(pool: &PgPool, call_timeout: Duration) -> Result<Backlogs, sqlx::Error> {
    // Only active rows; these predicates use the existing partial indexes.
    // An orphaned AI-stage ticket is due for the sweep immediately.
    let query = format!(
        "WITH ai AS (
             SELECT ({SWEEP_DUE_AT}) AS due
             FROM tickets t LEFT JOIN ai_attempts a ON a.ticket_id = t.id AND a.state = 'processing'
             WHERE t.status = 'ai_processing'
         ), mail AS (
             SELECT available_at AS due FROM notification_outbox WHERE status = 'pending'
         ), closing AS (
             SELECT confirmation_deadline AS due FROM tickets
             WHERE status = 'awaiting_confirmation' AND confirmation_deadline IS NOT NULL
         ), retention AS (
             SELECT created_at AS due FROM retention_runs WHERE state = 'pending'
         )
         SELECT ai_count.pending AS ai_pending, ai_count.due AS ai_due, ai_count.lag AS ai_lag,
                mail_count.pending AS mail_pending, mail_count.due AS mail_due, mail_count.lag AS mail_lag,
                close_count.pending AS close_pending, close_count.due AS close_due, close_count.lag AS close_lag,
                retention_count.pending AS retention_pending, retention_count.due AS retention_due,
                retention_count.lag AS retention_lag
         FROM (
             SELECT count(*) AS pending, count(*) FILTER (WHERE due <= statement_timestamp()) AS due,
                    GREATEST(0, EXTRACT(EPOCH FROM statement_timestamp() - min(due)))::float8 AS lag
             FROM ai
         ) ai_count CROSS JOIN (
             SELECT count(*) AS pending, count(*) FILTER (WHERE due <= statement_timestamp()) AS due,
                    GREATEST(0, EXTRACT(EPOCH FROM statement_timestamp() - min(due)))::float8 AS lag
             FROM mail
         ) mail_count CROSS JOIN (
             SELECT count(*) AS pending, count(*) FILTER (WHERE due <= statement_timestamp()) AS due,
                    GREATEST(0, EXTRACT(EPOCH FROM statement_timestamp() - min(due)))::float8 AS lag
             FROM closing
         ) close_count CROSS JOIN (
             SELECT count(*) AS pending, count(*) FILTER (WHERE due <= statement_timestamp()) AS due,
                    GREATEST(0, EXTRACT(EPOCH FROM statement_timestamp() - min(due)))::float8 AS lag
             FROM retention
         ) retention_count"
    );
    sqlx::query_as(&query)
        .bind(MAX_CLAIMS)
        .bind(sweep_grace_seconds(call_timeout))
        .fetch_one(pool)
        .await
}

async fn scrape(State(state): State<AppState>) -> impl IntoResponse {
    let mut output = state.metrics.render();
    let checks = crate::probe_readiness(&state.pool).await;
    let ready = checks.migrations == "ok";
    let snapshot = if ready {
        tokio::time::timeout(
            Duration::from_secs(2),
            backlogs(&state.pool, state.ai_client.total_timeout()),
        )
        .await
        .ok()
        .and_then(Result::ok)
    } else {
        None
    };
    for (name, value, help) in [
        (
            "tikkit_database_ready",
            ready,
            "Whether PostgreSQL is reachable with the expected schema.",
        ),
        (
            "tikkit_worker_snapshot_up",
            snapshot.is_some(),
            "Whether the worker backlog snapshot was collected.",
        ),
    ] {
        metric_header(&mut output, name, "gauge", help);
        writeln!(output, "{name} {}", u8::from(value)).unwrap();
    }
    if let Some(s) = snapshot {
        let rows = [
            (Worker::AiAttempts, s.ai_pending, s.ai_due, s.ai_lag),
            (
                Worker::Notifications,
                s.mail_pending,
                s.mail_due,
                s.mail_lag,
            ),
            (Worker::AutoClose, s.close_pending, s.close_due, s.close_lag),
            (
                Worker::Retention,
                s.retention_pending,
                s.retention_due,
                s.retention_lag,
            ),
        ];
        worker_family(
            &mut output,
            "pending_jobs",
            "gauge",
            "Number of pending jobs, including ones not yet due.",
            rows.into_iter()
                .map(|(worker, pending, _, _)| (worker, pending)),
        );
        worker_family(
            &mut output,
            "due_jobs",
            "gauge",
            "Number of pending jobs at or past their due time.",
            rows.into_iter().map(|(worker, _, due, _)| (worker, due)),
        );
        worker_family(
            &mut output,
            "oldest_due_seconds",
            "gauge",
            "Seconds since the oldest due job became due.",
            rows.into_iter().map(|(worker, _, _, lag)| (worker, lag)),
        );
    }
    (
        [
            (
                header::CONTENT_TYPE,
                "text/plain; version=0.0.4; charset=utf-8",
            ),
            (header::CACHE_CONTROL, "no-store"),
        ],
        output,
    )
}
