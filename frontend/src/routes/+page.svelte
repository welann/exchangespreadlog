<script lang="ts">
  import { replaceState } from '$app/navigation';
  import { onMount } from 'svelte';
  import SpreadLightweightChart from '$lib/components/SpreadLightweightChart.svelte';
  import type { Market, QuoteRate, SpreadPoint, SpreadResponse } from '$lib/types';

  // --- localStorage cache helpers ---
  const MARKETS_CACHE_KEY = 'spreadlog_markets_v1';
  const SPREAD_CACHE_PREFIX = 'spreadlog_spread_v1_';
  const MARKETS_CACHE_MAX_AGE_MS = 5 * 60 * 1000; // 5 minutes

  function saveMarketsToCache(data: { generatedAt: string; markets: Market[] }) {
    try {
      localStorage.setItem(MARKETS_CACHE_KEY, JSON.stringify(data));
    } catch { /* quota exceeded, ignore */ }
  }

  function loadMarketsFromCache(): { generatedAt: string; markets: Market[] } | null {
    try {
      const raw = localStorage.getItem(MARKETS_CACHE_KEY);
      if (!raw) return null;
      const parsed = JSON.parse(raw);
      const age = Date.now() - new Date(parsed.generatedAt).getTime();
      if (age > MARKETS_CACHE_MAX_AGE_MS) return null;
      return parsed;
    } catch { return null; }
  }

  function saveSpreadToCache(key: string, data: SpreadResponse) {
    try {
      const cacheEntry = { savedAt: Date.now(), data };
      localStorage.setItem(SPREAD_CACHE_PREFIX + key, JSON.stringify(cacheEntry));
    } catch { /* ignore */ }
  }

  function loadSpreadFromCache(key: string): SpreadResponse | null {
    try {
      const raw = localStorage.getItem(SPREAD_CACHE_PREFIX + key);
      if (!raw) return null;
      const parsed = JSON.parse(raw);
      return parsed.data ?? null;
    } catch { return null; }
  }

  const RATE_STORAGE_KEY = 'exchangespreadlog.quoteRates';
  const MAX_SPREAD_CACHE_ENTRIES = 12;
  const SPREAD_CACHE_TTL_MS = 5 * 60 * 1000;
  const LIVE_PAIR_GRACE_MS = 5 * 60 * 1000;
  const LIVE_ANCHOR_INTERVAL_MS = 15 * 1000;
  const MERGE_WINDOW_MS = 30_000; // incremental poll overlap window (handles out-of-order ticks)
  const MAX_POINTS = 10_000;      // cap merged points to prevent unbounded growth
  const presets = [
    { label: '1H', value: '1h', ms: 60 * 60 * 1000 },
    { label: '6H', value: '6h', ms: 6 * 60 * 60 * 1000 },
    { label: '24H', value: '24h', ms: 24 * 60 * 60 * 1000 },
    { label: '7D', value: '7d', ms: 7 * 24 * 60 * 60 * 1000 },
    { label: '30D', value: '30d', ms: 30 * 24 * 60 * 60 * 1000 },
    { label: 'Custom', value: 'custom', ms: 0 }
  ];

  const defaultRates: QuoteRate[] = [
    { from: 'USDC', to: 'USD', rate: '1' },
    { from: 'USDT', to: 'USD', rate: '1' }
  ];

  const averageScopeOptions = [
    { label: '全量', value: 'all' },
    { label: '最大', value: 'top' },
    { label: '最小', value: 'bottom' }
  ] satisfies Array<{ label: string; value: AverageScope }>;
  const averagePercentPresets = [10, 80];

  type QueryState = {
    baseAsset?: string;
    catalogA?: string;
    catalogB?: string;
    preset?: string;
    intervalSeconds?: number;
    fromMs?: number;
    toMs?: number;
  };

  type Opportunity = {
    label: string;
    route: string;
    value: number | null;
    bp: number | null;
    tone: 'positive' | 'negative' | 'neutral';
  };

  type DisplayMode = 'best' | 'both';
  type AverageScope = 'all' | 'top' | 'bottom';
  type SelectionMode = 'market' | 'venue';
  type Instrument = Market['instruments'][number];
  type AverageLine = {
    id: 'best' | 'aToB' | 'bToA';
    label: string;
    value: number;
    tone: 'best' | 'a' | 'b';
    sampleCount: number;
    totalCount: number;
  };
  type VenueOption = {
    venue: string;
    markets: number;
    instruments: number;
  };
  type VenuePairMarket = {
    market: Market;
    instrumentA: Instrument;
    instrumentB: Instrument;
    quoteLabel: string;
  };
  type LoadSpreadOptions = {
    preservePoint?: boolean;
    silent?: boolean;
    slideWindow?: boolean;
    updateUrl?: boolean;
    delta?: boolean;
  };
  type CachedSpread = {
    response: SpreadResponse;
    loadedAt: number;
  };

  type IntervalStats = {
    max: number | null;
    min: number | null;
    avg: number | null;
    volatility: number | null;
    meanReversionMs: number | null;
    windowCount: number;
    positiveShare: number | null;
  };

  let markets: Market[] = [];
  let selectedBase = '';
  let selectedA = '';
  let selectedB = '';
  let selectionMode: SelectionMode = 'market';
  let selectedVenueA = '';
  let selectedVenueB = '';
  let selectedPreset = '24h';
  let chartIntervalSeconds = '';
  let customStart = toDateInput(Date.now() - 60 * 60 * 1000);
  let customEnd = toDateInput(Date.now());
  let rangeAnchorMs = Date.now();
  let rates: QuoteRate[] = structuredClone(defaultRates);
  let hydrated = false;
  let initialLoad = true;
  let showAToB = true;
  let showBToA = true;
  let displayMode: DisplayMode = 'both';
  let averageScope: AverageScope = 'all';
  let averagePercent = '10';
  let autoRefresh = true;
  let refreshSeconds = 15;
  let refreshTimer: ReturnType<typeof setInterval> | null = null;
  let spreadRequestSeq = 0;
  const spreadCache = new Map<string, CachedSpread>();

  let marketError = '';
  let queryError = '';
  let loadingMarkets = false;
  let loadingSpread = false;
  let refreshingSpread = false;
  let spread: SpreadResponse | null = null;
  let selectedIndex = -1;
  let hoverIndex = -1;

  $: currentMarket = markets.find((market) => market.baseAsset === selectedBase);
  $: currentInstruments = currentMarket?.instruments ?? [];
  $: selectedPairIsLive = selectionFollowsLive(currentInstruments, selectedA, selectedB);
  $: venueOptions = buildVenueOptions(markets);
  $: venuePairMarkets = commonMarketsForVenues(markets, selectedVenueA, selectedVenueB);
  $: selectedInstrumentA = currentInstruments.find((instrument) => instrument.catalogId === selectedA) ?? null;
  $: selectedInstrumentB = currentInstruments.find((instrument) => instrument.catalogId === selectedB) ?? null;
  $: selectedRange = currentRange(selectedPreset, customStart, customEnd, rangeAnchorMs);
  $: spreadBusy = loadingSpread || refreshingSpread;
  $: points = spread?.points ?? [];
  $: averagePercentValue = parseAveragePercent(averagePercent);
  $: averageLines = computeAverageLines(
    points,
    displayMode,
    showAToB,
    showBToA,
    averageScope,
    averagePercentValue
  );
  $: averageScopeSummary = averageScopeLabel(averageScope, averagePercentValue);
  $: activeIndex = hoverIndex >= 0 ? hoverIndex : selectedIndex;
  $: activePoint = points[activeIndex] ?? null;
  $: latestPoint = points.length > 0 ? points[points.length - 1] : null;
  $: latestOpportunity = opportunityForPoint(latestPoint);
  $: activeOpportunity = opportunityForPoint(activePoint);
  $: pointRows = pointTableRows(points, activeIndex);
  $: intervalStats = computeIntervalStats(points);
  $: routeCode =
    marketError
      ? 'SETUP'
      : latestOpportunity?.label === 'Sell A / Buy B'
        ? 'A/B'
        : latestOpportunity?.label === 'Sell B / Buy A'
          ? 'B/A'
          : 'WAIT';
  $: spreadStatus = latestOpportunity
    ? latestOpportunity.tone === 'positive'
      ? 'Actionable on latest state sample'
      : latestOpportunity.tone === 'negative'
        ? 'No positive cross on latest state sample'
        : 'Flat at latest state sample'
    : marketError
      ? 'ClickHouse connection is not ready'
      : 'Waiting for comparable samples';

  onMount(() => {
    hydrated = true;
    loadStoredRates();

    // Try loading from localStorage cache first
    const cachedMarkets = loadMarketsFromCache();
    if (cachedMarkets) {
      markets = cachedMarkets.markets ?? [];
      if (markets.length > 0) {
        const queryState = readQueryState();
        applySelectionState(queryState);
        syncVenueSelectionFromSelectedLegs();

        // Try loading cached spread
        if (selectedA && selectedB && selectedA !== selectedB) {
          const range = currentRange(selectedPreset, customStart, customEnd, rangeAnchorMs);
          const spreadPayload = {
            catalogA: selectedA,
            catalogB: selectedB,
            fromMs: range.fromMs,
            toMs: range.toMs,
            ...spreadQueryOptions(range),
            rates: cleanRates(rates)
          };
          const spreadCacheKeyStr = spreadCacheKey(spreadPayload);
          const cachedSpread = loadSpreadFromCache(spreadCacheKeyStr);
          if (cachedSpread) {
            spread = cachedSpread;
            selectedIndex = cachedSpread.points.length > 0 ? cachedSpread.points.length - 1 : -1;
          }
        }
      }
    }

    // Always fetch fresh data
    const queryState = readQueryState();
    void loadMarkets(queryState).finally(() => {
      initialLoad = false;
      configureAutoRefresh();
    });

    return () => {
      stopAutoRefresh();
    };
  });

  async function loadMarkets(state: QueryState | null = captureQueryState()) {
    loadingMarkets = true;
    marketError = '';
    try {
      const response = await fetch('/api/markets');
      const body = await response.json();
      if (!response.ok) throw new Error(body.error ?? 'Failed to load markets');
      saveMarketsToCache(body);
      markets = body.markets ?? [];
      if (markets.length > 0) {
        applySelectionState(state);
        syncVenueSelectionFromSelectedLegs();
        await loadSpread({ slideWindow: true });
      } else {
        marketError = 'No comparable markets were found. Check /api/health for ClickHouse table status.';
      }
    } catch (error) {
      marketError = error instanceof Error ? error.message : 'Failed to load markets';
    } finally {
      loadingMarkets = false;
    }
  }

  async function loadSpread(options: LoadSpreadOptions = {}) {
    const catalogA = selectedA;
    const catalogB = selectedB;
    if (!catalogA || !catalogB || catalogA === catalogB) {
      queryError = 'Choose two different instruments from the same market';
      return;
    }

    if (options.slideWindow && selectedPreset !== 'custom') {
      rangeAnchorMs = anchorForSelection(instrumentsForBase(selectedBase), catalogA, catalogB);
    }

    let range = currentRange(selectedPreset, customStart, customEnd, rangeAnchorMs);
    if (!Number.isFinite(range.fromMs) || !Number.isFinite(range.toMs) || range.fromMs >= range.toMs) {
      queryError = 'Choose a valid time range with From before To';
      return;
    }

    // Delta mode: narrow range to only recent data with merge-window overlap
    if (options.delta && spread && spread.points.length > 0) {
      const lastTs = spread.points[spread.points.length - 1].tsMs;
      range = { fromMs: lastTs - MERGE_WINDOW_MS, toMs: Date.now() };
    }

    // Try loading from localStorage cache before network request (skip for delta)
    let cachedFromStorage: SpreadResponse | null = null;
    if (!options.delta) {
      const localStorageCacheKey = spreadCacheKey({
        catalogA,
        catalogB,
        fromMs: range.fromMs,
        toMs: range.toMs
      });
      cachedFromStorage = loadSpreadFromCache(localStorageCacheKey);
    }
    if (cachedFromStorage && points.length === 0 && !spread) {
      spread = cachedFromStorage;
      selectedIndex = cachedFromStorage.points.length > 0 ? cachedFromStorage.points.length - 1 : -1;
    }

    const requestId = ++spreadRequestSeq;
    const previousSelectedPoint = selectedIndex >= 0 ? points[selectedIndex] : null;
    const wasFollowingLatest = selectedIndex < 0 || selectedIndex >= points.length - 1;
    const payload: Record<string, unknown> = {
      catalogA,
      catalogB,
      fromMs: range.fromMs,
      toMs: range.toMs,
      rates: cleanRates(rates)
    };
    // Delta mode: let server auto-select granularity from short window
    if (!options.delta) {
      Object.assign(payload, spreadQueryOptions(range));
    }
    const cacheKey = spreadCacheKey(payload);
    const cached = options.delta ? null : readSpreadCache(cacheKey);
    const usedCached = cached !== null && !options.silent;
    const hasExistingPoints = points.length > 0;

    if (usedCached) {
      spread = cached.response;
      selectedIndex = nextSelectedIndex(
        cached.response.points,
        previousSelectedPoint?.tsMs ?? null,
        wasFollowingLatest,
        options.preservePoint
      );
      loadingSpread = false;
      refreshingSpread = true;
    } else if (options.silent) {
      refreshingSpread = true;
    } else if (hasExistingPoints) {
      refreshingSpread = true;
    } else {
      loadingSpread = true;
    }
    queryError = '';
    if (!options.preservePoint) {
      hoverIndex = -1;
    }
    try {
      const response = await fetch('/api/spread', {
        method: 'POST',
        headers: { 'content-type': 'application/json' },
        body: JSON.stringify(payload)
      });
      const body = await response.json();
      if (!response.ok) throw new Error(body.error ?? 'Failed to query spread');
      if (requestId !== spreadRequestSeq || selectedA !== catalogA || selectedB !== catalogB) return;
      if (options.delta && spread && spread.points.length > 0) {
        // Incremental merge: deduplicate by tsMs, sort, cap
        const existingTs = new Set(spread.points.map(p => p.tsMs));
        const newPoints = (body as SpreadResponse).points.filter(p => !existingTs.has(p.tsMs));
        const merged = [...spread.points, ...newPoints].sort((a, b) => a.tsMs - b.tsMs);
        const capped = merged.length > MAX_POINTS ? merged.slice(merged.length - MAX_POINTS) : merged;
        spread = { ...(body as SpreadResponse), points: capped };
        selectedIndex = capped.length - 1;
      } else {
        const nextSpread = body as SpreadResponse;
        rememberSpread(cacheKey, nextSpread);
        spread = nextSpread;
        selectedIndex = nextSelectedIndex(
          nextSpread.points,
          previousSelectedPoint?.tsMs ?? null,
          wasFollowingLatest,
          options.preservePoint
        );
      }
      if (options.updateUrl !== false) {
        syncQueryState();
      }
    } catch (error) {
      if (requestId !== spreadRequestSeq || selectedA !== catalogA || selectedB !== catalogB) return;
      if (!usedCached) {
        spread = null;
        selectedIndex = -1;
        queryError = error instanceof Error ? error.message : 'Failed to query spread';
      }
    } finally {
      if (requestId === spreadRequestSeq) {
        loadingSpread = false;
        refreshingSpread = false;
      }
    }
  }

  async function selectBase(baseAsset: string) {
    applySelectionState({ ...captureQueryState(), baseAsset });
    syncVenueSelectionFromSelectedLegs();
    await loadSpreadWhenReady({ slideWindow: true });
  }

  function selectLegA(catalogId: string) {
    selectedA = catalogId;
    if (selectedA === selectedB) {
      selectedB = currentInstruments.find((instrument) => instrument.catalogId !== catalogId)?.catalogId ?? '';
    }
    selectedIndex = -1;
    hoverIndex = -1;
    syncVenueSelectionFromSelectedLegs();
  }

  function selectLegB(catalogId: string) {
    selectedB = catalogId;
    if (selectedA === selectedB) {
      selectedA = currentInstruments.find((instrument) => instrument.catalogId !== catalogId)?.catalogId ?? '';
    }
    selectedIndex = -1;
    hoverIndex = -1;
    syncVenueSelectionFromSelectedLegs();
  }

  function swapLegs() {
    if (!selectedA || !selectedB) return;
    const previousA = selectedA;
    selectedA = selectedB;
    selectedB = previousA;
    selectedIndex = -1;
    hoverIndex = -1;
    syncVenueSelectionFromSelectedLegs();
  }

  async function selectLegAAndQuery(catalogId: string) {
    selectLegA(catalogId);
    await loadSpreadWhenReady({ slideWindow: true });
  }

  async function selectLegBAndQuery(catalogId: string) {
    selectLegB(catalogId);
    await loadSpreadWhenReady({ slideWindow: true });
  }

  async function swapLegsAndQuery() {
    swapLegs();
    await loadSpreadWhenReady({ slideWindow: true });
  }

  function setSelectionMode(mode: SelectionMode) {
    selectionMode = mode;
    if (mode === 'venue') {
      syncVenueSelectionFromSelectedLegs();
    }
  }

  function selectVenueA(venue: string) {
    selectedVenueA = venue;
    if (selectedVenueA === selectedVenueB) {
      selectedVenueB = venueOptions.find((option) => option.venue !== venue)?.venue ?? '';
    }
  }

  function selectVenueB(venue: string) {
    selectedVenueB = venue;
    if (selectedVenueA === selectedVenueB) {
      selectedVenueA = venueOptions.find((option) => option.venue !== venue)?.venue ?? '';
    }
  }

  function swapVenues() {
    if (!selectedVenueA || !selectedVenueB) return;
    const previousA = selectedVenueA;
    selectedVenueA = selectedVenueB;
    selectedVenueB = previousA;
  }

  async function openVenuePairMarket(option: VenuePairMarket) {
    selectedBase = option.market.baseAsset;
    selectedA = option.instrumentA.catalogId;
    selectedB = option.instrumentB.catalogId;
    selectedIndex = -1;
    hoverIndex = -1;
    rangeAnchorMs = anchorForSelection(option.market.instruments, selectedA, selectedB);
    syncVenueSelectionFromSelectedLegs();
    await loadSpreadWhenReady({ slideWindow: true });
  }

  async function loadSpreadWhenReady(options: LoadSpreadOptions = {}) {
    if (!selectedA || !selectedB || selectedA === selectedB) return;
    await loadSpread(options);
  }

  async function refreshCurrentSpread(options: LoadSpreadOptions = {}) {
    if (options.silent && !selectionFollowsLive(currentInstruments, selectedA, selectedB)) return;
    await loadSpreadWhenReady({ slideWindow: true, ...options });
  }

  function toggleAutoRefresh(enabled: boolean) {
    autoRefresh = enabled;
    configureAutoRefresh();
  }

  function updateRefreshSeconds(value: string) {
    const parsed = Number(value);
    refreshSeconds = Number.isFinite(parsed) ? parsed : 15;
    configureAutoRefresh();
  }

  function configureAutoRefresh() {
    stopAutoRefresh();
    if (!autoRefresh) return;
    refreshTimer = setInterval(() => {
      if (!loadingMarkets && !spreadBusy) {
        void refreshCurrentSpread({ preservePoint: true, silent: true, updateUrl: false, delta: true });
      }
    }, refreshSeconds * 1000);
  }

  function stopAutoRefresh() {
    if (refreshTimer) clearInterval(refreshTimer);
    refreshTimer = null;
  }

  function toggleSeries(series: 'aToB' | 'bToA') {
    displayMode = 'both';
    if (series === 'aToB') {
      showAToB = !showAToB;
      if (!showAToB && !showBToA) showBToA = true;
    } else {
      showBToA = !showBToA;
      if (!showAToB && !showBToA) showAToB = true;
    }
  }

  async function toggleSeriesAndQuery(series: 'aToB' | 'bToA') {
    toggleSeries(series);
    if (points.length === 0) await loadSpreadWhenReady({ slideWindow: true });
  }

  function setDisplayMode(mode: DisplayMode) {
    displayMode = mode;
    if (mode === 'both' && !showAToB && !showBToA) {
      showAToB = true;
      showBToA = true;
    }
  }

  async function setDisplayModeAndQuery(mode: DisplayMode) {
    setDisplayMode(mode);
    if (points.length === 0) await loadSpreadWhenReady({ slideWindow: true });
  }

  function selectPoint(index: number) {
    selectedIndex = clamp(index, 0, points.length - 1);
    hoverIndex = -1;
  }

  function jumpPoint(delta: number) {
    if (points.length === 0) return;
    const current = selectedIndex >= 0 ? selectedIndex : points.length - 1;
    selectPoint(current + delta);
  }

  function jumpLatest() {
    if (points.length === 0) return;
    selectPoint(points.length - 1);
  }

  function updateRate(index: number, field: keyof QuoteRate, value: string) {
    rates = rates.map((rate, current) => (current === index ? { ...rate, [field]: value } : rate));
    storeRates();
  }

  function addRate() {
    rates = [...rates, { from: '', to: '', rate: '1' }];
    storeRates();
  }

  function removeRate(index: number) {
    rates = rates.filter((_, current) => current !== index);
    storeRates();
  }

  function resetRates() {
    rates = structuredClone(defaultRates);
    storeRates();
  }

  function loadStoredRates() {
    try {
      const raw = localStorage.getItem(RATE_STORAGE_KEY);
      if (!raw) return;
      const parsed = JSON.parse(raw);
      if (Array.isArray(parsed)) {
        rates = parsed.filter(isQuoteRate);
      }
    } catch {
      rates = structuredClone(defaultRates);
    }
  }

  function storeRates() {
    localStorage.setItem(RATE_STORAGE_KEY, JSON.stringify(rates));
  }

  function isQuoteRate(value: unknown): value is QuoteRate {
    return (
      typeof value === 'object' &&
      value !== null &&
      typeof (value as QuoteRate).from === 'string' &&
      typeof (value as QuoteRate).to === 'string' &&
      typeof (value as QuoteRate).rate === 'string'
    );
  }

  function cleanRates(input: QuoteRate[]): QuoteRate[] {
    return input
      .map((rate) => ({
        from: rate.from.trim(),
        to: rate.to.trim(),
        rate: rate.rate.trim()
      }))
      .filter((rate) => rate.from && rate.to && rate.rate);
  }

  function spreadCacheKey(payload: object) {
    return JSON.stringify(payload);
  }

  function readSpreadCache(key: string): CachedSpread | null {
    const cached = spreadCache.get(key);
    if (!cached) return null;
    if (Date.now() - cached.loadedAt > SPREAD_CACHE_TTL_MS) {
      spreadCache.delete(key);
      return null;
    }
    spreadCache.delete(key);
    spreadCache.set(key, cached);
    return cached;
  }

  function rememberSpread(key: string, response: SpreadResponse) {
    spreadCache.delete(key);
    spreadCache.set(key, { response, loadedAt: Date.now() });
    saveSpreadToCache(key, response);
    while (spreadCache.size > MAX_SPREAD_CACHE_ENTRIES) {
      const oldest = spreadCache.keys().next().value;
      if (typeof oldest !== 'string') break;
      spreadCache.delete(oldest);
    }
  }

  function spreadQueryOptions(range: { fromMs: number; toMs: number }) {
    const intervalSeconds = parseChartInterval(chartIntervalSeconds);
    if (intervalSeconds !== null) {
      return { precision: 'bucket', bucketSeconds: intervalSeconds };
    }
    // Auto-select: let the server pick raw for ≤1min, candle otherwise
    if (range.toMs - range.fromMs <= 60_000) {
      return { precision: 'raw' };
    }
    return {};
  }

  function parseChartInterval(value: string): number | null {
    if (value.trim() === '') return null;
    const parsed = Number(value);
    if (!Number.isFinite(parsed) || parsed <= 0) return null;
    return clamp(Math.trunc(parsed), 1, 3600);
  }

  function currentRange(presetValue: string, start: string, end: string, anchorMs: number) {
    const preset = presets.find((item) => item.value === presetValue);
    if (preset && preset.value !== 'custom') {
      const toMs = Number.isFinite(anchorMs) ? anchorMs : Date.now();
      return { fromMs: toMs - preset.ms, toMs };
    }

    return {
      fromMs: new Date(start).getTime(),
      toMs: new Date(end).getTime()
    };
  }

  function captureQueryState(): QueryState {
    const range = currentRange(selectedPreset, customStart, customEnd, rangeAnchorMs);
    return {
      baseAsset: selectedBase,
      catalogA: selectedA,
      catalogB: selectedB,
      preset: selectedPreset,
      intervalSeconds: parseChartInterval(chartIntervalSeconds) ?? undefined,
      fromMs: range.fromMs,
      toMs: range.toMs
    };
  }

  function applySelectionState(state: QueryState | null = null) {
    const baseAsset =
      markets.find((market) => market.baseAsset === state?.baseAsset)?.baseAsset ??
      markets.find((market) => market.baseAsset === selectedBase)?.baseAsset ??
      markets[0]?.baseAsset ??
      '';
    selectedBase = baseAsset;

    const instruments = markets.find((market) => market.baseAsset === baseAsset)?.instruments ?? [];
    const catalogA =
      instruments.find((instrument) => instrument.catalogId === state?.catalogA)?.catalogId ??
      instruments.find((instrument) => instrument.catalogId === selectedA)?.catalogId ??
      instruments[0]?.catalogId ??
      '';
    selectedA = catalogA;

    const catalogB =
      instruments.find(
        (instrument) => instrument.catalogId === state?.catalogB && instrument.catalogId !== selectedA
      )?.catalogId ??
      instruments.find(
        (instrument) => instrument.catalogId === selectedB && instrument.catalogId !== selectedA
      )?.catalogId ??
      instruments.find((instrument) => instrument.catalogId !== selectedA)?.catalogId ??
      selectedA;
    selectedB = catalogB;

    rangeAnchorMs = anchorForSelection(instruments, selectedA, selectedB);
    selectedPreset = presets.some((preset) => preset.value === state?.preset)
      ? (state?.preset ?? selectedPreset)
      : selectedPreset;
    if (state?.intervalSeconds !== undefined) {
      chartIntervalSeconds = String(state.intervalSeconds);
    }

    if (
      selectedPreset === 'custom' &&
      state?.fromMs !== undefined &&
      state?.toMs !== undefined &&
      Number.isFinite(state.fromMs) &&
      Number.isFinite(state.toMs)
    ) {
      customStart = toDateInput(state.fromMs);
      customEnd = toDateInput(state.toMs);
    } else {
      const range = currentRange(selectedPreset, customStart, customEnd, rangeAnchorMs);
      customStart = toDateInput(range.fromMs);
      customEnd = toDateInput(range.toMs);
    }

    selectedIndex = -1;
    hoverIndex = -1;
  }

  function readQueryState(): QueryState | null {
    if (typeof window === 'undefined') return null;
    const params = new URLSearchParams(window.location.search);
    const state: QueryState = {};
    const baseAsset = params.get('base');
    const catalogA = params.get('a');
    const catalogB = params.get('b');
    const preset = params.get('preset');
    const interval = params.get('interval');
    const fromMs = Number(params.get('from'));
    const toMs = Number(params.get('to'));

    if (baseAsset) state.baseAsset = baseAsset;
    if (catalogA) state.catalogA = catalogA;
    if (catalogB) state.catalogB = catalogB;
    if (preset) state.preset = preset;
    if (interval) {
      const intervalSeconds = parseChartInterval(interval);
      if (intervalSeconds !== null) state.intervalSeconds = intervalSeconds;
    }
    if (Number.isFinite(fromMs)) state.fromMs = fromMs;
    if (Number.isFinite(toMs)) state.toMs = toMs;

    return Object.keys(state).length > 0 ? state : null;
  }

  function syncQueryState() {
    if (typeof window === 'undefined') return;
    const range = currentRange(selectedPreset, customStart, customEnd, rangeAnchorMs);
    const params = new URLSearchParams();
    if (selectedBase) params.set('base', selectedBase);
    if (selectedA) params.set('a', selectedA);
    if (selectedB) params.set('b', selectedB);
    params.set('preset', selectedPreset);
    const intervalSeconds = parseChartInterval(chartIntervalSeconds);
    if (intervalSeconds !== null) params.set('interval', String(intervalSeconds));
    params.set('from', String(Math.trunc(range.fromMs)));
    params.set('to', String(Math.trunc(range.toMs)));
    replaceState(`${window.location.pathname}?${params.toString()}${window.location.hash}`, {});
  }

  function handlePreset(value: string) {
    selectedPreset = value;
    if (value !== 'custom') {
      const range = currentRange(value, customStart, customEnd, rangeAnchorMs);
      customStart = toDateInput(range.fromMs);
      customEnd = toDateInput(range.toMs);
    }
    selectedIndex = -1;
    hoverIndex = -1;
  }

  async function handlePresetAndQuery(value: string) {
    handlePreset(value);
    if (value !== 'custom' && selectedA && selectedB && selectedA !== selectedB) {
      await loadSpread({ slideWindow: true });
    }
  }

  async function handleChartIntervalAndQuery(value: string) {
    const intervalSeconds = parseChartInterval(value);
    chartIntervalSeconds = intervalSeconds === null ? '' : String(intervalSeconds);
    selectedIndex = -1;
    hoverIndex = -1;
    await loadSpreadWhenReady({ slideWindow: true });
  }

  function selectValue(event: Event) {
    return (event.currentTarget as HTMLSelectElement).value;
  }

  function inputValue(event: Event) {
    return (event.currentTarget as HTMLInputElement).value;
  }

  function handleChartNavigation(key: string) {
    if (points.length === 0) return;
    const current = selectedIndex >= 0 ? selectedIndex : points.length - 1;
    if (key === 'ArrowLeft') {
      selectedIndex = clamp(current - 1, 0, points.length - 1);
    }
    if (key === 'ArrowRight') {
      selectedIndex = clamp(current + 1, 0, points.length - 1);
    }
    if (key === 'Home') {
      selectedIndex = 0;
    }
    if (key === 'End') {
      selectedIndex = points.length - 1;
    }
  }

  let lastGranularityChangeMs = 0;
  const GRANULARITY_CHANGE_DEBOUNCE_MS = 500;

  function handleGranularityChange(detail: { fromMs: number; toMs: number }) {
    const now = Date.now();
    if (now - lastGranularityChangeMs < GRANULARITY_CHANGE_DEBOUNCE_MS) return;

    const rangeMs = detail.toMs - detail.fromMs;
    if (rangeMs <= 0) return;

    // Only reload if the range crosses a granularity boundary
    const currentGranularity = spread?.meta.granularity ?? '1h';
    const nextGranularity = granularityForRange(rangeMs);
    if (nextGranularity === currentGranularity) return;

    lastGranularityChangeMs = now;
    customStart = toDateInput(detail.fromMs - 60_000); // pad slightly
    customEnd = toDateInput(detail.toMs + 60_000);
    selectedPreset = 'custom';

    void loadSpread({ silent: true, preservePoint: true });
  }

  /** Client-side mirror of selectGranularity for boundary detection only. */
  function granularityForRange(rangeMs: number): string {
    if (rangeMs <= 60_000) return 'raw';
    if (rangeMs <= 10 * 60_000) return '1s';
    if (rangeMs <= 60 * 60_000) return '1m';
    if (rangeMs <= 6 * 3600_000) return '5m';
    if (rangeMs <= 24 * 3600_000) return '15m';
    return '1h';
  }

  function computeAverageLines(
    data: SpreadPoint[],
    mode: DisplayMode,
    includeAToB: boolean,
    includeBToA: boolean,
    scope: AverageScope,
    percent: number
  ): AverageLine[] {
    return averageSeries(data, mode, includeAToB, includeBToA)
      .map((series) => {
        const { values: seriesValues, ...line } = series;
        const values = averageSubset(seriesValues, scope, percent);
        if (values.length === 0) return null;
        return {
          ...line,
          value: values.reduce((sum, value) => sum + value, 0) / values.length,
          sampleCount: values.length,
          totalCount: seriesValues.length
        };
      })
      .filter((line): line is AverageLine => line !== null);
  }

  function averageSeries(
    data: SpreadPoint[],
    mode: DisplayMode,
    includeAToB: boolean,
    includeBToA: boolean
  ): Array<Omit<AverageLine, 'value' | 'sampleCount' | 'totalCount'> & { values: number[] }> {
    if (mode === 'best') {
      return [
        {
          id: 'best',
          label: 'Best AVG',
          tone: 'best',
          values: data
            .map((point) => bestSpreadValue(point))
            .filter((value): value is number => value !== null && Number.isFinite(value))
        }
      ];
    }

    const series: Array<Omit<AverageLine, 'value' | 'sampleCount' | 'totalCount'> & { values: number[] }> = [];
    if (includeAToB) {
      series.push({
        id: 'aToB',
        label: 'A-B AVG',
        tone: 'a',
        values: data.map((point) => point.aToBBp).filter((value): value is number => value !== null && Number.isFinite(value))
      });
    }
    if (includeBToA) {
      series.push({
        id: 'bToA',
        label: 'B-A AVG',
        tone: 'b',
        values: data.map((point) => point.bToABp).filter((value): value is number => value !== null && Number.isFinite(value))
      });
    }
    return series;
  }

  function averageSubset(values: number[], scope: AverageScope, percent: number) {
    if (scope === 'all') return values;
    const sorted = [...values].sort((left, right) => left - right);
    const count = clamp(Math.ceil(sorted.length * (percent / 100)), 1, sorted.length);
    return scope === 'top' ? sorted.slice(-count) : sorted.slice(0, count);
  }

  function toDateInput(ms: number) {
    const date = new Date(ms);
    date.setMinutes(date.getMinutes() - date.getTimezoneOffset());
    return date.toISOString().slice(0, 16);
  }

  function selectedLabel(catalogId: string) {
    return currentInstruments.find((instrument) => instrument.catalogId === catalogId)?.label ?? '-';
  }

  function buildVenueOptions(inputMarkets: Market[]): VenueOption[] {
    const byVenue = new Map<string, { markets: Set<string>; instruments: number }>();
    inputMarkets.forEach((market) => {
      market.instruments.forEach((instrument) => {
        const venue = instrument.venueInstanceId || 'unknown';
        const current = byVenue.get(venue) ?? { markets: new Set<string>(), instruments: 0 };
        current.markets.add(market.baseAsset);
        current.instruments += 1;
        byVenue.set(venue, current);
      });
    });

    return [...byVenue.entries()]
      .map(([venue, stats]) => ({
        venue,
        markets: stats.markets.size,
        instruments: stats.instruments
      }))
      .sort((left, right) => left.venue.localeCompare(right.venue));
  }

  function commonMarketsForVenues(inputMarkets: Market[], venueA: string, venueB: string): VenuePairMarket[] {
    if (!venueA || !venueB || venueA === venueB) return [];

    return inputMarkets
      .map((market) => {
        const instrumentA = bestInstrumentForVenue(market.instruments, venueA);
        const instrumentB = bestInstrumentForVenue(market.instruments, venueB);
        if (!instrumentA || !instrumentB) return null;
        return {
          market,
          instrumentA,
          instrumentB,
          quoteLabel: quoteLabelForInstruments([instrumentA, instrumentB])
        };
      })
      .filter((value): value is VenuePairMarket => value !== null)
      .sort((left, right) => left.market.baseAsset.localeCompare(right.market.baseAsset));
  }

  function bestInstrumentForVenue(instruments: Instrument[], venue: string): Instrument | null {
    return (
      instruments
        .filter((instrument) => instrument.venueInstanceId === venue)
        .sort(
          (left, right) =>
            (right.latestRecvMs ?? 0) - (left.latestRecvMs ?? 0) ||
            left.label.localeCompare(right.label)
        )[0] ?? null
    );
  }

  function quoteLabelForInstruments(instruments: Instrument[]) {
    const quotes = [...new Set(instruments.map((instrument) => instrument.quoteAsset).filter(Boolean))];
    return quotes.length > 0 ? quotes.join('+') : 'QUOTE';
  }

  function syncVenueSelectionFromSelectedLegs() {
    const options = buildVenueOptions(markets);
    const instruments = markets.find((market) => market.baseAsset === selectedBase)?.instruments ?? [];
    const instrumentA = instruments.find((instrument) => instrument.catalogId === selectedA) ?? null;
    const instrumentB = instruments.find((instrument) => instrument.catalogId === selectedB) ?? null;

    if (instrumentA) selectedVenueA = instrumentA.venueInstanceId;
    if (instrumentB) selectedVenueB = instrumentB.venueInstanceId;

    if (!options.some((option) => option.venue === selectedVenueA)) {
      selectedVenueA = options[0]?.venue ?? '';
    }
    if (!options.some((option) => option.venue === selectedVenueB) || selectedVenueA === selectedVenueB) {
      selectedVenueB = options.find((option) => option.venue !== selectedVenueA)?.venue ?? '';
    }
  }

  function latestForInstruments(instruments: Market['instruments']) {
    const latest = instruments
      .map((instrument) => instrument.latestRecvMs)
      .filter((value): value is number => value !== null && Number.isFinite(value));
    return latest.length > 0 ? Math.max(...latest) : null;
  }

  function instrumentsForBase(baseAsset: string) {
    return markets.find((market) => market.baseAsset === baseAsset)?.instruments ?? [];
  }

  function anchorForSelection(
    instruments: Market['instruments'],
    catalogA: string,
    catalogB: string
  ) {
    const pairLatest = [catalogA, catalogB]
      .map(
        (catalogId) =>
          instruments.find((instrument) => instrument.catalogId === catalogId)?.latestRecvMs ?? null
      )
      .filter((value): value is number => value !== null && Number.isFinite(value));
    const comparableLatest = pairLatest.length > 0 ? Math.min(...pairLatest) : null;
    const globalLatest = latestForInstruments(markets.flatMap((market) => market.instruments));
    if (selectionFollowsLive(instruments, catalogA, catalogB)) {
      return Math.floor(Date.now() / LIVE_ANCHOR_INTERVAL_MS) * LIVE_ANCHOR_INTERVAL_MS;
    }
    return comparableLatest ?? globalLatest ?? Date.now();
  }

  function selectionFollowsLive(
    instruments: Market['instruments'],
    catalogA: string,
    catalogB: string
  ) {
    const pairLatest = [catalogA, catalogB]
      .map(
        (catalogId) =>
          instruments.find((instrument) => instrument.catalogId === catalogId)?.latestRecvMs ?? null
      )
      .filter((value): value is number => value !== null && Number.isFinite(value));
    if (pairLatest.length < 2) return false;
    const globalLatest = latestForInstruments(markets.flatMap((market) => market.instruments));
    return globalLatest !== null && globalLatest - Math.min(...pairLatest) <= LIVE_PAIR_GRACE_MS;
  }

  function marketComparableLatest(market: Market) {
    const latest = market.instruments
      .map((instrument) => instrument.latestRecvMs)
      .filter((value): value is number => value !== null && Number.isFinite(value))
      .sort((left, right) => right - left);
    return latest[1] ?? latest[0] ?? null;
  }

  function marketActivityLabel(market: Market) {
    const latest = marketComparableLatest(market);
    const globalLatest = latestForInstruments(markets.flatMap((item) => item.instruments));
    if (latest === null) return '无样本';
    if (globalLatest !== null && globalLatest - latest <= LIVE_PAIR_GRACE_MS) return '活跃';
    return `截至 ${formatTime(latest)}`;
  }

  function marketPairLabel(market: Market) {
    const quotes = [...new Set(market.instruments.map((instrument) => instrument.quoteAsset).filter(Boolean))];
    if (quotes.length === 0) return `${market.baseAsset}/QUOTE`;
    return `${market.baseAsset}/${quotes.slice(0, 2).join('+')}`;
  }

  function marketVenueLabel(market: Market) {
    const venues = [...new Set(market.instruments.map((instrument) => instrument.venueInstanceId))];
    if (venues.length === 0) return 'No venues';
    if (venues.length <= 2) return venues.join(' · ');
    return `${venues.slice(0, 2).join(' · ')} +${venues.length - 2}`;
  }

  function presetLabel(value: string) {
    return presets.find((preset) => preset.value === value)?.label ?? value;
  }

  function setAverageScope(scope: AverageScope) {
    averageScope = scope;
  }

  function updateAveragePercent(value: string) {
    averagePercent = value;
  }

  function setAveragePercentPreset(value: number) {
    averagePercent = String(value);
    if (averageScope === 'all') averageScope = 'top';
  }

  function parseAveragePercent(value: string) {
    const parsed = Number(value);
    return Number.isFinite(parsed) ? clamp(parsed, 1, 100) : 10;
  }

  function averageScopeLabel(scope: AverageScope, percent: number) {
    if (scope === 'all') return '全量平均';
    return `${scope === 'top' ? '最大' : '最小'} ${formatPercentNumber(percent)}% 平均`;
  }

  function formatPercentNumber(value: number) {
    return new Intl.NumberFormat(undefined, {
      maximumFractionDigits: Number.isInteger(value) ? 0 : 1
    }).format(value);
  }

  function opportunityForPoint(point: SpreadPoint | null): Opportunity | null {
    if (!point) return null;
    const aBp = point.aToBBp ?? Number.NEGATIVE_INFINITY;
    const bBp = point.bToABp ?? Number.NEGATIVE_INFINITY;
    if (aBp === Number.NEGATIVE_INFINITY && bBp === Number.NEGATIVE_INFINITY) return null;

    const useA = aBp >= bBp;
    const bp = useA ? point.aToBBp : point.bToABp;
    const value = useA ? point.aToB : point.bToA;
    const tone = bp === null || Math.abs(bp) < 0.01 ? 'neutral' : bp > 0 ? 'positive' : 'negative';

    return {
      label: useA ? 'Sell A / Buy B' : 'Sell B / Buy A',
      route: useA
        ? `${selectedLabel(selectedA)} bid minus ${selectedLabel(selectedB)} ask`
        : `${selectedLabel(selectedB)} bid minus ${selectedLabel(selectedA)} ask`,
      value,
      bp,
      tone
    };
  }

  function bestSpreadValue(point: SpreadPoint | null) {
    return opportunityForPoint(point)?.bp ?? null;
  }

  function bestBpValue(point: SpreadPoint | null) {
    return opportunityForPoint(point)?.bp ?? null;
  }

  function computeIntervalStats(data: SpreadPoint[]): IntervalStats {
    const samples = data
      .map((point) => ({ tsMs: point.tsMs, bp: bestBpValue(point) }))
      .filter((sample): sample is { tsMs: number; bp: number } => sample.bp !== null && Number.isFinite(sample.bp));

    if (samples.length === 0) {
      return {
        max: null,
        min: null,
        avg: null,
        volatility: null,
        meanReversionMs: null,
        windowCount: 0,
        positiveShare: null
      };
    }

    const values = samples.map((sample) => sample.bp);
    const avg = values.reduce((sum, value) => sum + value, 0) / values.length;
    const variance = values.reduce((sum, value) => sum + (value - avg) ** 2, 0) / values.length;
    let windowCount = 0;
    let positiveCount = 0;
    let previousPositive = false;
    let runStart: number | null = null;
    const runDurations: number[] = [];

    samples.forEach((sample, index) => {
      const positive = sample.bp > 0;
      if (positive) positiveCount += 1;
      if (positive && !previousPositive) {
        windowCount += 1;
        runStart = sample.tsMs;
      }
      if (!positive && previousPositive && runStart !== null) {
        runDurations.push(samples[index - 1].tsMs - runStart);
        runStart = null;
      }
      previousPositive = positive;
    });

    if (previousPositive && runStart !== null) {
      runDurations.push(samples[samples.length - 1].tsMs - runStart);
    }

    return {
      max: Math.max(...values),
      min: Math.min(...values),
      avg,
      volatility: Math.sqrt(variance),
      meanReversionMs:
        runDurations.length > 0
          ? runDurations.reduce((sum, value) => sum + value, 0) / runDurations.length
          : null,
      windowCount,
      positiveShare: positiveCount / samples.length
    };
  }

  function pointTableRows(data: SpreadPoint[], active: number) {
    if (data.length === 0) return [];
    const limit = 9;
    const anchor = active >= 0 ? active : data.length - 1;
    const start = clamp(anchor - Math.floor(limit / 2), 0, Math.max(0, data.length - limit));
    return data.slice(start, start + limit).map((point, offset) => ({
      point,
      index: start + offset
    }));
  }

  function nextSelectedIndex(
    data: SpreadPoint[],
    previousTsMs: number | null,
    wasFollowingLatest: boolean,
    preservePoint = false
  ) {
    if (data.length === 0) return -1;
    if (!preservePoint || wasFollowingLatest || previousTsMs === null) return data.length - 1;

    let best = 0;
    let distance = Number.POSITIVE_INFINITY;
    data.forEach((point, index) => {
      const currentDistance = Math.abs(point.tsMs - previousTsMs);
      if (currentDistance < distance) {
        best = index;
        distance = currentDistance;
      }
    });
    return best;
  }

  function formatDuration(ms: number) {
    const seconds = Math.round(ms / 1000);
    if (seconds < 60) return `${seconds}s`;
    const minutes = Math.round(seconds / 60);
    if (minutes < 60) return `${minutes}m`;
    const hours = Math.round(minutes / 60);
    if (hours < 48) return `${hours}h`;
    return `${Math.round(hours / 24)}d`;
  }

  function formatMaybeDuration(ms: number | null) {
    if (ms === null || !Number.isFinite(ms)) return '-';
    return formatDuration(ms);
  }

  function formatInteger(value: number) {
    return new Intl.NumberFormat(undefined, { maximumFractionDigits: 0 }).format(value);
  }

  function formatNumber(value: number | null, digits = 6) {
    if (value === null || !Number.isFinite(value)) return '-';
    const maximumFractionDigits = Math.abs(value) >= 100 ? 2 : Math.abs(value) >= 1 ? 4 : digits;
    return new Intl.NumberFormat(undefined, {
      maximumFractionDigits
    }).format(value);
  }

  function formatDepthLevel(sizeText: string | null, size: number | null, orderCount: number | null) {
    const sizeLabel = sizeText ?? formatNumber(size, 8);
    if (sizeLabel === '-') return '-';
    if (orderCount === null || !Number.isFinite(orderCount)) return sizeLabel;
    return `${sizeLabel} · ${formatInteger(orderCount)} ord`;
  }

  function sampleLabel(meta: SpreadResponse['meta'] | undefined) {
    if (!meta) return '-';
    if (meta.granularity === 'raw') {
      return `${formatInteger(meta.sourceRows)} raw ticks`;
    }
    if (meta.granularity === 'bucket') {
      return `${meta.bucketSeconds}s snapshot`;
    }
    return `${meta.granularity} candle`;
  }

  function formatBp(value: number | null) {
    if (value === null || !Number.isFinite(value)) return '-';
    return `${new Intl.NumberFormat(undefined, {
      minimumFractionDigits: 2,
      maximumFractionDigits: 2
    }).format(value)} bp`;
  }

  function formatSignedBp(value: number | null) {
    if (value === null || !Number.isFinite(value)) return '-';
    const formatted = new Intl.NumberFormat(undefined, {
      minimumFractionDigits: 2,
      maximumFractionDigits: 2
    }).format(value);
    return `${value > 0 ? '+' : ''}${formatted} bps`;
  }

  function formatPercent(value: number | null) {
    if (value === null || !Number.isFinite(value)) return '-';
    return `${new Intl.NumberFormat(undefined, {
      minimumFractionDigits: 1,
      maximumFractionDigits: 1
    }).format(value * 100)}%`;
  }

  function formatTime(ms: number | null) {
    if (ms === null || !Number.isFinite(ms)) return '-';
    return new Intl.DateTimeFormat(undefined, {
      month: '2-digit',
      day: '2-digit',
      hour: '2-digit',
      minute: '2-digit',
      second: '2-digit'
    }).format(new Date(ms));
  }

  function formatAxisTime(ms: number | null) {
    if (ms === null || !Number.isFinite(ms)) return '-';
    return new Intl.DateTimeFormat(undefined, {
      hour: '2-digit',
      minute: '2-digit'
    }).format(new Date(ms));
  }

  function clamp(value: number, min: number, max: number) {
    return Math.min(max, Math.max(min, value));
  }
</script>

<svelte:head>
  <title>SpreadDesk - Cross Exchange Spread Monitor</title>
  <meta name="description" content="Monitor cross-venue BBO spread curves from ClickHouse." />
  <meta name="theme-color" content="#090b0f" />
</svelte:head>

<a class="skip-link" href="#spread-main">Skip to main content</a>

<main id="spread-main" class="desk-shell">
  <header class="topbar">
    <div class="brand-lockup">
      <div>
        <strong>SPREADDESK</strong>
        <span>跨交易所价差监控</span>
      </div>
    </div>
    <div class="topbar-meta" aria-live="polite">
      <span>数据源: ClickHouse</span>
      <span>{points.length > 0 ? `${formatInteger(points.length)} samples / ${sampleLabel(spread?.meta)}` : loadingMarkets ? '读取市场中' : '等待样本'}</span>
      <span>
        {refreshingSpread
          ? '同步当前组合中'
          : !selectedPairIsLive
            ? '历史快照'
            : autoRefresh
              ? `实时 ${refreshSeconds}s`
              : '实时已暂停'}
      </span>
      <span class="route-badge">{routeCode}</span>
      <button
        type="button"
        on:click={() => void refreshCurrentSpread()}
        disabled={loadingMarkets || spreadBusy || currentInstruments.length < 2}
      >
        {spreadBusy ? '同步中' : '更新当前'}
      </button>
    </div>
  </header>

  {#if marketError || queryError}
    <div class="notice-stack" aria-live="assertive">
      {#if marketError}
        <section class="notice error" role="alert">{marketError}</section>
      {/if}
      {#if queryError}
        <section class="notice error" role="alert">{queryError}</section>
      {/if}
    </div>
  {/if}

  <div class="desk-layout">
    <aside class="market-sidebar" aria-label="监控交易对">
      <section class="sidebar-block mode-block">
        <div class="sidebar-heading">
          <span>选择方式</span>
          <strong>{selectionMode === 'market' ? '交易对' : '交易所'}</strong>
        </div>
        <div class="mode-switch" aria-label="选择列表维度">
          <button
            type="button"
            class:active={selectionMode === 'market'}
            aria-pressed={selectionMode === 'market'}
            on:click={() => setSelectionMode('market')}
          >
            按交易对
          </button>
          <button
            type="button"
            class:active={selectionMode === 'venue'}
            aria-pressed={selectionMode === 'venue'}
            on:click={() => setSelectionMode('venue')}
          >
            按交易所
          </button>
        </div>
      </section>

      {#if selectionMode === 'market'}
        <section class="sidebar-block">
          <div class="sidebar-heading">
            <span>监控交易对</span>
            <strong>{formatInteger(markets.length)}</strong>
          </div>

          <div class="market-list">
            {#if loadingMarkets && markets.length === 0}
              <div class="skeleton-sidebar">
                {#each Array(8) as _}
                  <div class="skeleton skeleton-row"></div>
                {/each}
              </div>
            {:else if markets.length === 0}
              <p class="sidebar-empty">没有可比较的交易对。</p>
            {:else}
              {#each markets as market}
                <button
                  type="button"
                  class:active={market.baseAsset === selectedBase}
                  aria-pressed={market.baseAsset === selectedBase}
                  on:click={() => void selectBase(market.baseAsset)}
                >
                  <span class="market-main">
                    <strong>{marketPairLabel(market)}</strong>
                    <em>{marketVenueLabel(market)}</em>
                  </span>
                  <span class="market-meta">
                    <span>{formatInteger(market.instruments.length)} venues</span>
                    <span>{marketActivityLabel(market)}</span>
                  </span>
                </button>
              {/each}
            {/if}
          </div>
        </section>

        <details class="sidebar-details" open>
          <summary>交易所组合</summary>
          <div class="leg-picker">
            <div class="selected-legs" aria-label="当前 A/B 组合">
              <div>
                <span>A</span>
                <strong>{selectedLabel(selectedA)}</strong>
              </div>
              <button
                class="swap-button"
                type="button"
                disabled={!selectedA || !selectedB || selectedA === selectedB}
                on:click={() => void swapLegsAndQuery()}
              >
                交换
              </button>
              <div>
                <span>B</span>
                <strong>{selectedLabel(selectedB)}</strong>
              </div>
            </div>

            <div class="leg-choice-list" aria-label="选择交易所腿">
              {#if currentInstruments.length < 2}
                <p class="sidebar-empty">当前交易对没有足够的交易所可比较。</p>
              {:else}
                {#each currentInstruments as instrument}
                  <article
                    class:selected={instrument.catalogId === selectedA || instrument.catalogId === selectedB}
                    class:a-selected={instrument.catalogId === selectedA}
                    class:b-selected={instrument.catalogId === selectedB}
                  >
                    <div class="leg-choice-main">
                      <strong>{instrument.venueInstanceId}</strong>
                      <span>{instrument.rawSymbol}/{instrument.quoteAsset}</span>
                    </div>
                    <div class="leg-choice-actions">
                      <button
                        type="button"
                        class:active={instrument.catalogId === selectedA}
                        disabled={instrument.catalogId === selectedB}
                        aria-pressed={instrument.catalogId === selectedA}
                        on:click={() => void selectLegAAndQuery(instrument.catalogId)}
                      >
                        A
                      </button>
                      <button
                        type="button"
                        class:active={instrument.catalogId === selectedB}
                        disabled={instrument.catalogId === selectedA}
                        aria-pressed={instrument.catalogId === selectedB}
                        on:click={() => void selectLegBAndQuery(instrument.catalogId)}
                      >
                        B
                      </button>
                    </div>
                  </article>
                {/each}
              {/if}
            </div>
          </div>
        </details>
      {:else}
        <section class="sidebar-block">
          <div class="sidebar-heading">
            <span>交易所对</span>
            <strong>{formatInteger(venueOptions.length)}</strong>
          </div>

          <div class="venue-selector">
            <label>
              <span>Exchange A</span>
              <select
                name="venue-a"
                value={selectedVenueA}
                disabled={venueOptions.length < 2}
                on:change={(event) => selectVenueA(selectValue(event))}
              >
                {#each venueOptions as option}
                  <option value={option.venue}>{option.venue}</option>
                {/each}
              </select>
            </label>

            <button
              class="swap-button"
              type="button"
              disabled={!selectedVenueA || !selectedVenueB || selectedVenueA === selectedVenueB}
              on:click={swapVenues}
            >
              交换交易所
            </button>

            <label>
              <span>Exchange B</span>
              <select
                name="venue-b"
                value={selectedVenueB}
                disabled={venueOptions.length < 2}
                on:change={(event) => selectVenueB(selectValue(event))}
              >
                {#each venueOptions as option}
                  <option value={option.venue}>{option.venue}</option>
                {/each}
              </select>
            </label>
          </div>
        </section>

        <section class="sidebar-block">
          <div class="sidebar-heading">
            <span>共同交易对</span>
            <strong>{formatInteger(venuePairMarkets.length)}</strong>
          </div>

          <div class="exchange-market-list">
            {#if loadingMarkets && markets.length === 0}
              <div class="skeleton-sidebar">
                {#each Array(8) as _}
                  <div class="skeleton skeleton-row"></div>
                {/each}
              </div>
            {:else if !selectedVenueA || !selectedVenueB || selectedVenueA === selectedVenueB}
              <p class="sidebar-empty">请选择两个不同的交易所。</p>
            {:else if venuePairMarkets.length === 0}
              <p class="sidebar-empty">这两个交易所当前没有共同交易对。</p>
            {:else}
              {#each venuePairMarkets as option}
                <button
                  type="button"
                  class:active={selectedBase === option.market.baseAsset && selectedA === option.instrumentA.catalogId && selectedB === option.instrumentB.catalogId}
                  aria-pressed={selectedBase === option.market.baseAsset && selectedA === option.instrumentA.catalogId && selectedB === option.instrumentB.catalogId}
                  on:click={() => void openVenuePairMarket(option)}
                >
                  <span class="market-main">
                    <strong>{option.market.baseAsset}/{option.quoteLabel}</strong>
                    <em>{formatInteger(option.market.instruments.length)} venues</em>
                  </span>
                  <span class="market-meta">
                    <span>{option.instrumentA.rawSymbol} vs {option.instrumentB.rawSymbol}</span>
                  </span>
                </button>
              {/each}
            {/if}
          </div>
        </section>
      {/if}

      <details class="sidebar-details">
        <summary>换算率 / 刷新</summary>
        <div class="rate-stack">
          {#each rates as rate, index (index)}
            <div class="rate-row">
              <input
                aria-label="Quote from"
                name={`quote-from-${index}`}
                autocomplete="off"
                value={rate.from}
                placeholder="USDC"
                on:input={(event) => updateRate(index, 'from', inputValue(event))}
              />
              <input
                aria-label="Quote to"
                name={`quote-to-${index}`}
                autocomplete="off"
                value={rate.to}
                placeholder="USD"
                on:input={(event) => updateRate(index, 'to', inputValue(event))}
              />
              <input
                aria-label="Quote rate"
                name={`quote-rate-${index}`}
                value={rate.rate}
                inputmode="decimal"
                placeholder="1"
                on:input={(event) => updateRate(index, 'rate', inputValue(event))}
              />
              <button type="button" aria-label="Remove quote rate" on:click={() => removeRate(index)}>×</button>
            </div>
          {/each}
          <div class="detail-actions">
            <button type="button" on:click={addRate}>新增</button>
            <button type="button" on:click={resetRates}>重置</button>
          </div>
          <label class="checkbox-line">
            <input
              type="checkbox"
              name="auto-refresh"
              checked={autoRefresh}
              on:change={(event) => toggleAutoRefresh((event.currentTarget as HTMLInputElement).checked)}
            />
            自动更新当前交易对
          </label>
          <label>
            <span>刷新间隔</span>
            <select
              name="refresh-interval"
              aria-label="Auto refresh interval"
              value={String(refreshSeconds)}
              disabled={!autoRefresh}
              on:change={(event) => updateRefreshSeconds(selectValue(event))}
            >
              <option value="5">5s</option>
              <option value="15">15s</option>
              <option value="30">30s</option>
              <option value="60">60s</option>
            </select>
          </label>
        </div>
      </details>

    </aside>

    <section class="main-panel" aria-label="价差曲线">
      <div class="pair-header">
        <div>
          <h1>{selectedBase || 'No market'} <span>/ {spread?.meta.targetQuote ?? selectedInstrumentA?.quoteAsset ?? '-'}</span></h1>
          <p>
            <i class="leg-dot a"></i>{selectedLabel(selectedA)}
            <span class="versus">versus</span>
            <i class="leg-dot b"></i>{selectedLabel(selectedB)}
          </p>
        </div>
        <div class="latest-card" class:positive={latestOpportunity?.tone === 'positive'} class:negative={latestOpportunity?.tone === 'negative'}>
          <span>Latest best</span>
          <strong>{formatSignedBp(latestOpportunity?.bp ?? null)}</strong>
          <small>{spreadStatus}</small>
        </div>
      </div>

      <div
        class="route-ribbon"
        class:positive={activeOpportunity?.tone === 'positive'}
        class:negative={activeOpportunity?.tone === 'negative'}
        aria-live="polite"
      >
        <div>
          <span>Route at crosshair</span>
          <strong>{activeOpportunity?.label ?? '等待可比较盘口'}</strong>
        </div>
        <output>{formatSignedBp(activeOpportunity?.bp ?? null)}</output>
        <small>{activePoint ? formatTime(activePoint.tsMs) : '移动十字光标查看任意样本'}</small>
      </div>

      <div class="control-strip trading-toolbar query-toolbar">
        <div class="toolbar-cluster range-cluster">
          <span class="toolbar-label">时间</span>
          <div class="segmented preset-tabs" aria-label="时间范围">
            {#each presets as preset}
              <button
                type="button"
                class:active={selectedPreset === preset.value}
                aria-pressed={selectedPreset === preset.value}
                on:click={() => void handlePresetAndQuery(preset.value)}
              >
                {preset.label}
              </button>
            {/each}
          </div>
        </div>

        <label class="toolbar-cluster interval-cluster">
          <span class="toolbar-label">Interval</span>
          <input
            list="chart-interval-options"
            name="chart-interval"
            aria-label="Chart interval in seconds"
            type="number"
            min="1"
            max="3600"
            step="1"
            value={chartIntervalSeconds}
            placeholder="Auto"
            on:change={(event) => void handleChartIntervalAndQuery(inputValue(event))}
          />
          <span class="interval-unit">s</span>
          <datalist id="chart-interval-options">
            <option value="1"></option>
            <option value="5"></option>
            <option value="15"></option>
            <option value="60"></option>
            <option value="300"></option>
            <option value="900"></option>
            <option value="3600"></option>
          </datalist>
        </label>

        <div class="toolbar-cluster mode-cluster">
          <span class="toolbar-label">视图</span>
          <div class="segmented display-mode" aria-label="曲线显示方式">
            <button
              type="button"
              class:active={displayMode === 'best'}
              aria-pressed={displayMode === 'best'}
              on:click={() => void setDisplayModeAndQuery('best')}
            >
              单边最大
            </button>
            <button
              type="button"
              class:active={displayMode === 'both'}
              aria-pressed={displayMode === 'both'}
              on:click={() => void setDisplayModeAndQuery('both')}
            >
              双边价差
            </button>
          </div>
        </div>

        <button
          class="primary-button"
          type="button"
          disabled={spreadBusy || loadingMarkets || currentInstruments.length < 2}
          on:click={() => void loadSpread({ slideWindow: true })}
        >
          {spreadBusy ? '同步中' : '查询'}
        </button>
      </div>

      <div class="chart-options trading-toolbar tools-toolbar">
        {#if displayMode === 'both'}
          <div class="toolbar-cluster series-cluster">
            <span class="toolbar-label">方向</span>
            <div class="series-pills" aria-label="双边方向">
              <button type="button" class:active={showAToB} aria-pressed={showAToB} on:click={() => void toggleSeriesAndQuery('aToB')}>
                A bid - B ask
              </button>
              <button type="button" class:active={showBToA} aria-pressed={showBToA} on:click={() => void toggleSeriesAndQuery('bToA')}>
                B bid - A ask
              </button>
            </div>
          </div>
        {/if}

        <div class="toolbar-cluster average-control" aria-label="平均线口径">
          <span class="toolbar-label">平均线</span>
          <div class="segmented compact" aria-label="平均线取样范围">
            {#each averageScopeOptions as option}
              <button
                type="button"
                class:active={averageScope === option.value}
                aria-pressed={averageScope === option.value}
                on:click={() => setAverageScope(option.value)}
              >
                {option.label}
              </button>
            {/each}
          </div>
          <label class="percent-input" class:disabled={averageScope === 'all'}>
            <input
              type="number"
              min="1"
              max="100"
              step="1"
              inputmode="decimal"
              aria-label="平均线百分比"
              value={averagePercent}
              disabled={averageScope === 'all'}
              on:input={(event) => updateAveragePercent(inputValue(event))}
            />
            <span>%</span>
          </label>
          <div class="percent-presets" aria-label="平均线百分比快捷值">
            {#each averagePercentPresets as percent}
              <button
                type="button"
                class:active={averageScope !== 'all' && Math.abs(averagePercentValue - percent) < 0.001}
                aria-pressed={averageScope !== 'all' && Math.abs(averagePercentValue - percent) < 0.001}
                on:click={() => setAveragePercentPreset(percent)}
              >
                {percent}%
              </button>
            {/each}
          </div>
        </div>
      </div>

      {#if selectedPreset === 'custom'}
        <div class="custom-range">
          <label>
            <span>From</span>
            <input type="datetime-local" name="spread-from" autocomplete="off" bind:value={customStart} />
          </label>
          <label>
            <span>To</span>
            <input type="datetime-local" name="spread-to" autocomplete="off" bind:value={customEnd} />
          </label>
        </div>
      {/if}

      <section class="chart-shell">
        <div class="chart-heading">
          <div>
            <span>{presetLabel(selectedPreset)} 区间</span>
            <strong>{formatTime(selectedRange.fromMs)} - {formatTime(selectedRange.toMs)}</strong>
          </div>
          <div>
            <span>目标报价</span>
            <strong>{spread?.meta.targetQuote ?? selectedInstrumentA?.quoteAsset ?? '-'}</strong>
          </div>
          <div>
            <span>采样</span>
            <strong>{sampleLabel(spread?.meta)}</strong>
          </div>
          <div>
            <span>平均线</span>
            <strong>{averageScopeSummary}</strong>
          </div>
        </div>

        <p id="chart-help" class="chart-help">
          鼠标悬停或点击可锁定任意一点；聚焦图表后可用 Left / Right / Home / End 查看样本。
        </p>

        {#if loadingSpread && points.length === 0}
          <div class="skeleton-chart">
            <div class="skeleton" style="width: 100%; height: 100%; border-radius: var(--radius);"></div>
          </div>
        {:else if points.length === 0}
          <div class="empty-state">当前组合没有可比较的盘口状态样本。请调整交易所腿或时间范围。</div>
        {:else}
          <SpreadLightweightChart
            {points}
            {displayMode}
            {showAToB}
            {showBToA}
            {averageLines}
            {selectedIndex}
            labelA={selectedLabel(selectedA)}
            labelB={selectedLabel(selectedB)}
            on:hover={(event) => (hoverIndex = event.detail.index)}
            on:select={(event) => (selectedIndex = event.detail.index)}
            on:navigate={(event) => handleChartNavigation(event.detail.key)}
            on:granularityChange={(event) => handleGranularityChange(event.detail)}
          />
        {/if}

        <p class="chart-caption">
          {#if spread?.meta.granularity === 'raw'}
            当前短窗口使用数据库逐 tick BBO 更新计算价差；任一侧更新时都会与另一侧最后有效盘口对齐，未变化的一侧会持续沿用。
          {:else if spread?.meta.granularity === 'bucket'}
            每个 bucket 展示结束时刻的 A/B 最新有效盘口，纵轴统一为 bp；盘口会持续沿用到该腿出现新状态，质量异常数据会被跳过。
          {:else}
            预聚合 OHLC candle 视图（{spread?.meta.granularity ?? '-'}），纵轴统一为 bp；每个数据点展示该粒度下最新有效盘口。
          {/if}
        </p>
      </section>

      <section class="tape-panel" aria-label="附近样本">
        <div class="section-heading">
          <span>Spread tape</span>
          <strong>{pointRows.length > 0 ? `${pointRows[0].index + 1}-${pointRows[pointRows.length - 1].index + 1}` : '0'} / {points.length}</strong>
        </div>
        <div class="tape-grid">
          {#each pointRows as row}
            {@const rowOpportunity = opportunityForPoint(row.point)}
            <button
              type="button"
              class:active={row.index === activeIndex}
              aria-pressed={row.index === activeIndex}
              on:click={() => selectPoint(row.index)}
            >
              <span>{formatAxisTime(row.point.tsMs)}</span>
              <strong class:positive={rowOpportunity?.tone === 'positive'} class:negative={rowOpportunity?.tone === 'negative'}>
                {formatSignedBp(rowOpportunity?.bp ?? null)}
              </strong>
              <small>{rowOpportunity?.label ?? '-'}</small>
            </button>
          {/each}
        </div>
      </section>

      <section class="venue-panel" aria-label="交易所数据质量">
        <div class="section-heading">
          <span>Venue legs</span>
          <strong>{formatInteger(currentInstruments.length)} venues</strong>
        </div>
        <div class="venue-grid">
          {#each currentInstruments as instrument}
            <article class:selected={instrument.catalogId === selectedA || instrument.catalogId === selectedB}>
              <div>
                <strong>{instrument.venueInstanceId}</strong>
                <span>{instrument.rawSymbol}</span>
              </div>
              <dl>
                <div><dt>Quote</dt><dd>{instrument.quoteAsset}</dd></div>
                <div><dt>Latest</dt><dd>{formatTime(instrument.latestRecvMs)}</dd></div>
              </dl>
              <div class="venue-role">
                {#if instrument.catalogId === selectedA}
                  <span>A leg</span>
                {:else if instrument.catalogId === selectedB}
                  <span>B leg</span>
                {:else}
                  <span>Available</span>
                {/if}
              </div>
            </article>
          {/each}
        </div>
      </section>
    </section>

    <aside class="stats-sidebar" aria-label="区间统计">
      <section class="stats-card">
        <div class="stats-heading">
          <span>区间统计</span>
          <strong>{presetLabel(selectedPreset)}</strong>
        </div>
        <dl class="stat-list">
          <div>
            <dt>最大价差</dt>
            <dd class:positive={intervalStats.max !== null && intervalStats.max > 0} class:negative={intervalStats.max !== null && intervalStats.max < 0}>
              {formatSignedBp(intervalStats.max)}
            </dd>
          </div>
          <div>
            <dt>最小价差</dt>
            <dd class:positive={intervalStats.min !== null && intervalStats.min > 0} class:negative={intervalStats.min !== null && intervalStats.min < 0}>
              {formatSignedBp(intervalStats.min)}
            </dd>
          </div>
          <div>
            <dt>平均价差</dt>
            <dd class:positive={intervalStats.avg !== null && intervalStats.avg > 0} class:negative={intervalStats.avg !== null && intervalStats.avg < 0}>
              {formatSignedBp(intervalStats.avg)}
            </dd>
          </div>
          <div>
            <dt>波动率 σ</dt>
            <dd>{formatSignedBp(intervalStats.volatility)}</dd>
          </div>
          <div>
            <dt>平均回归时间</dt>
            <dd>{formatMaybeDuration(intervalStats.meanReversionMs)}</dd>
          </div>
          <div>
            <dt>套利窗口次数</dt>
            <dd>{formatInteger(intervalStats.windowCount)} 次</dd>
          </div>
          <div>
            <dt>正价差占比</dt>
            <dd>{formatPercent(intervalStats.positiveShare)}</dd>
          </div>
        </dl>
      </section>

      <section class="point-card">
        <div class="stats-heading">
          <span>当前样本</span>
          <strong>{activePoint ? formatTime(activePoint.tsMs) : '-'}</strong>
        </div>
        {#if activePoint && spread}
          <dl class="point-ledger">
            <div><dt>Best route</dt><dd>{activeOpportunity?.label ?? '-'}</dd></div>
            <div><dt>Best bp</dt><dd>{formatSignedBp(activeOpportunity?.bp ?? null)}</dd></div>
            <div><dt>A -> B</dt><dd>{formatNumber(activePoint.aToB)} {spread.meta.targetQuote}</dd></div>
            <div><dt>B -> A</dt><dd>{formatNumber(activePoint.bToA)} {spread.meta.targetQuote}</dd></div>
            <div><dt>Mid diff</dt><dd>{formatNumber(activePoint.midDiff)} {spread.meta.targetQuote}</dd></div>
            <div><dt>A book</dt><dd>{formatNumber(activePoint.aBid)} / {formatNumber(activePoint.aAsk)}</dd></div>
            <div><dt>B book</dt><dd>{formatNumber(activePoint.bBid)} / {formatNumber(activePoint.bAsk)}</dd></div>
            <div>
              <dt>A bid depth</dt>
              <dd>{formatDepthLevel(activePoint.aBidSizeText, activePoint.aBidSize, activePoint.aBidOrderCount)}</dd>
            </div>
            <div>
              <dt>A ask depth</dt>
              <dd>{formatDepthLevel(activePoint.aAskSizeText, activePoint.aAskSize, activePoint.aAskOrderCount)}</dd>
            </div>
            <div>
              <dt>B bid depth</dt>
              <dd>{formatDepthLevel(activePoint.bBidSizeText, activePoint.bBidSize, activePoint.bBidOrderCount)}</dd>
            </div>
            <div>
              <dt>B ask depth</dt>
              <dd>{formatDepthLevel(activePoint.bAskSizeText, activePoint.bAskSize, activePoint.bAskOrderCount)}</dd>
            </div>
          </dl>
          <div class="point-actions">
            <button type="button" on:click={() => jumpPoint(-1)}>Prev</button>
            <button type="button" on:click={() => jumpPoint(1)}>Next</button>
            <button type="button" on:click={jumpLatest}>Latest</button>
          </div>
        {:else}
          <p class="sidebar-empty">在曲线上悬停或点击后，这里会显示该点的完整 bid/ask、深度与价差信息。</p>
        {/if}
      </section>

      <section class="meta-card">
        <div class="meta-row">
          <span>Mode</span>
          <strong>
            {selectedPairIsLive ? (autoRefresh ? `${refreshSeconds}s live` : 'Paused') : 'Historical'}
          </strong>
        </div>
        <div class="meta-row">
          <span>Window</span>
          <strong>{hydrated ? `${formatTime(selectedRange.fromMs)} - ${formatTime(selectedRange.toMs)}` : '-'}</strong>
        </div>
        <div class="meta-row">
          <span>Rates</span>
          <strong>{formatInteger(cleanRates(rates).length)} active</strong>
        </div>
      </section>
    </aside>
  </div>
</main>


<style>
  :global(*) {
    box-sizing: border-box;
  }

  :global(html) {
    color-scheme: dark;
  }

  :global(body) {
    --background: #090b0f;
    --foreground: #e9ebef;
    --card: #111316;
    --sidebar: #0d1013;
    --muted: #1b1d21;
    --muted-foreground: #83868c;
    --border: rgba(255, 255, 255, 0.09);
    --input: rgba(255, 255, 255, 0.13);
    --primary: #30d697;
    --primary-foreground: #06100b;
    --negative: #fc5855;
    --warning: #edb345;
    --radius: 6px;
    margin: 0;
    min-width: 320px;
    color: var(--foreground);
    background: var(--background);
    font-family:
      ui-sans-serif, -apple-system, BlinkMacSystemFont, "Segoe UI", "PingFang SC",
      "Hiragino Sans GB", "Noto Sans CJK SC", "Microsoft YaHei UI", Arial, sans-serif;
    -webkit-font-smoothing: antialiased;
    -webkit-tap-highlight-color: rgba(48, 214, 151, 0.18);
  }

  :global(button),
  :global(input),
  :global(select) {
    font: inherit;
  }

  .skip-link {
    position: fixed;
    top: 12px;
    left: 12px;
    z-index: 50;
    border: 1px solid var(--border);
    border-radius: var(--radius);
    padding: 8px 10px;
    color: var(--primary-foreground);
    background: var(--primary);
    transform: translateY(-160%);
    transition: transform 150ms ease;
  }

  .skip-link:focus-visible {
    transform: translateY(0);
    outline: 2px solid rgba(48, 214, 151, 0.55);
  }

  .desk-shell {
    display: flex;
    min-height: 100dvh;
    flex-direction: column;
    background: var(--background);
  }

  .topbar {
    display: flex;
    min-height: 58px;
    align-items: center;
    justify-content: space-between;
    gap: 16px;
    border-bottom: 1px solid var(--border);
    padding: 10px 16px;
    background: rgba(9, 11, 15, 0.92);
  }

  .brand-lockup,
  .topbar-meta,
  .control-strip,
  .series-pills,
  .detail-actions,
  .point-actions {
    display: flex;
    align-items: center;
  }

  .brand-lockup {
    gap: 10px;
    min-width: 0;
  }

  .brand-lockup div {
    display: grid;
    gap: 1px;
  }

  .brand-lockup strong {
    font-size: 0.8rem;
    letter-spacing: 0.08em;
  }

  .brand-lockup span,
  .topbar-meta,
  .chart-help,
  .chart-caption,
  .sidebar-empty {
    color: var(--muted-foreground);
  }

  .brand-lockup span {
    font-size: 0.78rem;
  }

  .topbar-meta {
    flex-wrap: wrap;
    justify-content: flex-end;
    gap: 10px;
    font-size: 0.76rem;
  }

  .route-badge {
    border: 1px solid var(--border);
    border-radius: 999px;
    padding: 3px 8px;
    color: var(--primary);
    background: rgba(48, 214, 151, 0.08);
  }

  .notice-stack {
    display: grid;
    gap: 8px;
    border-bottom: 1px solid var(--border);
    padding: 10px 16px;
  }

  .notice {
    border: 1px solid rgba(252, 88, 85, 0.35);
    border-radius: var(--radius);
    padding: 10px 12px;
    color: #ffd3d2;
    background: rgba(252, 88, 85, 0.09);
    font-size: 0.9rem;
    overflow-wrap: anywhere;
  }

  .desk-layout {
    display: flex;
    flex: 1;
    flex-direction: column;
    min-width: 0;
  }

  .market-sidebar,
  .stats-sidebar {
    background: var(--sidebar);
  }

  .market-sidebar {
    border-bottom: 1px solid var(--border);
  }

  .stats-sidebar {
    display: grid;
    align-content: start;
    gap: 0;
    border-top: 1px solid var(--border);
  }

  .main-panel {
    display: grid;
    flex: 1;
    min-width: 0;
    align-content: start;
    gap: 0;
    background: var(--background);
  }

  .sidebar-block,
  .sidebar-details,
  .stats-card,
  .point-card,
  .meta-card {
    border-bottom: 1px solid var(--border);
    padding: 14px;
  }

  .sidebar-heading,
  .section-heading,
  .stats-heading,
  .chart-heading {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 12px;
  }

  .sidebar-heading,
  .section-heading,
  .stats-heading {
    margin-bottom: 12px;
    color: var(--muted-foreground);
    font-size: 0.75rem;
    font-weight: 600;
    letter-spacing: 0.02em;
  }

  .sidebar-heading strong,
  .section-heading strong,
  .stats-heading strong {
    color: var(--foreground);
    font-weight: 600;
    letter-spacing: 0;
    text-transform: none;
  }

  .market-list {
    display: grid;
    gap: 4px;
  }

  .mode-switch {
    display: grid;
    grid-template-columns: repeat(2, minmax(0, 1fr));
    overflow: hidden;
    border: 1px solid var(--border);
    border-radius: var(--radius);
    background: var(--card);
  }

  .mode-switch button {
    min-height: 34px;
    border: 0;
    border-right: 1px solid var(--border);
    border-radius: 0;
    background: transparent;
    font-size: 0.82rem;
  }

  .mode-switch button:last-child {
    border-right: 0;
  }

  .mode-switch button.active {
    color: var(--primary-foreground);
    background: var(--primary);
  }

  .exchange-market-list {
    display: grid;
    gap: 4px;
  }

  .market-list button,
  .exchange-market-list button {
    display: grid;
    gap: 9px;
    width: 100%;
    min-height: 72px;
    border: 1px solid transparent;
    border-left: 2px solid transparent;
    border-radius: 0;
    padding: 10px 11px;
    color: var(--foreground);
    background: transparent;
    text-align: left;
  }

  .market-list button:hover,
  .market-list button.active,
  .exchange-market-list button:hover,
  .exchange-market-list button.active {
    border-color: var(--border);
    border-left-color: var(--primary);
    background: var(--muted);
  }

  .market-main,
  .market-meta {
    display: grid;
    grid-template-columns: minmax(0, 1fr) minmax(7.5ch, max-content);
    align-items: baseline;
    min-width: 0;
    gap: 10px;
  }

  .market-main strong {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    font-size: 0.95rem;
    font-weight: 600;
  }

  .market-main em {
    min-width: 7.5ch;
    color: var(--muted-foreground);
    font-size: 0.78rem;
    font-style: normal;
    overflow: hidden;
    text-align: right;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .market-meta {
    color: var(--muted-foreground);
    font-size: 0.72rem;
  }

  .market-meta span {
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .market-meta span:last-child {
    min-width: 7.5ch;
    justify-self: end;
    text-align: right;
  }

  .sidebar-empty {
    margin: 0;
    border: 1px dashed var(--border);
    border-radius: var(--radius);
    padding: 12px;
    font-size: 0.86rem;
    line-height: 1.5;
  }

  .sidebar-details {
    padding-block: 0;
  }

  .sidebar-details summary {
    cursor: pointer;
    padding: 14px 0;
    color: var(--foreground);
    font-size: 0.88rem;
    font-weight: 600;
  }

  .leg-picker,
  .venue-selector,
  .rate-stack {
    display: grid;
    gap: 10px;
    padding-bottom: 14px;
  }

  .venue-selector label {
    min-width: 0;
  }

  .selected-legs {
    display: grid;
    grid-template-columns: minmax(0, 1fr) 54px minmax(0, 1fr);
    gap: 8px;
    align-items: stretch;
  }

  .selected-legs div {
    display: grid;
    min-width: 0;
    align-content: center;
    gap: 4px;
    border: 1px solid var(--border);
    border-radius: var(--radius);
    padding: 8px;
    background: rgba(255, 255, 255, 0.025);
  }

  .selected-legs span {
    color: var(--muted-foreground);
    font-size: 0.68rem;
    font-weight: 700;
    letter-spacing: 0.12em;
  }

  .selected-legs strong {
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    font-size: 0.78rem;
    font-weight: 600;
  }

  .swap-button {
    min-height: 100%;
    padding: 0 8px;
    font-size: 0.78rem;
  }

  .leg-choice-list {
    display: grid;
    gap: 6px;
  }

  .leg-choice-list article {
    display: grid;
    grid-template-columns: minmax(0, 1fr) 74px;
    gap: 8px;
    align-items: center;
    border: 1px solid var(--border);
    border-left: 2px solid transparent;
    border-radius: var(--radius);
    padding: 8px;
    background: var(--card);
  }

  .leg-choice-list article.a-selected {
    border-left-color: var(--primary);
  }

  .leg-choice-list article.b-selected {
    border-left-color: var(--warning);
  }

  .leg-choice-main {
    display: grid;
    min-width: 0;
    gap: 3px;
  }

  .leg-choice-main strong,
  .leg-choice-main span {
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .leg-choice-main strong {
    font-size: 0.86rem;
    font-weight: 650;
  }

  .leg-choice-main span {
    color: var(--muted-foreground);
    font-size: 0.72rem;
  }

  .leg-choice-actions {
    display: grid;
    grid-template-columns: repeat(2, 1fr);
    gap: 5px;
  }

  .leg-choice-actions button {
    min-height: 30px;
    padding: 0;
  }

  label {
    display: grid;
    gap: 6px;
    min-width: 0;
  }

  label span {
    color: var(--muted-foreground);
    font-size: 0.72rem;
    font-weight: 600;
    letter-spacing: 0.02em;
  }

  select,
  input,
  button {
    min-height: 36px;
    border: 1px solid var(--input);
    border-radius: var(--radius);
    color: var(--foreground);
    background: #111316;
    outline: 2px solid transparent;
  }

  select,
  input {
    width: 100%;
    min-width: 0;
    padding: 0 10px;
  }

  button {
    padding: 0 11px;
    cursor: pointer;
    font-weight: 600;
    transition:
      border-color 150ms ease,
      background 150ms ease,
      color 150ms ease,
      opacity 150ms ease;
  }

  button:hover:not(:disabled),
  button.active {
    border-color: rgba(48, 214, 151, 0.55);
    background: rgba(48, 214, 151, 0.12);
  }

  button:disabled {
    cursor: not-allowed;
    opacity: 0.5;
  }

  select:focus-visible,
  input:focus-visible,
  button:focus-visible {
    border-color: var(--primary);
    outline-color: rgba(48, 214, 151, 0.45);
  }

  .primary-button,
  .segmented button.active {
    color: var(--primary-foreground);
    border-color: var(--primary);
    background: var(--primary);
  }

  .rate-row {
    display: grid;
    grid-template-columns: minmax(4.4rem, 1fr) minmax(3.8rem, 1fr) minmax(3rem, 0.75fr) 32px;
    gap: 6px;
  }

  .rate-row input {
    padding-inline: 8px;
    font-size: 0.86rem;
  }

  .rate-row button {
    padding: 0;
    min-width: 0;
  }

  .detail-actions,
  .point-actions {
    gap: 8px;
  }

  .detail-actions button,
  .point-actions button {
    flex: 1;
  }

  .checkbox-line {
    display: flex;
    min-height: 36px;
    align-items: center;
    gap: 8px;
    color: var(--muted-foreground);
    font-size: 0.86rem;
  }

  .checkbox-line input {
    width: 16px;
    min-height: 16px;
    accent-color: var(--primary);
  }

  .pair-header,
  .control-strip,
  .chart-options,
  .custom-range,
  .chart-shell,
  .tape-panel,
  .venue-panel {
    border-bottom: 1px solid var(--border);
    padding: 16px;
  }

  .pair-header {
    display: flex;
    align-items: flex-start;
    justify-content: space-between;
    gap: 18px;
  }

  h1,
  p,
  dl {
    margin: 0;
  }

  h1 {
    font-size: clamp(1.6rem, 3vw, 2.65rem);
    font-weight: 650;
    letter-spacing: -0.035em;
    line-height: 1;
  }

  .pair-header p {
    margin-top: 7px;
    color: var(--muted-foreground);
    font-size: 0.92rem;
  }

  .latest-card {
    display: grid;
    min-width: 190px;
    gap: 3px;
    border: 1px solid var(--border);
    border-radius: var(--radius);
    padding: 11px 12px;
    background: var(--card);
  }

  .latest-card span,
  .latest-card small {
    color: var(--muted-foreground);
    font-size: 0.76rem;
  }

  .latest-card strong {
    font-family: ui-monospace, "SFMono-Regular", Consolas, monospace;
    font-size: 1.15rem;
    font-variant-numeric: tabular-nums;
  }

  .trading-toolbar {
    display: flex;
    align-items: center;
    gap: 10px;
  }

  .control-strip {
    justify-content: space-between;
    padding-block: 12px;
  }

  .query-toolbar {
    display: grid;
    grid-template-columns: minmax(320px, 1fr) auto auto auto;
  }

  .toolbar-cluster {
    display: inline-flex;
    min-width: 0;
    align-items: center;
    gap: 8px;
  }

  .toolbar-label {
    flex: 0 0 auto;
    color: var(--muted-foreground);
    font-size: 0.72rem;
    font-weight: 750;
    letter-spacing: 0.02em;
  }

  .range-cluster {
    min-width: 0;
  }

  .range-cluster .preset-tabs {
    max-width: 100%;
    overflow-x: auto;
  }

  .mode-cluster {
    justify-content: flex-end;
  }

  .interval-cluster {
    gap: 5px;
  }

  .interval-cluster input {
    width: 76px;
    min-height: 34px;
    padding: 6px 8px;
    font-variant-numeric: tabular-nums;
  }

  .interval-unit {
    color: var(--muted-foreground);
    font-size: 0.72rem;
  }

  .query-toolbar .primary-button {
    min-height: 34px;
    min-width: 78px;
    justify-self: end;
  }

  .segmented,
  .series-pills {
    display: inline-flex;
    overflow: hidden;
    border: 1px solid var(--border);
    border-radius: var(--radius);
    background: var(--card);
  }

  .segmented button,
  .series-pills button {
    min-height: 34px;
    border: 0;
    border-right: 1px solid var(--border);
    border-radius: 0;
    background: transparent;
    padding-inline: 12px;
    font-size: 0.86rem;
    line-height: 1;
    white-space: nowrap;
  }

  .segmented button:last-child,
  .series-pills button:last-child {
    border-right: 0;
  }

  .chart-options {
    justify-content: space-between;
    min-height: 52px;
    padding-block: 10px;
    background: rgba(17, 19, 22, 0.32);
  }

  .average-control {
    flex-wrap: wrap;
    gap: 8px;
    margin-left: auto;
  }

  .segmented.compact button,
  .percent-presets button {
    min-height: 30px;
    padding-inline: 8px;
    font-size: 0.76rem;
  }

  .percent-input {
    display: inline-flex;
    align-items: center;
    gap: 5px;
    min-height: 30px;
    border: 1px solid var(--border);
    border-radius: var(--radius);
    padding: 0 8px;
    color: var(--muted-foreground);
    background: var(--card);
    font-size: 0.78rem;
    font-variant-numeric: tabular-nums;
  }

  .percent-input.disabled {
    opacity: 0.5;
  }

  .percent-input input {
    width: 52px;
    min-height: 0;
    border: 0;
    padding: 0;
    color: var(--foreground);
    background: transparent;
    font-variant-numeric: tabular-nums;
    text-align: right;
  }

  .percent-input input:disabled {
    color: var(--muted-foreground);
  }

  .percent-presets {
    display: inline-flex;
    overflow: hidden;
    gap: 0;
    border: 1px solid var(--border);
    border-radius: var(--radius);
    background: var(--card);
  }

  .percent-presets button {
    border: 0;
    border-right: 1px solid var(--border);
    border-radius: 0;
    background: transparent;
  }

  .percent-presets button:last-child {
    border-right: 0;
  }

  .custom-range {
    display: grid;
    grid-template-columns: repeat(2, minmax(0, 260px));
    gap: 12px;
    background: rgba(17, 19, 22, 0.45);
  }

  .chart-shell {
    display: grid;
    gap: 12px;
  }

  .chart-heading {
    color: var(--muted-foreground);
    font-size: 0.78rem;
  }

  .chart-heading div {
    display: grid;
    gap: 3px;
  }

  .chart-heading strong {
    color: var(--foreground);
    font-family: ui-monospace, "SFMono-Regular", Consolas, monospace;
    font-variant-numeric: tabular-nums;
    font-weight: 600;
  }

  .chart-help,
  .chart-caption {
    font-size: 0.78rem;
    line-height: 1.6;
  }

  .empty-state {
    display: grid;
    min-height: 320px;
    place-items: center;
    border: 1px dashed var(--border);
    border-radius: var(--radius);
    color: var(--muted-foreground);
    text-align: center;
  }

  .tape-grid {
    display: grid;
    grid-template-columns: repeat(9, minmax(0, 1fr));
    gap: 0;
    overflow: hidden;
    border: 1px solid var(--border);
    border-radius: var(--radius);
  }

  .tape-grid button {
    display: grid;
    min-height: 88px;
    align-content: space-between;
    justify-items: start;
    border: 0;
    border-right: 1px solid var(--border);
    border-radius: 0;
    padding: 10px;
    background: var(--card);
    text-align: left;
  }

  .tape-grid button:last-child {
    border-right: 0;
  }

  .tape-grid strong,
  .point-ledger dd,
  .stat-list dd,
  .meta-row strong {
    font-family: ui-monospace, "SFMono-Regular", Consolas, monospace;
    font-feature-settings:
      "tnum" 1,
      "zero" 1;
    font-variant-numeric: tabular-nums;
  }

  .tape-grid span,
  .tape-grid small {
    color: var(--muted-foreground);
    font-size: 0.75rem;
  }

  .venue-grid {
    display: grid;
    grid-template-columns: repeat(auto-fit, minmax(180px, 1fr));
    gap: 8px;
  }

  .venue-grid article {
    display: grid;
    gap: 12px;
    border: 1px solid var(--border);
    border-left: 2px solid transparent;
    border-radius: var(--radius);
    padding: 12px;
    background: var(--card);
  }

  .venue-grid article.selected {
    border-left-color: var(--primary);
  }

  .venue-grid article > div:first-child {
    display: flex;
    justify-content: space-between;
    gap: 10px;
  }

  .venue-grid article > div:first-child span {
    color: var(--muted-foreground);
    font-size: 0.8rem;
  }

  .venue-role {
    display: flex;
    justify-content: flex-end;
  }

  .venue-role span {
    border: 1px solid var(--border);
    border-radius: 999px;
    padding: 4px 8px;
    color: var(--muted-foreground);
    font-size: 0.7rem;
  }

  .venue-grid article.selected .venue-role span {
    border-color: rgba(48, 214, 151, 0.45);
    color: var(--primary);
    background: rgba(48, 214, 151, 0.08);
  }

  .venue-grid dl,
  .point-ledger,
  .stat-list {
    display: grid;
    gap: 0;
  }

  .venue-grid dl div,
  .point-ledger div,
  .stat-list div {
    display: grid;
    grid-template-columns: minmax(74px, 0.8fr) minmax(0, 1fr);
    gap: 10px;
    border-top: 1px solid var(--border);
    padding: 8px 0;
  }

  dt {
    color: var(--muted-foreground);
    font-size: 0.72rem;
    font-weight: 600;
    letter-spacing: 0.01em;
  }

  dd {
    margin: 0;
    min-width: 0;
    overflow-wrap: anywhere;
  }

  .stats-sidebar {
    font-size: 0.88rem;
  }

  .stat-list div {
    grid-template-columns: minmax(112px, 1fr) 11ch;
    align-items: baseline;
  }

  .stat-list dd {
    width: 11ch;
    justify-self: end;
    font-size: 1rem;
    font-weight: 650;
    text-align: right;
    white-space: nowrap;
  }

  .point-card,
  .meta-card {
    background: rgba(13, 16, 19, 0.82);
  }

  .point-ledger {
    margin-bottom: 12px;
  }

  .point-ledger div {
    grid-template-columns: minmax(78px, 0.8fr) minmax(12ch, 1fr);
  }

  .point-ledger dd {
    justify-self: end;
    text-align: right;
  }

  .meta-card {
    display: grid;
    gap: 0;
  }

  .meta-row {
    display: grid;
    grid-template-columns: minmax(86px, 0.65fr) minmax(12ch, 1fr);
    gap: 10px;
    border-bottom: 1px solid var(--border);
    padding: 9px 0;
  }

  .meta-row strong {
    min-width: 12ch;
    justify-self: end;
    text-align: right;
  }

  .meta-row:last-child {
    border-bottom: 0;
  }

  .meta-row span {
    color: var(--muted-foreground);
    font-size: 0.72rem;
  }

  .positive {
    color: var(--primary) !important;
  }

  .negative {
    color: var(--negative) !important;
  }

  @media (min-width: 1024px) {
    .desk-layout {
      flex-direction: row;
      align-items: stretch;
    }

    .market-sidebar {
      width: 18rem;
      flex: 0 0 18rem;
      border-right: 1px solid var(--border);
      border-bottom: 0;
    }

    .stats-sidebar {
      width: 18rem;
      flex: 0 0 18rem;
      border-top: 0;
      border-left: 1px solid var(--border);
    }
  }

  @media (max-width: 1240px) {
    .tape-grid {
      grid-template-columns: repeat(3, minmax(0, 1fr));
    }

    .query-toolbar {
      grid-template-columns: minmax(0, 1fr) auto;
    }

    .mode-cluster {
      justify-content: flex-start;
    }

    .chart-options {
      align-items: flex-start;
      flex-direction: column;
    }

    .average-control {
      margin-left: 0;
    }
  }

  @media (max-width: 760px) {
    .market-list,
    .exchange-market-list {
      max-height: min(46vh, 420px);
      overflow-y: auto;
      overscroll-behavior: contain;
    }

    .topbar,
    .pair-header,
    .chart-heading {
      align-items: stretch;
      flex-direction: column;
    }

    .topbar-meta {
      justify-content: flex-start;
    }

    .latest-card {
      min-width: 0;
      width: 100%;
    }

    .control-strip,
    .chart-options,
    .toolbar-cluster,
    .segmented,
    .series-pills,
    .average-control {
      width: 100%;
    }

    .query-toolbar {
      grid-template-columns: 1fr;
    }

    .toolbar-cluster {
      flex-wrap: wrap;
      align-items: stretch;
    }

    .toolbar-label {
      width: 100%;
    }

    .segmented,
    .series-pills {
      display: grid;
      grid-auto-flow: column;
      overflow-x: auto;
    }

    .segmented button,
    .series-pills button {
      white-space: nowrap;
    }

    .chart-options {
      align-items: stretch;
    }

    .percent-presets {
      flex: 1;
    }

    .percent-presets button {
      flex: 1;
    }

    .custom-range,
    .rate-row,
    .venue-grid dl div,
    .point-ledger div,
    .stat-list div,
    .meta-row {
      grid-template-columns: 1fr;
    }

    .tape-grid {
      grid-template-columns: 1fr;
    }

    .tape-grid button {
      border-right: 0;
      border-bottom: 1px solid var(--border);
    }

    .tape-grid button:last-child {
      border-bottom: 0;
    }

    .stat-list dd {
      width: auto;
      min-width: 0;
      justify-self: start;
      text-align: left;
    }

    .point-ledger dd,
    .meta-row strong {
      min-width: 0;
      justify-self: start;
      text-align: left;
    }
  }

  /* Decision cockpit: flat hierarchy, directional color, chart-first density. */
  :global(body) {
    --background: #07101a;
    --foreground: #dce5ef;
    --card: #0b1521;
    --sidebar: #08121d;
    --muted: #0f1c2a;
    --muted-foreground: #74849a;
    --border: #1b2a3c;
    --input: #26374b;
    --primary: #29d9c2;
    --primary-foreground: #031714;
    --profit: #4bd19b;
    --negative: #e26d6a;
    --warning: #f0a94b;
    --radius: 3px;
    letter-spacing: 0.002em;
  }

  .desk-shell {
    background: #07101a;
  }

  .topbar {
    position: sticky;
    top: 0;
    z-index: 20;
    min-height: 52px;
    padding: 8px 14px;
    background: rgba(7, 16, 26, 0.96);
  }

  .brand-lockup strong {
    color: #f3f7fb;
    font-size: 0.78rem;
    font-weight: 760;
    letter-spacing: 0.13em;
  }

  .brand-lockup span {
    font-size: 0.68rem;
  }

  .topbar-meta {
    gap: 12px;
    font-size: 0.7rem;
  }

  .topbar-meta > span {
    white-space: nowrap;
  }

  .topbar-meta > span + span {
    position: relative;
    padding-left: 13px;
  }

  .topbar-meta > span + span::before {
    position: absolute;
    top: 50%;
    left: 0;
    width: 2px;
    height: 2px;
    background: #435268;
    content: "";
  }

  .route-badge {
    border: 0;
    border-radius: 0;
    padding: 0;
    color: var(--primary);
    background: transparent;
    font-variant-numeric: tabular-nums;
    letter-spacing: 0.04em;
  }

  .topbar-meta button {
    min-height: 30px;
    border-color: #30445b;
    background: #0c1825;
    font-size: 0.72rem;
  }

  .desk-layout {
    align-items: start;
  }

  .market-sidebar,
  .stats-sidebar {
    background: #08121d;
  }

  .market-sidebar {
    max-height: calc(100dvh - 52px);
    overflow-y: auto;
    scrollbar-color: #293c52 transparent;
    scrollbar-width: thin;
  }

  .main-panel {
    background: #07101a;
  }

  .sidebar-block,
  .sidebar-details,
  .stats-card,
  .point-card,
  .meta-card {
    padding: 13px 14px;
  }

  .sidebar-heading,
  .section-heading,
  .stats-heading {
    margin-bottom: 10px;
    font-size: 0.68rem;
    letter-spacing: 0.07em;
    text-transform: uppercase;
  }

  .sidebar-heading strong,
  .section-heading strong,
  .stats-heading strong {
    font-size: 0.7rem;
    font-variant-numeric: tabular-nums;
  }

  .mode-switch,
  .segmented,
  .series-pills,
  .percent-presets {
    border-radius: 2px;
    background: #0a1623;
  }

  .mode-switch button.active,
  .segmented button.active,
  .series-pills button.active,
  .percent-presets button.active {
    color: #e9fffb;
    background: #12322f;
    box-shadow: inset 0 -2px 0 var(--primary);
  }

  .market-list,
  .exchange-market-list {
    gap: 0;
  }

  .market-list button,
  .exchange-market-list button {
    min-height: 62px;
    border-top: 1px solid transparent;
    border-right: 0;
    border-bottom: 1px solid #142336;
    border-left-width: 2px;
    padding: 8px 9px;
  }

  .market-list button:hover,
  .market-list button.active,
  .exchange-market-list button:hover,
  .exchange-market-list button.active {
    border-top-color: transparent;
    border-right-color: transparent;
    border-bottom-color: #21344a;
    border-left-color: var(--primary);
    background: #0d1b29;
  }

  .market-main strong {
    font-size: 0.87rem;
  }

  .market-main em,
  .market-meta {
    font-size: 0.68rem;
  }

  .sidebar-details summary {
    padding: 12px 0;
    font-size: 0.8rem;
  }

  .selected-legs div,
  .leg-choice-list article {
    border-radius: 2px;
    background: #0a1623;
  }

  .selected-legs div:first-child,
  .leg-choice-list article.a-selected {
    border-left-color: var(--primary);
  }

  .selected-legs div:last-child,
  .leg-choice-list article.b-selected {
    border-left-color: var(--warning);
  }

  select,
  input,
  button {
    border-radius: 2px;
    background: #0b1724;
  }

  button:hover:not(:disabled),
  button.active {
    border-color: #2e5c5a;
    background: #102522;
  }

  .primary-button {
    color: var(--primary-foreground);
    border-color: var(--primary);
    background: var(--primary);
  }

  .pair-header {
    padding: 20px 18px 15px;
    background: #08121d;
  }

  h1 {
    color: #f0f5fa;
    font-size: clamp(1.8rem, 3vw, 3.15rem);
    font-weight: 690;
    letter-spacing: -0.03em;
  }

  h1 span {
    color: #718197;
    font-weight: 520;
  }

  .pair-header p {
    display: flex;
    align-items: center;
    gap: 7px;
    margin-top: 9px;
    font-size: 0.78rem;
  }

  .versus {
    margin-inline: 3px;
    color: #52657c;
  }

  .leg-dot {
    display: inline-block;
    width: 9px;
    height: 2px;
    border-radius: 0;
    background: var(--primary);
  }

  .leg-dot.b {
    background: var(--warning);
  }

  .latest-card {
    min-width: 176px;
    border: 0;
    border-left: 1px solid #26374b;
    border-radius: 0;
    padding: 4px 0 4px 16px;
    background: transparent;
    text-align: right;
  }

  .latest-card strong {
    font-size: 1.32rem;
  }

  .route-ribbon {
    display: grid;
    grid-template-columns: minmax(0, 1fr) auto minmax(132px, auto);
    align-items: center;
    gap: 18px;
    min-height: 58px;
    border-bottom: 1px solid #1b2a3c;
    border-left: 3px solid #52657c;
    padding: 9px 16px 9px 15px;
    background: #0b1724;
  }

  .route-ribbon.positive {
    border-left-color: var(--profit);
  }

  .route-ribbon.negative {
    border-left-color: var(--negative);
  }

  .route-ribbon div {
    display: grid;
    min-width: 0;
    gap: 3px;
  }

  .route-ribbon span,
  .route-ribbon small {
    color: #718197;
    font-size: 0.67rem;
    letter-spacing: 0.04em;
  }

  .route-ribbon strong {
    overflow: hidden;
    color: #e6edf5;
    font-size: 0.82rem;
    font-weight: 650;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .route-ribbon output {
    color: #dfe9f3;
    font-family: ui-monospace, "SFMono-Regular", Consolas, monospace;
    font-size: 1.05rem;
    font-weight: 700;
    font-variant-numeric: tabular-nums;
  }

  .route-ribbon.positive output {
    color: var(--profit);
  }

  .route-ribbon.negative output {
    color: var(--negative);
  }

  .route-ribbon small {
    text-align: right;
  }

  .control-strip,
  .chart-options,
  .custom-range {
    padding: 9px 14px;
    background: #08131f;
  }

  .chart-options {
    min-height: 46px;
    background: #091622;
  }

  .toolbar-label {
    font-size: 0.65rem;
    letter-spacing: 0.07em;
    text-transform: uppercase;
  }

  .segmented button,
  .series-pills button {
    min-height: 31px;
    padding-inline: 10px;
    font-size: 0.74rem;
  }

  .chart-shell {
    gap: 8px;
    padding: 11px 10px 10px;
    background: #0a111b;
  }

  .chart-heading {
    padding: 0 6px 2px;
    font-size: 0.68rem;
  }

  .chart-help,
  .chart-caption {
    padding-inline: 6px;
    font-size: 0.7rem;
  }

  .chart-help {
    position: absolute;
    width: 1px;
    height: 1px;
    overflow: hidden;
    clip: rect(0 0 0 0);
    white-space: nowrap;
  }

  .chart-caption {
    color: #67778c;
    line-height: 1.5;
  }

  .empty-state {
    min-height: 410px;
    border-style: solid;
    background: #0a111b;
  }

  .tape-panel,
  .venue-panel {
    padding: 13px 14px 15px;
    background: #08121d;
  }

  .tape-grid {
    border-radius: 2px;
  }

  .tape-grid button {
    min-height: 76px;
    background: #0a1623;
  }

  .tape-grid button.active {
    border-bottom: 2px solid var(--primary);
    background: #102421;
  }

  .venue-grid {
    gap: 1px;
    background: #1b2a3c;
  }

  .venue-grid article {
    border: 0;
    border-left: 2px solid transparent;
    border-radius: 0;
    background: #0a1623;
  }

  .venue-role {
    justify-content: flex-start;
  }

  .venue-role span,
  .venue-grid article.selected .venue-role span {
    border: 0;
    border-radius: 0;
    padding: 0;
    color: #6d8198;
    background: transparent;
    font-size: 0.67rem;
  }

  .venue-grid article.selected .venue-role span {
    color: var(--primary);
  }

  .stats-sidebar {
    font-size: 0.8rem;
  }

  .stats-card,
  .point-card,
  .meta-card {
    background: #08121d;
  }

  .stat-list div,
  .point-ledger div,
  .meta-row {
    border-top-color: #172638;
    padding-block: 7px;
  }

  .stat-list dd {
    font-size: 0.88rem;
  }

  .point-ledger dd {
    color: #cad6e2;
    font-size: 0.75rem;
  }

  .positive {
    color: var(--profit) !important;
  }

  @media (min-width: 1200px) {
    .desk-layout {
      display: grid;
      grid-template-columns: 17rem minmax(42rem, 1fr) 18rem;
    }

    .market-sidebar,
    .stats-sidebar {
      position: sticky;
      top: 52px;
      max-height: calc(100dvh - 52px);
      overflow-y: auto;
    }

    .market-sidebar {
      width: auto;
      border-right: 1px solid var(--border);
    }

    .stats-sidebar {
      width: auto;
      border-top: 0;
      border-left: 1px solid var(--border);
    }
  }

  @media (min-width: 820px) and (max-width: 1199px) {
    .desk-layout {
      display: grid;
      grid-template-columns: 16rem minmax(0, 1fr);
    }

    .market-sidebar {
      position: sticky;
      top: 52px;
      width: auto;
      max-height: calc(100dvh - 52px);
      border-right: 1px solid var(--border);
      border-bottom: 0;
      overflow-y: auto;
    }

    .stats-sidebar {
      grid-column: 1 / -1;
      display: grid;
      grid-template-columns: 0.85fr 1.4fr 0.75fr;
      width: auto;
      max-width: none;
      border-top: 1px solid var(--border);
      border-left: 0;
    }

    .stats-sidebar > section {
      border-right: 1px solid var(--border);
    }
  }

  @media (max-width: 819px) {
    .market-sidebar {
      max-height: none;
      overflow: visible;
    }

    .main-panel {
      width: 100%;
      max-width: 100vw;
      overflow-x: clip;
    }

    .topbar {
      position: relative;
    }

    .pair-header > div,
    .latest-card,
    .query-toolbar,
    .chart-options,
    .toolbar-cluster,
    .segmented,
    .series-pills,
    .average-control {
      min-width: 0;
      max-width: 100%;
    }

    .latest-card {
      width: 100%;
    }

    .preset-tabs {
      display: flex;
      width: 100%;
      overflow-x: auto;
    }

    .preset-tabs button {
      flex: 0 0 auto;
      min-width: 54px;
    }

    .display-mode,
    .series-pills {
      grid-auto-flow: row;
      grid-template-columns: repeat(2, minmax(0, 1fr));
    }

    .display-mode button,
    .series-pills button {
      min-width: 0;
    }

    .average-control > .segmented {
      grid-auto-flow: row;
      grid-template-columns: repeat(3, minmax(0, 1fr));
    }

    .route-ribbon {
      grid-template-columns: minmax(0, 1fr) auto;
    }

    .route-ribbon small {
      grid-column: 1 / -1;
      text-align: left;
    }
  }

  @media (max-width: 560px) {
    .pair-header {
      padding-inline: 13px;
    }

    .route-ribbon {
      gap: 10px;
      padding-inline: 11px;
    }

    .route-ribbon output {
      font-size: 0.92rem;
    }

    .chart-heading {
      display: grid;
      grid-template-columns: repeat(2, minmax(0, 1fr));
      gap: 9px;
    }

    .tape-grid {
      grid-template-columns: repeat(3, minmax(0, 1fr));
      overflow-x: auto;
    }

    .tape-grid button {
      min-width: 118px;
      border-right: 1px solid var(--border);
      border-bottom: 0;
    }
  }

  @media (prefers-reduced-motion: reduce) {
    *,
    *::before,
    *::after {
      scroll-behavior: auto !important;
      transition-duration: 0.01ms !important;
      animation-duration: 0.01ms !important;
      animation-iteration-count: 1 !important;
    }
  }

  /* --- skeleton loading --- */
  .skeleton {
    background: linear-gradient(90deg, var(--muted) 25%, rgba(255,255,255,0.06) 50%, var(--muted) 75%);
    background-size: 200% 100%;
    animation: skeleton-shimmer 1.5s ease-in-out infinite;
    border-radius: var(--radius);
  }

  @keyframes skeleton-shimmer {
    0% { background-position: 200% 0; }
    100% { background-position: -200% 0; }
  }

  @media (prefers-reduced-motion: reduce) {
    .skeleton {
      animation: none;
      background: var(--muted);
    }
  }

  .skeleton-row {
    height: 36px;
    margin-bottom: 6px;
  }

  .skeleton-chart {
    height: clamp(410px, 54vh, 560px);
    min-height: 410px;
    display: flex;
    align-items: center;
    justify-content: center;
    color: var(--muted-foreground);
    font-size: 0.85rem;
  }

  .skeleton-sidebar {
    display: grid;
    gap: 6px;
    padding: 8px 12px;
  }
</style>
