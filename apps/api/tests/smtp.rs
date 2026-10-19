//! Admin SMTP configuration, against an in-process TLS server. No external relay.

mod support;

use std::{
    net::SocketAddr,
    sync::{Arc, Mutex, Once},
    time::{Duration, Instant},
};

use base64::{Engine, engine::general_purpose::STANDARD};
use rcgen::{
    BasicConstraints, CertificateParams, DistinguishedName, DnType, IsCa, KeyPair, KeyUsagePurpose,
};
use rustls::{
    ServerConfig,
    pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer},
};
use serde_json::{Value, json};
use support::{TestDb, call, fixtures, login, test_app_with};
use tikkit_api::{
    auth::Settings,
    mail::{self, DeliverOptions, Delivered, MailError, OutboundMessage},
};
use tokio::{
    io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader},
    net::{TcpListener, TcpStream},
};
use tokio_rustls::TlsAcceptor;

const SECRET: &str = "smtp-secret-value";
const USER: &str = "mailer";

#[derive(Clone, Copy)]
enum Listen {
    StartTls,
    Implicit,
}

#[derive(Clone, Copy)]
enum Fault {
    None,
    RejectAuth,
    RejectRecipient,
    /// First DATA reply is 450; the retry is accepted.
    TransientOnce,
    /// EHLO does not advertise STARTTLS.
    NoStartTls,
}

struct FakeSmtp {
    port: u16,
    ca_pem: String,
    messages: Arc<Mutex<Vec<String>>>,
    commands: Arc<Mutex<Vec<String>>>,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for FakeSmtp {
    fn drop(&mut self) {
        self.task.abort();
    }
}

struct Identity {
    ca_pem: String,
    acceptor: TlsAcceptor,
}

fn install_ring() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        let _ = rustls::crypto::ring::default_provider().install_default();
    });
}

fn identity() -> Identity {
    install_ring();
    let mut ca_params = CertificateParams::new(Vec::<String>::new()).unwrap();
    ca_params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    ca_params.key_usages = vec![
        KeyUsagePurpose::KeyCertSign,
        KeyUsagePurpose::DigitalSignature,
    ];
    let mut name = DistinguishedName::new();
    name.push(DnType::CommonName, "Tikkit Test CA");
    ca_params.distinguished_name = name;
    let ca_key = KeyPair::generate().unwrap();
    let ca = ca_params.self_signed(&ca_key).unwrap();

    let mut params = CertificateParams::new(vec!["127.0.0.1".into()]).unwrap();
    let mut name = DistinguishedName::new();
    name.push(DnType::CommonName, "127.0.0.1");
    params.distinguished_name = name;
    let key = KeyPair::generate().unwrap();
    let cert = params.signed_by(&key, &ca, &ca_key).unwrap();

    let server = ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(
            vec![CertificateDer::from(cert.der().to_vec())],
            PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(key.serialize_der())),
        )
        .unwrap();
    Identity {
        ca_pem: ca.pem(),
        acceptor: TlsAcceptor::from(Arc::new(server)),
    }
}

async fn listen(mode: Listen, fault: Fault) -> FakeSmtp {
    let identity = identity();
    let listener = TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], 0)))
        .await
        .unwrap();
    let port = listener.local_addr().unwrap().port();
    let messages = Arc::new(Mutex::new(Vec::new()));
    let commands = Arc::new(Mutex::new(Vec::new()));
    let attempts = Arc::new(std::sync::atomic::AtomicU32::new(0));
    let ca_pem = identity.ca_pem.clone();
    let task = tokio::spawn({
        let messages = messages.clone();
        let commands = commands.clone();
        async move {
            loop {
                let Ok((sock, _)) = listener.accept().await else {
                    break;
                };
                let acceptor = identity.acceptor.clone();
                let messages = messages.clone();
                let commands = commands.clone();
                let attempts = attempts.clone();
                tokio::spawn(async move {
                    let _ =
                        session(sock, acceptor, mode, fault, messages, commands, attempts).await;
                });
            }
        }
    });
    FakeSmtp {
        port,
        ca_pem,
        messages,
        commands,
        task,
    }
}

async fn session(
    sock: TcpStream,
    acceptor: TlsAcceptor,
    mode: Listen,
    fault: Fault,
    messages: Arc<Mutex<Vec<String>>>,
    commands: Arc<Mutex<Vec<String>>>,
    attempts: Arc<std::sync::atomic::AtomicU32>,
) -> std::io::Result<()> {
    match mode {
        Listen::Implicit => {
            let tls = acceptor.accept(sock).await?;
            dialogue(tls, true, true, fault, messages, commands, attempts).await
        }
        Listen::StartTls => {
            let mut io = BufReader::new(sock);
            write_line(&mut io, "220 localhost ESMTP").await?;
            loop {
                let line = read_line(&mut io).await?;
                let verb = verb_of(&line);
                commands.lock().unwrap().push(verb.clone());
                if verb == "EHLO" || verb == "HELO" {
                    if matches!(fault, Fault::NoStartTls) {
                        write_line(&mut io, "250 localhost").await?;
                    } else {
                        write_line(&mut io, "250-localhost").await?;
                        write_line(&mut io, "250 STARTTLS").await?;
                    }
                } else if verb == "STARTTLS" {
                    write_line(&mut io, "220 Ready").await?;
                    let sock = io.into_inner();
                    let tls = acceptor.accept(sock).await?;
                    return dialogue(tls, false, true, fault, messages, commands, attempts).await;
                } else if verb == "QUIT" {
                    write_line(&mut io, "221 Bye").await?;
                    return Ok(());
                } else {
                    write_line(&mut io, "502 Command not implemented").await?;
                }
            }
        }
    }
}

async fn dialogue<S>(
    io: S,
    greet: bool,
    encrypted: bool,
    fault: Fault,
    messages: Arc<Mutex<Vec<String>>>,
    commands: Arc<Mutex<Vec<String>>>,
    attempts: Arc<std::sync::atomic::AtomicU32>,
) -> std::io::Result<()>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let mut io = BufReader::new(io);
    if greet {
        write_line(&mut io, "220 localhost ESMTP").await?;
    }
    loop {
        let line = read_line(&mut io).await?;
        let verb = verb_of(&line);
        commands.lock().unwrap().push(if verb == "AUTH" {
            "AUTH".into()
        } else {
            verb.clone()
        });
        if verb == "EHLO" || verb == "HELO" {
            if encrypted {
                write_line(&mut io, "250-localhost").await?;
                write_line(&mut io, "250 AUTH PLAIN").await?;
            } else {
                write_line(&mut io, "250 localhost").await?;
            }
        } else if verb == "AUTH" {
            let ok = plain_ok(&line) && !matches!(fault, Fault::RejectAuth);
            if ok {
                write_line(&mut io, "235 Authentication successful").await?;
            } else {
                write_line(&mut io, "535 Authentication failed").await?;
                return Ok(());
            }
        } else if verb == "MAIL" {
            write_line(&mut io, "250 OK").await?;
        } else if verb == "RCPT" {
            if matches!(fault, Fault::RejectRecipient) {
                write_line(&mut io, "550 Recipient rejected").await?;
            } else {
                write_line(&mut io, "250 OK").await?;
            }
        } else if verb == "DATA" {
            write_line(&mut io, "354 End data with <CR><LF>.<CR><LF>").await?;
            let body = read_data(&mut io).await?;
            let n = attempts.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            if matches!(fault, Fault::TransientOnce) && n == 0 {
                write_line(&mut io, "450 Mailbox busy").await?;
            } else {
                messages.lock().unwrap().push(body);
                write_line(&mut io, "250 Queued").await?;
            }
        } else if verb == "RSET" || verb == "NOOP" {
            write_line(&mut io, "250 OK").await?;
        } else if verb == "QUIT" {
            write_line(&mut io, "221 Bye").await?;
            return Ok(());
        } else {
            write_line(&mut io, "502 Command not implemented").await?;
        }
    }
}

fn verb_of(line: &str) -> String {
    line.split_whitespace()
        .next()
        .unwrap_or("")
        .to_ascii_uppercase()
}

fn plain_ok(line: &str) -> bool {
    let Some(encoded) = line.split_whitespace().nth(2) else {
        return false;
    };
    let Ok(bytes) = STANDARD.decode(encoded) else {
        return false;
    };
    let mut parts = bytes.split(|b| *b == 0);
    let _authz = parts.next();
    let user = parts.next().unwrap_or_default();
    let pass = parts.next().unwrap_or_default();
    user == USER.as_bytes() && pass == SECRET.as_bytes()
}

async fn read_line(io: &mut (impl AsyncBufReadExt + Unpin)) -> std::io::Result<String> {
    let mut line = String::new();
    if io.read_line(&mut line).await? == 0 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::UnexpectedEof,
            "eof",
        ));
    }
    Ok(line.trim_end_matches(['\r', '\n']).to_owned())
}

async fn read_data(io: &mut (impl AsyncBufReadExt + Unpin)) -> std::io::Result<String> {
    let mut body = String::new();
    loop {
        let mut line = String::new();
        if io.read_line(&mut line).await? == 0 {
            break;
        }
        if line == ".\r\n" || line == ".\n" {
            break;
        }
        body.push_str(&line);
    }
    Ok(body)
}

async fn write_line(io: &mut (impl AsyncWriteExt + Unpin), line: &str) -> std::io::Result<()> {
    io.write_all(format!("{line}\r\n").as_bytes()).await?;
    io.flush().await
}

fn router(pool: &sqlx::PgPool, trust: Option<&str>) -> axum::Router {
    let settings = Settings {
        smtp_trust_root_pem: trust.map(|pem| pem.as_bytes().to_vec()),
        ..Settings::default()
    };
    test_app_with(pool, settings)
}

fn write_body(port: u16, security: &str, password: Option<&str>, enabled: bool) -> Value {
    let mut body = json!({
        "host": "127.0.0.1",
        "port": port,
        "security": security,
        "sender_name": "Tikkit Help",
        "sender_address": "help@example.com",
        "username": USER,
        "enabled": enabled,
    });
    if let Some(password) = password {
        body["password"] = json!(password);
    }
    body
}

async fn save(app: &axum::Router, cookie: &str, body: Value) -> support::Reply {
    call(app, "PUT", "/api/settings/smtp", Some(body), Some(cookie)).await
}

async fn wait_messages(messages: &Mutex<Vec<String>>, n: usize) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        if messages.lock().unwrap().len() >= n {
            return;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!(
        "timed out waiting for {n} messages, have {}",
        messages.lock().unwrap().len()
    );
}

/// Undoes quoted-printable soft breaks so assertions can see the original link.
fn decode_mail(body: &str) -> String {
    body.replace("=\r\n", "")
        .replace("=\n", "")
        .replace("=3D", "=")
}

fn assert_secret_absent(value: &impl std::fmt::Display) {
    let text = value.to_string();
    assert!(!text.contains(SECRET), "{text}");
}

async fn ciphertext(pool: &sqlx::PgPool) -> Vec<u8> {
    sqlx::query_scalar("SELECT password_ciphertext FROM smtp_connection")
        .fetch_one(pool)
        .await
        .unwrap()
}

#[tokio::test]
async fn outsiders_cannot_touch_smtp_and_the_portal_works_without_it() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    fixtures::bootstrap_admin(&db.pool).await;
    for (role, name) in [
        (fixtures::Role::User, "ada"),
        (fixtures::Role::Operator, "op"),
        (fixtures::Role::Specialist, "spec"),
        (fixtures::Role::TeamLead, "lead"),
    ] {
        fixtures::account(&db.pool, role, name).await;
    }
    let app = router(&db.pool, None);
    let anon = call(&app, "GET", "/api/settings/smtp", None, None).await;
    assert_eq!(anon.code(), "unauthorized");

    let body = write_body(587, "starttls", Some(SECRET), true);
    for name in ["ada", "op", "spec", "lead"] {
        let cookie = login(&app, name, fixtures::PASSWORD).await.session();
        for (method, path) in [
            ("GET", "/api/settings/smtp"),
            ("PUT", "/api/settings/smtp"),
            ("DELETE", "/api/settings/smtp"),
            ("POST", "/api/settings/smtp/test"),
        ] {
            let payload = (method == "PUT" || method == "POST").then(|| {
                if method == "POST" {
                    json!({ "to": "shared@example.com" })
                } else {
                    body.clone()
                }
            });
            let reply = call(&app, method, path, payload, Some(&cookie)).await;
            assert_eq!(reply.status, 403, "{name} {method} {path} {}", reply.body);
            assert_eq!(reply.code(), "forbidden");
            assert_secret_absent(&reply.body);
        }
    }

    let admin = login(&app, "admin", fixtures::PASSWORD).await.session();
    let empty = call(&app, "GET", "/api/settings/smtp", None, Some(&admin)).await;
    assert_eq!(empty.status, 200, "{}", empty.body);
    assert!(empty.body["connection"].is_null());
    let invited = call(
        &app,
        "POST",
        "/api/invitations",
        Some(json!({ "email": "person@example.com", "role": "user" })),
        Some(&admin),
    )
    .await;
    assert_eq!(invited.status, 201, "{}", invited.body);
    assert_eq!(invited.body["emailed"], false);
    assert!(
        invited.body["url"]
            .as_str()
            .unwrap()
            .contains("http://localhost:3000/invite/")
    );
}

#[tokio::test]
async fn admin_stores_the_password_encrypted_and_never_returns_it() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    fixtures::bootstrap_admin(&db.pool).await;
    let server = listen(Listen::StartTls, Fault::None).await;
    let app = router(&db.pool, Some(&server.ca_pem));
    let admin = login(&app, "admin", fixtures::PASSWORD).await.session();

    let missing = save(
        &app,
        &admin,
        write_body(server.port, "starttls", None, true),
    )
    .await;
    assert_eq!(missing.status, 422, "{}", missing.body);
    assert_eq!(
        missing.body["error"]["details"]["fields"]["password"][0],
        "required"
    );

    let bad = save(
        &app,
        &admin,
        json!({
            "host": "not a host",
            "port": 0,
            "security": "none",
            "sender_name": "Bad\nName",
            "sender_address": "nope",
            "username": "mailer",
            "password": SECRET,
            "enabled": true,
        }),
    )
    .await;
    assert_eq!(bad.status, 422, "{}", bad.body);
    assert_secret_absent(&bad.body);

    let saved = save(
        &app,
        &admin,
        write_body(server.port, "starttls", Some(SECRET), true),
    )
    .await;
    assert_eq!(saved.status, 200, "{}", saved.body);
    assert_eq!(saved.body["connection"]["password_set"], true);
    assert!(saved.body["connection"].get("password").is_none());
    assert_secret_absent(&saved.body);
    let stored = ciphertext(&db.pool).await;
    assert!(
        !stored.windows(SECRET.len()).any(|w| w == SECRET.as_bytes()),
        "password stored in plaintext"
    );

    let got = call(&app, "GET", "/api/settings/smtp", None, Some(&admin)).await;
    assert_eq!(got.body["connection"]["host"], "127.0.0.1");
    assert_eq!(got.body["connection"]["security"], "starttls");
    assert_secret_absent(&got.body);

    let meta: Vec<Value> = sqlx::query_scalar(
        "SELECT metadata FROM audit_events WHERE event_type = 'smtp_configured'",
    )
    .fetch_all(&db.pool)
    .await
    .unwrap();
    assert_eq!(meta.len(), 1);
    assert_eq!(meta[0]["password_changed"], true);
    assert!(meta[0].get("password").is_none());
    assert!(meta[0].get("username").is_none());
    assert_secret_absent(&meta[0]);
}

#[tokio::test]
async fn starttls_delivers_test_invitation_and_recovery_mail() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    fixtures::bootstrap_admin(&db.pool).await;
    let server = listen(Listen::StartTls, Fault::None).await;
    let app = router(&db.pool, Some(&server.ca_pem));
    let admin = login(&app, "admin", fixtures::PASSWORD).await.session();
    let saved = save(
        &app,
        &admin,
        write_body(server.port, "starttls", Some(SECRET), true),
    )
    .await;
    assert_eq!(saved.status, 200, "{}", saved.body);

    let test = call(
        &app,
        "POST",
        "/api/settings/smtp/test",
        Some(json!({ "to": "shared@example.com" })),
        Some(&admin),
    )
    .await;
    assert_eq!(test.status, 200, "{}", test.body);
    assert_eq!(test.body["delivered"], true);
    assert_secret_absent(&test.body);
    wait_messages(&server.messages, 1).await;
    let body = server.messages.lock().unwrap()[0].clone();
    assert!(body.contains("http://localhost:3000"), "{body}");
    assert!(body.contains("Do not reply to this email"), "{body}");
    assert!(!body.contains(SECRET), "{body}");

    let sender = call(
        &app,
        "POST",
        "/api/settings/smtp/test",
        Some(json!({ "to": "help@example.com" })),
        Some(&admin),
    )
    .await;
    assert_eq!(sender.status, 200, "{}", sender.body);
    wait_messages(&server.messages, 2).await;

    let elsewhere = call(
        &app,
        "POST",
        "/api/settings/smtp/test",
        Some(json!({ "to": "other@example.com" })),
        Some(&admin),
    )
    .await;
    assert_eq!(elsewhere.status, 422, "{}", elsewhere.body);
    assert_eq!(
        elsewhere.body["error"]["details"]["fields"]["to"][0],
        "not_allowed"
    );
    assert_eq!(server.messages.lock().unwrap().len(), 2);

    let invited = call(
        &app,
        "POST",
        "/api/invitations",
        Some(json!({ "email": "person@example.com", "language": "en" })),
        Some(&admin),
    )
    .await;
    assert_eq!(invited.status, 201, "{}", invited.body);
    assert_eq!(invited.body["emailed"], true);
    let url = invited.body["url"].as_str().unwrap().to_owned();
    wait_messages(&server.messages, 3).await;
    let mail = decode_mail(&server.messages.lock().unwrap()[2]);
    assert!(mail.contains(&url), "{mail}");
    assert!(
        mail.contains("Create and reply to tickets in the portal"),
        "{mail}"
    );
    assert!(!mail.contains(SECRET));

    fixtures::account(&db.pool, fixtures::Role::User, "recover-me").await;
    let recovery = call(
        &app,
        "POST",
        "/api/auth/recovery",
        Some(json!({ "username": "recover-me" })),
        None,
    )
    .await;
    assert_eq!(recovery.status, 202, "{}", recovery.body);
    wait_messages(&server.messages, 4).await;
    let mail = decode_mail(&server.messages.lock().unwrap()[3]);
    assert!(mail.contains("/reset-password/"), "{mail}");
    assert!(mail.contains("Do not reply to this email"), "{mail}");
    assert!(
        !server
            .commands
            .lock()
            .unwrap()
            .iter()
            .any(|c| c.contains(SECRET))
    );

    let tested: Vec<Value> = sqlx::query_scalar(
        "SELECT metadata FROM audit_events WHERE event_type = 'smtp_tested' ORDER BY id",
    )
    .fetch_all(&db.pool)
    .await
    .unwrap();
    assert!(tested.iter().any(|m| m["result"] == "ok"));
    assert!(tested.iter().all(|m| !m.to_string().contains(SECRET)));
    assert!(tested.iter().all(|m| m.get("to").is_none()));
}

#[tokio::test]
async fn implicit_tls_and_german_test_mail_work() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    fixtures::bootstrap_admin(&db.pool).await;
    sqlx::query("UPDATE accounts SET language = 'de' WHERE username = 'admin'")
        .execute(&db.pool)
        .await
        .unwrap();
    let server = listen(Listen::Implicit, Fault::None).await;
    let app = router(&db.pool, Some(&server.ca_pem));
    let admin = login(&app, "admin", fixtures::PASSWORD).await.session();
    let saved = save(
        &app,
        &admin,
        write_body(server.port, "implicit_tls", Some(SECRET), true),
    )
    .await;
    assert_eq!(saved.status, 200, "{}", saved.body);
    let test = call(
        &app,
        "POST",
        "/api/settings/smtp/test",
        Some(json!({ "to": "shared@example.com" })),
        Some(&admin),
    )
    .await;
    assert_eq!(test.status, 200, "{}", test.body);
    wait_messages(&server.messages, 1).await;
    let body = &server.messages.lock().unwrap()[0];
    assert!(
        body.contains("Antworten Sie nicht auf diese E-Mail"),
        "{body}"
    );
    assert!(body.contains("http://localhost:3000"), "{body}");
}

#[tokio::test]
async fn auth_tls_and_delivery_failures_are_distinct_and_safe() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    fixtures::bootstrap_admin(&db.pool).await;
    let admin_app = router(&db.pool, None);
    let admin = login(&admin_app, "admin", fixtures::PASSWORD)
        .await
        .session();

    let auth_server = listen(Listen::StartTls, Fault::RejectAuth).await;
    let app = router(&db.pool, Some(&auth_server.ca_pem));
    save(
        &app,
        &admin,
        write_body(auth_server.port, "starttls", Some(SECRET), true),
    )
    .await;
    let failed = call(
        &app,
        "POST",
        "/api/settings/smtp/test",
        Some(json!({ "to": "shared@example.com" })),
        Some(&admin),
    )
    .await;
    assert_eq!(failed.status, 502, "{}", failed.body);
    assert_eq!(failed.code(), "smtp_auth_failed");
    assert_secret_absent(&failed.body);
    assert!(auth_server.messages.lock().unwrap().is_empty());

    let plain = listen(Listen::StartTls, Fault::NoStartTls).await;
    let app = router(&db.pool, Some(&plain.ca_pem));
    save(
        &app,
        &admin,
        write_body(plain.port, "starttls", Some(SECRET), true),
    )
    .await;
    let tls = call(
        &app,
        "POST",
        "/api/settings/smtp/test",
        Some(json!({ "to": "shared@example.com" })),
        Some(&admin),
    )
    .await;
    assert_eq!(tls.code(), "smtp_tls_failed", "{}", tls.body);
    assert_secret_absent(&tls.body);
    assert!(!plain.commands.lock().unwrap().iter().any(|c| c == "AUTH"));

    let untrusted = listen(Listen::StartTls, Fault::None).await;
    let app = router(&db.pool, None);
    save(
        &app,
        &admin,
        write_body(untrusted.port, "starttls", Some(SECRET), true),
    )
    .await;
    let tls = call(
        &app,
        "POST",
        "/api/settings/smtp/test",
        Some(json!({ "to": "shared@example.com" })),
        Some(&admin),
    )
    .await;
    assert_eq!(tls.code(), "smtp_tls_failed", "{}", tls.body);
    assert!(untrusted.messages.lock().unwrap().is_empty());

    let other = identity();
    let trusted_wrong = listen(Listen::StartTls, Fault::None).await;
    let app = router(&db.pool, Some(&other.ca_pem));
    save(
        &app,
        &admin,
        write_body(trusted_wrong.port, "starttls", Some(SECRET), true),
    )
    .await;
    let tls = call(
        &app,
        "POST",
        "/api/settings/smtp/test",
        Some(json!({ "to": "shared@example.com" })),
        Some(&admin),
    )
    .await;
    assert_eq!(tls.code(), "smtp_tls_failed", "{}", tls.body);

    let reject = listen(Listen::StartTls, Fault::RejectRecipient).await;
    let app = router(&db.pool, Some(&reject.ca_pem));
    save(
        &app,
        &admin,
        write_body(reject.port, "starttls", Some(SECRET), true),
    )
    .await;
    let delivery = call(
        &app,
        "POST",
        "/api/settings/smtp/test",
        Some(json!({ "to": "shared@example.com" })),
        Some(&admin),
    )
    .await;
    assert_eq!(delivery.code(), "smtp_delivery_failed", "{}", delivery.body);
    let mails = reject
        .commands
        .lock()
        .unwrap()
        .iter()
        .filter(|c| *c == "MAIL")
        .count();
    assert_eq!(mails, 1, "permanent refusal must not be retried");

    let closed = listen(Listen::StartTls, Fault::None).await;
    let port = closed.port;
    drop(closed);
    let app = router(&db.pool, Some(&other.ca_pem));
    save(
        &app,
        &admin,
        write_body(port, "starttls", Some(SECRET), true),
    )
    .await;
    let started = Instant::now();
    let network = call(
        &app,
        "POST",
        "/api/settings/smtp/test",
        Some(json!({ "to": "shared@example.com" })),
        Some(&admin),
    )
    .await;
    assert_eq!(network.code(), "smtp_network_failed", "{}", network.body);
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "retries must stay bounded"
    );
    assert_secret_absent(&network.body);
    let status = call(&app, "GET", "/api/settings/smtp", None, Some(&admin)).await;
    assert_eq!(
        status.body["connection"]["last_test_error"],
        "smtp_network_failed"
    );
    assert_secret_absent(&status.body);

    let invited = call(
        &app,
        "POST",
        "/api/invitations",
        Some(json!({ "email": "person@example.com" })),
        Some(&admin),
    )
    .await;
    assert_eq!(invited.status, 201, "{}", invited.body);
    assert_eq!(invited.body["emailed"], true);
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let status = call(&app, "GET", "/api/settings/smtp", None, Some(&admin)).await;
        if status.body["connection"]["last_delivery_error"] == "smtp_network_failed" {
            assert_secret_absent(&status.body);
            break;
        }
        assert!(Instant::now() < deadline, "{}", status.body);
        tokio::time::sleep(Duration::from_millis(30)).await;
    }
}

#[tokio::test]
async fn transient_failures_are_retried_and_duplicate_keys_are_not() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    fixtures::bootstrap_admin(&db.pool).await;
    let server = listen(Listen::StartTls, Fault::TransientOnce).await;
    let app = router(&db.pool, Some(&server.ca_pem));
    let admin = login(&app, "admin", fixtures::PASSWORD).await.session();
    save(
        &app,
        &admin,
        write_body(server.port, "starttls", Some(SECRET), true),
    )
    .await;
    let test = call(
        &app,
        "POST",
        "/api/settings/smtp/test",
        Some(json!({ "to": "shared@example.com" })),
        Some(&admin),
    )
    .await;
    assert_eq!(test.status, 200, "{}", test.body);
    assert_eq!(server.messages.lock().unwrap().len(), 1);
    let mails = server
        .commands
        .lock()
        .unwrap()
        .iter()
        .filter(|c| *c == "MAIL")
        .count();
    assert_eq!(mails, 2, "the 450 must be tried again");

    let key = [0u8; 32];
    let message = OutboundMessage {
        to: "shared@example.com".into(),
        subject: "Again".into(),
        body: "portal only".into(),
        idempotency_key: Some("invitation:once".into()),
    };
    let opts = DeliverOptions {
        allow_disabled: true,
        ..DeliverOptions::default()
    };
    let first = mail::deliver(
        &db.pool,
        &key,
        Some(server.ca_pem.as_bytes()),
        &message,
        opts,
    )
    .await
    .unwrap();
    assert_eq!(first, Delivered::Sent);
    let second = mail::deliver(
        &db.pool,
        &key,
        Some(server.ca_pem.as_bytes()),
        &message,
        opts,
    )
    .await
    .unwrap();
    assert_eq!(second, Delivered::Duplicate);
    assert_eq!(server.messages.lock().unwrap().len(), 2);
}

#[tokio::test]
async fn disabling_or_removing_smtp_keeps_the_portal_usable() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    fixtures::bootstrap_admin(&db.pool).await;
    let server = listen(Listen::StartTls, Fault::None).await;
    let app = router(&db.pool, Some(&server.ca_pem));
    let admin = login(&app, "admin", fixtures::PASSWORD).await.session();
    save(
        &app,
        &admin,
        write_body(server.port, "starttls", Some(SECRET), false),
    )
    .await;
    let invited = call(
        &app,
        "POST",
        "/api/invitations",
        Some(json!({ "email": "person@example.com" })),
        Some(&admin),
    )
    .await;
    assert_eq!(invited.body["emailed"], false, "{}", invited.body);
    assert!(
        invited.body["url"]
            .as_str()
            .unwrap()
            .starts_with("http://localhost:3000/invite/")
    );
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(server.messages.lock().unwrap().is_empty());

    let test = call(
        &app,
        "POST",
        "/api/settings/smtp/test",
        Some(json!({ "to": "shared@example.com" })),
        Some(&admin),
    )
    .await;
    assert_eq!(test.status, 200, "{}", test.body);
    wait_messages(&server.messages, 1).await;

    let kept = save(
        &app,
        &admin,
        write_body(server.port, "starttls", None, true),
    )
    .await;
    assert_eq!(kept.status, 200, "{}", kept.body);
    assert_eq!(kept.body["connection"]["enabled"], true);
    let test = call(
        &app,
        "POST",
        "/api/settings/smtp/test",
        Some(json!({ "to": "help@example.com" })),
        Some(&admin),
    )
    .await;
    assert_eq!(test.status, 200, "{}", test.body);
    wait_messages(&server.messages, 2).await;

    let removed = call(&app, "DELETE", "/api/settings/smtp", None, Some(&admin)).await;
    assert_eq!(removed.status, 204, "{}", removed.body);
    let gone = call(&app, "GET", "/api/settings/smtp", None, Some(&admin)).await;
    assert!(gone.body["connection"].is_null());
    let invited = call(
        &app,
        "POST",
        "/api/invitations",
        Some(json!({ "email": "person@example.com" })),
        Some(&admin),
    )
    .await;
    assert_eq!(invited.status, 201, "{}", invited.body);
    assert_eq!(invited.body["emailed"], false);
    let missing = call(
        &app,
        "POST",
        "/api/settings/smtp/test",
        Some(json!({ "to": "shared@example.com" })),
        Some(&admin),
    )
    .await;
    assert_eq!(missing.code(), "smtp_not_configured");
}

#[tokio::test]
async fn a_changed_connection_is_what_the_next_delivery_uses() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    fixtures::bootstrap_admin(&db.pool).await;
    let first = listen(Listen::StartTls, Fault::None).await;
    let second = listen(Listen::StartTls, Fault::None).await;
    let app_first = router(&db.pool, Some(&first.ca_pem));
    let admin = login(&app_first, "admin", fixtures::PASSWORD)
        .await
        .session();
    save(
        &app_first,
        &admin,
        write_body(first.port, "starttls", Some(SECRET), true),
    )
    .await;
    call(
        &app_first,
        "POST",
        "/api/invitations",
        Some(json!({ "email": "a@example.com" })),
        Some(&admin),
    )
    .await;
    wait_messages(&first.messages, 1).await;

    let mut body = write_body(second.port, "starttls", Some(SECRET), true);
    body["sender_name"] = json!("Updated Desk");
    let app_second = router(&db.pool, Some(&second.ca_pem));
    let updated = save(&app_second, &admin, body).await;
    assert_eq!(updated.status, 200, "{}", updated.body);
    assert_eq!(updated.body["connection"]["sender_name"], "Updated Desk");
    call(
        &app_second,
        "POST",
        "/api/invitations",
        Some(json!({ "email": "b@example.com" })),
        Some(&admin),
    )
    .await;
    wait_messages(&second.messages, 1).await;
    assert_eq!(first.messages.lock().unwrap().len(), 1);
    assert!(second.messages.lock().unwrap()[0].contains("Updated Desk"));

    let events: Vec<String> = sqlx::query_scalar(
        "SELECT event_type FROM audit_events WHERE event_type LIKE 'smtp_%' ORDER BY id",
    )
    .fetch_all(&db.pool)
    .await
    .unwrap();
    assert_eq!(events, ["smtp_configured", "smtp_updated"]);
}

#[tokio::test]
async fn dns_failure_is_a_bounded_network_error() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    fixtures::bootstrap_admin(&db.pool).await;
    let app = router(&db.pool, None);
    let admin = login(&app, "admin", fixtures::PASSWORD).await.session();
    save(
        &app,
        &admin,
        json!({
            "host": "no-such-host.invalid",
            "port": 587,
            "security": "starttls",
            "sender_name": "Tikkit",
            "sender_address": "help@example.com",
            "username": USER,
            "password": SECRET,
            "enabled": true,
        }),
    )
    .await;
    let started = Instant::now();
    let error = mail::deliver(
        &db.pool,
        &[0u8; 32],
        None,
        &OutboundMessage {
            to: "shared@example.com".into(),
            subject: "dns".into(),
            body: "portal".into(),
            idempotency_key: None,
        },
        DeliverOptions {
            timeout: Duration::from_secs(2),
            max_attempts: 1,
            allow_disabled: true,
        },
    )
    .await;
    assert_eq!(error, Err(MailError::Network));
    assert!(started.elapsed() < Duration::from_secs(5));
}
