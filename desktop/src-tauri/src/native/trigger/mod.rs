//! The Telegram trigger path: matching an inbound message to a rule, and
//! running the agent it names (#319).
//!
//! Mirrors `internal/trigger`.

pub mod dispatcher;
pub mod match_rule;
pub mod receiver;
pub mod telegram_api;

use axum::http::Method;

/// This module's entry in `native::ENDPOINTS`.
pub const ENDPOINT: crate::native::Endpoint = crate::native::Endpoint {
    name: "telegram-webhook",
    claims,
    serve,
};

/// `POST /webhooks/telegram/{id}` — and nothing else.
///
/// **Not under `/api`**, which is the only route in `ENDPOINTS` that is true of.
/// `proxy::is_api_path` already admits `/webhooks`, so the request reaches the
/// seam; `guards.rs` deliberately does not, because the request arrives from
/// Telegram with a foreign `Host` and is authenticated by its own secret.
fn claims(method: &Method, path: &str) -> bool {
    method == Method::POST && webhook_id(path).is_some()
}

/// The integration id in `/webhooks/telegram/{id}`, if the path is one.
///
/// One segment, as chi routes it — a trailing slash or a nested path is not this
/// route.
fn webhook_id(path: &str) -> Option<&str> {
    let rest = path.strip_prefix("/webhooks/telegram/")?;
    if rest.is_empty() || rest.contains('/') {
        return None;
    }
    Some(rest)
}

fn serve(
    ctx: &crate::native::Ctx,
    req: &crate::native::Request,
) -> Result<crate::native::Answer, String> {
    let Some(id) = webhook_id(req.path) else {
        return Err(format!("{} is not the telegram webhook", req.path));
    };

    match receiver::receive(&ctx.db_path, id, req.secret_token, req.body) {
        // Go writes the 200 **before** dispatching, so Telegram is never held
        // open for an agent run and never retries because of one.
        receiver::Inbound::Dispatch { bot_token, update } => {
            dispatcher::handle_update(&ctx.db_path, id, &bot_token, update);
            Ok(crate::native::Answer::status_only(
                axum::http::StatusCode::OK,
            ))
        }
        receiver::Inbound::Ignore => Ok(crate::native::Answer::status_only(
            axum::http::StatusCode::OK,
        )),
        // `http.Error`, which writes the message and a trailing newline as
        // `text/plain` — not the JSON envelope every `/api` error uses.
        receiver::Inbound::Forbidden => Ok(crate::native::Answer::text_status(
            axum::http::StatusCode::FORBIDDEN,
            "forbidden\n",
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_post_webhook_path_is_claimed() {
        assert!(claims(&Method::POST, "/webhooks/telegram/abc"));
        // The method matters: chi mounts only POST.
        assert!(!claims(&Method::GET, "/webhooks/telegram/abc"));
        // One segment, and non-empty.
        assert!(!claims(&Method::POST, "/webhooks/telegram/"));
        assert!(!claims(&Method::POST, "/webhooks/telegram"));
        assert!(!claims(&Method::POST, "/webhooks/telegram/a/b"));
        assert!(!claims(&Method::POST, "/webhooks/slack/abc"));
        assert!(!claims(&Method::POST, "/api/webhooks/telegram/abc"));
    }
}
