// Update wording for the dashboard (status-view.js); crates/control/src/landing.rs words its Discord post the same way.

export const STEP_LABELS = {
  download: 'Downloading',
  stop: 'Stopping the stream',
  swap: 'Replacing files',
  check: 'Checking the new version',
  start: 'Starting the stream',
  done: 'Back on air',
};

export function stepLabel(step) {
  return STEP_LABELS[step] || String(step);
}

/** "The update to X failed at replacing files: reason. Still on Y." */
export function failureText(state) {
  const reason = (state.reason || 'no reason was recorded').replace(/\.+$/, '');
  return `The update to ${state.to} failed at ${stepLabel(state.step).toLowerCase()}: ${reason}. Still on ${state.from}.`;
}
