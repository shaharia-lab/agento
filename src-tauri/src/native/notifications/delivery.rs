//! A task's output by email: the `email` delivery destination (#640, epic
//! #626).
//!
//! **The provider, and nothing else, is shared with the notifications.** The
//! mail goes out through the SMTP server configured in Settings →
//! Notifications ([`super::stored_provider`]), but the global `enabled` switch
//! and the `on_finished`/`on_failed` preferences are not consulted: they govern
//! the metadata notification [`super::handle`] sends, and a destination the
//! user added to one task is its own opt-in. Gating it on an unrelated switch
//! would make it silently do nothing.
//!
//! **One message per destination**, every recipient on `To` — `build_message`'s
//! existing shape, so the recipients see each other, and the form says so.
//!
//! **Blocking.** `smtp::send` is lettre's blocking transport, so [`deliver`]
//! runs on the blocking pool (`schedule::delivery` calls it through
//! `db::blocking`), never on a tokio worker.
//!
//! Nothing here writes `notification_log`: the outcome is the destination's
//! `job_deliveries` row (#635).

use std::path::Path;

use super::smtp::{self, Mail};

/// What the email says about one run.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RunSummary {
    pub task_name: String,
    pub succeeded: bool,
    pub duration_ms: i64,
    pub model: String,
    pub chat_session_id: String,
    /// The answer on a successful run, the error on a failed one.
    pub body: String,
}

/// How the destination's one send ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EmailOutcome {
    Sent,
    Skipped(String),
    Failed(String),
}

/// What a run records when there is no provider to send through.
pub const SKIPPED_NO_SMTP: &str = "SMTP is not configured — set it up in Settings → Notifications";

/// The mail for `run`: `Agento Notification - <task>: <Completed|Failed>`, and a
/// body of header lines, a blank line, then the output in full. The HTML part
/// is `smtp::send`'s `build_email_html` over this same body, so it escapes on
/// the golden's table and keeps the newlines.
pub fn mail(run: &RunSummary) -> Mail {
    let status = if run.succeeded { "Completed" } else { "Failed" };
    let body = format!(
        "Task: {}\nStatus: {status}\nDuration: {} ms\nModel: {}\nChat Session ID: {}\n\n{}",
        run.task_name, run.duration_ms, run.model, run.chat_session_id, run.body
    );
    Mail {
        subject: super::task_output_subject(&run.task_name, run.succeeded),
        body,
    }
}

/// Send `run` to `recipients` through the stored provider. Blocking: see the
/// module header.
pub fn deliver(db_path: &Path, recipients: &[String], run: &RunSummary) -> EmailOutcome {
    let provider = match super::stored_provider(db_path) {
        Ok(Some(provider)) => provider,
        Ok(None) => return EmailOutcome::Skipped(SKIPPED_NO_SMTP.to_string()),
        Err(e) => {
            log::error!("email delivery: failed to load notification settings: {e}");
            return EmailOutcome::Failed("could not read the SMTP settings".to_string());
        }
    };
    match smtp::send_to(&provider, recipients, &mail(run)) {
        Ok(()) => EmailOutcome::Sent,
        Err(e) => EmailOutcome::Failed(e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SECRET: &str = "EMAIL-DELIVERY-SECRET";

    fn with_settings(settings_json: &str) -> tempfile::NamedTempFile {
        let file = tempfile::NamedTempFile::new().expect("temp file");
        let mut conn = rusqlite::Connection::open(file.path()).expect("open");
        crate::native::migrate::apply(&mut conn).expect("migrate");
        conn.execute(
            "INSERT INTO user_settings (id, notification_settings) VALUES (1, ?1)",
            [settings_json],
        )
        .expect("seed");
        file
    }

    fn run() -> RunSummary {
        RunSummary {
            task_name: "Daily report".into(),
            succeeded: true,
            duration_ms: 1234,
            model: "claude-sonnet-5".into(),
            chat_session_id: "chat-1".into(),
            body: "line one\nline <two> + more".into(),
        }
    }

    #[test]
    fn the_mail_is_the_header_a_blank_line_and_the_output() {
        let mail = mail(&run());
        assert_eq!(
            mail.subject,
            "Agento Notification - Daily report: Completed"
        );
        assert_eq!(
            mail.body,
            "Task: Daily report\nStatus: Completed\nDuration: 1234 ms\nModel: claude-sonnet-5\n\
             Chat Session ID: chat-1\n\nline one\nline <two> + more"
        );
        let failed = super::mail(&RunSummary {
            succeeded: false,
            ..run()
        });
        assert_eq!(failed.subject, "Agento Notification - Daily report: Failed");
        assert!(failed
            .body
            .starts_with("Task: Daily report\nStatus: Failed\n"));
    }

    #[test]
    fn the_html_part_escapes_on_the_golden_table_and_keeps_newlines() {
        let mail = mail(&run());
        let html = super::super::template::build_email_html(&mail.subject, &mail.body);
        assert!(
            html.contains("line one\nline &lt;two&gt; &#43; more"),
            "{html}"
        );
        assert!(html.contains("white-space:pre-wrap"), "{html}");
    }

    #[test]
    fn no_provider_is_skipped_with_the_settings_pointer() {
        for settings in [
            "{}",
            r#"{"enabled":true,"provider":{"host":"","from_address":"a@example.com"}}"#,
            r#"{"enabled":true,"provider":{"host":"smtp.example.com","from_address":""}}"#,
        ] {
            let file = with_settings(settings);
            assert_eq!(
                deliver(file.path(), &["to@example.com".into()], &run()),
                EmailOutcome::Skipped(SKIPPED_NO_SMTP.to_string()),
                "{settings}"
            );
        }
    }

    #[test]
    fn stored_provider_is_none_for_an_empty_host() {
        let file = with_settings(r#"{"provider":{"host":"","from_address":"a@example.com"}}"#);
        assert_eq!(super::super::stored_provider(file.path()), Ok(None));
    }

    /// Independent of the global switch: `enabled: false` with opted-out
    /// preferences still dials — here, a port nothing listens on — and the
    /// failure is the send's, without the password in it.
    #[test]
    fn an_unreachable_server_fails_without_the_password_and_ignores_the_switch() {
        let port = {
            let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
            listener.local_addr().expect("addr").port()
        };
        let file = with_settings(&format!(
            r#"{{"enabled":false,"provider":{{"host":"127.0.0.1","port":{port},
                "username":"mailer","password":"{SECRET}","from_address":"agento@example.com",
                "to_addresses":"","encryption":"none"}},
               "preferences":{{"scheduled_tasks":{{"on_finished":false,"on_failed":false}}}}}}"#
        ));
        let EmailOutcome::Failed(e) = deliver(file.path(), &["to@example.com".into()], &run())
        else {
            panic!("an unreachable server must fail");
        };
        assert!(e.starts_with("sending mail: "), "{e}");
        assert!(!e.contains(SECRET), "{e}");
    }
}
