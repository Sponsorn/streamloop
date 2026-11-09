export const STEP_LABELS: Record<string, string>;
export function stepLabel(step: string): string;
export function failureText(state: { to: string; from: string; step: string; reason: string | null }): string;
