//! Real HTTP entry point for the browser suite. This target is only built for tests.
#[path = "../support/mod.rs"]
mod support;

use std::{future::IntoFuture, io::Read, net::SocketAddr, time::Duration};

use serde_json::json;
use support::{TestDb, provider_mock, smtp, test_state};
use tikkit_api::{ai::Endpoint, ai_attempts, auth::Settings, autoclose, notifications};

#[tokio::main]
async fn main() {
    if !std::env::args().any(|arg| arg == "--serve-workflows") {
        return;
    }

    #[cfg(unix)]
    let (mut term, mut interrupt, mut hangup) = {
        use tokio::signal::unix::{SignalKind, signal};
        (
            signal(SignalKind::terminate()).unwrap(),
            signal(SignalKind::interrupt()).unwrap(),
            signal(SignalKind::hangup()).unwrap(),
        )
    };
    let (eof, stdin_closed) = tokio::sync::oneshot::channel();
    std::thread::spawn(move || {
        let _ = std::io::stdin().read(&mut [0]);
        let _ = eof.send(());
    });
    let db = TestDb::new().await.expect("set TEST_DATABASE_URL");
    let reply = |explanation, steps| {
        let answer = json!({ "explanation": explanation, "steps": steps });
        provider_mock::Reply::json(
            200,
            json!({
                "status": "completed",
                "output": [{ "type": "message", "role": "assistant", "status": "completed",
                    "content": [{ "type": "output_text", "text": answer.to_string() }]
                }],
            }),
        )
    };
    let answer = if std::env::var("WORKFLOW_SETUP_LANGUAGE").as_deref() == Ok("de") {
        reply(
            "Das Anwendungsprofil ist möglicherweise beschädigt.",
            ["Anwendung schließen.", "Anwendung neu öffnen."],
        )
    } else {
        reply(
            "The application profile may be damaged.",
            ["Close the application.", "Open it again."],
        )
    };
    let provider = provider_mock::serve(vec![answer]).await;
    let mail = smtp::listen_on(18503, smtp::Listen::StartTls, smtp::Fault::None).await;
    let settings = Settings {
        public_origin: "http://127.0.0.1:18500".into(),
        smtp_trust_root_pem: Some(mail.ca_pem.as_bytes().to_vec()),
        ..Settings::default()
    };
    let mut state = test_state(&db.pool);
    state.ai_endpoint = Endpoint::Override {
        base_url: provider.url,
        allow_private_network: true,
    };
    std::fs::create_dir_all(&state.screenshot_dir).unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:18501")
        .await
        .unwrap();
    // Only the loopback test executable exposes a capture counter. The
    // gateway does not forward this path, and no mail content leaves the fixture.
    let messages = mail.messages.clone();
    let app = tikkit_api::app_with(state.clone(), settings.clone()).route(
        "/__workflow/mail-count",
        axum::routing::get(move || async move { axum::Json(messages.lock().unwrap().len()) }),
    );
    let ai = ai_attempts::Worker::new(&state, settings.encryption_key);
    let ai_task = tokio::spawn(ai_attempts::worker_loop(ai.clone()));
    let mail_task = tokio::spawn(notifications::worker_loop(state.clone(), settings));
    let close_task = tokio::spawn(autoclose::worker_loop(state));

    // EOF handles a lost launcher; signal handlers cover process-group shutdown.
    let (shutdown, mut stopped) = tokio::sync::watch::channel(false);
    let mut drain = stopped.clone();
    tokio::spawn(async move {
        #[cfg(unix)]
        tokio::select! {
            _ = stdin_closed => {}
            _ = term.recv() => {}
            _ = interrupt.recv() => {}
            _ = hangup.recv() => {}
        }
        #[cfg(not(unix))]
        let _ = stdin_closed.await;
        ai.stop();
        notifications::stop_workers();
        shutdown.send_replace(true);
    });
    let server = axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(async move {
        let _ = stopped.wait_for(|stop| *stop).await;
    })
    .into_future();
    tokio::select! {
        result = server => result.unwrap(),
        _ = async {
            let _ = drain.wait_for(|stop| *stop).await;
            tokio::time::sleep(Duration::from_secs(7)).await;
        } => {}
    }
    let _ = tokio::time::timeout(Duration::from_secs(3), async {
        let _ = tokio::join!(ai_task, mail_task, close_task);
    })
    .await;
    // TestDb drops the database and forcibly ends any remaining connections.
}
