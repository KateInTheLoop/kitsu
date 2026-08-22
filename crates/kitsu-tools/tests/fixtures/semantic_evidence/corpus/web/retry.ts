export const MAX_RETRY_ATTEMPTS = 3;

export function shouldRetry(attempt: number): boolean {
  return attempt < MAX_RETRY_ATTEMPTS;
}
