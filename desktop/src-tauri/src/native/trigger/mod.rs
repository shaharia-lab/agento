//! The Telegram trigger path: matching an inbound message to a rule, and
//! running the agent it names (#319).
//!
//! Mirrors `internal/trigger`.

pub mod dispatcher;
pub mod match_rule;
pub mod receiver;
pub mod registration;
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

/// Run an async handler to completion from the seam's **synchronous** `serve`.
///
/// `serve` is a sync `fn` the proxy calls on `spawn_blocking`, and the three
/// registration routes are network calls — so something has to bridge, and
/// unlike `registry::block_on_detached` this one needs the *result*.
///
/// `Handle::block_on` is correct here for the reason it is wrong there: a
/// `spawn_blocking` thread is not a runtime worker, so blocking it parks a
/// thread from the blocking pool rather than stalling the executor. That is
/// exactly what the pool is for.
fn block_on_result<F, T>(what: &str, future: F) -> Result<T, crate::native::writes::WriteError>
where
    F: std::future::Future<Output = Result<T, crate::native::writes::WriteError>>,
{
    match tokio::runtime::Handle::try_current() {
        Ok(handle) => handle.block_on(future),
        // Only reachable from a unit test calling the handler directly; the
        // proxy is axum and always has a runtime.
        Err(_) => Err(crate::native::writes::WriteError::Fallback(format!(
            "{what}: no tokio runtime on this thread"
        ))),
    }
}

/// `POST /api/integrations/{id}/webhook/register`.
pub fn serve_register(
    db_path: &std::path::Path,
    id: &str,
) -> Result<crate::native::Answer, crate::native::writes::WriteError> {
    block_on_result("registering webhook", registration::register(db_path, id))?;
    status_response("registered")
}

/// `DELETE /api/integrations/{id}/webhook/register`. 204, no body.
pub fn serve_delete(
    db_path: &std::path::Path,
    id: &str,
) -> Result<crate::native::Answer, crate::native::writes::WriteError> {
    block_on_result("deleting webhook", registration::delete(db_path, id))?;
    Ok(crate::native::Answer::no_content())
}

/// `POST /api/integrations/{id}/webhook/regenerate-secret`.
pub fn serve_regenerate(
    db_path: &std::path::Path,
    id: &str,
) -> Result<crate::native::Answer, crate::native::writes::WriteError> {
    block_on_result(
        "regenerating webhook secret",
        registration::regenerate(db_path, id),
    )?;
    status_response("regenerated")
}

/// `map[string]string{"status": …}` — one key, so nothing to sort.
fn status_response(
    status: &str,
) -> Result<crate::native::Answer, crate::native::writes::WriteError> {
    #[derive(serde::Serialize)]
    struct StatusResponse<'a> {
        status: &'a str,
    }
    let body = crate::native::gojson::to_vec(&StatusResponse { status }).map_err(|e| {
        crate::native::writes::WriteError::Fallback(format!("encoding status: {e}"))
    })?;
    Ok(crate::native::Answer::json(body))
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
