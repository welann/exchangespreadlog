const CAPABILITY_CACHE_TTL_MS = 60_000;
const LIVE_REQUEST_CLOCK_SKEW_MS = 5_000;

export type CandleCoverage = {
  ready: boolean;
  coverageFromMs: number | null;
  coverageToMs: number | null;
};

export function candleRangeIsCovered(
  coverage: CandleCoverage,
  fromMs: number,
  toMs: number,
  nowMs: number,
  maxStaleMs: number
): boolean {
  if (!coverage.ready || coverage.coverageFromMs === null) return false;
  if (fromMs < coverage.coverageFromMs) return false;
  if (coverage.coverageToMs === null) return false;
  if (toMs <= coverage.coverageToMs) return true;

  const liveCoverageGraceMs = CAPABILITY_CACHE_TTL_MS + maxStaleMs;
  const coverageIsLive = coverage.coverageToMs >= nowMs - liveCoverageGraceMs;
  const requestEndsNow = toMs <= nowMs + LIVE_REQUEST_CLOCK_SKEW_MS;
  return coverageIsLive && requestEndsNow;
}
