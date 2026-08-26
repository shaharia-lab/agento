//! Asking one provider's own upstream which models it serves (#470), and — since
//! #472 — whether a credential the user has just typed is one it accepts.
//!
//! # One dialect table, two callers
//!
//! `GET /api/gateway/providers/{id}/models` asks a **stored** row; `POST
//! /api/gateway/providers/validate` asks an **unsaved** draft. They are the same
//! request to the same endpoint with the same authentication, differing only in
//! where the three inputs came from — so [`fetch`] takes those three rather than
//! a [`ProviderRow`], and there is exactly one [`Shape::of`] in the tree.
//!
//! That is deliberate and it is what #472's spec asked to be done differently:
//! it predates #470 and describes a second per-type table in
//! `gateway/validate.rs`. Two tables of provider endpoints is two things to keep
//! in step with four upstreams, and the failure when they drift is silent — one
//! surface would validate a credential against an endpoint the other never
//! calls.
//!
//! # Why this is new code rather than something reused
//!
//! Ferrox's `GET /v1/models` and `GET /anthropic/v1/models` list **ferrox's own
//! configured aliases** — they are the client-facing discovery endpoint, i.e.
//! the same thing `GET /api/gateway/models` already is here — and
//! `ferrox-providers` exposes no model-listing capability at all, its adapters
//! being completions-only. So neither can be lifted, and the per-provider
//! catalog fetch is Agento's own.
//!
//! # What leaves this module
//!
//! **Model ids, and nothing else.** Not the key, not the resolved URL, not the
//! upstream body. Every failure is a [`CatalogError`] whose `message` names a
//! *cause* — a transport failure, an upstream status, a shape that did not
//! parse — and interpolates neither the credential nor the endpoint, because
//! the whole thing is rendered into a form the user is looking at and a base URL
//! can carry a token in its path.
//!
//! # The base URL is model input reaching a URL
//!
//! `gateway_providers.base_url` is typed by a person, so it is the same class
//! `native/integrations/base_url.rs` exists for: `url::Url::parse` — which
//! `reqwest` builds every request through — applies WHATWG dot-segment removal,
//! so a stored base of `https://api.openai.com/v1/..` would send the key
//! somewhere the user did not configure. [`Base`] is that guard, and it is
//! reused rather than re-derived; see its header for why comparing two parsers
//! of the same string is not enough on its own.
//!
//! One adaptation: [`Base::new`] takes the base with trailing slashes already
//! trimmed by whatever the caller's Go trimmed them with, because it
//! *concatenates*. There is no Go here, and a user who stores
//! `https://api.openai.com/v1/` means the same endpoint as one who does not — so
//! this trims, the way `confluence::validate_site_url` does, rather than sending
//! the `//models` that faithful concatenation would produce.
//!
//! # Where each path comes from
//!
//! Not from this issue's sketch but from what the *completions* call already
//! does with the same column, because a stored base has to work for both:
//!
//! | type | ferrox appends | so the base is | and the catalog is |
//! |---|---|---|---|
//! | `openai`, `glm` | `/chat/completions` | the **versioned** root | `/models` |
//! | `anthropic` | `/v1/messages` | the **host** root | `/v1/models` |
//! | `gemini` | `/v1beta/models/…` | the **host** root | `/v1beta/models` |
//!
//! `glm` shares OpenAI's defaults because it *is* the OpenAI adapter with a
//! custom base (`config::ProviderType::Glm`), and a GLM row that has not set one
//! is misconfigured for completions in exactly the same way.

use std::time::Duration;

use serde::Deserialize;

use crate::gateway::config::ProviderType;
use crate::native::integrations::base_url::Base;

/// How long one catalog fetch may take, end to end.
///
/// Short, and deliberately shorter than any integration client's: this is on a
/// UI interaction — a user picked a provider in a form — not on a turn. A slow
/// upstream must degrade to the free-text field quickly rather than leave a
/// combobox spinning.
const TIMEOUT: Duration = Duration::from_secs(10);

/// The most catalog JSON this will read before giving up.
///
/// The largest real answer here is a few tens of kilobytes; the cap is the same
/// belt-and-braces every integration client wears, so a misconfigured base
/// pointing at something enormous cannot buffer it into this process.
const MAX_BYTES: usize = 2 * 1024 * 1024;

/// The one client every catalog fetch goes through.
///
/// **Built once**, like `github::client`'s and for the same two reasons: a
/// `reqwest::Client` owns a connection pool, and `build()` reads the *platform*
/// trust store — so constructing one per request throws the pool away and
/// re-parses every root certificate on the machine on each keystroke-adjacent
/// interaction. It answers `Option` rather than panicking because that store can
/// be unusable, which `build()` reports as a **builder** error reachable with no
/// network at all.
///
/// **Redirects are refused, and that is a credential decision rather than a
/// preference.** `base_url` is typed by a person and [`Base`] checks where the
/// *first* request goes; a redirect moves it afterwards, which is precisely the
/// hole that guard cannot see. `reqwest` strips `Authorization` on a cross-origin
/// redirect — and strips nothing else, so a `302` would carry `x-api-key` and
/// `x-goog-api-key` to whatever host answered it. A list endpoint has no
/// legitimate need to redirect, so a `3xx` falls through to the non-success arm
/// and is reported as the status it is, which is both safe and actionable.
fn client() -> Option<&'static reqwest::Client> {
    static CLIENT: std::sync::OnceLock<Option<reqwest::Client>> = std::sync::OnceLock::new();
    CLIENT
        .get_or_init(|| {
            reqwest::Client::builder()
                .timeout(TIMEOUT)
                .redirect(reqwest::redirect::Policy::none())
                .build()
                .map_err(|e| log::warn!("gateway model catalog: no usable HTTP client: {e}"))
                .ok()
        })
        .as_ref()
}

/// Why a catalog could not be produced.
///
/// `message` is what the UI renders, so it names a cause and never a value:
/// no key, no URL, no upstream body.
///
/// **Three variants rather than two, because #472 has to tell them apart.** The
/// catalog route maps all three onto two statuses and could not care; a
/// validation verdict is precisely the question "which of these happened" — a
/// refused credential, a base URL nothing answers on, and an upstream that
/// answered something unexpected are three different things for a user to fix.
/// Splitting the old `Upstream` in two costs the catalog route nothing: its
/// mapping is unchanged, because [`Self::Unreachable`] carries the message that
/// arm already produced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CatalogError {
    /// The provider is configured in a way that makes the question unaskable —
    /// no API key, or a `base_url` this build will not send a request through.
    /// `400`, because nothing was asked and retrying will not change it.
    Unaskable(String),
    /// The request could not be delivered at all: DNS, TLS, connect, or a body
    /// that stopped arriving. Nobody answered, so there is no status to report.
    Unreachable(String),
    /// The provider answered, and the answer was not a catalog — either because
    /// of its `status`, or because a `200` carried something else. `502`, since
    /// this build did its part and somebody else's did not.
    Upstream {
        message: String,
        /// The upstream's status, when there was one. `None` means a `200`
        /// whose body did not parse.
        status: Option<u16>,
    },
}

impl CatalogError {
    pub fn message(&self) -> &str {
        match self {
            Self::Unaskable(m) | Self::Unreachable(m) => m,
            Self::Upstream { message, .. } => message,
        }
    }

    /// A `200` that did not carry a catalog.
    fn unparseable(message: &str) -> Self {
        Self::Upstream {
            message: message.to_string(),
            status: None,
        }
    }
}

/// Every model id this provider's upstream reports, in the order it gave them.
///
/// Order is the upstream's own — OpenAI's is by creation, Anthropic's is newest
/// first — and re-sorting it would bury the model a user most likely wants under
/// an alphabetical accident. Duplicates are dropped, which costs nothing and
/// stops a paginated answer that overlaps from listing an id twice.
///
/// The three arguments are the three things a request needs, rather than a
/// `ProviderRow`, because #472's caller has no row: it validates a draft the
/// user is still typing. `key` is borrowed and never stored — the same
/// discipline `ProviderRow`'s private field enforces on the other side of the
/// call.
pub async fn fetch(
    provider_type: ProviderType,
    base_url: &str,
    key: &str,
) -> Result<Vec<String>, CatalogError> {
    if key.is_empty() {
        return Err(CatalogError::Unaskable(
            "this provider has no API key, so its model list cannot be fetched".into(),
        ));
    }

    let shape = Shape::of(provider_type);
    // Trailing slashes trimmed before `Base::new`, which concatenates: see the
    // module header.
    let raw = base_url.trim_end_matches('/');
    let raw = if raw.is_empty() {
        shape.default_base
    } else {
        raw
    };
    let base = Base::new(raw).map_err(|_| {
        CatalogError::Unaskable(
            "this provider's base URL is not one Agento can send a request to".into(),
        )
    })?;
    let url = base.resolve(shape.path).ok_or_else(|| {
        CatalogError::Unaskable(
            "this provider's base URL is not one Agento can send a request to".into(),
        )
    })?;

    let client = client().ok_or_else(|| {
        CatalogError::Unaskable("no usable TLS trust store on this machine".into())
    })?;

    let request = match shape.auth {
        Auth::Bearer => client.get(url).bearer_auth(key),
        Auth::AnthropicKey => client
            .get(url)
            .header("x-api-key", key)
            .header("anthropic-version", "2023-06-01"),
        // The header form rather than `?key=`, so the credential is never in a
        // URL — which is what an error string, a redirect and a proxy log all
        // see.
        Auth::GoogleKey => client.get(url).header("x-goog-api-key", key),
    };

    let response = request
        .send()
        .await
        // Deliberately not `{e}`: a `reqwest::Error`'s `Display` carries the URL
        // it was built from, which is the one thing this must not hand back.
        .map_err(|_| CatalogError::Unreachable("the provider could not be reached".into()))?;

    let status = response.status();
    if !status.is_success() {
        // The status is the useful half and leaks nothing — `401` says "the key
        // is wrong" far better than any wording here could. The body is not
        // read: it is the upstream's, and echoing it is what this module exists
        // not to do.
        return Err(CatalogError::Upstream {
            message: format!("the provider answered {}", status.as_u16()),
            status: Some(status.as_u16()),
        });
    }

    let body = read_capped(response).await?;
    let ids = shape.parse(&body)?;

    let mut seen = std::collections::HashSet::new();
    Ok(ids
        .into_iter()
        .filter(|id| !id.is_empty() && seen.insert(id.clone()))
        .collect())
}

/// Read at most [`MAX_BYTES`], failing rather than truncating.
///
/// Truncation would be worse than an error here: a JSON document cut in half
/// does not parse, so the only outcome it can produce is the same failure with a
/// less accurate cause.
async fn read_capped(response: reqwest::Response) -> Result<Vec<u8>, CatalogError> {
    use tokio_stream::StreamExt;

    let mut stream = Box::pin(response.bytes_stream());
    let mut buf: Vec<u8> = Vec::new();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|_| {
            CatalogError::Unreachable("the provider's answer could not be read".into())
        })?;
        if buf.len() + chunk.len() > MAX_BYTES {
            return Err(CatalogError::unparseable(
                "the provider's model list is larger than Agento will read",
            ));
        }
        buf.extend_from_slice(&chunk);
    }
    Ok(buf)
}

/// Which authentication one provider type's list endpoint takes.
#[derive(Clone, Copy)]
enum Auth {
    /// `Authorization: Bearer <key>` — OpenAI and every compatible surface.
    Bearer,
    /// `x-api-key` plus the pinned `anthropic-version`.
    AnthropicKey,
    /// `x-goog-api-key`.
    GoogleKey,
}

/// Everything that differs between the four providers' list endpoints.
struct Shape {
    default_base: &'static str,
    path: &'static str,
    auth: Auth,
    body: BodyShape,
}

/// Which JSON shape the ids arrive in.
#[derive(Clone, Copy, PartialEq, Eq)]
enum BodyShape {
    /// `{"data":[{"id":"…"}]}` — OpenAI, GLM and Anthropic all answer this.
    DataId,
    /// `{"models":[{"name":"models/…"}]}` — Gemini, whose `name` is a resource
    /// path rather than the id a request carries.
    ModelsName,
}

impl Shape {
    fn of(provider_type: ProviderType) -> Self {
        match provider_type {
            // Same defaults for both, because `Glm` *is* the OpenAI adapter with
            // a custom base — see the module header.
            ProviderType::Openai | ProviderType::Glm => Self {
                default_base: "https://api.openai.com/v1",
                path: "/models",
                auth: Auth::Bearer,
                body: BodyShape::DataId,
            },
            // The two paginated endpoints are asked for their whole catalog in
            // one request. Anthropic's defaults to 20 per page and Gemini's to
            // 50, both capping at 1000 — which covers every real catalog many
            // times over and keeps the handler's cost a fixed single round
            // trip, where a follow-the-cursor loop would put an unbounded number
            // of upstream calls behind one click. OpenAI's is not paginated and
            // takes no such parameter.
            ProviderType::Anthropic => Self {
                default_base: "https://api.anthropic.com",
                path: "/v1/models?limit=1000",
                auth: Auth::AnthropicKey,
                body: BodyShape::DataId,
            },
            ProviderType::Gemini => Self {
                default_base: "https://generativelanguage.googleapis.com",
                path: "/v1beta/models?pageSize=1000",
                auth: Auth::GoogleKey,
                body: BodyShape::ModelsName,
            },
        }
    }

    fn parse(&self, body: &[u8]) -> Result<Vec<String>, CatalogError> {
        let unparseable = || {
            CatalogError::unparseable(
                "the provider's answer was not a model list Agento understands",
            )
        };
        match self.body {
            BodyShape::DataId => {
                let parsed: DataIdBody = serde_json::from_slice(body).map_err(|_| unparseable())?;
                Ok(parsed.data.into_iter().map(|m| m.id).collect())
            }
            BodyShape::ModelsName => {
                let parsed: ModelsNameBody =
                    serde_json::from_slice(body).map_err(|_| unparseable())?;
                Ok(parsed
                    .models
                    .into_iter()
                    // `models/gemini-2.5-pro` is a resource path; the id a
                    // request carries is the last segment, and storing the
                    // prefix would produce an alias that 404s upstream — the
                    // exact failure this issue exists to remove.
                    .map(|m| m.name.rsplit('/').next().unwrap_or_default().to_string())
                    .collect())
            }
        }
    }
}

/// Unknown fields are ignored on purpose: a catalog carries pricing, context
/// windows and capability flags this route deliberately does not answer with,
/// and a provider adding one must not turn the list into an error.
#[derive(Deserialize)]
struct DataIdBody {
    #[serde(default)]
    data: Vec<DataIdEntry>,
}

#[derive(Deserialize)]
struct DataIdEntry {
    #[serde(default)]
    id: String,
}

#[derive(Deserialize)]
struct ModelsNameBody {
    #[serde(default)]
    models: Vec<ModelsNameEntry>,
}

#[derive(Deserialize)]
struct ModelsNameEntry {
    #[serde(default)]
    name: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn body_of(shape: BodyShape, raw: &str) -> Result<Vec<String>, CatalogError> {
        Shape {
            default_base: "",
            path: "",
            auth: Auth::Bearer,
            body: shape,
        }
        .parse(raw.as_bytes())
    }

    #[test]
    fn an_openai_shaped_catalog_yields_its_ids_in_order() {
        let ids = body_of(
            BodyShape::DataId,
            r#"{"object":"list","data":[{"id":"gpt-4o-2024-11-20","object":"model"},
                {"id":"gpt-4o-mini","created":1}]}"#,
        )
        .expect("parses");
        assert_eq!(ids, ["gpt-4o-2024-11-20", "gpt-4o-mini"]);
    }

    /// Gemini names a model by its **resource path**. Keeping the prefix would
    /// store `models/gemini-2.5-pro` as an alias target, which is precisely the
    /// upstream-404-at-request-time this feature exists to prevent.
    #[test]
    fn a_gemini_name_is_reduced_to_the_id_a_request_carries() {
        let ids = body_of(
            BodyShape::ModelsName,
            r#"{"models":[{"name":"models/gemini-2.5-pro","displayName":"Gemini"},
                {"name":"models/gemini-2.5-flash"}]}"#,
        )
        .expect("parses");
        assert_eq!(ids, ["gemini-2.5-pro", "gemini-2.5-flash"]);
    }

    /// A catalog carries pricing and capability metadata this route does not
    /// answer with; a provider adding a field must not turn the list into an
    /// error.
    #[test]
    fn unknown_fields_and_a_missing_list_are_both_tolerated() {
        assert_eq!(
            body_of(BodyShape::DataId, r#"{"data":[{"id":"a","nonesuch":{}}]}"#).unwrap(),
            ["a"]
        );
        assert_eq!(
            body_of(BodyShape::DataId, r#"{"object":"list"}"#).unwrap(),
            Vec::<String>::new()
        );
    }

    #[test]
    fn a_body_that_is_not_a_catalog_is_an_upstream_error_naming_no_value() {
        let err = body_of(BodyShape::DataId, "<html>nope</html>").unwrap_err();
        // A `200` whose body is not a catalog: the provider answered, so it is
        // `Upstream` — with **no** status, which is what tells #472's verdict
        // that there is nothing about the status code to report.
        assert!(matches!(err, CatalogError::Upstream { status: None, .. }));
        assert!(!err.message().contains("html"), "{}", err.message());
    }

    /// The three bases each provider type falls back to must be ones the guard
    /// itself accepts — a default that `Base::new` refuses would make every
    /// row without an explicit `base_url` unaskable, and nothing else would say
    /// so.
    #[test]
    fn every_default_base_survives_the_base_guard() {
        for provider_type in [
            ProviderType::Openai,
            ProviderType::Glm,
            ProviderType::Anthropic,
            ProviderType::Gemini,
        ] {
            let shape = Shape::of(provider_type);
            let base = Base::new(shape.default_base)
                .unwrap_or_else(|e| panic!("{} base refused: {e:?}", provider_type.as_str()));
            assert!(
                base.resolve(shape.path).is_some(),
                "{} path {} did not resolve",
                provider_type.as_str(),
                shape.path,
            );
        }
    }

    /// The guard's whole point, at this call site: a stored base carrying a dot
    /// segment resolves somewhere other than it reads, so no request is built
    /// at all rather than one carrying the API key there.
    ///
    /// It is refused by `Base::new` — `url` removes the dot segment while
    /// nothing else does, so the rendered path differs from the raw text and
    /// that *is* `Mismatch::PathEncoding` — which is why `fetch` maps both
    /// `new` and `resolve` to the same `Unaskable`: which of the two notices is
    /// an implementation detail of the guard, and a caller that only handled
    /// `resolve` would let this through the day the other stopped catching it.
    #[test]
    fn a_base_with_a_dot_segment_is_refused_rather_than_followed() {
        assert!(Base::new("https://api.openai.com/v1/..").is_err());
    }
}
