import type { SpreadPoint } from '$lib/types';

export type TimeWeightedStats = {
  max: number | null;
  min: number | null;
  avg: number | null;
  volatility: number | null;
  meanReversionMs: number | null;
  windowCount: number;
  positiveShare: number | null;
  coverageShare: number;
};

type WeightedSample = {
  value: number;
  startMs: number;
  endMs: number;
  durationMs: number;
};

export function bestBp(point: SpreadPoint): number | null {
  const values = [point.aToBBp, point.bToABp].filter(
    (value): value is number => value !== null && Number.isFinite(value)
  );
  return values.length === 0 ? null : Math.max(...values);
}

export function computeTimeWeightedStats(
  points: SpreadPoint[],
  fromMs: number,
  toMs: number,
  maxStaleMs = 120_000
): TimeWeightedStats {
  const samples = weightedSamples(points, fromMs, toMs, maxStaleMs, bestBp);
  const windowMs = Math.max(0, toMs - fromMs);
  if (samples.length === 0) {
    return {
      max: null,
      min: null,
      avg: null,
      volatility: null,
      meanReversionMs: null,
      windowCount: 0,
      positiveShare: null,
      coverageShare: 0
    };
  }

  const coveredMs = samples.reduce((sum, sample) => sum + sample.durationMs, 0);
  const weightedSum = samples.reduce(
    (sum, sample) => sum + sample.value * sample.durationMs,
    0
  );
  const avg = coveredMs > 0 ? weightedSum / coveredMs : null;
  const variance =
    avg === null || coveredMs === 0
      ? null
      : samples.reduce(
          (sum, sample) => sum + (sample.value - avg) ** 2 * sample.durationMs,
          0
        ) / coveredMs;
  const positiveMs = samples.reduce(
    (sum, sample) => sum + (sample.value > 0 ? sample.durationMs : 0),
    0
  );
  const runs = positiveRuns(samples);

  return {
    max: Math.max(...samples.map((sample) => sample.value)),
    min: Math.min(...samples.map((sample) => sample.value)),
    avg,
    volatility: variance === null ? null : Math.sqrt(variance),
    meanReversionMs:
      runs.length === 0 ? null : runs.reduce((sum, duration) => sum + duration, 0) / runs.length,
    windowCount: runs.length,
    positiveShare: coveredMs > 0 ? positiveMs / coveredMs : null,
    coverageShare: windowMs > 0 ? Math.min(1, coveredMs / windowMs) : 0
  };
}

export function timeWeightedAverage(
  points: SpreadPoint[],
  fromMs: number,
  toMs: number,
  valueForPoint: (point: SpreadPoint) => number | null,
  maxStaleMs = 120_000
): number | null {
  const samples = weightedSamples(points, fromMs, toMs, maxStaleMs, valueForPoint);
  const duration = samples.reduce((sum, sample) => sum + sample.durationMs, 0);
  if (duration === 0) return null;
  return samples.reduce((sum, sample) => sum + sample.value * sample.durationMs, 0) / duration;
}

function weightedSamples(
  points: SpreadPoint[],
  fromMs: number,
  toMs: number,
  maxStaleMs: number,
  valueForPoint: (point: SpreadPoint) => number | null
): WeightedSample[] {
  const sorted = [...points]
    .filter((point) => Number.isFinite(point.tsMs))
    .sort((left, right) => left.tsMs - right.tsMs);
  const samples: WeightedSample[] = [];

  sorted.forEach((point, index) => {
    const value = valueForPoint(point);
    if (value === null || !Number.isFinite(value)) return;
    const nextTs = sorted[index + 1]?.tsMs ?? toMs;
    const stateExpiry = Math.min(point.aStateTsMs, point.bStateTsMs) + maxStaleMs;
    const startMs = Math.max(fromMs, point.tsMs);
    const endMs = Math.min(toMs, nextTs, stateExpiry);
    if (endMs <= startMs) return;
    samples.push({ value, startMs, endMs, durationMs: endMs - startMs });
  });

  return samples;
}

function positiveRuns(samples: WeightedSample[]): number[] {
  const durations: number[] = [];
  let runStart: number | null = null;
  let runEnd = 0;

  for (const sample of samples) {
    const contiguous = runStart !== null && sample.startMs <= runEnd;
    if (sample.value > 0) {
      if (!contiguous) {
        if (runStart !== null) durations.push(runEnd - runStart);
        runStart = sample.startMs;
      }
      runEnd = Math.max(runEnd, sample.endMs);
    } else if (runStart !== null) {
      durations.push(runEnd - runStart);
      runStart = null;
      runEnd = 0;
    }
  }
  if (runStart !== null) durations.push(runEnd - runStart);
  return durations;
}
