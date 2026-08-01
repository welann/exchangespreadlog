<script lang="ts">
  import { onMount } from 'svelte';
  import SpreadChart from '$lib/SpreadChart.svelte';
  import {
    directionLabel,
    opportunityLegs,
    oppositeDirection,
    type SpreadDirection
  } from '$lib/spread-direction';
  import type {
    Health,
    HistoryResponse,
    Instrument,
    LiveSpread,
    Market,
    SpreadPoint
  } from '$lib/types';

  const ranges = [
    { label: '15m', ms: 15 * 60_000 },
    { label: '1h', ms: 60 * 60_000 },
    { label: '6h', ms: 6 * 60 * 60_000 },
    { label: '24h', ms: 24 * 60 * 60_000 },
    { label: '7d', ms: 7 * 24 * 60 * 60_000 },
    { label: '31d', ms: 31 * 24 * 60 * 60_000 }
  ];
  const topShareOptions = [10, 20, 30, 50];

  type FilterMode = 'market' | 'venue';
  type DirectionStats = {
    average: number | null;
    topAverage: number | null;
    topThreshold: number | null;
    maximum: number | null;
    sampleCount: number;
    topSampleCount: number;
  };
  type OpportunityRank = {
    key: string;
    buy: Instrument;
    sell: Instrument;
    opportunityMs: number;
    windowCount: number;
    typicalBp: number;
    latestOpportunityMs: number;
  };
  type HistoryCacheEntry = {
    fetchedAtMs: number;
    response: HistoryResponse;
  };

  const opportunityQueryConcurrency = 4;
  const historyCacheTtlMs = 30_000;

  let markets: Market[] = [];
  let selectedMarketKey = '';
  let legAKey = '';
  let legBKey = '';
  let rangeMs = ranges[3].ms;
  let history: HistoryResponse | null = null;
  let live: LiveSpread | null = null;
  let health: Health | null = null;
  let marketSearch = '';
  let filterMode: FilterMode = 'market';
  let primaryVenue = '';
  let counterVenue = '';
  let loadingMarkets = true;
  let loadingHistory = false;
  let error = '';
  let stream: EventSource | null = null;
  let healthTimer: ReturnType<typeof setInterval> | null = null;
  let clockTimer: ReturnType<typeof setInterval> | null = null;
  let clockNowMs = Date.now();
  let serverClockOffsetMs = 0;
  let topSharePercent = 30;
  let openDirection: SpreadDirection = 'bToA';
  let opportunityRanking: OpportunityRank[] = [];
  let loadingOpportunities = false;
  let opportunityError = '';
  let opportunityRequestId = 0;
  let opportunityAbortController: AbortController | null = null;
  const historyCache = new Map<string, HistoryCacheEntry>();

  $: selectedMarket =
    markets.find((market) => marketKey(market) === selectedMarketKey) ?? null;
  $: legA =
    selectedMarket?.instruments.find((instrument) => instrument.instrumentKey === legAKey) ??
    null;
  $: legB =
    selectedMarket?.instruments.find((instrument) => instrument.instrumentKey === legBKey) ??
    null;
  $: displayLegA =
    live?.legA?.instrumentKey === legAKey ? live.legA : legA;
  $: displayLegB =
    live?.legB?.instrumentKey === legBKey ? live.legB : legB;
  $: venueOptions = buildVenueOptions(markets);
  $: counterVenueOptions = buildCounterVenueOptions(markets, primaryVenue);
  $: filteredMarkets = filterMarkets(
    markets,
    marketSearch,
    filterMode,
    primaryVenue,
    counterVenue
  );
  $: current = live?.state === 'valid' ? live.point : null;
  $: points = mergePoints(history?.points ?? [], current);
  $: rangeStats = {
    aToB: summarizeDirection(points.map((point) => point.aToBBp), topSharePercent),
    bToA: summarizeDirection(points.map((point) => point.bToABp), topSharePercent)
  };
  $: closeDirection = oppositeDirection(openDirection);
  $: rangeRoutes = [
    { name: directionLabel(openDirection), tone: 'copper', stats: rangeStats[openDirection] },
    { name: directionLabel(closeDirection), tone: 'blue', stats: rangeStats[closeDirection] }
  ];
  $: openAction = routeAction(openDirection, displayLegA?.venue, displayLegB?.venue);
  $: closeAction = routeAction(closeDirection, displayLegA?.venue, displayLegB?.venue);
  $: bestRoute = current
    ? current.aToBBp >= current.bToABp
      ? { name: 'A → B', bp: current.aToBBp, value: current.aToB }
      : { name: 'B → A', bp: current.bToABp, value: current.bToA }
    : null;
  $: railPosition = bestRoute ? clamp(50 + bestRoute.bp * 2.5, 3, 97) : 50;

  onMount(() => {
    void boot();
    healthTimer = setInterval(() => void loadHealth(), 10_000);
    clockTimer = setInterval(() => {
      clockNowMs = Date.now();
    }, 1_000);
    return () => {
      stream?.close();
      opportunityAbortController?.abort();
      if (healthTimer) clearInterval(healthTimer);
      if (clockTimer) clearInterval(clockTimer);
    };
  });

  async function boot() {
    await Promise.all([loadMarkets(), loadHealth()]);
  }

  async function loadMarkets() {
    loadingMarkets = true;
    try {
      const response = await fetch('/v1/markets');
      if (!response.ok) throw new Error(await publicError(response));
      const payload = (await response.json()) as { serverTimeMs: number; markets: Market[] };
      serverClockOffsetMs = payload.serverTimeMs - Date.now();
      markets = payload.markets;
      if (markets.length > 0) {
        selectMarket(markets[0]);
      }
    } catch (cause) {
      error = message(cause);
    } finally {
      loadingMarkets = false;
    }
  }

  async function loadHealth() {
    try {
      const response = await fetch('/v1/health');
      health = (await response.json()) as Health;
    } catch {
      health = null;
    }
  }

  function selectMarket(market: Market) {
    const marketChanged = marketKey(market) !== selectedMarketKey;
    if (filterMode === 'venue') {
      const primaryInstrument = market.instruments.find(
        (instrument) => instrument.venue === primaryVenue
      );
      const counterInstrument = market.instruments.find(
        (instrument) => instrument.venue === counterVenue
      );
      if (!primaryInstrument || !counterInstrument) return;

      selectedMarketKey = marketKey(market);
      legAKey = primaryInstrument.instrumentKey;
      legBKey = counterInstrument.instrumentKey;
      if (marketChanged) {
        prepareOpportunityRanking();
        void refreshSelectedMarket(market);
      } else {
        void refreshPair();
      }
      return;
    }

    selectedMarketKey = marketKey(market);
    const liveInstruments = [...market.instruments].sort(
      (left, right) => (right.latestRecvMs ?? 0) - (left.latestRecvMs ?? 0)
    );
    legAKey = liveInstruments[0]?.instrumentKey ?? '';
    legBKey = liveInstruments.find((instrument) => instrument.instrumentKey !== legAKey)?.instrumentKey ?? '';
    if (marketChanged) {
      prepareOpportunityRanking();
      void refreshSelectedMarket(market);
    } else {
      void refreshPair();
    }
  }

  function setFilterMode(mode: FilterMode) {
    if (filterMode === mode) return;
    filterMode = mode;
    if (mode === 'market') return;

    setVenuePair(legA?.venue ?? primaryVenue, legB?.venue ?? counterVenue);
  }

  function changePrimaryVenue(event: Event) {
    setVenuePair((event.currentTarget as HTMLSelectElement).value, counterVenue);
  }

  function changeCounterVenue(event: Event) {
    setVenuePair(primaryVenue, (event.currentTarget as HTMLSelectElement).value);
  }

  function setVenuePair(nextPrimary: string, preferredCounter: string) {
    primaryVenue =
      venueOptions.find((option) => option.venue === nextPrimary)?.venue ??
      venueOptions[0]?.venue ??
      '';
    const availableCounters = buildCounterVenueOptions(markets, primaryVenue);
    counterVenue =
      availableCounters.find((option) => option.venue === preferredCounter)?.venue ??
      availableCounters[0]?.venue ??
      '';

    const candidates = filterMarkets(markets, '', 'venue', primaryVenue, counterVenue);
    const currentSelection = candidates.find((market) => marketKey(market) === selectedMarketKey);
    const nextMarket = currentSelection ?? candidates[0];
    if (nextMarket) {
      selectMarket(nextMarket);
    } else {
      clearPairSelection();
    }
  }

  function clearPairSelection() {
    selectedMarketKey = '';
    legAKey = '';
    legBKey = '';
    history = null;
    live = null;
    stream?.close();
    stream = null;
    prepareOpportunityRanking();
    loadingOpportunities = false;
  }

  function chooseLeg(which: 'a' | 'b', instrument: Instrument) {
    if (filterMode === 'venue') {
      if (which === 'a') {
        setVenuePair(instrument.venue, legB?.venue ?? counterVenue);
      } else {
        setVenuePair(legA?.venue ?? primaryVenue, instrument.venue);
      }
      return;
    }

    if (which === 'a') {
      if (instrument.instrumentKey === legBKey) legBKey = legAKey;
      legAKey = instrument.instrumentKey;
    } else {
      if (instrument.instrumentKey === legAKey) legAKey = legBKey;
      legBKey = instrument.instrumentKey;
    }
    void refreshPair();
  }

  function swapLegs() {
    openDirection = oppositeDirection(openDirection);
    if (filterMode === 'venue') {
      setVenuePair(counterVenue, primaryVenue);
      return;
    }

    [legAKey, legBKey] = [legBKey, legAKey];
    void refreshPair();
  }

  function selectOpportunity(opportunity: OpportunityRank) {
    openDirection = 'bToA';
    if (filterMode === 'venue') {
      setVenuePair(opportunity.buy.venue, opportunity.sell.venue);
      return;
    }

    legAKey = opportunity.buy.instrumentKey;
    legBKey = opportunity.sell.instrumentKey;
    void refreshPair();
  }

  async function setRange(value: number) {
    rangeMs = value;
    prepareOpportunityRanking();
    await loadHistory();
    await loadOpportunityRanking(selectedMarket);
  }

  async function refreshSelectedMarket(market: Market) {
    await refreshPair();
    if (marketKey(market) === selectedMarketKey) {
      await loadOpportunityRanking(market);
    }
  }

  async function refreshPair() {
    if (!legAKey || !legBKey || legAKey === legBKey) return;
    error = '';
    history = null;
    live = null;
    connectLive();
    await loadHistory();
  }

  async function loadHistory(force = false) {
    if (!legAKey || !legBKey) return;
    loadingHistory = true;
    try {
      history = await fetchHistory(legAKey, legBKey, rangeMs, undefined, force);
    } catch (cause) {
      error = message(cause);
    } finally {
      loadingHistory = false;
    }
  }

  async function refreshHistoricalData() {
    historyCache.clear();
    prepareOpportunityRanking();
    await loadHistory(true);
    await loadOpportunityRanking(selectedMarket);
  }

  function prepareOpportunityRanking() {
    opportunityRequestId += 1;
    opportunityAbortController?.abort();
    opportunityAbortController = null;
    opportunityRanking = [];
    opportunityError = '';
    loadingOpportunities = true;
  }

  async function loadOpportunityRanking(market: Market | null) {
    if (!market || market.instruments.length < 2) {
      loadingOpportunities = false;
      return;
    }

    const requestedMarketKey = marketKey(market);
    const requestedRangeMs = rangeMs;
    const requestId = ++opportunityRequestId;
    opportunityAbortController?.abort();
    const controller = new AbortController();
    opportunityAbortController = controller;
    loadingOpportunities = true;
    opportunityError = '';

    const pairs = buildInstrumentPairs(market.instruments);
    let failedPairs = 0;

    try {
      const results = await mapWithConcurrency(
        pairs,
        opportunityQueryConcurrency,
        async ([first, second]) => {
          try {
            const response = await fetchHistory(
              first.instrumentKey,
              second.instrumentKey,
              requestedRangeMs,
              controller.signal
            );
            return summarizeOpportunityPair(response, first, second);
          } catch (cause) {
            if (isAbortError(cause)) throw cause;
            failedPairs += 1;
            return null;
          }
        }
      );

      if (
        requestId !== opportunityRequestId ||
        requestedMarketKey !== selectedMarketKey ||
        requestedRangeMs !== rangeMs
      ) {
        return;
      }

      opportunityRanking = results
        .filter((result): result is OpportunityRank => result !== null)
        .sort(
          (left, right) =>
            right.typicalBp - left.typicalBp ||
            right.opportunityMs - left.opportunityMs ||
            right.latestOpportunityMs - left.latestOpportunityMs
        );
      opportunityError =
        failedPairs > 0
          ? `${failedPairs} 组交易所暂时无法读取，排行已使用其余数据。`
          : '';
    } catch (cause) {
      if (!isAbortError(cause) && requestId === opportunityRequestId) {
        opportunityError = '机会排行暂时无法读取。';
      }
    } finally {
      if (requestId === opportunityRequestId) {
        loadingOpportunities = false;
        opportunityAbortController = null;
      }
    }
  }

  async function fetchHistory(
    firstKey: string,
    secondKey: string,
    requestedRangeMs: number,
    signal?: AbortSignal,
    force = false
  ) {
    const cacheKey = `${firstKey}|${secondKey}|${requestedRangeMs}`;
    const cached = historyCache.get(cacheKey);
    if (!force && cached && Date.now() - cached.fetchedAtMs < historyCacheTtlMs) {
      return cached.response;
    }

    const query = new URLSearchParams({
      leg_a: firstKey,
      leg_b: secondKey,
      range_ms: String(requestedRangeMs)
    });
    const response = await fetch(`/v1/spreads?${query}`, { signal });
    if (!response.ok) throw new Error(await publicError(response));
    const payload = (await response.json()) as HistoryResponse;
    historyCache.set(cacheKey, { fetchedAtMs: Date.now(), response: payload });
    return payload;
  }

  function buildInstrumentPairs(instruments: Instrument[]) {
    const byVenue = new Map<string, Instrument>();
    for (const instrument of [...instruments].sort(
      (left, right) => (right.latestRecvMs ?? 0) - (left.latestRecvMs ?? 0)
    )) {
      if (!byVenue.has(instrument.venue)) byVenue.set(instrument.venue, instrument);
    }

    const uniqueInstruments = [...byVenue.values()];
    const pairs: Array<[Instrument, Instrument]> = [];
    for (let first = 0; first < uniqueInstruments.length; first += 1) {
      for (let second = first + 1; second < uniqueInstruments.length; second += 1) {
        pairs.push([uniqueInstruments[first], uniqueInstruments[second]]);
      }
    }
    return pairs;
  }

  async function mapWithConcurrency<T, R>(
    items: T[],
    concurrency: number,
    task: (item: T) => Promise<R>
  ) {
    const results = new Array<R>(items.length);
    let nextIndex = 0;

    async function worker() {
      while (nextIndex < items.length) {
        const index = nextIndex;
        nextIndex += 1;
        results[index] = await task(items[index]);
      }
    }

    await Promise.all(
      Array.from({ length: Math.min(concurrency, items.length) }, () => worker())
    );
    return results;
  }

  function summarizeOpportunityPair(
    response: HistoryResponse,
    first: Instrument,
    second: Instrument
  ) {
    const aToB = summarizeOpportunityDirection(
      response,
      (point) => point.aToBBp,
      first,
      second,
      'aToB'
    );
    const bToA = summarizeOpportunityDirection(
      response,
      (point) => point.bToABp,
      first,
      second,
      'bToA'
    );
    if (!aToB) return bToA;
    if (!bToA) return aToB;
    return aToB.typicalBp > bToA.typicalBp ||
      (aToB.typicalBp === bToA.typicalBp && aToB.opportunityMs >= bToA.opportunityMs)
      ? aToB
      : bToA;
  }

  function summarizeOpportunityDirection(
    response: HistoryResponse,
    valueForPoint: (point: SpreadPoint) => number,
    first: Instrument,
    second: Instrument,
    direction: SpreadDirection
  ): OpportunityRank | null {
    const points = [...response.points].sort((left, right) => left.tsMs - right.tsMs);
    const resolutionMs = Math.max(1, response.resolutionMs);
    const positiveValues: number[] = [];
    let opportunityMs = 0;
    let windowCount = 0;
    let windowOpen = false;
    let previousTsMs: number | null = null;
    let latestOpportunityMs = 0;

    for (let index = 0; index < points.length; index += 1) {
      const point = points[index];
      const value = valueForPoint(point);
      const positive = Number.isFinite(value) && value > 0;
      const contiguous =
        previousTsMs !== null && point.tsMs - previousTsMs <= resolutionMs * 1.5;

      if (positive) {
        if (!windowOpen || !contiguous) windowCount += 1;
        const nextTsMs = points[index + 1]?.tsMs ?? response.toMs;
        opportunityMs += Math.max(
          0,
          Math.min(resolutionMs, nextTsMs - point.tsMs, response.toMs - point.tsMs)
        );
        positiveValues.push(value);
        latestOpportunityMs = Math.max(latestOpportunityMs, point.tsMs);
      }

      windowOpen = positive;
      previousTsMs = point.tsMs;
    }

    if (positiveValues.length === 0) return null;
    const sortedValues = positiveValues.sort((left, right) => left - right);
    const { buy, sell } = opportunityLegs(direction, first, second);
    return {
      key: `${buy.instrumentKey}|${sell.instrumentKey}`,
      buy,
      sell,
      opportunityMs,
      windowCount,
      typicalBp: median(sortedValues),
      latestOpportunityMs
    };
  }

  function median(sortedValues: number[]) {
    const middle = Math.floor(sortedValues.length / 2);
    return sortedValues.length % 2 === 0
      ? (sortedValues[middle - 1] + sortedValues[middle]) / 2
      : sortedValues[middle];
  }

  function routeAction(
    direction: SpreadDirection,
    venueA: string | undefined,
    venueB: string | undefined
  ) {
    const a = venueA ? `A ${venueA}` : 'A';
    const b = venueB ? `B ${venueB}` : 'B';
    return direction === 'aToB' ? `卖 ${a} / 买 ${b}` : `卖 ${b} / 买 ${a}`;
  }

  function isAbortError(cause: unknown) {
    return cause instanceof Error && cause.name === 'AbortError';
  }

  function connectLive() {
    stream?.close();
    const query = new URLSearchParams({ leg_a: legAKey, leg_b: legBKey });
    stream = new EventSource(`/v1/live/spread?${query}`);
    const receive = (event: MessageEvent<string>) => {
      const next = JSON.parse(event.data) as LiveSpread;
      live = next;
      mergeLiveInstruments(next);
    };
    stream.addEventListener('snapshot', receive as EventListener);
    stream.addEventListener('spread', receive as EventListener);
    stream.addEventListener('resync', () => void loadHistory(true));
    stream.onerror = () => {
      error = '实时连接正在重试；历史数据仍可查看。';
    };
    stream.onopen = () => {
      if (error.startsWith('实时连接')) error = '';
    };
  }

  function mergeLiveInstruments(next: LiveSpread) {
    const updates = new Map<string, Instrument>();
    for (const instrument of [next.legA, next.legB]) {
      if (instrument?.instrumentKey) updates.set(instrument.instrumentKey, instrument);
    }
    if (updates.size === 0) return;

    markets = markets.map((market) => {
      let changed = false;
      const instruments = market.instruments.map((instrument) => {
        const update = updates.get(instrument.instrumentKey);
        if (!update) return instrument;
        changed = true;
        return update;
      });
      return changed ? { ...market, instruments } : market;
    });
  }

  function mergePoints(base: SpreadPoint[], point: SpreadPoint | null): SpreadPoint[] {
    if (!point) return base;
    const next = new Map(base.map((item) => [item.tsMs, item]));
    next.set(point.tsMs, point);
    return [...next.values()].sort((left, right) => left.tsMs - right.tsMs);
  }

  function summarizeDirection(values: number[], topPercent: number): DirectionStats {
    const sorted = values.filter(Number.isFinite).sort((left, right) => right - left);
    if (sorted.length === 0) {
      return {
        average: null,
        topAverage: null,
        topThreshold: null,
        maximum: null,
        sampleCount: 0,
        topSampleCount: 0
      };
    }

    const topSampleCount = Math.max(1, Math.ceil(sorted.length * topPercent / 100));
    const topValues = sorted.slice(0, topSampleCount);
    return {
      average: mean(sorted),
      topAverage: mean(topValues),
      topThreshold: topValues.at(-1) ?? null,
      maximum: sorted[0],
      sampleCount: sorted.length,
      topSampleCount
    };
  }

  function mean(values: number[]) {
    return values.reduce((sum, value) => sum + value, 0) / values.length;
  }

  function formatBp(value: number | null) {
    if (value === null || !Number.isFinite(value)) return '—';
    return `${value > 0 ? '+' : ''}${formatNumber(value)} bp`;
  }

  function marketKey(market: Market) {
    return market.baseAsset;
  }

  function buildVenueOptions(source: Market[]) {
    const counts = new Map<string, number>();
    for (const market of source) {
      for (const venue of new Set(market.instruments.map((instrument) => instrument.venue))) {
        counts.set(venue, (counts.get(venue) ?? 0) + 1);
      }
    }
    return [...counts.entries()]
      .map(([venue, marketCount]) => ({ venue, marketCount }))
      .sort(
        (left, right) =>
          right.marketCount - left.marketCount || left.venue.localeCompare(right.venue)
      );
  }

  function buildCounterVenueOptions(source: Market[], selectedVenue: string) {
    if (!selectedVenue) return [];
    const counts = new Map(
      buildVenueOptions(source)
        .filter((option) => option.venue !== selectedVenue)
        .map((option) => [option.venue, 0])
    );
    for (const market of source) {
      const venues = new Set(market.instruments.map((instrument) => instrument.venue));
      if (!venues.has(selectedVenue)) continue;
      for (const venue of venues) {
        if (venue !== selectedVenue) counts.set(venue, (counts.get(venue) ?? 0) + 1);
      }
    }
    return [...counts.entries()]
      .map(([venue, sharedMarketCount]) => ({ venue, sharedMarketCount }))
      .sort(
        (left, right) =>
          right.sharedMarketCount - left.sharedMarketCount ||
          left.venue.localeCompare(right.venue)
      );
  }

  function filterMarkets(
    source: Market[],
    search: string,
    mode: FilterMode,
    selectedVenue: string,
    selectedCounterVenue: string
  ) {
    const normalizedSearch = search.trim().toLowerCase();
    return source.filter((market) => {
      if (
        normalizedSearch &&
        !`${market.baseAsset} ${market.quoteAssets.join(' ')}`.toLowerCase().includes(normalizedSearch)
      ) {
        return false;
      }
      if (mode === 'market') return true;
      const venues = new Set(market.instruments.map((instrument) => instrument.venue));
      return (
        venues.has(selectedVenue) &&
        (!selectedCounterVenue || venues.has(selectedCounterVenue))
      );
    });
  }

  function formatNumber(value: number | null | undefined, digits = 2) {
    if (value === null || value === undefined || !Number.isFinite(value)) return '—';
    return value.toLocaleString(undefined, {
      minimumFractionDigits: digits,
      maximumFractionDigits: digits
    });
  }

  function formatAge(timestamp: number | null | undefined, nowMs: number) {
    if (!timestamp) return '等待首个报价';
    const age = Math.max(0, nowMs + serverClockOffsetMs - timestamp);
    if (age < 1_000) return '刚刚';
    if (age < 60_000) return `${Math.floor(age / 1_000)} 秒前`;
    if (age < 3_600_000) return `${Math.floor(age / 60_000)} 分钟前`;
    if (age < 86_400_000) {
      const hours = Math.floor(age / 3_600_000);
      const minutes = Math.floor(age % 3_600_000 / 60_000);
      return minutes > 0 ? `${hours} 小时 ${minutes} 分钟前` : `${hours} 小时前`;
    }
    const days = Math.floor(age / 86_400_000);
    const hours = Math.floor(age % 86_400_000 / 3_600_000);
    return hours > 0 ? `${days} 天 ${hours} 小时前` : `${days} 天前`;
  }

  function formatBookTime(timestamp: number | null | undefined) {
    if (!timestamp) return '尚未收到有效订单簿';
    return `最近接收：${new Date(timestamp).toLocaleString()}`;
  }

  function resolutionLabel(value: number | undefined) {
    if (!value) return '—';
    if (value < 60_000) return `${value / 1_000} 秒`;
    if (value < 3_600_000) return `${value / 60_000} 分钟`;
    return `${value / 3_600_000} 小时`;
  }

  function selectedRangeLabel() {
    return ranges.find((range) => range.ms === rangeMs)?.label ?? '当前区间';
  }

  function formatOpportunityDuration(value: number) {
    if (value < 60_000) return '<1 分钟';
    if (value < 3_600_000) return `${Math.round(value / 60_000)} 分钟`;
    if (value < 86_400_000) {
      return `${formatNumber(value / 3_600_000, value < 10_800_000 ? 1 : 0)} 小时`;
    }
    return `${formatNumber(value / 86_400_000, 1)} 天`;
  }

  function liveStateLabel(value: LiveSpread | null) {
    if (!value) return '等待实时流';
    if (value.state === 'valid') return '实时 BBO 有效';
    if (value.reason?.includes('no valid BBO')) return '等待两腿有效 BBO';
    return '当前价差不可用';
  }

  async function publicError(response: Response) {
    try {
      const payload = await response.json();
      return payload.error?.message ?? `请求失败（${response.status}）`;
    } catch {
      return `请求失败（${response.status}）`;
    }
  }

  function message(cause: unknown) {
    return cause instanceof Error ? cause.message : '请求失败，请检查服务状态。';
  }

  function clamp(value: number, min: number, max: number) {
    return Math.min(max, Math.max(min, value));
  }
</script>

<svelte:head>
  <title>Spread Observatory</title>
</svelte:head>

<div class="shell">
  <header class="masthead">
    <div class="identity">
      <div>
        <strong>Spread Observatory</strong>
        <span>跨所最优价监控台</span>
      </div>
    </div>
    <div class="system-line">
      <a class="catalog-link" href="/admin/catalog/">交易对审核</a>
      <span class="divider"></span>
      <span class:healthy={health?.status === 'ok'} class="status-dot"></span>
      <span>{health?.status === 'ok' ? '采集链路正常' : '链路检查中'}</span>
      <span class="divider"></span>
      <span>WAL {health?.walPending ?? '—'}</span>
      <span class="divider"></span>
      <span>CH {health?.clickhouseVersion ?? '—'}</span>
    </div>
  </header>

  {#if error}
    <div class="notice" role="status">
      <strong>数据提示</strong>
      <span>{error}</span>
      <button on:click={() => (error = '')} aria-label="关闭提示">×</button>
    </div>
  {/if}

  <main class="workbench">
    <aside class:venue-mode={filterMode === 'venue'} class="market-rail">
      <div class="rail-heading">
        <span>市场</span>
        <small>{filteredMarkets.length} / {markets.length} 个市场</small>
      </div>
      <div class="filter-mode" aria-label="筛选方式">
        <button
          class:active={filterMode === 'market'}
          aria-pressed={filterMode === 'market'}
          on:click={() => setFilterMode('market')}
        >
          市场优先
        </button>
        <button
          class:active={filterMode === 'venue'}
          aria-pressed={filterMode === 'venue'}
          on:click={() => setFilterMode('venue')}
        >
          交易所优先
        </button>
      </div>
      {#if filterMode === 'venue'}
        <div class="venue-filters">
          <label>
            <span>主交易所 A</span>
            <select value={primaryVenue} on:change={changePrimaryVenue}>
              {#each venueOptions as option}
                <option value={option.venue}>
                  {option.venue} · {option.marketCount} 个市场
                </option>
              {/each}
            </select>
          </label>
          <label>
            <span>对手交易所 B</span>
            <select value={counterVenue} on:change={changeCounterVenue}>
              {#each counterVenueOptions as option}
                <option value={option.venue}>
                  {option.venue} · {option.sharedMarketCount} 个共有市场
                </option>
              {/each}
            </select>
          </label>
        </div>
      {/if}
      <label class="search">
        <span>搜索交易对</span>
        <input bind:value={marketSearch} placeholder="BTC / ETH / SOL" />
      </label>
      <nav aria-label={filterMode === 'venue' ? '所选交易所的共有市场' : '可比较市场'}>
        {#if loadingMarkets}
          {#each Array(7) as _}
            <div class="market-skeleton"></div>
          {/each}
        {:else if filteredMarkets.length === 0}
          <p class="empty">
            {filterMode === 'venue'
              ? '这两个交易所没有符合搜索条件的共有市场。'
              : '没有符合搜索条件的可比较市场。'}
          </p>
        {:else}
          {#each filteredMarkets as market}
            <button
              class:active={marketKey(market) === selectedMarketKey}
              on:click={() => selectMarket(market)}
            >
              <strong>{market.baseAsset}</strong>
              <span>/{market.quoteAssets.join('+')}</span>
              <em>{market.instruments.length} 个交易所</em>
            </button>
          {/each}
        {/if}
      </nav>
    </aside>

    <section class="main-stage">
      <div class="pair-line">
        <div>
          <span class="eyebrow">当前路由</span>
          <h1>
            {legA?.venue ?? 'A'}
            <span>↔</span>
            {legB?.venue ?? 'B'}
          </h1>
          <p>
            {selectedMarket
              ? `${selectedMarket.baseAsset}/${live?.targetQuote ?? history?.targetQuote ?? selectedMarket.quoteAssets.join('+')}`
              : '等待市场目录'}
            · USD / USDC / AUSD 按显式 1:1 汇率统一计价
          </p>
        </div>
        <button
          class="swap"
          on:click={swapLegs}
          disabled={!legA || !legB}
          title="仅交换 A/B 的显示顺序，保持当前开仓交易不变"
        >
          交换 A / B
        </button>
      </div>

      <section class:unavailable={!current} class="route-rail" aria-label="当前最佳价差方向">
        <div class="route-summary">
          <span>可执行方向</span>
          <strong class:positive={(bestRoute?.bp ?? 0) > 0}>
            {bestRoute?.name ?? '等待双腿'}
          </strong>
          <small>{liveStateLabel(live)}</small>
        </div>
        <div class="track">
          <span class="zero">0 bp</span>
          <div class="track-line"></div>
          <div
            class:positive={(bestRoute?.bp ?? 0) > 0}
            class="signal"
            style={`left:${railPosition}%`}
          >
            <i></i>
            <output>{formatNumber(bestRoute?.bp)} bp</output>
          </div>
        </div>
        <div class="route-value">
          <span>价差</span>
          <strong>{formatNumber(bestRoute?.value, 4)}</strong>
        </div>
      </section>

      <div class="query-strip">
        <div class="ranges" aria-label="历史范围">
          {#each ranges as range}
            <button class:active={rangeMs === range.ms} on:click={() => setRange(range.ms)}>
              {range.label}
            </button>
          {/each}
        </div>
        <div class="query-meta">
          <span>{loadingHistory ? '查询中…' : `${history?.points.length ?? 0} 个数据点`}</span>
          <span>{resolutionLabel(history?.resolutionMs)} 粒度</span>
          <span>服务端时钟锚定</span>
          <button on:click={refreshHistoricalData} disabled={loadingHistory || loadingOpportunities}>
            刷新历史
          </button>
        </div>
      </div>

      <section class="range-stats" aria-label="当前时间范围价差统计">
        <header>
          <div>
            <strong>区间统计</strong>
            <span>{rangeStats.aToB.sampleCount} 个对齐点</span>
          </div>
          <label>
            高位样本
            <select bind:value={topSharePercent} aria-label="高位样本比例">
              {#each topShareOptions as percentage}
                <option value={percentage}>最高 {percentage}%</option>
              {/each}
            </select>
          </label>
        </header>
        <div class="stats-comparison">
          {#each rangeRoutes as route}
            <article class={route.tone}>
              <div class="stat-route">
                <i></i>
                <strong>{route.name}</strong>
                <span>取 {route.stats.topSampleCount} / {route.stats.sampleCount} 点</span>
              </div>
              <dl>
                <div>
                  <dt>区间平均</dt>
                  <dd class:positive={(route.stats.average ?? 0) > 0}>
                    {formatBp(route.stats.average)}
                  </dd>
                </div>
                <div>
                  <dt>高位均值</dt>
                  <dd class:positive={(route.stats.topAverage ?? 0) > 0}>
                    {formatBp(route.stats.topAverage)}
                  </dd>
                </div>
                <div>
                  <dt>高位门槛</dt>
                  <dd class:positive={(route.stats.topThreshold ?? 0) > 0}>
                    {formatBp(route.stats.topThreshold)}
                  </dd>
                </div>
                <div>
                  <dt>最大值</dt>
                  <dd class:positive={(route.stats.maximum ?? 0) > 0}>
                    {formatBp(route.stats.maximum)}
                  </dd>
                </div>
              </dl>
            </article>
          {/each}
        </div>
      </section>

      <section class="chart-panel">
        <header>
          <div class="chart-title">
            <strong>跨时点套利机会 · bp</strong>
            <span>点击图表锁定橙线开仓时刻；交换 A/B 不会反转这笔交易</span>
            <em>毛收益，尚未扣除手续费、资金费和滑点</em>
          </div>
          <div class="chart-controls">
            <div class="direction-picker" aria-label="选择开仓方向">
              <button
                class:active={openDirection === 'bToA'}
                aria-pressed={openDirection === 'bToA'}
                on:click={() => (openDirection = 'bToA')}
              >B → A 开仓</button>
              <button
                class:active={openDirection === 'aToB'}
                aria-pressed={openDirection === 'aToB'}
                on:click={() => (openDirection = 'aToB')}
              >A → B 开仓</button>
            </div>
            <div class="legend">
              <span title={openAction}><i class="copper"></i>开仓 · {openAction}</span>
              <span title={closeAction}><i class="blue"></i>平仓 · {closeAction}</span>
              <span><i class="capture"></i>开仓后可平仓毛收益</span>
            </div>
          </div>
        </header>
        {#if points.length > 0}
          <SpreadChart
            {points}
            {openDirection}
            legAVenue={displayLegA?.venue ?? ''}
            legBVenue={displayLegB?.venue ?? ''}
          />
        {:else}
          <div class="chart-empty">
            <strong>{loadingHistory ? '正在读取聚合数据' : '当前时间范围没有双腿同时有效的报价'}</strong>
            <span>可以扩大时间范围，或检查右侧两条腿的实时采集状态。</span>
          </div>
        {/if}
      </section>

      <section class="tape">
        <div>
          <span>A 买 / 卖</span>
          <strong>{formatNumber(current?.aBid, 4)}</strong>
          <strong>{formatNumber(current?.aAsk, 4)}</strong>
        </div>
        <div>
          <span>B 买 / 卖</span>
          <strong>{formatNumber(current?.bBid, 4)}</strong>
          <strong>{formatNumber(current?.bAsk, 4)}</strong>
        </div>
        <div>
          <span>A → B</span>
          <strong class:positive={(current?.aToBBp ?? 0) > 0}>
            {formatNumber(current?.aToBBp)} bp
          </strong>
        </div>
        <div>
          <span>B → A</span>
          <strong class:positive={(current?.bToABp ?? 0) > 0}>
            {formatNumber(current?.bToABp)} bp
          </strong>
        </div>
      </section>
    </section>

    <aside class="leg-panel">
      <div class="rail-heading">
        <span>两腿</span>
        <small>选择不同交易所</small>
      </div>

      <section class="leg-block a">
        <header>
          <span>A</span>
          <div class="leg-identity">
            <strong>{displayLegA?.venue ?? '未选择'}</strong>
            <small>{displayLegA?.symbol ?? '—'}</small>
          </div>
          <div class="book-age" title={formatBookTime(displayLegA?.latestRecvMs)}>
            <span>最新订单簿</span>
            <strong>{formatAge(displayLegA?.latestRecvMs, clockNowMs)}</strong>
          </div>
        </header>
        <div class="venue-list">
          {#each selectedMarket?.instruments ?? [] as instrument}
            <button
              class:active={instrument.instrumentKey === legAKey}
              disabled={instrument.instrumentKey === legBKey}
              on:click={() => chooseLeg('a', instrument)}
            >
              <span>{instrument.venue}</span>
              <small>{formatAge(instrument.latestRecvMs, clockNowMs)}</small>
            </button>
          {/each}
        </div>
      </section>

      <section class="leg-block b">
        <header>
          <span>B</span>
          <div class="leg-identity">
            <strong>{displayLegB?.venue ?? '未选择'}</strong>
            <small>{displayLegB?.symbol ?? '—'}</small>
          </div>
          <div class="book-age" title={formatBookTime(displayLegB?.latestRecvMs)}>
            <span>最新订单簿</span>
            <strong>{formatAge(displayLegB?.latestRecvMs, clockNowMs)}</strong>
          </div>
        </header>
        <div class="venue-list">
          {#each selectedMarket?.instruments ?? [] as instrument}
            <button
              class:active={instrument.instrumentKey === legBKey}
              disabled={instrument.instrumentKey === legAKey}
              on:click={() => chooseLeg('b', instrument)}
            >
              <span>{instrument.venue}</span>
              <small>{formatAge(instrument.latestRecvMs, clockNowMs)}</small>
            </button>
          {/each}
        </div>
      </section>

      <section class="opportunity-block" aria-label="当前交易对的交易所机会排行">
        <div class="rail-heading">
          <span>机会排行</span>
          <small>{selectedRangeLabel()} · 典型毛价差</small>
        </div>
        {#if loadingOpportunities}
          <div class="opportunity-loading" aria-label="正在计算机会排行">
            {#each Array(3) as _}
              <span></span>
            {/each}
          </div>
        {:else if opportunityRanking.length > 0}
          <div class="opportunity-list">
            {#each opportunityRanking.slice(0, 3) as opportunity, index}
              <button
                class:active={legAKey === opportunity.buy.instrumentKey &&
                  legBKey === opportunity.sell.instrumentKey}
                on:click={() => selectOpportunity(opportunity)}
                aria-label={`选择 ${opportunity.buy.venue} 买入、${opportunity.sell.venue} 卖出`}
              >
                <span class="opportunity-rank">{index + 1}</span>
                <span class="opportunity-route">
                  <strong>
                    {opportunity.buy.venue}
                    <i>→</i>
                    {opportunity.sell.venue}
                  </strong>
                  <small>
                    {formatOpportunityDuration(opportunity.opportunityMs)}机会 ·
                    {opportunity.windowCount} 段
                  </small>
                </span>
                <span class="opportunity-value">
                  <strong>典型 {formatNumber(opportunity.typicalBp)} bp</strong>
                  <small>{formatAge(opportunity.latestOpportunityMs, clockNowMs)}出现</small>
                </span>
              </button>
            {/each}
          </div>
          {#if opportunityError}
            <p class="opportunity-note">{opportunityError}</p>
          {/if}
        {:else}
          <p class="opportunity-empty">
            {opportunityError || '当前区间没有出现正价差。'}
          </p>
        {/if}
      </section>

      <section class="health-block">
        <div class="rail-heading">
          <span>采集状态</span>
          <small>实时</small>
        </div>
        <dl>
          <div><dt>接收 tick</dt><dd>{health?.receivedTicks ?? '—'}</dd></div>
          <div><dt>有效 tick</dt><dd>{health?.acceptedTicks ?? '—'}</dd></div>
          <div><dt>拒绝 tick</dt><dd>{health?.rejectedTicks ?? '—'}</dd></div>
          <div><dt>重连失效</dt><dd>{health?.venueResets ?? '—'}</dd></div>
        </dl>
      </section>
    </aside>
  </main>
</div>

<style>
  :global(*) {
    box-sizing: border-box;
  }

  :global(html) {
    color-scheme: light;
    background: #e7edf1;
  }

  :global(body) {
    margin: 0;
    min-width: 320px;
    color: #13243a;
    background: #e7edf1;
    font-family: "Avenir Next", Avenir, "Segoe UI", sans-serif;
  }

  button,
  input,
  select {
    font: inherit;
  }

  button {
    color: inherit;
  }

  button:focus-visible,
  input:focus-visible,
  select:focus-visible {
    outline: 2px solid #2e6f95;
    outline-offset: 2px;
  }

  .shell {
    width: min(1660px, calc(100% - 24px));
    min-height: calc(100vh - 24px);
    margin: 12px auto;
    border: 1px solid #aebdc8;
    background: #f3f6f8;
  }

  .masthead {
    min-height: 66px;
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 24px;
    padding: 0 20px;
    border-bottom: 1px solid #aebdc8;
    background: rgba(248, 250, 251, 0.94);
  }

  .identity {
    display: flex;
    align-items: center;
  }

  .identity strong,
  h1,
  .route-summary strong {
    font-family: "Avenir Next Condensed", "Arial Narrow", sans-serif;
    font-stretch: condensed;
  }

  .identity strong {
    display: block;
    font-size: 18px;
    letter-spacing: 0.04em;
  }

  .system-line,
  .venue-list small,
  .health-block dd {
    font-family: "IBM Plex Mono", "SFMono-Regular", Consolas, monospace;
  }

  .identity > div > span {
    display: block;
    margin-top: 2px;
    color: #657789;
    font-size: 11px;
    letter-spacing: 0.08em;
  }

  .system-line {
    display: flex;
    align-items: center;
    gap: 9px;
    color: #526579;
    font-size: 11px;
    letter-spacing: 0.03em;
  }

  .catalog-link {
    color: #315d7a;
    font-weight: 700;
    text-decoration: none;
  }

  .catalog-link:hover {
    color: #c06e37;
  }

  .status-dot {
    width: 8px;
    height: 8px;
    border-radius: 50%;
    background: #b84e45;
  }

  .status-dot.healthy {
    background: #4c8876;
  }

  .divider {
    width: 1px;
    height: 16px;
    background: #c9d3da;
  }

  .notice {
    display: grid;
    grid-template-columns: auto 1fr auto;
    gap: 12px;
    align-items: center;
    padding: 10px 18px;
    color: #6c312b;
    border-bottom: 1px solid #dcb6ae;
    background: #f6e9e6;
    font-size: 13px;
  }

  .notice button {
    border: 0;
    background: transparent;
    cursor: pointer;
    font-size: 20px;
  }

  .workbench {
    display: grid;
    grid-template-columns: 238px minmax(500px, 1fr) 250px;
    min-height: calc(100vh - 92px);
  }

  .market-rail,
  .leg-panel {
    background: #edf2f5;
  }

  .market-rail {
    border-right: 1px solid #aebdc8;
  }

  .leg-panel {
    border-left: 1px solid #aebdc8;
  }

  .rail-heading {
    display: flex;
    align-items: baseline;
    justify-content: space-between;
    padding: 14px 14px 10px;
    border-bottom: 1px solid #ccd6dd;
    letter-spacing: 0.03em;
  }

  .rail-heading span {
    font-size: 12px;
    font-weight: 700;
  }

  .rail-heading small {
    color: #728392;
    font-size: 10px;
  }

  .filter-mode {
    display: grid;
    grid-template-columns: 1fr 1fr;
    padding: 12px 14px 0;
  }

  .filter-mode button {
    padding: 8px 6px;
    border: 1px solid #aebdc8;
    background: #edf2f5;
    cursor: pointer;
    font-size: 11px;
  }

  .filter-mode button + button {
    border-left: 0;
  }

  .filter-mode button:hover {
    background: #f8fafb;
  }

  .filter-mode button.active {
    color: #f8fafb;
    background: #2e536e;
  }

  .venue-filters {
    display: grid;
    gap: 10px;
    padding: 12px 14px;
    border-bottom: 1px solid #ccd6dd;
  }

  .venue-filters label > span {
    display: block;
    margin-bottom: 5px;
    color: #526579;
    font-size: 10px;
    font-weight: 650;
  }

  .venue-filters select {
    width: 100%;
    padding: 8px 9px;
    color: #13243a;
    border: 1px solid #b7c5cf;
    border-radius: 2px;
    background: #f8fafb;
    font-size: 11px;
  }

  .search {
    display: block;
    padding: 12px 14px;
    border-bottom: 1px solid #ccd6dd;
  }

  .search span {
    display: block;
    margin-bottom: 6px;
    color: #657789;
    font-size: 11px;
    letter-spacing: 0.03em;
  }

  .search input {
    width: 100%;
    padding: 9px 10px;
    color: #13243a;
    border: 1px solid #b7c5cf;
    border-radius: 2px;
    background: #f8fafb;
  }

  nav {
    max-height: calc(100vh - 190px);
    overflow: auto;
  }

  .market-rail.venue-mode nav {
    max-height: calc(100vh - 328px);
  }

  nav button {
    width: 100%;
    display: grid;
    grid-template-columns: auto 1fr auto;
    align-items: baseline;
    padding: 12px 14px;
    border: 0;
    border-bottom: 1px solid #d2dbe1;
    background: transparent;
    text-align: left;
    cursor: pointer;
  }

  nav button:hover,
  nav button.active {
    background: #f8fafb;
  }

  nav button.active {
    box-shadow: inset 4px 0 #2e6f95;
  }

  nav button strong {
    font-size: 15px;
  }

  nav button span {
    color: #657789;
    font-size: 11px;
  }

  nav button em {
    color: #718392;
    font-size: 10px;
    font-style: normal;
  }

  .market-skeleton {
    height: 46px;
    margin: 1px 0;
    background: linear-gradient(90deg, #e2e8ec, #f7f9fa, #e2e8ec);
    background-size: 240% 100%;
    animation: shimmer 1.4s linear infinite;
  }

  .empty {
    padding: 20px 14px;
    color: #657789;
    font-size: 12px;
    line-height: 1.6;
  }

  .main-stage {
    min-width: 0;
    background: #f8fafb;
  }

  .pair-line {
    display: flex;
    justify-content: space-between;
    align-items: center;
    gap: 18px;
    padding: 22px 24px 18px;
    border-bottom: 1px solid #ccd6dd;
  }

  .eyebrow {
    display: block;
    margin-bottom: 6px;
    color: #657789;
    font-size: 10px;
    font-weight: 650;
    letter-spacing: 0.05em;
  }

  h1 {
    margin: 0;
    font-size: clamp(27px, 3vw, 42px);
    font-weight: 650;
    letter-spacing: -0.02em;
  }

  h1 span {
    margin: 0 8px;
    color: #8a9aa7;
    font-weight: 400;
  }

  .pair-line p {
    margin: 5px 0 0;
    color: #657789;
    font-size: 12px;
  }

  .swap,
  .query-meta button {
    padding: 8px 11px;
    border: 1px solid #9eafbb;
    border-radius: 2px;
    background: #f3f6f8;
    cursor: pointer;
  }

  .swap:disabled,
  button:disabled {
    cursor: not-allowed;
    opacity: 0.45;
  }

  .route-rail {
    display: grid;
    grid-template-columns: 120px minmax(260px, 1fr) 120px;
    align-items: center;
    gap: 20px;
    min-height: 112px;
    padding: 18px 24px;
    border-bottom: 1px solid #aebdc8;
    background: #13243a;
    color: #f0f4f6;
  }

  .route-summary span,
  .route-value span {
    display: block;
    margin-bottom: 7px;
    color: #9fb0bc;
    font-size: 10px;
    font-weight: 650;
    letter-spacing: 0.04em;
  }

  .route-summary strong {
    font-size: 22px;
  }

  .route-summary small {
    display: block;
    margin-top: 7px;
    color: #9fb0bc;
    font-size: 10px;
  }

  .positive {
    color: #4c8876 !important;
  }

  .route-rail .positive {
    color: #72b49e !important;
  }

  .track {
    height: 66px;
    position: relative;
  }

  .track-line {
    position: absolute;
    top: 35px;
    left: 0;
    right: 0;
    height: 1px;
    background:
      linear-gradient(90deg, #c97842 0 49.8%, #7f929f 49.8% 50.2%, #2e6f95 50.2%);
  }

  .zero {
    position: absolute;
    left: 50%;
    top: 43px;
    transform: translateX(-50%);
    color: #8fa0ad;
    font-family: "IBM Plex Mono", monospace;
    font-size: 10px;
  }

  .signal {
    position: absolute;
    top: 25px;
    transform: translateX(-50%);
    transition: left 280ms ease;
  }

  .signal i {
    display: block;
    width: 18px;
    height: 18px;
    border: 4px solid #13243a;
    border-radius: 50%;
    background: #c97842;
    box-shadow: 0 0 0 1px #c97842;
  }

  .signal.positive i {
    background: #72b49e;
    box-shadow: 0 0 0 1px #72b49e;
  }

  .signal output {
    position: absolute;
    left: 50%;
    bottom: 22px;
    transform: translateX(-50%);
    white-space: nowrap;
    font-family: "IBM Plex Mono", monospace;
    font-size: 11px;
  }

  .route-rail.unavailable .track-line {
    background: #617482;
  }

  .route-rail.unavailable .signal i {
    background: #7f929f;
    box-shadow: 0 0 0 1px #7f929f;
  }

  .route-value {
    text-align: right;
  }

  .route-value strong {
    font-family: "IBM Plex Mono", monospace;
    font-size: 17px;
  }

  .query-strip {
    min-height: 52px;
    display: flex;
    justify-content: space-between;
    align-items: center;
    gap: 16px;
    padding: 8px 16px;
    border-bottom: 1px solid #ccd6dd;
  }

  .ranges {
    display: flex;
  }

  .ranges button {
    min-width: 45px;
    padding: 7px 9px;
    border: 1px solid #b6c4ce;
    border-right: 0;
    background: transparent;
    cursor: pointer;
    font-family: "IBM Plex Mono", monospace;
    font-size: 10px;
  }

  .ranges button:last-child {
    border-right: 1px solid #b6c4ce;
  }

  .ranges button.active {
    color: #f8fafb;
    background: #2e536e;
  }

  .query-meta {
    display: flex;
    align-items: center;
    gap: 12px;
    color: #657789;
    font-size: 10px;
  }

  .query-meta button {
    font-family: inherit;
    font-size: 10px;
  }

  .range-stats {
    border-bottom: 1px solid #ccd6dd;
    background: #eef3f6;
  }

  .range-stats > header {
    min-height: 44px;
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 16px;
    padding: 7px 16px;
    border-bottom: 1px solid #d4dde3;
  }

  .range-stats > header > div {
    display: flex;
    align-items: baseline;
    gap: 9px;
  }

  .range-stats > header strong {
    font-size: 12px;
  }

  .range-stats > header span,
  .range-stats label {
    color: #657789;
    font-size: 10px;
  }

  .range-stats label {
    display: flex;
    align-items: center;
    gap: 8px;
  }

  .range-stats select {
    min-width: 104px;
    height: 29px;
    padding: 0 28px 0 9px;
    border: 1px solid #aebdc8;
    border-radius: 0;
    color: #20354a;
    background: #f8fafb;
    font-family: "IBM Plex Mono", monospace;
    font-size: 10px;
  }

  .stats-comparison {
    display: grid;
    grid-template-columns: 1fr 1fr;
  }

  .stats-comparison article {
    min-width: 0;
    display: grid;
    grid-template-rows: 39px auto;
  }

  .stats-comparison article:first-child {
    border-right: 1px solid #ccd6dd;
  }

  .stat-route {
    display: grid;
    grid-template-columns: 4px auto 1fr;
    align-items: center;
    gap: 9px;
    padding: 7px 11px;
    border-bottom: 1px solid #d4dde3;
  }

  .stat-route i {
    width: 4px;
    height: 100%;
    min-height: 24px;
    background: #2e6f95;
  }

  .copper .stat-route i {
    background: #c97842;
  }

  .stat-route strong {
    font-family: "IBM Plex Mono", monospace;
    font-size: 12px;
  }

  .stat-route span {
    justify-self: end;
    color: #718291;
    font-family: "IBM Plex Mono", monospace;
    font-size: 9px;
  }

  .stats-comparison dl {
    min-width: 0;
    display: grid;
    grid-template-columns: repeat(4, minmax(0, 1fr));
    margin: 0;
  }

  .stats-comparison dl > div {
    min-width: 0;
    display: grid;
    align-content: center;
    gap: 4px;
    padding: 10px;
    border-right: 1px solid #d4dde3;
  }

  .stats-comparison dl > div:last-child {
    border-right: 0;
  }

  .stats-comparison dt {
    color: #718291;
    font-size: 9px;
  }

  .stats-comparison dd {
    overflow: hidden;
    margin: 0;
    color: #22384c;
    font-family: "IBM Plex Mono", monospace;
    font-size: 11px;
    font-weight: 700;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .stats-comparison dd.positive {
    color: #287760;
  }

  .chart-panel {
    min-height: 478px;
    border-bottom: 1px solid #ccd6dd;
  }

  .chart-panel > header {
    display: flex;
    align-items: flex-end;
    justify-content: space-between;
    gap: 18px;
    padding: 13px 18px 8px;
  }

  .chart-panel header strong {
    font-size: 13px;
  }

  .chart-title {
    display: grid;
    gap: 3px;
  }

  .chart-title span {
    color: #718392;
    font-size: 9px;
  }

  .chart-title em {
    color: #99603f;
    font-size: 9px;
    font-style: normal;
  }

  .chart-controls {
    display: grid;
    justify-items: end;
    gap: 7px;
  }

  .direction-picker {
    display: flex;
    border: 1px solid #aebdc8;
    border-radius: 2px;
    overflow: hidden;
  }

  .direction-picker button {
    padding: 5px 8px;
    color: #657789;
    border: 0;
    border-right: 1px solid #aebdc8;
    background: #f3f6f8;
    cursor: pointer;
    font-size: 9px;
  }

  .direction-picker button:last-child {
    border-right: 0;
  }

  .direction-picker button.active {
    color: #f8fafb;
    background: #9f572c;
  }

  .legend {
    display: flex;
    flex-wrap: wrap;
    justify-content: flex-end;
    gap: 14px;
    color: #657789;
    font-size: 10px;
  }

  .legend i {
    width: 16px;
    height: 2px;
    display: inline-block;
    margin-right: 6px;
    vertical-align: middle;
  }

  .legend .blue {
    background: #2e6f95;
  }

  .legend .copper {
    background: #c97842;
  }

  .legend .capture {
    height: 3px;
    background: linear-gradient(90deg, #a14942 0 48%, #287760 52% 100%);
  }

  .chart-empty {
    height: 420px;
    display: grid;
    place-content: center;
    gap: 8px;
    color: #657789;
    text-align: center;
  }

  .chart-empty strong {
    color: #2c4053;
  }

  .chart-empty span {
    font-size: 12px;
  }

  .tape {
    display: grid;
    grid-template-columns: repeat(4, minmax(0, 1fr));
  }

  .tape > div {
    min-height: 74px;
    display: grid;
    grid-template-columns: 1fr auto;
    gap: 5px 10px;
    align-content: center;
    padding: 10px 15px;
    border-right: 1px solid #ccd6dd;
  }

  .tape > div:last-child {
    border-right: 0;
  }

  .tape span {
    grid-column: 1 / -1;
    color: #657789;
    font-size: 10px;
  }

  .tape strong {
    font-family: "IBM Plex Mono", monospace;
    font-size: 12px;
  }

  .leg-block {
    padding: 14px;
    border-bottom: 1px solid #aebdc8;
  }

  .leg-block > header {
    display: grid;
    grid-template-columns: 28px minmax(0, 1fr) auto;
    align-items: center;
    gap: 10px;
    margin-bottom: 12px;
  }

  .leg-block > header > span {
    width: 28px;
    height: 28px;
    display: grid;
    place-items: center;
    color: #fff;
    background: #2e6f95;
    font-family: "IBM Plex Mono", monospace;
    font-size: 11px;
  }

  .leg-block.b > header > span {
    background: #c97842;
  }

  .leg-block header strong,
  .leg-block header small {
    display: block;
  }

  .leg-identity {
    min-width: 0;
  }

  .leg-identity strong,
  .leg-identity small {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .leg-identity strong {
    font-size: 14px;
  }

  .leg-identity small {
    margin-top: 2px;
    color: #657789;
    font-size: 10px;
  }

  .book-age {
    min-width: 78px;
    padding-left: 9px;
    border-left: 1px solid #c7d2d9;
    text-align: right;
  }

  .book-age span {
    display: block;
    color: #718392;
    font-size: 8px;
    letter-spacing: 0.03em;
  }

  .book-age strong {
    display: block;
    margin-top: 3px;
    color: #385d72;
    font-family: "IBM Plex Mono", "SFMono-Regular", Consolas, monospace;
    font-size: 9px;
    white-space: nowrap;
  }

  .venue-list {
    display: grid;
    gap: 5px;
  }

  .venue-list button {
    display: flex;
    justify-content: space-between;
    padding: 8px 9px;
    border: 1px solid transparent;
    background: #e4eaee;
    cursor: pointer;
    text-align: left;
  }

  .venue-list button:hover {
    background: #f8fafb;
  }

  .venue-list button.active {
    border-color: #7f96a7;
    background: #f8fafb;
  }

  .venue-list span {
    font-size: 11px;
    font-weight: 650;
  }

  .venue-list small {
    color: #718392;
    font-size: 10px;
  }

  .opportunity-block {
    border-bottom: 1px solid #aebdc8;
  }

  .opportunity-list button {
    width: 100%;
    display: grid;
    grid-template-columns: 18px minmax(0, 1fr) auto;
    align-items: center;
    gap: 8px;
    padding: 10px 12px;
    border: 0;
    border-bottom: 1px solid #d2dbe1;
    background: transparent;
    cursor: pointer;
    text-align: left;
  }

  .opportunity-list button:last-child {
    border-bottom: 0;
  }

  .opportunity-list button:hover,
  .opportunity-list button.active {
    background: #f8fafb;
  }

  .opportunity-list button.active {
    box-shadow: inset 3px 0 #4c8876;
  }

  .opportunity-rank {
    color: #8a9aa7;
    font-family: "IBM Plex Mono", "SFMono-Regular", Consolas, monospace;
    font-size: 9px;
  }

  .opportunity-route,
  .opportunity-value {
    min-width: 0;
  }

  .opportunity-route strong,
  .opportunity-route small,
  .opportunity-value strong,
  .opportunity-value small {
    display: block;
    white-space: nowrap;
  }

  .opportunity-route strong {
    overflow: hidden;
    font-size: 10px;
    text-overflow: ellipsis;
  }

  .opportunity-route i {
    color: #8a9aa7;
    font-style: normal;
    font-weight: 400;
  }

  .opportunity-route small,
  .opportunity-value small {
    margin-top: 3px;
    color: #718392;
    font-size: 8px;
  }

  .opportunity-value {
    padding-left: 8px;
    border-left: 1px solid #ccd6dd;
    text-align: right;
  }

  .opportunity-value strong {
    color: #287760;
    font-family: "IBM Plex Mono", "SFMono-Regular", Consolas, monospace;
    font-size: 9px;
  }

  .opportunity-loading {
    display: grid;
    gap: 1px;
  }

  .opportunity-loading span {
    height: 48px;
    background: linear-gradient(90deg, #e2e8ec, #f7f9fa, #e2e8ec);
    background-size: 240% 100%;
    animation: shimmer 1.4s linear infinite;
  }

  .opportunity-empty,
  .opportunity-note {
    margin: 0;
    color: #657789;
    font-size: 9px;
    line-height: 1.55;
  }

  .opportunity-empty {
    padding: 17px 14px;
  }

  .opportunity-note {
    padding: 8px 12px;
    border-top: 1px dotted #bbc7d0;
  }

  .health-block dl {
    margin: 0;
    padding: 8px 14px 18px;
  }

  .health-block dl div {
    display: flex;
    justify-content: space-between;
    padding: 8px 0;
    border-bottom: 1px dotted #bbc7d0;
  }

  .health-block dt {
    color: #657789;
    font-size: 10px;
  }

  .health-block dd {
    margin: 0;
    font-size: 11px;
    font-weight: 700;
  }

  @keyframes shimmer {
    to {
      background-position: -140% 0;
    }
  }

  @media (max-width: 1180px) {
    .workbench {
      grid-template-columns: 210px minmax(480px, 1fr);
    }

    .leg-panel {
      grid-column: 1 / -1;
      display: grid;
      grid-template-columns: 1fr 1fr;
      border-top: 1px solid #aebdc8;
      border-left: 0;
    }

    .leg-panel > .rail-heading {
      display: none;
    }

    .leg-block {
      border-bottom: 1px solid #aebdc8;
    }

    .leg-block.a,
    .opportunity-block {
      border-right: 1px solid #aebdc8;
    }
  }

  @media (max-width: 760px) {
    .shell {
      width: 100%;
      min-height: 100vh;
      margin: 0;
      border-width: 0;
    }

    .masthead,
    .pair-line,
    .query-strip {
      align-items: flex-start;
      flex-direction: column;
    }

    .masthead {
      padding: 14px;
    }

    .system-line {
      flex-wrap: wrap;
    }

    .workbench {
      display: flex;
      flex-direction: column;
    }

    .market-rail {
      border-right: 0;
      border-bottom: 1px solid #aebdc8;
    }

    nav,
    .market-rail.venue-mode nav {
      max-height: 230px;
    }

    .route-rail {
      grid-template-columns: 1fr;
      gap: 10px;
    }

    .route-value {
      text-align: left;
    }

    .ranges {
      width: 100%;
      overflow-x: auto;
    }

    .ranges button {
      flex: 1 0 45px;
    }

    .query-meta {
      flex-wrap: wrap;
    }

    .range-stats > header {
      align-items: flex-start;
      flex-direction: column;
    }

    .chart-panel > header {
      align-items: flex-start;
      flex-direction: column;
    }

    .legend {
      justify-content: flex-start;
    }

    .stats-comparison {
      grid-template-columns: 1fr;
    }

    .stats-comparison article:first-child {
      border-right: 0;
      border-bottom: 1px solid #ccd6dd;
    }

    .stats-comparison dl {
      grid-template-columns: 1fr 1fr;
    }

    .stats-comparison dl > div:nth-child(-n + 2) {
      border-bottom: 1px solid #d4dde3;
    }

    .stats-comparison dl > div:nth-child(2) {
      border-right: 0;
    }

    .tape {
      grid-template-columns: 1fr 1fr;
    }

    .tape > div:nth-child(2) {
      border-right: 0;
    }

    .tape > div:nth-child(-n + 2) {
      border-bottom: 1px solid #ccd6dd;
    }

    .leg-panel {
      display: block;
    }

    .leg-block {
      border-right: 0;
      border-bottom: 1px solid #aebdc8;
    }

    .opportunity-block {
      border-right: 0;
    }
  }

  @media (prefers-reduced-motion: reduce) {
    .signal,
    .market-skeleton,
    .opportunity-loading span {
      animation: none;
      transition: none;
    }
  }
</style>
