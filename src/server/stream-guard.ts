// StreamLoop 3.0 installs beside 2.x. Two encoders on one stream key knock each other
// off Twitch, so 2.x asks 3.0's supervisor before starting its own stream.
export const STREAMLOOP3_STATUS_URL = 'http://127.0.0.1:4417/status';

export function encoderIsLive(body: unknown): boolean {
  if (typeof body !== 'object' || body === null) return false;
  const encoder = (body as { encoder?: unknown }).encoder;
  return typeof encoder === 'object' && encoder !== null && (encoder as { alive?: unknown }).alive === true;
}

/** Fails open: anything but a 200 saying the encoder is alive lets 2.x stream,
 *  so a stray program on the port can never keep 2.x off air. */
export async function streamloop3IsStreaming(fetchImpl: typeof fetch = fetch, timeoutMs = 1000): Promise<boolean> {
  try {
    const res = await fetchImpl(STREAMLOOP3_STATUS_URL, { signal: AbortSignal.timeout(timeoutMs) });
    if (res.status !== 200) return false;
    return encoderIsLive(await res.json());
  } catch {
    return false;
  }
}
