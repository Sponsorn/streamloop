//! Update wording shared with the server's Discord post, ported from the web dashboard's update-text.js.

use crate::api::types::UpdateProgress;

const STEP_LABELS: [(&str, &str); 6] = [
    ("download", "Downloading"),
    ("stop", "Stopping the stream"),
    ("swap", "Replacing files"),
    ("check", "Checking the new version"),
    ("start", "Starting the stream"),
    ("done", "Back on air"),
];

/// An unknown step prints itself.
pub fn step_label(step: &str) -> String {
    STEP_LABELS
        .iter()
        .find(|(key, _)| *key == step)
        .map_or(step, |(_, label)| label)
        .to_string()
}

/// "The update to X failed at replacing files: reason. Still on Y."
pub fn failure_text(p: &UpdateProgress) -> String {
    let reason = p
        .reason
        .as_deref()
        .filter(|r| !r.is_empty())
        .unwrap_or("no reason was recorded");
    format!(
        "The update to {} failed at {}: {}. Still on {}.",
        p.to,
        step_label(&p.step).to_lowercase(),
        reason.trim_end_matches('.'),
        p.from
    )
}
