// Update wording shared by the dashboard (status-view.js) and the server's Discord post (full-update.ts).

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
