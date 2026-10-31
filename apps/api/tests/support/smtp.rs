//! In-process TLS SMTP server for tests (STARTTLS and implicit TLS, injectable faults).
//! Shared by the SMTP and notification tests; no external relay is ever used.

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
use tokio::{
    io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader},
    net::{TcpListener, TcpStream},
};
use tokio_rustls::TlsAcceptor;

pub const SECRET: &str = "smtp-secret-value";
pub const USER: &str = "mailer";

#[derive(Clone, Copy)]
pub enum Listen {
    StartTls,
    Implicit,
}

#[derive(Clone, Copy)]
pub enum Fault {
    None,
    RejectAuth,
    RejectRecipient,
    /// First DATA reply is 450; the retry is accepted.
    TransientOnce,
    /// EHLO does not advertise STARTTLS.
    NoStartTls,
}

pub struct FakeSmtp {
    pub port: u16,
    pub ca_pem: String,
    pub messages: Arc<Mutex<Vec<String>>>,
    pub commands: Arc<Mutex<Vec<String>>>,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for FakeSmtp {
    fn drop(&mut self) {
        self.task.abort();
    }
}

pub struct Identity {
    pub ca_pem: String,
    pub acceptor: TlsAcceptor,
}

fn install_ring() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        let _ = rustls::crypto::ring::default_provider().install_default();
    });
}

pub fn identity() -> Identity {
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

pub async fn listen(mode: Listen, fault: Fault) -> FakeSmtp {
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

pub async fn wait_messages(messages: &Mutex<Vec<String>>, n: usize) {
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
pub fn decode_mail(body: &str) -> String {
    body.replace("=\r\n", "")
        .replace("=\n", "")
        .replace("=3D", "=")
}
