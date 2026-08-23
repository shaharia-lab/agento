//! `internal/notification/smtp.go` in Rust: the one thing this shell does that
//! reaches a server we do not run.
//!
//! # `encryption` does not mean what it says
//!
//! `SMTPConfig.Encryption` reads `"none"`, `"starttls"`, `"ssl_tls"`, and the
//! obvious mapping — implicit TLS for the third — is **wrong**.
//! `tlsPolicyFromEncryption` hands go-mail a `TLSPolicy`, and every one of those
//! policies is about STARTTLS: `TLSMandatory` means "upgrade or fail",
//! `TLSOpportunistic` means "upgrade if offered", `NoTLS` means "do not".
//! go-mail's implicit-TLS switch is `WithSSL()`, which Agento never calls. So a
//! user who picked `ssl_tls` and port 465 is, today, doing mandatory STARTTLS on
//! 465 — and reproducing that is the parity bar, not improving it. Changing the
//! meaning here would move a working configuration to a port that never
//! answers, from a release note nobody reads.
//!
//! # Why a failure is a 500 rather than a worded 400
//!
//! The inherited behaviour answers a failed test send with `400` and the
//! underlying error text — `dial tcp …: connect: connection refused`, `failed
//! to create mail client: …`. Those strings come from the mail library and the
//! runtime it was written against; none is reproducible here, and inventing a
//! paraphrase would put a different sentence on the user's screen. So the
//! failure arm answers a 500 with the reason in the log, the precedent
//! `integration_credentials.rs` set for exactly this.
//!
//! **A retry must not re-send.** The rule is in [`send`] and in its one caller:
//! nothing that can fail may run after the server has accepted the message.
//! `SmtpTransport::send` returns `Ok` once the server has answered the final `.`
//! with a 2xx, so an `Err` from it means the message was *not* accepted — which
//! is what makes a user pressing the button again a second failed dial rather
//! than a second email.

use lettre::message::{header::ContentType, MultiPart, SinglePart};
use lettre::transport::smtp::authentication::{Credentials, Mechanism};
use lettre::transport::smtp::client::{Tls, TlsParameters};
use lettre::{Message, SmtpTransport, Transport};

use super::template::{build_email_html, SUBJECT_PREFIX};
use super::SmtpConfig;

/// `notification.Message`, minus the `To` field nothing sets.
pub struct Mail {
    pub subject: String,
    pub body: String,
}

/// The test send's contents. `TestNotification` builds these literally, and the
/// subject skips `buildSubject` — it concatenates the prefix itself.
pub fn test_mail() -> Mail {
    Mail {
        subject: format!("{SUBJECT_PREFIX}Test Notification"),
        body: "This is a test notification from Agento.\n\nYour SMTP configuration is working correctly."
            .to_string(),
    }
}

/// `SMTPProvider.Send`.
///
/// Every failure is a `String`, and every one of them becomes a 500 — see the
/// module header. The messages are for the log, not for the wire.
pub fn send(config: &SmtpConfig, mail: &Mail) -> Result<(), String> {
    let message = build_message(config, mail)?;
    let transport = build_transport(config)?;
    // Nothing fallible may follow this line. See the module header.
    transport
        .send(&message)
        .map(|_| ())
        .map_err(|e| format!("sending mail: {e}"))
}

/// The `mail.Msg` go-mail assembles: a plain-text body with the branded HTML as
/// an alternative, so a client that renders neither still shows the text.
///
/// Two details are Go's rather than lettre's defaults:
///
/// - **Recipients are a comma-separated string**, split, trimmed, and with
///   blanks *skipped* rather than rejected — so `"a@b.c, "` is one recipient,
///   not an error. A trailing comma in the settings field is the common case.
/// - **The HTML part is best-effort.** Go's `if html, err := …; err == nil`
///   silently omits the alternative when rendering fails and still sends the
///   plain text. Rendering cannot fail here (it is string substitution rather
///   than a template execution), so the plain-text-only path is unreachable —
///   which is why this builds the alternative unconditionally rather than
///   carrying a branch that could never be taken.
fn build_message(config: &SmtpConfig, mail: &Mail) -> Result<Message, String> {
    let from = config
        .from_address
        .parse()
        .map_err(|e| format!("invalid from address: {e}"))?;

    let mut builder = Message::builder().from(from).subject(&mail.subject);
    let mut recipients = 0;
    for raw in config.to_addresses.split(',') {
        let address = raw.trim();
        if address.is_empty() {
            continue;
        }
        builder = builder.to(address
            .parse()
            .map_err(|e| format!("invalid recipient {address:?}: {e}"))?);
        recipients += 1;
    }
    if recipients == 0 {
        // go-mail refuses to send with no recipients too, with its own wording.
        return Err("no recipients configured".to_string());
    }

    builder
        .multipart(
            MultiPart::alternative()
                .singlepart(
                    SinglePart::builder()
                        .header(ContentType::TEXT_PLAIN)
                        .body(mail.body.clone()),
                )
                .singlepart(
                    SinglePart::builder()
                        .header(ContentType::TEXT_HTML)
                        .body(build_email_html(&mail.subject, &mail.body)),
                ),
        )
        .map_err(|e| format!("building message: {e}"))
}

/// The `mail.Client` go-mail builds: the host and port from the settings, PLAIN
/// auth, and the STARTTLS policy the `encryption` field selects.
///
/// Auth is configured unconditionally, as `WithSMTPAuth(SMTPAuthPlain)` is —
/// including when the username is blank. A relay that wants no auth will
/// refuse, which is the inherited behaviour, and the refusal is a 500.
fn build_transport(config: &SmtpConfig) -> Result<SmtpTransport, String> {
    let tls = tls_policy(&config.encryption, &config.host)?;
    let port = u16::try_from(config.port).map_err(|_| format!("invalid port {}", config.port))?;

    Ok(SmtpTransport::builder_dangerous(&config.host)
        .port(port)
        .tls(tls)
        .authentication(vec![Mechanism::Plain])
        .credentials(Credentials::new(
            config.username.clone(),
            config.password.clone(),
        ))
        .build())
}

/// `tlsPolicyFromEncryption`, with go-mail's meanings rather than the obvious
/// ones — see the module header. Anything unrecognized is `NoTLS`, because Go's
/// `switch` has no case for it and falls to `default`.
fn tls_policy(encryption: &str, host: &str) -> Result<Tls, String> {
    let params = || {
        TlsParameters::new(host.to_string())
            .map_err(|e| format!("building TLS parameters for {host:?}: {e}"))
    };
    match encryption {
        // TLSMandatory: STARTTLS, and fail if the server will not.
        "ssl_tls" => Ok(Tls::Required(params()?)),
        // TLSOpportunistic: STARTTLS when the server offers it.
        "starttls" => Ok(Tls::Opportunistic(params()?)),
        _ => Ok(Tls::None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config() -> SmtpConfig {
        SmtpConfig {
            host: "smtp.example.com".into(),
            port: 587,
            username: "u".into(),
            password: "p".into(),
            from_address: "agento@example.com".into(),
            to_addresses: "one@example.com".into(),
            encryption: "starttls".into(),
        }
    }

    /// The mapping that looks wrong and is right. `ssl_tls` is *mandatory
    /// STARTTLS* on the Go side, because `WithTLSPolicy(TLSMandatory)` is what
    /// Agento passes and go-mail's implicit-TLS switch is a different call it
    /// never makes.
    #[test]
    fn ssl_tls_means_mandatory_starttls_not_implicit_tls() {
        assert!(matches!(
            tls_policy("ssl_tls", "h").expect("policy"),
            Tls::Required(_)
        ));
        assert!(matches!(
            tls_policy("starttls", "h").expect("policy"),
            Tls::Opportunistic(_)
        ));
        // `default` in Go's switch: "none" and anything unrecognized alike.
        for value in ["none", "", "tls", "SSL_TLS"] {
            assert!(
                matches!(tls_policy(value, "h").expect("policy"), Tls::None),
                "{value:?} must fall to the default arm"
            );
        }
    }

    /// A trailing comma in the recipients field is the common case, and Go
    /// skips the blank rather than failing on it.
    #[test]
    fn recipients_are_split_trimmed_and_blanks_skipped() {
        let mut cfg = config();
        cfg.to_addresses = " one@example.com , two@example.com,".into();
        let message = build_message(&cfg, &test_mail()).expect("message");
        let envelope = message.envelope();
        assert_eq!(envelope.to().len(), 2);
    }

    /// Nothing to send to is not something to dial for.
    #[test]
    fn no_usable_recipient_is_an_error_before_any_dial() {
        let mut cfg = config();
        cfg.to_addresses = " , ".into();
        assert!(build_message(&cfg, &test_mail()).is_err());
    }

    #[test]
    fn an_unparseable_from_address_names_the_field() {
        let mut cfg = config();
        cfg.from_address = "not an address".into();
        let err = build_message(&cfg, &test_mail()).unwrap_err();
        assert!(err.starts_with("invalid from address:"), "{err}");
    }

    /// Both bodies ride along, and the HTML one is the branded wrapper rather
    /// than the raw text.
    #[test]
    fn the_message_carries_the_plain_text_and_the_branded_html() {
        let message = build_message(&config(), &test_mail()).expect("message");
        let raw = String::from_utf8(message.formatted()).expect("utf-8");
        assert!(raw.contains("multipart/alternative"), "{raw}");
        assert!(raw.contains("text/plain"));
        assert!(raw.contains("text/html"));
        assert!(
            raw.contains("Agento Notification - Test Notification"),
            "the subject must reach the wire"
        );
    }

    /// A port is a `u16` on the wire and an `int` in the settings column, so a
    /// nonsense value has to fail rather than wrap.
    #[test]
    fn an_out_of_range_port_fails_rather_than_wrapping() {
        let mut cfg = config();
        cfg.port = 70000;
        assert!(build_transport(&cfg).is_err());
        cfg.port = -1;
        assert!(build_transport(&cfg).is_err());
    }

    // ─── Against a real socket ────────────────────────────────────────────────

    use std::io::{BufRead, BufReader, Write};
    use std::net::{TcpListener, TcpStream};

    /// A scripted SMTP server, in the spirit of the fake CLI `chat_turn.rs`
    /// drives the SDK with.
    ///
    /// The unit tests above check the *message*; this checks the *conversation*,
    /// which is the half that is a property of a sequence rather than of a
    /// function. Without it, "we build a plausible message" and "a server
    /// accepts it" are two different claims and only the first is tested — and
    /// the second is the one a user notices.
    fn serve_one_session(listener: TcpListener) -> std::thread::JoinHandle<String> {
        std::thread::spawn(move || {
            let (stream, _) = listener.accept().expect("accept");
            let mut writer = stream.try_clone().expect("clone");
            let mut reader = BufReader::new(stream);
            let say = |w: &mut TcpStream, line: &str| {
                w.write_all(line.as_bytes()).expect("write");
                w.write_all(b"\r\n").expect("write");
            };

            say(&mut writer, "220 fake.example.com ESMTP");
            let mut transcript = String::new();
            let mut in_data = false;
            loop {
                let mut line = String::new();
                if reader.read_line(&mut line).unwrap_or(0) == 0 {
                    break;
                }
                let trimmed = line.trim_end_matches(['\r', '\n']).to_string();

                if in_data {
                    if trimmed == "." {
                        in_data = false;
                        say(&mut writer, "250 2.0.0 Ok: queued");
                        continue;
                    }
                    transcript.push_str(&trimmed);
                    transcript.push('\n');
                    continue;
                }

                let upper = trimmed.to_ascii_uppercase();
                if upper.starts_with("EHLO") || upper.starts_with("HELO") {
                    // Multi-line, and AUTH has to be advertised or lettre will
                    // skip authenticating and the credentials would go untested.
                    say(&mut writer, "250-fake.example.com");
                    say(&mut writer, "250 AUTH PLAIN LOGIN");
                } else if upper.starts_with("AUTH") {
                    transcript.push_str(&format!("<{trimmed}>\n"));
                    say(&mut writer, "235 2.7.0 Authentication successful");
                } else if upper.starts_with("MAIL FROM") || upper.starts_with("RCPT TO") {
                    transcript.push_str(&format!("<{trimmed}>\n"));
                    say(&mut writer, "250 2.1.0 Ok");
                } else if upper.starts_with("DATA") {
                    in_data = true;
                    say(&mut writer, "354 End data with <CR><LF>.<CR><LF>");
                } else if upper.starts_with("QUIT") {
                    say(&mut writer, "221 2.0.0 Bye");
                    break;
                } else {
                    say(&mut writer, "250 2.0.0 Ok");
                }
            }
            transcript
        })
    }

    #[test]
    fn a_send_completes_the_smtp_conversation_and_delivers_both_bodies() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let port = i64::from(listener.local_addr().expect("addr").port());
        let server = serve_one_session(listener);

        let mut cfg = config();
        cfg.host = "127.0.0.1".into();
        cfg.port = port;
        // Plaintext: a TLS handshake against a scripted socket would be testing
        // rustls rather than this module.
        cfg.encryption = "none".into();
        cfg.to_addresses = "one@example.com, two@example.com".into();

        send(&cfg, &test_mail()).expect("the send must succeed");

        let transcript = server.join().expect("server thread");
        assert!(transcript.contains("<AUTH PLAIN"), "{transcript}");
        assert!(
            transcript.contains("<MAIL FROM:<agento@example.com>>"),
            "{transcript}"
        );
        assert!(
            transcript.contains("<RCPT TO:<one@example.com>>")
                && transcript.contains("<RCPT TO:<two@example.com>>"),
            "both recipients must reach the envelope: {transcript}"
        );
        assert!(
            transcript.contains("Subject: Agento Notification - Test Notification"),
            "{transcript}"
        );
        assert!(transcript.contains("multipart/alternative"), "{transcript}");
        assert!(transcript.contains("text/html"), "{transcript}");
    }

    /// The property the whole ordering rests on: a failed send must be a
    /// failure *to deliver*, or a user pressing the button again would send a
    /// second email rather than making a second failed dial.
    #[test]
    fn a_refused_connection_is_an_error_and_delivers_nothing() {
        // Bind and drop, so the port is almost certainly free and nothing
        // answers on it.
        let port = {
            let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
            i64::from(listener.local_addr().expect("addr").port())
        };
        let mut cfg = config();
        cfg.host = "127.0.0.1".into();
        cfg.port = port;
        cfg.encryption = "none".into();

        assert!(send(&cfg, &test_mail()).is_err());
    }
}
