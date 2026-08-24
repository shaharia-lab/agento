//! The LLM gateway's control plane — `/api/gateway/*` (#426).
//!
//! # Why this lives under `native/` and the listener does not
//!
//! `gateway/` is a second listener speaking somebody else's wire formats; this
//! is the `/api` seam, with Agento's own shapes, Agento's own guard and
//! Agento's own `read`/`write` scoping. The two are on opposite sides of that
//! line, which is why the control plane is here and the data plane is there —
//! and why `Scope::Llm` opens nothing on these twelve routes while a `read`
//! token opens every one of their GETs.
//!
//! # The one question the issue left open was already answered in the tree
//!
//! These are desktop-only routes with no Go ancestor, and the issue asks
//! whether `read_routes.json`/`write_routes.json` should grow a section or
//! whether control should fall back to Tauri commands. Neither: #405 hit this
//! exact problem for `/api/security/*` and created
//! **`parity/desktop_routes.json`**, which is explicitly not a Go golden and
//! carries a *stronger* guarantee than the two that are — set equality against
//! the module's own `ROUTES` const, so a route cannot be added, removed or
//! renamed without the file moving. This module is that file's second owner.
//!
//! The Tauri-command fallback would have been wrong for a reason worth stating:
//! `logs.rs` is a command because the app log belongs to the *process*. Gateway
//! configuration belongs to the API surface — the UI reads it over `fetch` like
//! everything else, and a command would put it outside the route table that
//! exists to stop the surface drifting.
//!
//! # Credentials
//!
//! Two rules, and the second is the one this module exists to get right.
//!
//! **A read never selects the key.** Every read path goes through
//! `config::load_provider_summaries`, whose projection replaces `api_key` with
//! a boolean computed in SQL. There is no masked form — a masked secret is
//! still a secret, and the UI only needs to know whether one is set.
//!
//! **An omitted key preserves the stored one.** `PUT /api/integrations/{id}`
//! wipes credentials the caller omits while `GET` scrubs them, so a
//! read-then-write round trip — which is exactly what an edit form does —
//! destroys the secret. That is a real data-loss bug on this repository's
//! known-bugs list, reproduced there deliberately because it was Go's
//! behaviour. **It must not be reproduced here**, and it is not: `api_key` is
//! `Option<String>` with three meanings, absent means "leave it alone", and
//! `config::update_provider`'s `None` arm does not name the column at all.

use axum::http::{Method, StatusCode};
use serde::{Deserialize, Serialize};

use super::writes::{self, WriteError};
use super::{Answer, Ctx, Request};
use crate::gateway::{config, registry, usage};

/// Every route this module claims.
///
/// Asserted as **set equality** against `parity/desktop_routes.json` by
/// `native::tests::the_desktop_only_routes_are_recorded_in_both_directions`,
/// so this list and that file cannot drift.
pub const ROUTES: &[(&str, &str)] = &[
    ("GET", "/api/gateway/settings"),
    ("PUT", "/api/gateway/settings"),
    ("GET", "/api/gateway/providers"),
    ("POST", "/api/gateway/providers"),
    ("PUT", "/api/gateway/providers/{id}"),
    ("DELETE", "/api/gateway/providers/{id}"),
    ("GET", "/api/gateway/models"),
    ("POST", "/api/gateway/models"),
    ("PUT", "/api/gateway/models/{id}"),
    ("DELETE", "/api/gateway/models/{id}"),
    ("GET", "/api/gateway/status"),
    ("GET", "/api/gateway/usage"),
];

pub const ENDPOINT: super::Endpoint = super::Endpoint {
    name: "gateway",
    claims,
    serve,
};

fn claims(method: &Method, path: &str) -> bool {
    ROUTES
        .iter()
        .any(|(m, pattern)| method.as_str() == *m && path_matches(pattern, path))
}

/// Match a chi-style pattern against a concrete path.
///
/// Copied from `native::security::path_matches`, which is private to that
/// module. A `{name}` segment matches exactly one **non-empty** segment, so
/// `/api/gateway/providers/` is not a match — chi routes a trailing slash to
/// nothing, and so does every other module here.
fn path_matches(pattern: &str, path: &str) -> bool {
    let mut want = pattern.split('/');
    let mut have = path.split('/');
    loop {
        match (want.next(), have.next()) {
            (None, None) => return true,
            (Some(w), Some(h)) => {
                let ok = if w.starts_with('{') && w.ends_with('}') {
                    !h.is_empty()
                } else {
                    w == h
                };
                if !ok {
                    return false;
                }
            }
            _ => return false,
        }
    }
}

/// The `{id}` of a `/api/gateway/<collection>/{id}` path.
fn id_under(path: &str, collection: &str) -> Option<String> {
    let prefix = format!("/api/gateway/{collection}/");
    let rest = path.strip_prefix(&prefix)?;
    if rest.is_empty() || rest.contains('/') {
        return None;
    }
    Some(rest.to_string())
}

fn serve(ctx: &Ctx, req: &Request) -> Result<Answer, String> {
    let db = &ctx.db_path;
    match (req.method.as_str(), req.path) {
        ("GET", "/api/gateway/settings") => read_settings(db),
        ("PUT", "/api/gateway/settings") => writes::finish(put_settings(db, req.body)),

        ("GET", "/api/gateway/providers") => read_providers(db),
        ("POST", "/api/gateway/providers") => writes::finish(create_provider(db, req.body)),

        ("GET", "/api/gateway/models") => read_models(db),
        ("POST", "/api/gateway/models") => writes::finish(create_alias(db, req.body)),

        ("GET", "/api/gateway/status") => read_status(),
        ("GET", "/api/gateway/usage") => read_usage(db, req.query),

        ("PUT", path) => match (id_under(path, "providers"), id_under(path, "models")) {
            (Some(id), _) => writes::finish(update_provider(db, &id, req.body)),
            (_, Some(id)) => writes::finish(update_alias(db, &id, req.body)),
            _ => Err(format!("PUT {path} is claimed but has no id")),
        },
        ("DELETE", path) => match (id_under(path, "providers"), id_under(path, "models")) {
            (Some(id), _) => writes::finish(delete_provider(db, &id)),
            (_, Some(id)) => writes::finish(delete_alias(db, &id)),
            _ => Err(format!("DELETE {path} is claimed but has no id")),
        },

        _ => Err(format!(
            "{} {} is claimed but unhandled",
            req.method, req.path
        )),
    }
}

/// Bring the running listener in line with what was just stored.
///
/// Spawned, never awaited: `reload` is stop-then-start over a socket bind, and
/// a save that blocked the HTTP response on it would make the settings form
/// feel like it hung. **A write and its effect still belong in one place** —
/// this is that place — but "in one place" is about where the call lives, not
/// about whether the response waits for it.
///
/// It runs *after* the row is stored, so a reload that fails leaves a stored
/// config and a `Status` saying why, which is exactly what
/// `GET /api/gateway/status` is for.
fn reload_after_write(db_path: &std::path::Path) {
    let db_path = db_path.to_path_buf();
    let Ok(handle) = tokio::runtime::Handle::try_current() else {
        // Only reachable from a synchronous test. `tokens::touch` degrades the
        // same way rather than panicking on a request path.
        return;
    };
    handle.spawn(async move {
        if let Err(e) = registry::reload(&db_path).await {
            log::warn!("llm gateway reload after a config write failed: {e}");
        }
    });
}

// ─── Settings ─────────────────────────────────────────────────────────────────

fn read_settings(db_path: &std::path::Path) -> Result<Answer, String> {
    let settings = config::load_settings(db_path)?;
    let body =
        super::gojson::to_vec(&settings).map_err(|e| format!("encoding gateway settings: {e}"))?;
    Ok(Answer::json(body))
}

/// The lowest port a non-root process can bind on Unix, and the floor this
/// refuses below.
const MIN_PORT: u16 = 1024;

fn put_settings(db_path: &std::path::Path, body: &[u8]) -> Result<Answer, WriteError> {
    let settings: config::GatewaySettings = writes::decode_body(body)?;

    // Fail before mutating: validate, then store, then reload. `validate`
    // already refuses port 0; the range check is this route's, because a port
    // the process cannot bind is a setting that can only ever produce a
    // `BindFailed` status.
    // `validate` covers two fields now, so it names the one that failed rather
    // than the route inferring it — a form that highlights the wrong input is
    // worse than one that highlights none.
    settings
        .validate()
        .map_err(|e| WriteError::validation(e.field, e.message))?;
    if settings.port < MIN_PORT {
        return Err(WriteError::validation(
            "port",
            format!("port must be {MIN_PORT} or above; lower ports need root"),
        ));
    }

    config::store_settings(db_path, &settings).map_err(WriteError::Fallback)?;
    reload_after_write(db_path);

    log::info!(
        "gateway settings saved enabled={} port={} start_with_app={} usage_retention_days={}",
        settings.enabled,
        settings.port,
        settings.start_with_app,
        settings.usage_retention_days
    );

    let body = super::gojson::to_vec(&settings)
        .map_err(|e| WriteError::Fallback(format!("encoding gateway settings: {e}")))?;
    Ok(Answer::json(body))
}

// ─── Providers ────────────────────────────────────────────────────────────────

/// A provider write's body.
///
/// `api_key` is `Option<String>` and that is the single most important line in
/// this module — see the header. On `POST` an absent key is simply an empty
/// one; on `PUT` it means *keep the stored key*.
#[derive(Debug, Deserialize)]
struct ProviderRequest {
    #[serde(default)]
    id: String,
    #[serde(default)]
    name: String,
    #[serde(default, rename = "type")]
    provider_type: String,
    #[serde(default)]
    api_key: Option<String>,
    #[serde(default)]
    base_url: String,
    #[serde(default)]
    timeouts: Option<config::Timeouts>,
    #[serde(default)]
    enabled: bool,
}

impl ProviderRequest {
    /// The fields every provider write validates the same way.
    fn validated(&self) -> Result<(String, config::ProviderType, config::Timeouts), WriteError> {
        let name = self.name.trim().to_string();
        if name.is_empty() {
            return Err(WriteError::validation("name", "name is required"));
        }
        let provider_type = config::ProviderType::parse(&self.provider_type).ok_or_else(|| {
            // The value is not quoted back. It is caller-supplied and lands in
            // a response body; naming the accepted set is what the caller
            // needs, and echoing their input is not.
            WriteError::validation(
                "type",
                "type must be \"anthropic\", \"openai\", \"gemini\" or \"glm\"",
            )
        })?;
        Ok((name, provider_type, self.timeouts.unwrap_or_default()))
    }
}

fn read_providers(db_path: &std::path::Path) -> Result<Answer, String> {
    let providers = config::load_provider_summaries(db_path)?;
    let body = super::gojson::to_vec(&providers)
        .map_err(|e| format!("encoding gateway providers: {e}"))?;
    Ok(Answer::json(body))
}

fn create_provider(db_path: &std::path::Path, body: &[u8]) -> Result<Answer, WriteError> {
    let req: ProviderRequest = writes::decode_body(body)?;
    let (name, provider_type, timeouts) = req.validated()?;

    let id = if req.id.trim().is_empty() {
        uuid::Uuid::new_v4().to_string()
    } else {
        req.id.trim().to_string()
    };

    // `store_provider` upserts, so a POST naming an id that already exists
    // would **overwrite another provider** — replacing its key and its base URL
    // — while answering `201 Created`. A caller-chosen id is worth keeping (it
    // makes seeding idempotent to write), so the answer is to refuse the
    // collision rather than to ignore the field.
    let existing = config::load_provider_summaries(db_path).map_err(WriteError::Fallback)?;
    if existing.iter().any(|p| p.id == id) {
        return Err(WriteError::Conflict {
            resource: "gateway provider".to_string(),
            id,
        });
    }
    // Two providers sharing a name is the same failure one level along: routing
    // targets refer to providers *by name*, so an alias pointing at a duplicate
    // resolves to whichever the registry happened to keep.
    let name_taken = existing.iter().any(|p| p.name == name);
    if name_taken {
        return Err(WriteError::Conflict {
            resource: "gateway provider name".to_string(),
            id: name,
        });
    }
    let api_key = req.api_key.unwrap_or_default();

    config::store_provider(
        db_path,
        &config::ProviderInput {
            id: &id,
            name: &name,
            provider_type,
            api_key: &api_key,
            base_url: req.base_url.trim(),
            timeouts,
            enabled: req.enabled,
        },
    )
    .map_err(WriteError::Fallback)?;
    reload_after_write(db_path);

    // The name is user-authored text and is logged deliberately, on the terms
    // `integrations created … name=` already established: a line that cannot
    // say which provider was created is most of what it is for. The key is not
    // logged, here or anywhere.
    log::info!(
        "gateway provider created id={id:?} name={name:?} type={:?}",
        provider_type.as_str()
    );

    answer_provider(db_path, &id, StatusCode::CREATED)
}

fn update_provider(db_path: &std::path::Path, id: &str, body: &[u8]) -> Result<Answer, WriteError> {
    let req: ProviderRequest = writes::decode_body(body)?;
    let (name, provider_type, timeouts) = req.validated()?;

    let updated = config::update_provider(
        db_path,
        &config::ProviderUpdate {
            id,
            name: &name,
            provider_type,
            // **The whole point.** `None` here is "the caller did not send a
            // key", and `config::update_provider` answers it by leaving the
            // column out of the SET list — so a scrubbed `GET` round-tripped
            // straight back preserves the secret instead of destroying it.
            api_key: req.api_key.as_deref(),
            base_url: req.base_url.trim(),
            timeouts,
            enabled: req.enabled,
        },
    )
    .map_err(WriteError::Fallback)?;

    if !updated {
        return Err(WriteError::NotFound {
            resource: "gateway provider".to_string(),
            id: id.to_string(),
        });
    }
    reload_after_write(db_path);

    log::info!("gateway provider updated id={id:?} name={name:?}");
    answer_provider(db_path, id, StatusCode::OK)
}

fn delete_provider(db_path: &std::path::Path, id: &str) -> Result<Answer, WriteError> {
    // The name, not the id, is what a routing target refers to — so the
    // reference check needs the row before it goes.
    let summaries = config::load_provider_summaries(db_path).map_err(WriteError::Fallback)?;
    let Some(provider) = summaries.into_iter().find(|p| p.id == id) else {
        return Err(WriteError::NotFound {
            resource: "gateway provider".to_string(),
            id: id.to_string(),
        });
    };

    // Nothing in SQL enforces this: an alias stores provider *names* inside a
    // JSON column. Deleting anyway would leave every target pointing at it
    // resolving to nothing, and the alias would fail at request time — far from
    // the action that caused it, and with no way to tell why.
    let used_by =
        config::aliases_using_provider(db_path, &provider.name).map_err(WriteError::Fallback)?;
    if !used_by.is_empty() {
        return Err(WriteError::Conflict {
            resource: format!(
                "gateway provider (still routed to by {})",
                used_by.join(", ")
            ),
            id: id.to_string(),
        });
    }

    if !config::delete_provider(db_path, id).map_err(WriteError::Fallback)? {
        return Err(WriteError::NotFound {
            resource: "gateway provider".to_string(),
            id: id.to_string(),
        });
    }
    reload_after_write(db_path);

    log::info!(
        "gateway provider deleted id={id:?} name={:?}",
        provider.name
    );
    Ok(Answer::no_content())
}

/// Re-read one provider through the **public** projection and answer with it.
///
/// Re-read rather than echoed back from the request, so the response is the
/// stored row — and so the key cannot be echoed even by accident, because this
/// projection has no field to echo it in.
fn answer_provider(
    db_path: &std::path::Path,
    id: &str,
    status: StatusCode,
) -> Result<Answer, WriteError> {
    let providers = config::load_provider_summaries(db_path).map_err(WriteError::Fallback)?;
    let Some(provider) = providers.into_iter().find(|p| p.id == id) else {
        return Err(WriteError::Fallback(
            "the provider just written could not be read back".to_string(),
        ));
    };
    let body = super::gojson::to_vec(&provider)
        .map_err(|e| WriteError::Fallback(format!("encoding gateway provider: {e}")))?;
    Ok(Answer::json_status(status, body))
}

// ─── Model aliases ────────────────────────────────────────────────────────────

fn read_models(db_path: &std::path::Path) -> Result<Answer, String> {
    let aliases = config::load_aliases(db_path)?;
    let body =
        super::gojson::to_vec(&aliases).map_err(|e| format!("encoding gateway aliases: {e}"))?;
    Ok(Answer::json(body))
}

/// A model alias write's body. No secrets here, so it is the plain shape.
#[derive(Debug, Deserialize)]
struct AliasRequest {
    #[serde(default)]
    id: String,
    #[serde(default)]
    alias: String,
    #[serde(default)]
    routing: config::Routing,
    #[serde(default)]
    enabled: bool,
}

impl AliasRequest {
    /// `own_id` is the row being updated, or `None` on a create — an update
    /// must not collide with itself.
    fn validated(
        &self,
        db_path: &std::path::Path,
        own_id: Option<&str>,
    ) -> Result<String, WriteError> {
        let alias = self.alias.trim().to_string();
        if alias.is_empty() {
            return Err(WriteError::validation("alias", "alias is required"));
        }

        // The alias **is** the routing key a client sends as `model`, and
        // `Dispatcher` collects aliases into a map keyed by it — so two rows
        // sharing one silently disable whichever the map does not keep, with
        // nothing anywhere to say which. Refusing the second is the only answer
        // that leaves the table meaning what it looks like it means.
        let taken = config::load_aliases(db_path)
            .map_err(WriteError::Fallback)?
            .into_iter()
            .any(|a| a.alias == alias && Some(a.id.as_str()) != own_id);
        if taken {
            return Err(WriteError::Conflict {
                resource: "gateway model alias".to_string(),
                id: alias,
            });
        }
        if self.routing.targets.is_empty() {
            return Err(WriteError::validation(
                "routing",
                "an alias needs at least one target",
            ));
        }
        // A target naming a provider that does not exist is the same mistake as
        // deleting a referenced one, caught from the other side: it would store
        // an alias that resolves to nothing and fails at request time.
        let known: std::collections::BTreeSet<String> = config::load_provider_summaries(db_path)
            .map_err(WriteError::Fallback)?
            .into_iter()
            .map(|p| p.name)
            .collect();
        for target in self
            .routing
            .targets
            .iter()
            .chain(self.routing.fallbacks.iter())
        {
            if !known.contains(&target.provider) {
                return Err(WriteError::validation(
                    "routing",
                    format!("no gateway provider is named {:?}", target.provider),
                ));
            }
        }
        Ok(alias)
    }
}

fn create_alias(db_path: &std::path::Path, body: &[u8]) -> Result<Answer, WriteError> {
    let req: AliasRequest = writes::decode_body(body)?;
    let alias = req.validated(db_path, None)?;
    let id = if req.id.trim().is_empty() {
        uuid::Uuid::new_v4().to_string()
    } else {
        req.id.trim().to_string()
    };

    let stored = config::ModelAlias {
        id: id.clone(),
        alias: alias.clone(),
        routing: req.routing,
        enabled: req.enabled,
    };
    config::store_alias(db_path, &stored).map_err(WriteError::Fallback)?;
    reload_after_write(db_path);

    log::info!("gateway model alias created id={id:?} alias={alias:?}");
    let body = super::gojson::to_vec(&stored)
        .map_err(|e| WriteError::Fallback(format!("encoding gateway alias: {e}")))?;
    Ok(Answer::json_status(StatusCode::CREATED, body))
}

fn update_alias(db_path: &std::path::Path, id: &str, body: &[u8]) -> Result<Answer, WriteError> {
    let req: AliasRequest = writes::decode_body(body)?;
    let alias = req.validated(db_path, Some(id))?;

    // `store_alias` upserts, so it would happily create a row for an id that
    // does not exist — which on a `PUT /{id}` is a silent create where the
    // caller asked for an update. Check first.
    let exists = config::load_aliases(db_path)
        .map_err(WriteError::Fallback)?
        .iter()
        .any(|a| a.id == id);
    if !exists {
        return Err(WriteError::NotFound {
            resource: "gateway model alias".to_string(),
            id: id.to_string(),
        });
    }

    let stored = config::ModelAlias {
        id: id.to_string(),
        alias: alias.clone(),
        routing: req.routing,
        enabled: req.enabled,
    };
    config::store_alias(db_path, &stored).map_err(WriteError::Fallback)?;
    reload_after_write(db_path);

    log::info!("gateway model alias updated id={id:?} alias={alias:?}");
    let body = super::gojson::to_vec(&stored)
        .map_err(|e| WriteError::Fallback(format!("encoding gateway alias: {e}")))?;
    Ok(Answer::json(body))
}

fn delete_alias(db_path: &std::path::Path, id: &str) -> Result<Answer, WriteError> {
    if !config::delete_alias(db_path, id).map_err(WriteError::Fallback)? {
        return Err(WriteError::NotFound {
            resource: "gateway model alias".to_string(),
            id: id.to_string(),
        });
    }
    reload_after_write(db_path);
    log::info!("gateway model alias deleted id={id:?}");
    Ok(Answer::no_content())
}

// ─── Status ───────────────────────────────────────────────────────────────────

/// What `GET /api/gateway/status` answers.
///
/// A flat shape rather than a tagged enum, because the UI's question is "what
/// do I show" and the answer is a state plus two optional details. `error` is
/// `None` on the healthy states rather than `""`, so a client can branch on its
/// presence.
#[derive(Debug, Serialize)]
struct StatusBody {
    state: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    port: Option<u16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

fn read_status() -> Result<Answer, String> {
    // A **stored** value, not "is there a handle" — that is what makes a bind
    // failure distinguishable from a gateway that is simply off, and it is the
    // difference between the UI explaining a port collision and offering a
    // Start button that silently does nothing.
    let body = status_body(registry::status());
    let encoded =
        super::gojson::to_vec(&body).map_err(|e| format!("encoding gateway status: {e}"))?;
    Ok(Answer::json(encoded))
}

/// The `Status` → wire mapping, split out so it is testable without installing
/// a state into the process-wide registry — which is a shared global, and a
/// test that wrote one would break tests in files it never touched.
fn status_body(status: registry::Status) -> StatusBody {
    match status {
        registry::Status::Stopped => StatusBody {
            state: "stopped",
            port: None,
            error: None,
        },
        registry::Status::Running { port } => StatusBody {
            state: "running",
            port: Some(port),
            error: None,
        },
        registry::Status::BindFailed { port, error } => StatusBody {
            state: "bind_failed",
            port: Some(port),
            error: Some(error),
        },
        // No port: none was reached. Kept distinct from `bind_failed` because
        // the two send the user to different places — a taken port versus a
        // provider row this build cannot turn into an adapter.
        registry::Status::StartFailed { error } => StatusBody {
            state: "start_failed",
            port: None,
            error: Some(error),
        },
    }
}

// ─── Usage ────────────────────────────────────────────────────────────────────

#[derive(Debug, Default, Serialize)]
struct UsageTotals {
    requests: u64,
    prompt_tokens: u64,
    completion_tokens: u64,
    cache_read_tokens: u64,
    cache_write_tokens: u64,
    cost_usd: f64,
    /// Requests whose model the pricing catalog does not price.
    ///
    /// Surfaced as a count beside the total rather than folded into it as zero:
    /// the catalog is seeded for Claude models, so OpenAI and Gemini aliases
    /// miss routinely, and a total that silently absorbed them would read as a
    /// confident figure when it is a floor.
    unpriced_requests: u64,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    unpriced_models: Vec<String>,
}

#[derive(Debug, Default, Clone, Serialize)]
struct UsagePoint {
    date: String,
    requests: u64,
    prompt_tokens: u64,
    completion_tokens: u64,
    cache_read_tokens: u64,
    cache_write_tokens: u64,
    cost_usd: f64,
}

#[derive(Debug, Serialize)]
struct UsageGroup {
    key: String,
    /// What to show instead of `key`, when the key is not itself readable.
    ///
    /// Only `by_token` sets it: its key is an `api_tokens` row id, and that id
    /// appears nowhere else in the UI — the Security tab lists tokens by *name*.
    /// A breakdown whose stated purpose is informing a revocation decision has
    /// to name the credential, so the name is resolved here rather than left to
    /// a second request the view would have to make with a `write`-scoped token
    /// (`/api/security/*` needs `write` whatever the method).
    ///
    /// Absent rather than null when there is nothing better than the key, which
    /// is `skip_serializing_if`'s usual role on this surface.
    #[serde(skip_serializing_if = "Option::is_none")]
    label: Option<String>,
    requests: u64,
    prompt_tokens: u64,
    completion_tokens: u64,
    cost_usd: f64,
}

/// `api_tokens.id` → `name`, for the `by_token` breakdown's labels.
///
/// Every token ever issued, revoked ones included: a revoked credential's
/// spending history is most of what made the revocation an informed decision, so
/// dropping its name would empty the panel of exactly the rows worth reading.
/// A row whose token was deleted outright has no name and keeps its id.
///
/// Best-effort — the names are a label, and a usage window that cannot read them
/// is still a correct usage window.
fn token_names(conn: &rusqlite::Connection) -> std::collections::BTreeMap<String, String> {
    let mut names = std::collections::BTreeMap::new();
    let Ok(mut stmt) = conn.prepare("SELECT id, name FROM api_tokens") else {
        return names;
    };
    let Ok(rows) = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
    else {
        return names;
    };
    for row in rows.flatten() {
        names.insert(row.0, row.1);
    }
    names
}

/// Latency over the window, in milliseconds.
///
/// Percentiles rather than a mean: one 90-second cold start moves an average
/// over a hundred requests by a second, so a mean answers neither "is it usually
/// fast" nor "how bad does it get" (#428).
#[derive(Debug, Default, Serialize)]
struct UsageLatency {
    p50_ms: u64,
    p95_ms: u64,
    max_ms: u64,
}

#[derive(Debug, Serialize)]
struct UsageBody {
    granularity: &'static str,
    totals: UsageTotals,
    latency: UsageLatency,
    series: Vec<UsagePoint>,
    by_alias: Vec<UsageGroup>,
    by_provider: Vec<UsageGroup>,
    by_status: Vec<UsageGroup>,
    by_surface: Vec<UsageGroup>,
    by_token: Vec<UsageGroup>,
}

/// The nearest-rank percentile of an ascending slice, in milliseconds.
///
/// `ceil(p/100 * n)`-th value, one-based — the definition with no interpolation,
/// so every answer is a duration that a request actually took rather than a
/// number between two of them. An empty window is `0`, and the division is
/// guarded here rather than at the encoder: `serde_json` renders a NaN as
/// `null`, which is a wrong number rather than an error.
fn percentile(sorted: &[i64], p: u64) -> u64 {
    if sorted.is_empty() {
        return 0;
    }
    let rank = (sorted.len() as u64 * p).div_ceil(100).max(1) as usize;
    sorted[rank.min(sorted.len()) - 1].max(0) as u64
}

fn read_usage(db_path: &std::path::Path, query: &str) -> Result<Answer, String> {
    use super::analytics::buckets::{bucket_key, walk_buckets};
    use super::analytics::params::AnalyticsParams;
    use std::collections::BTreeMap;

    // The same parser the Claude analytics window uses, so `from`/`to`/`tz`
    // mean the same thing on both dashboards. Its `project` field is not
    // applicable here; the gateway's narrowing key is `alias`.
    let params = AnalyticsParams::parse(query)?;
    let alias = super::analytics::params::query_value(query, "alias");
    let granularity = params.granularity();

    let records = usage::load_window(db_path, params.from, params.to, &alias)?;

    let mut totals = UsageTotals::default();
    let mut unpriced_models: std::collections::BTreeSet<String> = Default::default();
    let mut series: BTreeMap<String, UsagePoint> = BTreeMap::new();
    let mut by_alias: BTreeMap<String, UsageGroup> = BTreeMap::new();
    let mut by_provider: BTreeMap<String, UsageGroup> = BTreeMap::new();
    let mut by_status: BTreeMap<String, UsageGroup> = BTreeMap::new();
    let mut by_surface: BTreeMap<String, UsageGroup> = BTreeMap::new();
    let mut by_token: BTreeMap<String, UsageGroup> = BTreeMap::new();
    let mut durations: Vec<i64> = Vec::with_capacity(records.len());

    for record in &records {
        let cost = record.cost_usd.unwrap_or(0.0);
        totals.requests += 1;
        totals.prompt_tokens += record.observed.prompt;
        totals.completion_tokens += record.observed.completion;
        totals.cache_read_tokens += record.observed.cache_read;
        totals.cache_write_tokens += record.observed.cache_write;
        totals.cost_usd += cost;
        if record.unpriced {
            totals.unpriced_requests += 1;
            if !record.model_id.is_empty() {
                unpriced_models.insert(record.model_id.clone());
            }
        }

        // Bucketed in the request's timezone while storage stays UTC — the rule
        // `analytics/buckets.rs` already enforces, and the reason `tz` is a
        // parameter at all: a "day" is only meaningful in one.
        let key = bucket_key(record.at, granularity, params.loc);
        let point = series.entry(key.clone()).or_insert_with(|| UsagePoint {
            date: key,
            ..Default::default()
        });
        point.requests += 1;
        point.prompt_tokens += record.observed.prompt;
        point.completion_tokens += record.observed.completion;
        point.cache_read_tokens += record.observed.cache_read;
        point.cache_write_tokens += record.observed.cache_write;
        point.cost_usd += cost;

        durations.push(record.duration_ms);

        for (map, key) in [
            (&mut by_alias, record.alias.clone()),
            (&mut by_provider, record.provider.clone()),
            (&mut by_status, record.status.clone()),
            (&mut by_surface, record.surface.clone()),
            // The token's `sub` — an `api_tokens` row id, not its secret, which
            // this process could not produce if it wanted to. A row written
            // before a token could be attributed carries `""`, and it is grouped
            // under that rather than dropped: an unattributable request still
            // spent money, and hiding it would make the breakdown's total
            // disagree with the window's.
            (&mut by_token, record.token_sub.clone()),
        ] {
            let group = map.entry(key.clone()).or_insert_with(|| UsageGroup {
                key,
                label: None,
                requests: 0,
                prompt_tokens: 0,
                completion_tokens: 0,
                cost_usd: 0.0,
            });
            group.requests += 1;
            group.prompt_tokens += record.observed.prompt;
            group.completion_tokens += record.observed.completion;
            group.cost_usd += cost;
        }
    }

    totals.unpriced_models = unpriced_models.into_iter().collect();

    // Dense rather than sparse: a chart with gaps where nothing happened is a
    // chart that misrepresents a quiet day as a missing one.
    let mut dense = Vec::new();
    walk_buckets(params.from, params.to, granularity, params.loc, |key, _| {
        dense.push(series.get(key).cloned().unwrap_or_else(|| UsagePoint {
            date: key.to_string(),
            ..Default::default()
        }));
    });

    // One read for the whole breakdown, and only when there is something to
    // label — a window with no traffic must not open a connection for nothing.
    if !by_token.is_empty() {
        if let Ok(conn) = super::db::open_read_only(db_path) {
            let names = token_names(&conn);
            for (id, group) in by_token.iter_mut() {
                group.label = names.get(id).cloned();
            }
        }
    }

    durations.sort_unstable();
    let latency = UsageLatency {
        p50_ms: percentile(&durations, 50),
        p95_ms: percentile(&durations, 95),
        max_ms: durations.last().copied().unwrap_or(0).max(0) as u64,
    };

    let body = UsageBody {
        granularity: granularity.as_str(),
        totals,
        latency,
        series: dense,
        by_alias: by_alias.into_values().collect(),
        by_provider: by_provider.into_values().collect(),
        by_status: by_status.into_values().collect(),
        by_surface: by_surface.into_values().collect(),
        by_token: by_token.into_values().collect(),
    };
    let encoded =
        super::gojson::to_vec(&body).map_err(|e| format!("encoding gateway usage: {e}"))?;
    Ok(Answer::json(encoded))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::NamedTempFile;

    fn migrated() -> NamedTempFile {
        let file = NamedTempFile::new().expect("tempfile");
        let mut conn = super::super::db::ensure_database(file.path()).expect("create");
        super::super::migrate::apply(&mut conn).expect("migrate");
        file
    }

    fn ctx(file: &NamedTempFile) -> Ctx {
        Ctx {
            db_path: file.path().to_path_buf(),
        }
    }

    fn call(ctx: &Ctx, method: &str, path: &str, body: &str) -> Result<Answer, String> {
        let method = Method::from_bytes(method.as_bytes()).expect("method");
        serve(
            ctx,
            &Request {
                method: &method,
                path,
                query: "",
                content_type: "application/json",
                secret_token: "",
                body: body.as_bytes(),
            },
        )
    }

    fn call_query(ctx: &Ctx, path: &str, query: &str) -> Result<Answer, String> {
        serve(
            ctx,
            &Request {
                method: &Method::GET,
                path,
                query,
                content_type: "",
                secret_token: "",
                body: b"",
            },
        )
    }

    fn body_of(answer: &Answer) -> String {
        String::from_utf8(answer.body.clone().unwrap_or_default()).expect("utf-8")
    }

    /// Read the stored key straight out of SQLite, bypassing every projection.
    ///
    /// The point of the tests below is what the *column* holds, and every
    /// public reader here is deliberately incapable of showing it.
    fn stored_key(file: &NamedTempFile, id: &str) -> String {
        let conn = super::super::db::open_read_only(file.path()).expect("open");
        conn.query_row(
            "SELECT api_key FROM gateway_providers WHERE id = ?1",
            [id],
            |r| r.get(0),
        )
        .expect("row")
    }

    fn create_a_provider(ctx: &Ctx) -> String {
        let answer = call(
            ctx,
            "POST",
            "/api/gateway/providers",
            r#"{"id":"p1","name":"openai","type":"openai","api_key":"sk-secret-value",
                "base_url":"https://api.openai.com/v1","enabled":true}"#,
        )
        .expect("created");
        assert_eq!(answer.status, StatusCode::CREATED);
        body_of(&answer)
    }

    // ── The credential rules ──────────────────────────────────────────────

    /// **The single highest-value guard on this surface.**
    ///
    /// `PUT /api/integrations/{id}` wipes credentials the caller omits while
    /// `GET` scrubs them, so a read-then-write round trip — which is exactly
    /// what an edit form does — destroys the stored secret. That is a real
    /// data-loss bug this repository carries deliberately because it was Go's
    /// behaviour, and reproducing it here would be inheriting a bug rather than
    /// a decision.
    ///
    /// The round trip is driven through the **real** `GET` body, not a
    /// hand-written one: the property is that what a client reads back is safe
    /// to send, and a fixture that happened to omit the field would prove
    /// nothing about what the client actually has.
    #[test]
    fn a_scrubbed_read_written_straight_back_preserves_the_stored_key() {
        let file = migrated();
        let ctx = ctx(&file);
        create_a_provider(&ctx);
        assert_eq!(stored_key(&file, "p1"), "sk-secret-value");

        let listed = body_of(&call(&ctx, "GET", "/api/gateway/providers", "").expect("read"));
        let providers: serde_json::Value = serde_json::from_str(&listed).expect("json");
        let mut provider = providers[0].clone();
        assert!(
            provider.get("api_key").is_none(),
            "the read must not carry the key at all: {provider}"
        );
        assert_eq!(provider["has_api_key"], true);

        // What an edit form does: change a field, send the whole object back.
        provider["enabled"] = serde_json::json!(false);
        let answer = call(
            &ctx,
            "PUT",
            "/api/gateway/providers/p1",
            &provider.to_string(),
        )
        .expect("updated");
        assert_eq!(answer.status, StatusCode::OK);

        assert_eq!(
            stored_key(&file, "p1"),
            "sk-secret-value",
            "an omitted api_key must preserve the stored one — this is the \
             PUT /api/integrations/{{id}} data-loss bug, and it must not be \
             reproduced here"
        );
    }

    /// The other two arms of the three-valued field, so "preserve" is not
    /// achieved by ignoring the field entirely.
    #[test]
    fn an_explicit_key_replaces_and_an_explicit_empty_string_clears() {
        let file = migrated();
        let ctx = ctx(&file);
        create_a_provider(&ctx);

        call(
            &ctx,
            "PUT",
            "/api/gateway/providers/p1",
            r#"{"name":"openai","type":"openai","api_key":"sk-rotated","base_url":"","enabled":true}"#,
        )
        .expect("updated");
        assert_eq!(stored_key(&file, "p1"), "sk-rotated");

        call(
            &ctx,
            "PUT",
            "/api/gateway/providers/p1",
            r#"{"name":"openai","type":"openai","api_key":"","base_url":"","enabled":true}"#,
        )
        .expect("updated");
        assert_eq!(
            stored_key(&file, "p1"),
            "",
            "an explicit empty string is a deliberate clear, not an omission"
        );
    }

    /// No response on any route carries the key — asserted over the **bytes**,
    /// not by inspecting a struct.
    ///
    /// A struct-level check proves only that the field this test knows about is
    /// absent. The failure worth catching is a *new* field, or a `Debug` of a
    /// row, carrying it somewhere nobody thought to look.
    #[test]
    fn no_response_body_on_any_route_contains_the_api_key() {
        const SECRET: &str = "sk-secret-value";
        let file = migrated();
        let ctx = ctx(&file);
        let created = create_a_provider(&ctx);
        call(
            &ctx,
            "POST",
            "/api/gateway/models",
            r#"{"id":"a1","alias":"my-alias","routing":{"targets":[{"provider":"openai","model_id":"gpt-4o"}]},"enabled":true}"#,
        )
        .expect("alias");

        let mut bodies = vec![created];
        for path in [
            "/api/gateway/settings",
            "/api/gateway/providers",
            "/api/gateway/models",
            "/api/gateway/status",
            "/api/gateway/usage",
        ] {
            bodies.push(body_of(&call(&ctx, "GET", path, "").expect(path)));
        }
        bodies.push(body_of(
            &call(
                &ctx,
                "PUT",
                "/api/gateway/providers/p1",
                r#"{"name":"openai","type":"openai","base_url":"","enabled":true}"#,
            )
            .expect("update"),
        ));

        for body in &bodies {
            assert!(
                !body.contains(SECRET),
                "a response carried the stored API key: {body}"
            );
        }
        // ...and the fixture really did store one, so the loop above is not
        // passing against a database with no secret in it.
        assert_eq!(stored_key(&file, "p1"), SECRET);
    }

    // ── Routing ───────────────────────────────────────────────────────────

    #[test]
    fn the_claim_patterns_match_what_they_should_and_nothing_else() {
        let get = Method::GET;
        let put = Method::PUT;
        for path in [
            "/api/gateway/settings",
            "/api/gateway/providers",
            "/api/gateway/models",
            "/api/gateway/status",
            "/api/gateway/usage",
        ] {
            assert!(claims(&get, path), "should claim GET {path}");
        }
        assert!(claims(&put, "/api/gateway/providers/abc"));
        assert!(claims(&put, "/api/gateway/models/abc"));

        // A trailing slash routes to nothing in chi, and so here.
        assert!(!claims(&put, "/api/gateway/providers/"));
        assert!(!claims(&get, "/api/gateway/settings/"));
        // A `{id}` is exactly one segment.
        assert!(!claims(&put, "/api/gateway/providers/a/b"));
        // Wrong method.
        assert!(!claims(&put, "/api/gateway/status"));
        assert!(!claims(&get, "/api/gateway/providers/abc"));
        // Neighbours.
        assert!(!claims(&get, "/api/gateway"));
        assert!(!claims(&get, "/api/gateways/settings"));
    }

    // ── Validation ────────────────────────────────────────────────────────

    /// A port the process cannot bind is refused **before** anything is stored,
    /// and the previous state survives.
    #[test]
    fn an_invalid_port_is_refused_before_it_is_stored() {
        let file = migrated();
        let ctx = ctx(&file);
        let before = config::load_settings(file.path()).expect("settings");

        for port in ["0", "80", "1023"] {
            let body = format!(r#"{{"enabled":true,"port":{port},"start_with_app":true}}"#);
            let answer = call(&ctx, "PUT", "/api/gateway/settings", &body).expect("answered");
            assert_eq!(
                answer.status,
                StatusCode::UNPROCESSABLE_ENTITY,
                "port {port} should be refused: {}",
                body_of(&answer)
            );
        }

        assert_eq!(
            config::load_settings(file.path()).expect("settings"),
            before,
            "a refused write must not have mutated anything"
        );
    }

    #[test]
    fn a_provider_needs_a_name_and_a_type_this_build_serves() {
        let file = migrated();
        let ctx = ctx(&file);
        for (body, why) in [
            (r#"{"name":"  ","type":"openai"}"#, "blank name"),
            (r#"{"name":"x","type":"bedrock"}"#, "unservable type"),
            (r#"{"name":"x","type":""}"#, "absent type"),
        ] {
            let answer = call(&ctx, "POST", "/api/gateway/providers", body).expect("answered");
            assert_eq!(
                answer.status,
                StatusCode::UNPROCESSABLE_ENTITY,
                "{why} should be refused: {}",
                body_of(&answer)
            );
        }
        // `bedrock` is refused because this build compiles no adapter for it —
        // its AWS SDK needs a Rust newer than this crate's floor — so storing
        // one would produce a gateway that cannot start.
        assert!(config::load_provider_summaries(file.path())
            .expect("providers")
            .is_empty());
    }

    /// A JSON array body is refused by `writes::decode_body`, on every write.
    ///
    /// Without that shape check `serde` builds a struct from an array
    /// positionally, so `["My Provider"]` would create one.
    #[test]
    fn an_array_body_is_refused_on_every_write() {
        let file = migrated();
        let ctx = ctx(&file);
        for (method, path) in [
            ("PUT", "/api/gateway/settings"),
            ("POST", "/api/gateway/providers"),
            ("POST", "/api/gateway/models"),
        ] {
            let answer = call(&ctx, method, path, r#"["My Provider"]"#).expect("answered");
            assert_eq!(
                answer.status,
                StatusCode::BAD_REQUEST,
                "{method} {path} accepted an array body"
            );
        }
    }

    // ── Referential integrity, which no foreign key can give ───────────────

    /// Deleting a provider an alias still routes to is a 409 naming the alias.
    ///
    /// Routing stores provider **names** inside a JSON column, so SQLite cannot
    /// enforce this. Deleting anyway would leave the alias resolving to nothing
    /// and failing at request time — far from the action that caused it.
    #[test]
    fn deleting_a_referenced_provider_is_refused_and_names_the_alias() {
        let file = migrated();
        let ctx = ctx(&file);
        create_a_provider(&ctx);
        call(
            &ctx,
            "POST",
            "/api/gateway/models",
            r#"{"id":"a1","alias":"my-alias","routing":{"targets":[{"provider":"openai","model_id":"gpt-4o"}]},"enabled":true}"#,
        )
        .expect("alias");

        let answer = call(&ctx, "DELETE", "/api/gateway/providers/p1", "").expect("answered");
        assert_eq!(answer.status, StatusCode::CONFLICT);
        assert!(
            body_of(&answer).contains("my-alias"),
            "the refusal must name what still refers to it: {}",
            body_of(&answer)
        );
        assert_eq!(
            config::load_provider_summaries(file.path())
                .expect("providers")
                .len(),
            1,
            "and the provider must still be there"
        );

        // Remove the alias and the delete goes through.
        call(&ctx, "DELETE", "/api/gateway/models/a1", "").expect("alias deleted");
        let answer = call(&ctx, "DELETE", "/api/gateway/providers/p1", "").expect("answered");
        assert_eq!(answer.status, StatusCode::NO_CONTENT);
    }

    /// The same rule from the other side: an alias naming no configured
    /// provider is refused rather than stored to fail at request time.
    #[test]
    fn an_alias_target_must_name_a_configured_provider() {
        let file = migrated();
        let ctx = ctx(&file);
        let answer = call(
            &ctx,
            "POST",
            "/api/gateway/models",
            r#"{"alias":"a","routing":{"targets":[{"provider":"nope","model_id":"m"}]},"enabled":true}"#,
        )
        .expect("answered");
        assert_eq!(answer.status, StatusCode::UNPROCESSABLE_ENTITY);
        assert!(config::load_aliases(file.path())
            .expect("aliases")
            .is_empty());
    }

    #[test]
    fn an_alias_needs_at_least_one_target() {
        let file = migrated();
        let ctx = ctx(&file);
        let answer = call(
            &ctx,
            "POST",
            "/api/gateway/models",
            r#"{"alias":"a","routing":{"targets":[]},"enabled":true}"#,
        )
        .expect("answered");
        assert_eq!(answer.status, StatusCode::UNPROCESSABLE_ENTITY);
    }

    /// `store_alias` upserts, so a `PUT` on an unknown id would silently create
    /// where the caller asked to update.
    #[test]
    fn updating_an_unknown_row_is_a_404_rather_than_a_silent_create() {
        let file = migrated();
        let ctx = ctx(&file);
        create_a_provider(&ctx);

        let answer = call(
            &ctx,
            "PUT",
            "/api/gateway/models/nope",
            r#"{"alias":"a","routing":{"targets":[{"provider":"openai","model_id":"m"}]},"enabled":true}"#,
        )
        .expect("answered");
        assert_eq!(answer.status, StatusCode::NOT_FOUND);
        assert!(config::load_aliases(file.path())
            .expect("aliases")
            .is_empty());

        let answer = call(
            &ctx,
            "PUT",
            "/api/gateway/providers/nope",
            r#"{"name":"x","type":"openai","base_url":"","enabled":true}"#,
        )
        .expect("answered");
        assert_eq!(answer.status, StatusCode::NOT_FOUND);
    }

    /// A `POST` naming an id that exists must not overwrite it.
    ///
    /// `store_provider` upserts, so without this check a create would replace
    /// another provider's key and base URL while answering `201 Created` —
    /// a silent credential swap dressed as a creation.
    #[test]
    fn creating_over_an_existing_id_is_refused_rather_than_overwriting() {
        let file = migrated();
        let ctx = ctx(&file);
        create_a_provider(&ctx);

        let answer = call(
            &ctx,
            "POST",
            "/api/gateway/providers",
            r#"{"id":"p1","name":"different","type":"gemini","api_key":"sk-attacker","base_url":"","enabled":true}"#,
        )
        .expect("answered");
        assert_eq!(answer.status, StatusCode::CONFLICT);
        assert_eq!(
            stored_key(&file, "p1"),
            "sk-secret-value",
            "the refused create must not have touched the stored key"
        );
        assert_eq!(
            config::load_provider_summaries(file.path()).expect("providers")[0].name,
            "openai"
        );
    }

    /// Two providers may not share a **name**, because routing refers to them
    /// by name — an alias pointing at a duplicate resolves to whichever the
    /// registry happened to keep.
    #[test]
    fn two_providers_may_not_share_a_name() {
        let file = migrated();
        let ctx = ctx(&file);
        create_a_provider(&ctx);

        let answer = call(
            &ctx,
            "POST",
            "/api/gateway/providers",
            r#"{"name":"openai","type":"gemini","api_key":"k","base_url":"","enabled":true}"#,
        )
        .expect("answered");
        assert_eq!(answer.status, StatusCode::CONFLICT);
        assert_eq!(
            config::load_provider_summaries(file.path())
                .expect("providers")
                .len(),
            1
        );
    }

    /// Two aliases may not share an alias string.
    ///
    /// The alias is the routing key a client sends as `model`, and the
    /// dispatcher collects them into a map keyed by it — so a duplicate
    /// silently disables one of the two rows, with nothing to say which.
    #[test]
    fn two_rows_may_not_share_an_alias() {
        let file = migrated();
        let ctx = ctx(&file);
        create_a_provider(&ctx);
        let alias = r#"{"alias":"my-alias","routing":{"targets":[{"provider":"openai","model_id":"m"}]},"enabled":true}"#;
        call(&ctx, "POST", "/api/gateway/models", alias).expect("first");

        let answer = call(&ctx, "POST", "/api/gateway/models", alias).expect("answered");
        assert_eq!(answer.status, StatusCode::CONFLICT);
        assert_eq!(config::load_aliases(file.path()).expect("aliases").len(), 1);
    }

    /// ...but an update must not collide with **itself**, which is what the
    /// `own_id` exception is for — otherwise saving an alias without renaming
    /// it would be a 409.
    #[test]
    fn updating_an_alias_without_renaming_it_is_not_a_self_collision() {
        let file = migrated();
        let ctx = ctx(&file);
        create_a_provider(&ctx);
        call(
            &ctx,
            "POST",
            "/api/gateway/models",
            r#"{"id":"a1","alias":"my-alias","routing":{"targets":[{"provider":"openai","model_id":"m"}]},"enabled":true}"#,
        )
        .expect("created");

        let answer = call(
            &ctx,
            "PUT",
            "/api/gateway/models/a1",
            r#"{"alias":"my-alias","routing":{"targets":[{"provider":"openai","model_id":"m2"}]},"enabled":false}"#,
        )
        .expect("answered");
        assert_eq!(answer.status, StatusCode::OK, "{}", body_of(&answer));
        let aliases = config::load_aliases(file.path()).expect("aliases");
        assert_eq!(aliases.len(), 1);
        assert_eq!(aliases[0].routing.targets[0].model_id, "m2");
    }

    #[test]
    fn deleting_an_unknown_row_is_a_404() {
        let file = migrated();
        let ctx = ctx(&file);
        for path in ["/api/gateway/providers/nope", "/api/gateway/models/nope"] {
            let answer = call(&ctx, "DELETE", path, "").expect("answered");
            assert_eq!(answer.status, StatusCode::NOT_FOUND, "{path}");
        }
    }

    // ── Status and usage ──────────────────────────────────────────────────

    /// Every `Status` maps to a distinct wire state, and only the two that have
    /// something to say carry an `error`.
    #[test]
    fn the_status_route_distinguishes_all_four_states() {
        let file = migrated();
        let ctx = ctx(&file);
        // A fresh process has started nothing.
        let body: serde_json::Value = serde_json::from_str(&body_of(
            &call(&ctx, "GET", "/api/gateway/status", "").expect("status"),
        ))
        .expect("json");
        assert_eq!(body["state"], "stopped");
        assert!(
            body.get("error").is_none(),
            "a stopped gateway has nothing to explain"
        );
        assert!(body.get("port").is_none());

        // The mapping itself, without touching the process-wide registry — that
        // is a shared global and installing a state here would break other
        // tests in this binary.
        for (status, state, has_port, has_error) in [
            (registry::Status::Stopped, "stopped", false, false),
            (
                registry::Status::Running { port: 8880 },
                "running",
                true,
                false,
            ),
            (
                registry::Status::BindFailed {
                    port: 8880,
                    error: "address already in use".into(),
                },
                "bind_failed",
                true,
                true,
            ),
            (
                registry::Status::StartFailed {
                    error: "provider 'p1' is misconfigured".into(),
                },
                "start_failed",
                false,
                true,
            ),
        ] {
            let body = status_body(status);
            assert_eq!(body.state, state);
            assert_eq!(body.port.is_some(), has_port, "{state} port");
            assert_eq!(body.error.is_some(), has_error, "{state} error");
        }
    }

    /// An empty log still answers a dense series rather than nothing.
    #[test]
    fn the_usage_route_answers_a_dense_series_over_an_empty_log() {
        let file = migrated();
        let ctx = ctx(&file);
        let answer = call_query(
            &ctx,
            "/api/gateway/usage",
            "from=2026-08-01&to=2026-08-07&tz=Europe/Berlin",
        )
        .expect("usage");
        let body: serde_json::Value = serde_json::from_str(&body_of(&answer)).expect("json");

        assert_eq!(body["totals"]["requests"], 0);
        assert_eq!(body["totals"]["cost_usd"], 0.0);
        let series = body["series"].as_array().expect("series");
        assert!(
            series.len() >= 7,
            "a quiet week is seven zero buckets, not an empty array: {}",
            body["series"]
        );
        assert!(series.iter().all(|p| p["requests"] == 0));

        // Zero traffic must produce zeros, not `null`s: `serde_json` renders a
        // NaN as `null`, so an unguarded `part / total` reaches the dashboard as
        // a missing number rather than as an error. #428's whole first
        // acceptance criterion is that this window renders.
        for key in ["p50_ms", "p95_ms", "max_ms"] {
            assert_eq!(
                body["latency"][key], 0,
                "latency.{key} over an empty window"
            );
        }
        for key in [
            "by_alias",
            "by_provider",
            "by_status",
            "by_surface",
            "by_token",
        ] {
            assert_eq!(
                body[key].as_array().expect(key).len(),
                0,
                "{key} over an empty window"
            );
        }
    }

    /// One hand-written usage row.
    ///
    /// A named struct rather than a tuple because the fixture below is a table
    /// of sixteen columns, and a positional row of that width is one transposed
    /// pair away from a test that asserts the wrong thing while still passing.
    struct Fixture {
        id: &'static str,
        /// `gotime::go_text`'s rendering — what the column actually holds.
        at: &'static str,
        alias: &'static str,
        provider: &'static str,
        surface: &'static str,
        token: &'static str,
        status: &'static str,
        prompt: i64,
        completion: i64,
        cache_read: i64,
        cache_write: i64,
        ms: i64,
        cost: Option<f64>,
        unpriced: bool,
        model: &'static str,
    }

    /// Insert one usage row directly, so the fixture below is a table of numbers
    /// rather than a simulated gateway run.
    fn insert_fixture(file: &NamedTempFile, row: &Fixture) {
        let Fixture {
            id,
            at,
            alias,
            provider,
            surface,
            token: token_sub,
            status,
            prompt,
            completion,
            cache_read,
            cache_write,
            ms: duration_ms,
            cost: cost_usd,
            unpriced,
            model: model_id,
        } = row;
        let conn = super::super::db::open_read_write(file.path()).expect("open");
        conn.execute(
            "INSERT INTO gateway_usage_log (id, created_at, alias, provider, model_id, \
             prompt_tokens, completion_tokens, cache_read_tokens, cache_write_tokens, \
             duration_ms, status, streamed, surface, token_sub, cost_usd, unpriced) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, 0, ?12, ?13, ?14, ?15)",
            rusqlite::params![
                id,
                at,
                alias,
                provider,
                model_id,
                prompt,
                completion,
                cache_read,
                cache_write,
                duration_ms,
                status,
                surface,
                token_sub,
                cost_usd,
                *unpriced as i64,
            ],
        )
        .expect("insert usage row");
    }

    /// **Every aggregate against a hand-counted fixture** — #428's second
    /// acceptance criterion, and the one that is arithmetic rather than shape.
    ///
    /// Five rows, chosen so no two aggregates share an answer: a test where the
    /// expected numbers coincide cannot tell a correct sum from a mislabelled
    /// one. The timestamps are `gotime::go_text`'s rendering, which is what the
    /// column holds and what the window compares against.
    #[test]
    fn every_usage_aggregate_matches_a_hand_counted_fixture() {
        let file = migrated();
        let ctx = ctx(&file);

        // id      alias  provider  surface     token  status          p    c   cr  cw   ms   cost
        // r1      fast   openai    openai      t-a    ok             10    1    2    3  100  0.10
        // r2      fast   openai    anthropic   t-a    ok             20    2    0    0  200  0.20
        // r3      fast   azure     openai      t-b    upstream_error 40    4    0    0  900  0.40
        // r4      slow   openai    openai      t-b    interrupted     80    8    0    0  400  NULL (unpriced)
        // r5      slow   azure     anthropic   t-b    refused        160   16    0    0  300  0.05
        #[rustfmt::skip]
        let rows = &[
            Fixture { id: "r1", at: "2026-08-03 09:00:00 +0000 UTC", alias: "fast", provider: "openai", surface: "openai",    token: "t-a", status: "ok",             prompt:  10, completion:  1, cache_read: 2, cache_write: 3, ms: 100, cost: Some(0.10), unpriced: false, model: "gpt-x" },
            Fixture { id: "r2", at: "2026-08-03 10:00:00 +0000 UTC", alias: "fast", provider: "openai", surface: "anthropic", token: "t-a", status: "ok",             prompt:  20, completion:  2, cache_read: 0, cache_write: 0, ms: 200, cost: Some(0.20), unpriced: false, model: "gpt-x" },
            Fixture { id: "r3", at: "2026-08-04 09:00:00 +0000 UTC", alias: "fast", provider: "azure",  surface: "openai",    token: "t-b", status: "upstream_error", prompt:  40, completion:  4, cache_read: 0, cache_write: 0, ms: 900, cost: Some(0.40), unpriced: false, model: "gpt-x" },
            Fixture { id: "r4", at: "2026-08-04 10:00:00 +0000 UTC", alias: "slow", provider: "openai", surface: "openai",    token: "t-b", status: "interrupted",    prompt:  80, completion:  8, cache_read: 0, cache_write: 0, ms: 400, cost: None,       unpriced: true,  model: "mystery-model" },
            Fixture { id: "r5", at: "2026-08-05 09:00:00 +0000 UTC", alias: "slow", provider: "azure",  surface: "anthropic", token: "t-b", status: "refused",        prompt: 160, completion: 16, cache_read: 0, cache_write: 0, ms: 300, cost: Some(0.05), unpriced: false, model: "gpt-x" },
        ];
        for row in rows {
            insert_fixture(&file, row);
        }

        let answer = call_query(
            &ctx,
            "/api/gateway/usage",
            "from=2026-08-01T00:00:00Z&to=2026-08-15T00:00:00Z&tz=UTC",
        )
        .expect("usage");
        let body: serde_json::Value = serde_json::from_str(&body_of(&answer)).expect("json");

        // Totals: 10+20+40+80+160 = 310 prompt, 1+2+4+8+16 = 31 completion.
        let totals = &body["totals"];
        assert_eq!(totals["requests"], 5);
        assert_eq!(totals["prompt_tokens"], 310);
        assert_eq!(totals["completion_tokens"], 31);
        assert_eq!(totals["cache_read_tokens"], 2);
        assert_eq!(totals["cache_write_tokens"], 3);
        // 0.10 + 0.20 + 0.40 + 0.05, with r4 contributing nothing because it is
        // unpriced — disclosed beside the total, never folded in as 0.0.
        assert!(
            (totals["cost_usd"].as_f64().expect("cost") - 0.75).abs() < 1e-9,
            "cost total: {}",
            totals["cost_usd"]
        );
        assert_eq!(totals["unpriced_requests"], 1);
        assert_eq!(
            totals["unpriced_models"],
            serde_json::json!(["mystery-model"])
        );

        // Latency over [100, 200, 300, 400, 900]: nearest-rank p50 is the
        // ceil(0.5*5) = 3rd value, p95 the ceil(0.95*5) = 5th.
        assert_eq!(body["latency"]["p50_ms"], 300);
        assert_eq!(body["latency"]["p95_ms"], 900);
        assert_eq!(body["latency"]["max_ms"], 900);

        // Each breakdown, keyed and summed. `by_status` keeps `interrupted`
        // apart from `upstream_error` — a closed tab must never read as a
        // provider outage, which is why #425 stored them separately.
        let group = |key: &str, want_key: &str| -> serde_json::Value {
            body[key]
                .as_array()
                .expect(key)
                .iter()
                .find(|g| g["key"] == want_key)
                .unwrap_or_else(|| panic!("{key} has no {want_key}: {}", body[key]))
                .clone()
        };

        assert_eq!(group("by_alias", "fast")["requests"], 3);
        assert_eq!(group("by_alias", "fast")["prompt_tokens"], 70);
        assert_eq!(group("by_alias", "slow")["requests"], 2);
        assert_eq!(group("by_alias", "slow")["prompt_tokens"], 240);

        assert_eq!(group("by_provider", "openai")["requests"], 3);
        assert_eq!(group("by_provider", "openai")["prompt_tokens"], 110);
        assert_eq!(group("by_provider", "azure")["requests"], 2);
        assert_eq!(group("by_provider", "azure")["prompt_tokens"], 200);

        assert_eq!(group("by_status", "ok")["requests"], 2);
        assert_eq!(group("by_status", "upstream_error")["requests"], 1);
        assert_eq!(group("by_status", "interrupted")["requests"], 1);
        assert_eq!(group("by_status", "refused")["requests"], 1);

        assert_eq!(group("by_surface", "openai")["requests"], 3);
        assert_eq!(group("by_surface", "anthropic")["requests"], 2);
        assert_eq!(group("by_surface", "anthropic")["prompt_tokens"], 180);

        assert_eq!(group("by_token", "t-a")["requests"], 2);
        assert_eq!(group("by_token", "t-a")["prompt_tokens"], 30);
        assert_eq!(group("by_token", "t-b")["requests"], 3);
        assert_eq!(group("by_token", "t-b")["prompt_tokens"], 280);

        // The series is bucketed daily in the request's zone: three active days
        // inside a dense fourteen-day window. Fourteen and not seven because
        // `AnalyticsParams::granularity` picks **hourly** for a span of seven
        // days or less, which is a shared rule with the Claude dashboard rather
        // than anything this endpoint chose — asserting day keys over a
        // seven-day window tests the wrong thing.
        let series = body["series"].as_array().expect("series");
        let active: Vec<&serde_json::Value> =
            series.iter().filter(|p| p["requests"] != 0).collect();
        assert_eq!(
            active.len(),
            3,
            "three days saw traffic: {}",
            body["series"]
        );
        assert_eq!(active[0]["date"], "2026-08-03");
        assert_eq!(active[0]["requests"], 2);
        assert_eq!(active[0]["prompt_tokens"], 30);
        assert_eq!(active[1]["date"], "2026-08-04");
        assert_eq!(active[1]["requests"], 2);
        assert_eq!(active[2]["date"], "2026-08-05");
        assert_eq!(active[2]["requests"], 1);
    }

    /// The per-token breakdown names the credential, not its row id.
    ///
    /// The id appears nowhere else in the product — the Security tab lists
    /// tokens by name — so a panel that printed one could not answer the
    /// question it exists for, which is whether a given credential is worth
    /// revoking. A **revoked** token keeps its name for the same reason: its
    /// spending history is most of what made the revocation an informed
    /// decision.
    #[test]
    fn the_per_token_breakdown_names_the_token_rather_than_its_row_id() {
        let file = migrated();
        let ctx = ctx(&file);
        {
            let conn = super::super::db::open_read_write(file.path()).expect("open");
            conn.execute(
                "INSERT INTO api_tokens (id, name, scope, jti, created_at) \
                 VALUES ('tok-live', 'Zed', 'llm', 'j1', '2026-08-01 00:00:00 +0000 UTC')",
                [],
            )
            .expect("live token");
            conn.execute(
                "INSERT INTO api_tokens (id, name, scope, jti, created_at, revoked_at) \
                 VALUES ('tok-dead', 'Old laptop', 'llm', 'j2', \
                         '2026-08-01 00:00:00 +0000 UTC', '2026-08-02 00:00:00 +0000 UTC')",
                [],
            )
            .expect("revoked token");
        }

        let row = |id: &'static str, token: &'static str| Fixture {
            id,
            at: "2026-08-03 09:00:00 +0000 UTC",
            alias: "a",
            provider: "p",
            surface: "openai",
            token,
            status: "ok",
            prompt: 1,
            completion: 1,
            cache_read: 0,
            cache_write: 0,
            ms: 10,
            cost: Some(0.01),
            unpriced: false,
            model: "m",
        };
        insert_fixture(&file, &row("u1", "tok-live"));
        insert_fixture(&file, &row("u2", "tok-dead"));
        // A token row that no longer exists, and a request no token could be
        // attributed to. Neither may vanish from the breakdown: both spent money,
        // and dropping them would make it disagree with the window's total.
        insert_fixture(&file, &row("u3", "tok-deleted"));
        insert_fixture(&file, &row("u4", ""));

        let answer = call_query(
            &ctx,
            "/api/gateway/usage",
            "from=2026-08-01T00:00:00Z&to=2026-08-15T00:00:00Z&tz=UTC",
        )
        .expect("usage");
        let body: serde_json::Value = serde_json::from_str(&body_of(&answer)).expect("json");
        let groups = body["by_token"].as_array().expect("by_token");
        assert_eq!(
            groups.len(),
            4,
            "every payer is listed: {}",
            body["by_token"]
        );

        let labelled = |key: &str| -> Option<String> {
            groups
                .iter()
                .find(|g| g["key"] == key)
                .and_then(|g| g["label"].as_str())
                .map(str::to_string)
        };
        assert_eq!(labelled("tok-live").as_deref(), Some("Zed"));
        assert_eq!(labelled("tok-dead").as_deref(), Some("Old laptop"));
        // Absent, not null — the view falls back to the key, and every other
        // breakdown's rows carry no `label` key at all.
        assert_eq!(labelled("tok-deleted"), None);
        assert_eq!(labelled(""), None);
        for key in ["by_alias", "by_provider", "by_status", "by_surface"] {
            for group in body[key].as_array().expect(key) {
                assert!(
                    group.get("label").is_none(),
                    "{key} must not carry a label: {group}"
                );
            }
        }
    }

    /// The bucket boundary is the *request's* timezone, not UTC.
    ///
    /// A row at 23:30 UTC is the next day in Berlin, so the same fixture read
    /// twice must move a request from one bucket to another. Sending no `tz`
    /// silently falls the whole dashboard back to UTC, and this is what makes
    /// that visible rather than a plausible-looking wrong chart.
    #[test]
    fn a_bucket_belongs_to_the_day_it_was_in_the_requests_timezone() {
        let file = migrated();
        let ctx = ctx(&file);
        insert_fixture(
            &file,
            &Fixture {
                id: "late",
                at: "2026-08-03 23:30:00 +0000 UTC",
                alias: "a",
                provider: "p",
                surface: "openai",
                token: "t",
                status: "ok",
                prompt: 1,
                completion: 1,
                cache_read: 0,
                cache_write: 0,
                ms: 10,
                cost: Some(0.01),
                unpriced: false,
                model: "m",
            },
        );

        let day_of = |tz: &str| -> String {
            let answer = call_query(
                &ctx,
                "/api/gateway/usage",
                &format!("from=2026-08-01T00:00:00Z&to=2026-08-15T00:00:00Z&tz={tz}"),
            )
            .expect("usage");
            let body: serde_json::Value = serde_json::from_str(&body_of(&answer)).expect("json");
            body["series"]
                .as_array()
                .expect("series")
                .iter()
                .find(|p| p["requests"] != 0)
                .expect("one active bucket")["date"]
                .as_str()
                .expect("date")
                .to_string()
        };

        assert_eq!(day_of("UTC"), "2026-08-03");
        assert_eq!(day_of("Europe/Berlin"), "2026-08-04");
    }

    /// The horizon is refused before it is stored, and the refusal names the
    /// field that failed rather than always naming `port`.
    #[test]
    fn an_absurd_retention_horizon_is_refused_before_it_is_stored() {
        let file = migrated();
        let ctx = ctx(&file);
        let err = call(
            &ctx,
            "PUT",
            "/api/gateway/settings",
            r#"{"enabled":true,"port":9099,"start_with_app":true,"usage_retention_days":99999}"#,
        )
        .expect("answered");
        assert_eq!(err.status, StatusCode::UNPROCESSABLE_ENTITY);
        let body = body_of(&err);
        // The **whole** `validation error for "<field>"` prefix, not a bare
        // `contains("usage_retention_days")`: the message itself names the field
        // too, so the loose assertion passed with the reported field hardcoded
        // to `port` — i.e. it could not fail on the revert it was written for.
        assert!(
            body.contains(r#"validation error for \"usage_retention_days\""#),
            "the refusal must name the field that failed: {body}"
        );

        // Nothing stored, so the read still answers the default.
        let read = body_of(&call(&ctx, "GET", "/api/gateway/settings", "").expect("read"));
        let settings: serde_json::Value = serde_json::from_str(&read).expect("json");
        assert_eq!(settings["usage_retention_days"], 90);
        assert_eq!(settings["enabled"], false);
    }

    /// A read written straight back preserves the horizon.
    ///
    /// The same round trip `a_scrubbed_read_written_straight_back_preserves_the_stored_key`
    /// drives for the API key, for the same reason: an edit form reads, mutates
    /// one field and writes the whole body back, and a field the read omits is a
    /// field the write destroys.
    #[test]
    fn a_settings_read_written_straight_back_preserves_the_retention_horizon() {
        let file = migrated();
        let ctx = ctx(&file);
        call(
            &ctx,
            "PUT",
            "/api/gateway/settings",
            r#"{"enabled":true,"port":9099,"start_with_app":true,"usage_retention_days":7}"#,
        )
        .expect("stored");

        let read = body_of(&call(&ctx, "GET", "/api/gateway/settings", "").expect("read"));
        let mut settings: serde_json::Value = serde_json::from_str(&read).expect("json");
        assert_eq!(settings["usage_retention_days"], 7);

        settings["port"] = serde_json::json!(9100);
        call(
            &ctx,
            "PUT",
            "/api/gateway/settings",
            &serde_json::to_string(&settings).expect("encode"),
        )
        .expect("stored again");

        let back = body_of(&call(&ctx, "GET", "/api/gateway/settings", "").expect("read"));
        let settings: serde_json::Value = serde_json::from_str(&back).expect("json");
        assert_eq!(settings["usage_retention_days"], 7);
        assert_eq!(settings["port"], 9100);
    }

    // ── Scope, and the log lines ──────────────────────────────────────────

    /// A `read` token opens every GET and no write; `llm` opens nothing.
    ///
    /// The `llm` half is the disjointness #423 built, checked from this
    /// surface's side: a credential pasted into a tool's config to spend
    /// provider credits must not also be able to *reconfigure* which provider
    /// the credits are spent with — which is a strictly larger power than the
    /// one it was issued for.
    #[test]
    fn scoping_is_the_ordinary_read_write_split_and_llm_opens_nothing() {
        use crate::native::security::required_scope;
        use crate::native::security::token::Scope;

        for (method, route) in ROUTES {
            let method = Method::from_bytes(method.as_bytes()).expect("method");
            let concrete = route.replace("{id}", "sample");
            let want = required_scope(&method, &concrete);

            let expected = if crate::guards::is_state_changing(&method) {
                Scope::Write
            } else {
                Scope::Read
            };
            assert_eq!(
                want, expected,
                "{method} {route} — gateway control is the ordinary split, not \
                 `/api/security/*`'s write-everything exception"
            );

            // A `read` token covers exactly the reads.
            assert_eq!(
                Scope::Read.covers(want),
                !crate::guards::is_state_changing(&method),
                "{method} {route} against a read token"
            );
            // A `write` token covers everything here.
            assert!(Scope::Write.covers(want), "{method} {route}");
            // And `llm` covers none of it, in either direction.
            assert!(
                !Scope::Llm.covers(want),
                "an llm token must not reconfigure the gateway it spends through: {method} {route}"
            );
        }
    }

    /// Every write emits a `#335`-convention line, after the effect, at info.
    ///
    /// A line with no test is a line that quietly stops being emitted, which
    /// for this half is the whole failure mode — nothing else notices.
    #[test]
    fn every_write_logs_what_it_did() {
        use crate::native::writes::testlog;
        testlog::install();

        let file = migrated();
        let ctx = ctx(&file);

        create_a_provider(&ctx);
        testlog::assert_info_present("gateway provider created id=\"p1\" name=\"openai\"");

        call(
            &ctx,
            "PUT",
            "/api/gateway/providers/p1",
            r#"{"name":"openai","type":"openai","base_url":"","enabled":true}"#,
        )
        .expect("updated");
        testlog::assert_info_present("gateway provider updated id=\"p1\"");

        call(
            &ctx,
            "POST",
            "/api/gateway/models",
            r#"{"id":"a1","alias":"my-alias","routing":{"targets":[{"provider":"openai","model_id":"m"}]},"enabled":true}"#,
        )
        .expect("alias");
        testlog::assert_info_present("gateway model alias created id=\"a1\" alias=\"my-alias\"");

        call(
            &ctx,
            "PUT",
            "/api/gateway/models/a1",
            r#"{"alias":"renamed","routing":{"targets":[{"provider":"openai","model_id":"m"}]},"enabled":true}"#,
        )
        .expect("updated");
        testlog::assert_info_present("gateway model alias updated id=\"a1\" alias=\"renamed\"");

        call(&ctx, "DELETE", "/api/gateway/models/a1", "").expect("deleted");
        testlog::assert_info_present("gateway model alias deleted id=\"a1\"");

        call(&ctx, "DELETE", "/api/gateway/providers/p1", "").expect("deleted");
        testlog::assert_info_present("gateway provider deleted id=\"p1\"");

        call(
            &ctx,
            "PUT",
            "/api/gateway/settings",
            r#"{"enabled":true,"port":8880,"start_with_app":true}"#,
        )
        .expect("settings");
        testlog::assert_info_present("gateway settings saved enabled=true port=8880");
    }

    /// **No log line carries an API key**, on any write.
    ///
    /// The provider name is logged deliberately — user-authored text, on the
    /// terms `integrations created … name=` already established, because a line
    /// that cannot say which provider was created is most of what it is for.
    /// The key is a different thing entirely, and the log is a plain file.
    #[test]
    fn no_log_line_carries_the_api_key() {
        use crate::native::writes::testlog;
        testlog::install();

        let file = migrated();
        let ctx = ctx(&file);
        call(
            &ctx,
            "POST",
            "/api/gateway/providers",
            r#"{"id":"logtest","name":"logtest-provider","type":"openai","api_key":"sk-must-not-be-logged","base_url":"","enabled":true}"#,
        )
        .expect("created");

        assert!(
            testlog::matching("sk-must-not-be-logged").is_empty(),
            "a write logged the API key"
        );
        // ...and the write really happened, so the assertion above is not
        // passing because nothing was logged at all.
        testlog::assert_info_present("gateway provider created id=\"logtest\"");
    }

    /// An unknown timezone is refused rather than silently bucketed in UTC.
    ///
    /// Same rule the Claude analytics window enforces: quietly answering in the
    /// wrong zone is a *wrong* figure rather than a missing one, and every
    /// number on the dashboard would be shifted with nothing to say so.
    #[test]
    fn an_unknown_timezone_is_refused_rather_than_silently_utc() {
        let file = migrated();
        let ctx = ctx(&file);
        assert!(call_query(&ctx, "/api/gateway/usage", "tz=Mars/Olympus").is_err());
    }
}
