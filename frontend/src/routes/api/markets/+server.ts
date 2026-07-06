import { json, type RequestHandler } from '@sveltejs/kit';
import { fetchInstruments, groupMarkets } from '$lib/server/catalog';
import { ClickHouseError } from '$lib/server/clickhouse';

export const GET: RequestHandler = async () => {
  try {
    const instruments = await fetchInstruments();
    return json({
      generatedAt: new Date().toISOString(),
      markets: groupMarkets(instruments)
    });
  } catch (error) {
    return apiError(error);
  }
};

function apiError(error: unknown): Response {
  const status = error instanceof ClickHouseError ? error.status : 500;
  const message = error instanceof Error ? error.message : 'Unknown server error';
  return json({ error: message }, { status });
}
