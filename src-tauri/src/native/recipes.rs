//! Task recipes (#698): a task exported as a small, versioned JSON file that
//! carries no secrets and no machine-local ids, and imported back as a new
//! task that is always `paused`.
//!
//! `GET /api/tasks/{id}/recipe` answers [`export`]'s recipe and
//! `POST /api/tasks/import` runs [`import`]; both are routed by `tasks.rs`.
//! The file I/O behind the native Save-As and Open dialogs is two Tauri
//! commands in `lib.rs` (`export_task_recipe`, `read_recipe_file`), the
//! `export_session` precedent: the webview cannot write a path it picked.
//!
//! Rules, each enforced in this file:
//!
//! - **The recipe is an allow-list, not a filtered task.** [`Recipe`] names
//!   every key it carries, so a column added to `scheduled_tasks` later is
//!   absent from every recipe until someone adds it here on purpose. Left out:
//!   `id`, `status`, the run and event counters, every timestamp,
//!   `stop_after_count`, `stop_after_time`, `working_directory` and
//!   `settings_profile_id` (machine-local; a home path names the user), every
//!   `integration_id`, Slack `channel_ids`, Telegram `chat_ids`, email
//!   `recipients`, and on a trigger rule `filter_chat_ids`, `filter_user_ids`
//!   and the rule's own execution settings. Webhook hook tokens are never read.
//!   Pinned by `an_export_carries_no_secret_from_any_place_one_can_sit`.
//! - **`prompt` and `description` are masked** with
//!   `security_scan::scan::mask_text`, so a key pasted into a prompt is not
//!   copied into a file that gets shared. Its false negatives are the
//!   Credentials Checker's.
//! - **`destinations` and `triggers` are descriptive.** They say what kind of
//!   wiring the task had, nothing that identifies it. Only rules on a Slack or
//!   Telegram integration are listed: no other integration type dispatches an
//!   inbound message.
//! - **Import creates the task, not its wiring.** The task is `paused`
//!   whatever the source was, so it cannot fire before a human has reviewed it
//!   and resumed it, and no timer is registered for it. `reply` destinations
//!   are kept; Slack, Telegram and email destinations and every trigger are
//!   not created, because each needs a local integration, recipients or a
//!   sender allow-list (deny-by-default, #688/#690). The UI lists them as "to
//!   connect".
//! - **Strict, and strictness is a 422.** A missing or unsupported
//!   `agento_recipe`, or an unknown key at the top level or inside a
//!   `destinations[i]`/`triggers[i]` entry, is a 422 naming the field and
//!   writes nothing — so `status`, `id` and `integration_id` are refused, and a
//!   later version cannot be half-read by this one. A body that is not JSON or
//!   not an object stays the 400 every other write answers.

use std::path::Path;

use serde::{Deserialize, Serialize};

use super::gojson::{GoList, GoStruct};
use super::security_scan::scan::mask_text;
use super::tasks::{ScheduleConfig, ScheduledTask, TaskBuiltin, TaskDestination};
use super::writes::{decode_body, WriteError};

/// The only recipe version this build reads and writes.
pub const RECIPE_VERSION: i64 = 1;

/// The integration types whose trigger rules a recipe describes, and the only
/// `triggers[i].type` values an import accepts.
const TRIGGER_TYPES: [&str; 2] = ["slack", "telegram"];

/// One recipe, version 1. **Field order is the wire order.**
#[derive(Debug, Clone, Serialize)]
pub struct Recipe {
    pub agento_recipe: i64,
    pub name: String,
    pub description: String,
    pub prompt: String,
    pub agent_slug: String,
    pub model: String,
    pub timeout_minutes: i64,
    pub schedule_type: String,
    pub schedule_config: ScheduleConfig,
    pub save_output: bool,
    pub continue_on_reply: bool,
    pub max_concurrent_runs: i64,
    pub max_queued_events: i64,
    pub max_runs_per_hour: i64,
    pub permission_mode: String,
    /// The task's built-in recipe (#699), carried whole: it names a report,
    /// not anything machine-local. Omitted when absent, so a recipe of a task
    /// that runs an agent keeps its bytes.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub builtin: Option<TaskBuiltin>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub destinations: Vec<RecipeDestination>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub triggers: Vec<RecipeTrigger>,
}

/// What kind of destination the task delivered to, and when. Nothing else: the
/// type's sub-object holds the integration and its targets.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RecipeDestination {
    #[serde(
        rename = "type",
        default,
        deserialize_with = "super::gojson::null_is_zero_value"
    )]
    pub r#type: String,
    #[serde(default, deserialize_with = "super::gojson::null_is_zero_value")]
    pub when: String,
}

/// What kind of trigger rule started the task, and its message filters. The
/// sender allow-lists and the integration are not here.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RecipeTrigger {
    #[serde(
        rename = "type",
        default,
        deserialize_with = "super::gojson::null_is_zero_value"
    )]
    pub r#type: String,
    #[serde(default, deserialize_with = "super::gojson::null_is_zero_value")]
    pub filter_prefix: String,
    #[serde(default, deserialize_with = "super::gojson::null_is_zero_value")]
    pub filter_keywords: GoList<String>,
}

/// The keys a version-1 recipe may carry, at each of its three levels.
const RECIPE_KEYS: [&str; 18] = [
    "agento_recipe",
    "name",
    "description",
    "prompt",
    "agent_slug",
    "model",
    "timeout_minutes",
    "schedule_type",
    "schedule_config",
    "save_output",
    "continue_on_reply",
    "max_concurrent_runs",
    "max_queued_events",
    "max_runs_per_hour",
    "permission_mode",
    "builtin",
    "destinations",
    "triggers",
];
const DESTINATION_KEYS: [&str; 2] = ["type", "when"];
const TRIGGER_KEYS: [&str; 3] = ["type", "filter_prefix", "filter_keywords"];

/// A recipe as an import decodes it, from the request's original bytes after
/// [`check_keys`] has passed. Every scalar is a zero value when `null`, and
/// the containers are `GoList`/`GoStruct` types (`docs/internal/native.md`).
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct RecipeImport {
    #[serde(deserialize_with = "super::gojson::null_is_zero_value")]
    name: String,
    #[serde(deserialize_with = "super::gojson::null_is_zero_value")]
    description: String,
    #[serde(deserialize_with = "super::gojson::null_is_zero_value")]
    prompt: String,
    #[serde(deserialize_with = "super::gojson::null_is_zero_value")]
    agent_slug: String,
    #[serde(deserialize_with = "super::gojson::null_is_zero_value")]
    model: String,
    #[serde(deserialize_with = "super::gojson::null_is_zero_value")]
    timeout_minutes: i64,
    #[serde(deserialize_with = "super::gojson::null_is_zero_value")]
    schedule_type: String,
    #[serde(deserialize_with = "super::gojson::null_is_zero_value")]
    schedule_config: GoStruct<ScheduleConfig>,
    #[serde(deserialize_with = "super::gojson::null_is_zero_value")]
    save_output: bool,
    #[serde(deserialize_with = "super::gojson::null_is_zero_value")]
    continue_on_reply: bool,
    #[serde(deserialize_with = "super::gojson::null_is_zero_value")]
    max_concurrent_runs: i64,
    #[serde(deserialize_with = "super::gojson::null_is_zero_value")]
    max_queued_events: i64,
    #[serde(deserialize_with = "super::gojson::null_is_zero_value")]
    max_runs_per_hour: i64,
    #[serde(deserialize_with = "super::gojson::null_is_zero_value")]
    permission_mode: String,
    builtin: Option<GoStruct<TaskBuiltin>>,
    destinations: Option<GoList<GoStruct<RecipeDestination>>>,
    triggers: Option<GoList<GoStruct<RecipeTrigger>>>,
}

/// The task `task_id` as a recipe, or `None` when no such task exists.
pub fn export(db_path: &Path, task_id: &str) -> Result<Option<Recipe>, String> {
    let Some(task) = super::tasks::get_task(db_path, task_id)? else {
        return Ok(None);
    };
    let triggers = linked_triggers(db_path, task_id)?;
    Ok(Some(Recipe {
        agento_recipe: RECIPE_VERSION,
        name: task.name,
        description: mask_text(&task.description),
        prompt: mask_text(&task.prompt),
        agent_slug: task.agent_slug,
        model: task.model,
        timeout_minutes: task.timeout_minutes,
        schedule_type: task.schedule_type,
        schedule_config: task.schedule_config,
        save_output: task.save_output,
        continue_on_reply: task.continue_on_reply,
        max_concurrent_runs: task.max_concurrent_runs,
        max_queued_events: task.max_queued_events,
        max_runs_per_hour: task.max_runs_per_hour,
        permission_mode: task.permission_mode,
        builtin: task.builtin,
        destinations: task
            .destinations
            .into_iter()
            .map(|d| RecipeDestination {
                r#type: d.r#type,
                when: d.when,
            })
            .collect(),
        triggers,
    }))
}

/// The recipe's file bytes: `gojson`'s encoding, as every response here is.
pub fn encode(recipe: &Recipe) -> Result<Vec<u8>, String> {
    super::gojson::to_vec(recipe).map_err(|e| format!("encoding recipe: {e}"))
}

/// `GET /api/tasks/{id}/recipe`: the recipe, or a 404 naming the task.
pub fn answer(db_path: &Path, task_id: &str) -> Result<super::Answer, WriteError> {
    match export(db_path, task_id).map_err(WriteError::Fallback)? {
        Some(recipe) => Ok(super::Answer::json(
            encode(&recipe).map_err(WriteError::Fallback)?,
        )),
        None => Err(WriteError::NotFound {
            resource: "task".to_string(),
            id: task_id.to_string(),
        }),
    }
}

/// The trigger rules linked to the task, oldest first, on a Slack or Telegram
/// integration — as descriptions only.
fn linked_triggers(db_path: &Path, task_id: &str) -> Result<Vec<RecipeTrigger>, String> {
    let conn = super::db::open_read_only(db_path)?;
    let mut stmt = conn
        .prepare(
            "SELECT i.type, r.filter_prefix, r.filter_keywords
             FROM trigger_rules r JOIN integrations i ON i.id = r.integration_id
             WHERE r.task_id = ?1 AND i.type IN ('slack', 'telegram')
             ORDER BY r.created_at ASC, r.id ASC",
        )
        .map_err(|e| format!("preparing linked trigger rules: {e}"))?;
    let rows = stmt
        .query_map([task_id], |row| {
            let keywords: String = row.get(2)?;
            Ok(RecipeTrigger {
                r#type: row.get(0)?,
                filter_prefix: row.get(1)?,
                filter_keywords: GoList(
                    super::gojson::decode_string_list(&keywords).unwrap_or_default(),
                ),
            })
        })
        .map_err(|e| format!("listing linked trigger rules: {e}"))?;
    rows.collect::<Result<_, _>>()
        .map_err(|e| format!("scanning linked trigger rule: {e}"))
}

/// `POST /api/tasks/import`: one new `paused` task from a recipe, answered
/// `201` with the task in `POST /api/tasks`'s encoding.
pub fn import(db_path: &Path, body: &[u8]) -> Result<super::Answer, WriteError> {
    let keys = decode_body::<serde_json::Map<String, serde_json::Value>>(body)?;
    check_version(&keys)?;
    check_keys(&keys)?;

    let recipe = decode_body::<RecipeImport>(body)?;
    let destinations = recipe
        .destinations
        .map(|list| list.0.into_iter().map(|entry| entry.0).collect())
        .unwrap_or_default();
    let triggers: Vec<RecipeTrigger> = recipe
        .triggers
        .map(|list| list.0.into_iter().map(|entry| entry.0).collect())
        .unwrap_or_default();
    let kept = check_destinations(destinations)?;
    check_triggers(&triggers)?;

    let mut task = ScheduledTask {
        id: String::new(),
        name: recipe.name,
        description: recipe.description,
        prompt: recipe.prompt,
        agent_slug: recipe.agent_slug,
        working_directory: String::new(),
        model: recipe.model,
        settings_profile_id: String::new(),
        timeout_minutes: recipe.timeout_minutes,
        schedule_type: recipe.schedule_type,
        schedule_config: recipe.schedule_config.0,
        stop_after_count: 0,
        stop_after_time: None,
        save_output: recipe.save_output,
        destinations: kept,
        builtin: recipe.builtin.map(|builtin| builtin.0),
        continue_on_reply: recipe.continue_on_reply,
        max_concurrent_runs: recipe.max_concurrent_runs,
        max_queued_events: recipe.max_queued_events,
        max_runs_per_hour: recipe.max_runs_per_hour,
        permission_mode: recipe.permission_mode,
        dropped_event_count: 0,
        rate_limited_event_count: 0,
        status: String::new(),
        run_count: 0,
        last_run_at: None,
        last_run_status: String::new(),
        next_run_at: None,
        created_at: Default::default(),
        updated_at: Default::default(),
    };
    super::tasks::validate_task(&mut task)?;
    // Whatever the source task was: the recipe has no `status` key, and this
    // is what keeps an imported task from firing before anyone has read it.
    task.status = "paused".to_string();
    super::tasks::insert_new_task(db_path, task)
}

/// `agento_recipe` must be present and be the number `1` — not `"1"`, not
/// `1.0`, not a later version this build would half-read.
fn check_version(keys: &serde_json::Map<String, serde_json::Value>) -> Result<(), WriteError> {
    let version = keys.get("agento_recipe");
    if version.is_some_and(|v| v.is_i64() && v.as_i64() == Some(RECIPE_VERSION)) {
        return Ok(());
    }
    Err(WriteError::validation(
        "agento_recipe",
        format!("unsupported recipe version; this Agento reads version {RECIPE_VERSION}"),
    ))
}

/// The first key that is not part of version 1, at the top level or inside a
/// `destinations[i]`/`triggers[i]` object, is a 422 naming it. Entries that
/// are not objects are left to the typed decode.
fn check_keys(keys: &serde_json::Map<String, serde_json::Value>) -> Result<(), WriteError> {
    for key in keys.keys() {
        if !RECIPE_KEYS.contains(&key.as_str()) {
            return Err(WriteError::validation(key, "unknown field"));
        }
    }
    for (list, allowed) in [
        ("destinations", &DESTINATION_KEYS[..]),
        ("triggers", &TRIGGER_KEYS[..]),
    ] {
        let Some(serde_json::Value::Array(entries)) = keys.get(list) else {
            continue;
        };
        for (i, entry) in entries.iter().enumerate() {
            let serde_json::Value::Object(entry) = entry else {
                continue;
            };
            for key in entry.keys() {
                if !allowed.contains(&key.as_str()) {
                    return Err(WriteError::validation(
                        &format!("{list}[{i}].{key}"),
                        "unknown field",
                    ));
                }
            }
        }
    }
    Ok(())
}

/// Validate every descriptive destination, so a bad entry is a 422 rather than
/// one the UI silently ignores, and answer the ones import creates: `reply`
/// alone, since it has nothing to configure.
fn check_destinations(
    destinations: Vec<RecipeDestination>,
) -> Result<Vec<TaskDestination>, WriteError> {
    let mut kept = Vec::new();
    for (i, dest) in destinations.into_iter().enumerate() {
        let field = |name: &str| format!("destinations[{i}].{name}");
        if !matches!(
            dest.r#type.as_str(),
            "slack" | "telegram" | "email" | "reply"
        ) {
            return Err(WriteError::validation(
                &field("type"),
                "type must be slack, telegram, email or reply",
            ));
        }
        if !matches!(dest.when.as_str(), "" | "success" | "always") {
            return Err(WriteError::validation(
                &field("when"),
                "when must be success or always",
            ));
        }
        if dest.r#type == "reply" {
            if !kept.is_empty() {
                return Err(WriteError::validation(
                    &field("type"),
                    "only one reply destination is allowed",
                ));
            }
            kept.push(TaskDestination {
                r#type: dest.r#type,
                when: dest.when,
                ..Default::default()
            });
        }
    }
    Ok(kept)
}

fn check_triggers(triggers: &[RecipeTrigger]) -> Result<(), WriteError> {
    for (i, trigger) in triggers.iter().enumerate() {
        if !TRIGGER_TYPES.contains(&trigger.r#type.as_str()) {
            return Err(WriteError::validation(
                &format!("triggers[{i}].type"),
                "type must be slack or telegram",
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests;
