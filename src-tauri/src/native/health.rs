//! `GET /health` — the liveness probe.
//!
//! The route sits outside `/api`, so the write-routes audit (writes-only by
//! design) never decided it. Answering wins on cost over dropping it: it is one
//! constant, and anything external that ever probed `/health` — a script, a
//! monitor — keeps getting the same bytes.
//!
//! The body is Go's literal `w.Write([]byte(`{"status":"ok"}`))`
//! (`internal/server/server.go`): no trailing newline, because it never went
//! through `writeJSON`'s encoder.

use axum::http::Method;

/// This module's entry in `native::ENDPOINTS`.
pub const ENDPOINT: super::Endpoint = super::Endpoint {
    name: "health",
    claims,
    serve,
};

fn claims(method: &Method, path: &str) -> bool {
    method == Method::GET && path == "/health"
}

fn serve(_ctx: &super::Ctx, _req: &super::Request) -> Result<super::Answer, String> {
    Ok(super::Answer::json(b"{\"status\":\"ok\"}".to_vec()))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Go's handler writes the literal without a trailing newline — it never
    /// went through the JSON encoder, so neither may this.
    #[test]
    fn the_health_body_is_gos_literal() {
        let answer = serve(
            &crate::native::Ctx {
                db_path: std::path::PathBuf::new(),
            },
            &crate::native::Request {
                method: &Method::GET,
                path: "/health",
                query: "",
                content_type: "",
                secret_token: "",
                body: &[],
            },
        )
        .expect("health always answers");
        assert_eq!(answer.status, axum::http::StatusCode::OK);
        assert_eq!(answer.body.as_deref(), Some(&b"{\"status\":\"ok\"}"[..]));
    }

    #[test]
    fn only_the_exact_route_is_claimed() {
        assert!(claims(&Method::GET, "/health"));
        assert!(!claims(&Method::POST, "/health"));
        assert!(!claims(&Method::GET, "/health/"));
        assert!(!claims(&Method::GET, "/api/health"));
    }
}
