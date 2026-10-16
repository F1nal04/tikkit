//! Optional outbound email for invitation and recovery links. Without `SMTP_URL`
//! nothing is sent: Admins copy invitation links, and recovery stays silent.

use std::fmt;

use lettre::{
    AsyncSmtpTransport, AsyncTransport, Message, Tokio1Executor, message::Mailbox,
    message::header::ContentType,
};

#[derive(Clone, Default)]
pub enum Mailer {
    #[default]
    None,
    Smtp {
        transport: Box<AsyncSmtpTransport<Tokio1Executor>>,
        from: Mailbox,
    },
}

/// Never prints the URL: it may carry SMTP credentials.
impl fmt::Debug for Mailer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::None => "Mailer::None",
            Self::Smtp { .. } => "Mailer::Smtp(<redacted>)",
        })
    }
}

impl Mailer {
    /// `url` is lettre's SMTP URL (`smtps://user:pass@host`, `smtp://host:587?tls=required`).
    /// Errors are fixed strings that never echo the values.
    pub fn smtp(url: &str, from: &str) -> Result<Self, &'static str> {
        let transport = AsyncSmtpTransport::<Tokio1Executor>::from_url(url)
            .map_err(|_| "SMTP_URL must be an smtp:// or smtps:// URL")?
            .build()
            .into();
        let from = from
            .parse()
            .map_err(|_| "SMTP_FROM must be an address such as Tikkit <help@example.com>")?;
        Ok(Self::Smtp { transport, from })
    }

    pub fn is_configured(&self) -> bool {
        matches!(self, Self::Smtp { .. })
    }

    /// Queues a plain-text message in the background and returns whether it was
    /// attempted. Failures are logged without addresses, subject, or body, which
    /// may contain a token.
    // ponytail: fire-and-forget, no retry; the transactional outbox (notifications, milestone 2) replaces this.
    pub fn send(&self, to: &str, subject: &str, body: String) -> bool {
        let Self::Smtp { transport, from } = self else {
            return false;
        };
        let Ok(to) = to.parse::<Mailbox>() else {
            tracing::warn!("not sending email: the stored address is not deliverable");
            return false;
        };
        let message = Message::builder()
            .from(from.clone())
            .to(to)
            .subject(subject)
            .header(ContentType::TEXT_PLAIN)
            .body(body);
        let Ok(message) = message else {
            tracing::warn!("not sending email: the message could not be built");
            return false;
        };
        let transport = transport.as_ref().clone();
        tokio::spawn(async move {
            if let Err(error) = transport.send(message).await {
                tracing::warn!(
                    permanent = error.is_permanent(),
                    transient = error.is_transient(),
                    "email delivery failed"
                );
            }
        });
        true
    }
}
