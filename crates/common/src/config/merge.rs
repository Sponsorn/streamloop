//! The legacy migrations and the save merge of `config.ts`, with JavaScript's truthiness and
//! object-spread rules, so a save lands the same keys in the same order as Node's.

use super::ordered::{assign, Value};

type Entries = Vec<(String, Value)>;

fn truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => *n != 0.0 && !n.is_nan(),
        Some(Value::String(s)) => !s.is_empty(),
        Some(Value::Array(_) | Value::Object(_)) => true,
    }
}

/// `v && typeof v === 'object'`: objects and lists.
fn is_object(v: Option<&Value>) -> bool {
    matches!(v, Some(Value::Object(_) | Value::Array(_)))
}

/// What `{ ...v }` copies: an object's entries and a list's items by index. Anything else copies
/// nothing (JavaScript would spread text into characters), so `v || {}` and `v ?? {}` need no case.
fn spread(v: Option<&Value>) -> Entries {
    match v {
        Some(Value::Object(o)) => o.clone(),
        Some(Value::Array(a)) => a.iter().enumerate().map(|(i, x)| (i.to_string(), x.clone())).collect(),
        _ => vec![],
    }
}

/// `section?.[key]`.
fn field<'a>(section: Option<&'a Value>, key: &str) -> Option<&'a Value> {
    section.and_then(|s| s.get(key))
}

/// `{ ...a, ...b }`.
fn spread_both(a: Option<&Value>, b: Option<&Value>) -> Entries {
    let mut out = spread(a);
    assign(&mut out, spread(b));
    out
}

/// The single `playlistId` becomes `playlists`, the flat `discordWebhookUrl` becomes `discord.webhookUrl`.
pub(crate) fn migrate(json: &mut Value) {
    if truthy(json.get("playlistId")) && !truthy(json.get("playlists")) {
        let id = json.get("playlistId").cloned().unwrap_or(Value::Null);
        json.set("playlists", Value::Array(vec![Value::Object(vec![("id".into(), id)])]));
        json.remove("playlistId");
    }
    if json.get("discordWebhookUrl").is_some() && !truthy(json.get("discord")) {
        let url = json.get("discordWebhookUrl").cloned().unwrap_or(Value::Null);
        json.set("discord", Value::Object(vec![("webhookUrl".into(), url)]));
        json.remove("discordWebhookUrl");
    }
}

/// `{ ...existing, ...patch }`, with `discord`'s events and templates merged a level deeper and
/// `supervisor` merged shallowly without an empty `streamKeyProtected` (only the stream-key route writes it).
pub(crate) fn merge(existing: &Value, patch: &Value) -> Value {
    let mut incoming = Value::Object(spread(Some(patch)));

    let (old, new) = (existing.get("discord"), incoming.get("discord"));
    if is_object(new) && is_object(old) {
        let mut discord = spread_both(old, new);
        let events = spread_both(field(old, "events"), field(new, "events"));
        let templates = spread_both(field(old, "templates"), field(new, "templates"));
        assign(&mut discord, vec![("events".into(), Value::Object(events)), ("templates".into(), Value::Object(templates))]);
        incoming.set("discord", Value::Object(discord));
    }

    let new = incoming.get("supervisor");
    if is_object(new) {
        let mut patch = spread(new);
        if !truthy(patch.iter().find(|(k, _)| k == "streamKeyProtected").map(|(_, v)| v)) {
            patch.retain(|(k, _)| k != "streamKeyProtected");
        }
        let mut supervisor = spread(existing.get("supervisor"));
        assign(&mut supervisor, patch);
        incoming.set("supervisor", Value::Object(supervisor));
    }

    Value::Object(spread_both(Some(existing), Some(&incoming)))
}
