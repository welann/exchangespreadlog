import { json, type RequestHandler } from '@sveltejs/kit';
import { fetchInstruments, groupMarkets } from '$lib/server/catalog';
import { ClickHouseError } from '$lib/server/clickhouse';

export const GET: RequestHandler = async () => {
  const requestId = crypto.randomUUID();
  try {
    const instruments = await fetchInstruments();
    return json(
      {
        generatedAt: new Date().toISOString(),
        markets: groupMarkets(instruments)
      },
      {
        headers: {
          'cache-control': 'public, max-age=15, stale-while-revalidate=45'
        }
      }
    );
  } catch (error) {
    return apiError(error, requestId);
  }
};

function apiError(error: unknown, requestId: string): Response {
  console.error(`[api/markets:${requestId}]`, error);
  const exposed = error instanceof ClickHouseError && error.expose;
  return json(
    {
      error: exposed ? error.message : 'The market catalog is temporarily unavailable.',
      code: exposed ? error.code : 'UPSTREAM_UNAVAILABLE',
      requestId
    },
    {
      status: exposed ? error.status : 502,
      headers: { 'cache-control': 'no-store', 'x-request-id': requestId }
    }
  );
}
