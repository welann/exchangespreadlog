<script lang="ts">
  import { onMount } from 'svelte';
  import type { Market, QuoteRate, SpreadPoint, SpreadResponse } from '$lib/types';

  const RATE_STORAGE_KEY = 'exchangespreadlog.quoteRates';
  const CHART = {
    width: 980,
    height: 430,
    top: 28,
    right: 24,
    bottom: 42,
    left: 68
  };

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

  type QueryState = {
    baseAsset?: string;
    catalogA?: string;
    catalogB?: string;
    preset?: string;
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
  type SelectionMode = 'market' | 'venue';
  type Instrument = Market['instruments'][number];
  type VenueOption = {
    venue: string;
    markets: number;
    instruments: number;
    tickCount: number;
  };
  type VenuePairMarket = {
    market: Market;
    instrumentA: Instrument;
    instrumentB: Instrument;
    quoteLabel: string;
    tickCount: number;
  };
  type LoadSpreadOptions = {
    preservePoint?: boolean;
    silent?: boolean;
    slideWindow?: boolean;
    updateUrl?: boolean;
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
  let customStart = toDateInput(Date.now() - 60 * 60 * 1000);
  let customEnd = toDateInput(Date.now());
  let rangeAnchorMs = Date.now();
  let rates: QuoteRate[] = structuredClone(defaultRates);
  let hydrated = false;
  let showAToB = true;
  let showBToA = true;
  let displayMode: DisplayMode = 'both';
  let autoRefresh = true;
  let refreshSeconds = 15;
  let refreshTimer: ReturnType<typeof setInterval> | null = null;
  let spreadRequestSeq = 0;

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
  $: venueOptions = buildVenueOptions(markets);
  $: venuePairMarkets = commonMarketsForVenues(markets, selectedVenueA, selectedVenueB);
  $: totalTicks = tickCountForInstruments(currentInstruments);
  $: selectedInstrumentA = currentInstruments.find((instrument) => instrument.catalogId === selectedA) ?? null;
  $: selectedInstrumentB = currentInstruments.find((instrument) => instrument.catalogId === selectedB) ?? null;
  $: selectedRange = currentRange(selectedPreset, customStart, customEnd, rangeAnchorMs);
  $: spreadBusy = loadingSpread || refreshingSpread;
  $: points = spread?.points ?? [];
  $: xBounds = computeXBounds(points, selectedRange);
  $: yBounds = computeYBounds(points, displayMode, showAToB, showBToA);
  $: averageSpreadValue = computeVisibleSpreadAverage(points, displayMode, showAToB, showBToA);
  $: bestPath = displayMode === 'best' ? bestLinePath(points, xBounds, yBounds) : '';
  $: aPath = displayMode === 'both' && showAToB ? linePath(points, 'aToB', xBounds, yBounds) : '';
  $: bPath = displayMode === 'both' && showBToA ? linePath(points, 'bToA', xBounds, yBounds) : '';
  $: zeroY = yScale(0, yBounds);
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
    const queryState = readQueryState();
    void loadMarkets(queryState).finally(() => configureAutoRefresh());

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
      rangeAnchorMs = Date.now();
    }

    const range = currentRange(selectedPreset, customStart, customEnd, rangeAnchorMs);
    if (!Number.isFinite(range.fromMs) || !Number.isFinite(range.toMs) || range.fromMs >= range.toMs) {
      queryError = 'Choose a valid time range with From before To';
      return;
    }

    const requestId = ++spreadRequestSeq;
    const previousSelectedPoint = selectedIndex >= 0 ? points[selectedIndex] : null;
    const wasFollowingLatest = selectedIndex < 0 || selectedIndex >= points.length - 1;

    if (options.silent) {
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
        body: JSON.stringify({
          catalogA,
          catalogB,
          fromMs: range.fromMs,
          toMs: range.toMs,
          ...spreadQueryOptions(range),
          rates: cleanRates(rates)
        })
      });
      const body = await response.json();
      if (!response.ok) throw new Error(body.error ?? 'Failed to query spread');
      if (requestId !== spreadRequestSeq || selectedA !== catalogA || selectedB !== catalogB) return;
      spread = body as SpreadResponse;
      selectedIndex = nextSelectedIndex(spread.points, previousSelectedPoint?.tsMs ?? null, wasFollowingLatest, options.preservePoint);
      if (options.updateUrl !== false) {
        syncQueryState();
      }
    } catch (error) {
      if (requestId !== spreadRequestSeq || selectedA !== catalogA || selectedB !== catalogB) return;
      spread = null;
      selectedIndex = -1;
      queryError = error instanceof Error ? error.message : 'Failed to query spread';
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
    rangeAnchorMs = latestForInstruments(option.market.instruments) ?? Date.now();
    syncVenueSelectionFromSelectedLegs();
    await loadSpreadWhenReady({ slideWindow: true });
  }

  async function loadSpreadWhenReady(options: LoadSpreadOptions = {}) {
    if (!selectedA || !selectedB || selectedA === selectedB) return;
    await loadSpread(options);
  }

  async function refreshCurrentSpread(options: LoadSpreadOptions = {}) {
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
        void refreshCurrentSpread({ preservePoint: true, silent: true, updateUrl: false });
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

  function spreadQueryOptions(range: { fromMs: number; toMs: number }) {
    if (range.toMs - range.fromMs <= 60 * 60 * 1000) {
      return { precision: 'bucket', bucketSeconds: 15 };
    }
    return { precision: 'bucket' };
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

    rangeAnchorMs = latestForInstruments(instruments) ?? Date.now();
    selectedPreset = presets.some((preset) => preset.value === state?.preset)
      ? (state?.preset ?? selectedPreset)
      : selectedPreset;

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
    const fromMs = Number(params.get('from'));
    const toMs = Number(params.get('to'));

    if (baseAsset) state.baseAsset = baseAsset;
    if (catalogA) state.catalogA = catalogA;
    if (catalogB) state.catalogB = catalogB;
    if (preset) state.preset = preset;
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
    params.set('from', String(Math.trunc(range.fromMs)));
    params.set('to', String(Math.trunc(range.toMs)));
    window.history.replaceState({}, '', `${window.location.pathname}?${params.toString()}${window.location.hash}`);
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

  function selectValue(event: Event) {
    return (event.currentTarget as HTMLSelectElement).value;
  }

  function inputValue(event: Event) {
    return (event.currentTarget as HTMLInputElement).value;
  }

  function handlePointerMove(event: PointerEvent) {
    if (points.length === 0) return;
    const svg = event.currentTarget as SVGSVGElement;
    const rect = svg.getBoundingClientRect();
    const viewX = ((event.clientX - rect.left) / rect.width) * CHART.width;
    const plotX = clamp((viewX - CHART.left) / plotWidth(), 0, 1);
    const ts = xBounds.min + plotX * (xBounds.max - xBounds.min);
    hoverIndex = nearestPointIndex(ts);
  }

  function handleChartClick() {
    if (hoverIndex >= 0) selectedIndex = hoverIndex;
  }

  function handleChartKeydown(event: KeyboardEvent) {
    if (points.length === 0) return;
    const current = selectedIndex >= 0 ? selectedIndex : points.length - 1;
    if (event.key === 'ArrowLeft') {
      selectedIndex = clamp(current - 1, 0, points.length - 1);
      event.preventDefault();
    }
    if (event.key === 'ArrowRight') {
      selectedIndex = clamp(current + 1, 0, points.length - 1);
      event.preventDefault();
    }
    if (event.key === 'Home') {
      selectedIndex = 0;
      event.preventDefault();
    }
    if (event.key === 'End') {
      selectedIndex = points.length - 1;
      event.preventDefault();
    }
  }

  function nearestPointIndex(tsMs: number): number {
    let best = 0;
    let distance = Number.POSITIVE_INFINITY;
    points.forEach((point, index) => {
      const currentDistance = Math.abs(point.tsMs - tsMs);
      if (currentDistance < distance) {
        best = index;
        distance = currentDistance;
      }
    });
    return best;
  }

  function computeXBounds(data: SpreadPoint[], range: { fromMs: number; toMs: number }) {
    if (data.length === 0) {
      return { min: range.fromMs, max: range.toMs };
    }
    return {
      min: data[0].tsMs,
      max: data[data.length - 1].tsMs
    };
  }

  function computeYBounds(
    data: SpreadPoint[],
    mode: DisplayMode,
    includeAToB: boolean,
    includeBToA: boolean
  ) {
    const values = visibleSpreadValues(data, mode, includeAToB, includeBToA);

    if (values.length === 0) return { min: -1, max: 1 };
    const min = Math.min(0, ...values);
    const max = Math.max(0, ...values);
    if (Math.abs(max - min) < Number.EPSILON) return { min: -1, max: 1 };
    const padding = Math.max((max - min) * 0.12, 0.0001);
    return { min: min - padding, max: max + padding };
  }

  function computeVisibleSpreadAverage(
    data: SpreadPoint[],
    mode: DisplayMode,
    includeAToB: boolean,
    includeBToA: boolean
  ) {
    const values = visibleSpreadValues(data, mode, includeAToB, includeBToA);
    if (values.length === 0) return null;
    return values.reduce((sum, value) => sum + value, 0) / values.length;
  }

  function visibleSpreadValues(
    data: SpreadPoint[],
    mode: DisplayMode,
    includeAToB: boolean,
    includeBToA: boolean
  ) {
    return data
      .flatMap((point) =>
        mode === 'best'
          ? [bestSpreadValue(point)]
          : [
              includeAToB ? point.aToB : null,
              includeBToA ? point.bToA : null
            ]
      )
      .filter((value): value is number => value !== null && Number.isFinite(value));
  }

  function linePath(
    data: SpreadPoint[],
    key: 'aToB' | 'bToA',
    x: { min: number; max: number },
    y: { min: number; max: number }
  ) {
    return data
      .filter((point) => point[key] !== null)
      .map((point, index) => {
        const command = index === 0 ? 'M' : 'L';
        return `${command}${xScale(point.tsMs, x)},${yScale(point[key] ?? 0, y)}`;
      })
      .join(' ');
  }

  function bestLinePath(
    data: SpreadPoint[],
    x: { min: number; max: number },
    y: { min: number; max: number }
  ) {
    return data
      .map((point) => ({ point, value: bestSpreadValue(point) }))
      .filter((entry): entry is { point: SpreadPoint; value: number } => entry.value !== null && Number.isFinite(entry.value))
      .map((entry, index) => {
        const command = index === 0 ? 'M' : 'L';
        return `${command}${xScale(entry.point.tsMs, x)},${yScale(entry.value, y)}`;
      })
      .join(' ');
  }

  function xScale(value: number, bounds: { min: number; max: number }) {
    const span = Math.max(1, bounds.max - bounds.min);
    return CHART.left + ((value - bounds.min) / span) * plotWidth();
  }

  function yScale(value: number, bounds: { min: number; max: number }) {
    const span = Math.max(0.000001, bounds.max - bounds.min);
    return CHART.top + (1 - (value - bounds.min) / span) * plotHeight();
  }

  function plotWidth() {
    return CHART.width - CHART.left - CHART.right;
  }

  function plotHeight() {
    return CHART.height - CHART.top - CHART.bottom;
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
    const byVenue = new Map<string, { markets: Set<string>; instruments: number; tickCount: number }>();
    inputMarkets.forEach((market) => {
      market.instruments.forEach((instrument) => {
        const venue = instrument.venueInstanceId || 'unknown';
        const current = byVenue.get(venue) ?? { markets: new Set<string>(), instruments: 0, tickCount: 0 };
        current.markets.add(market.baseAsset);
        current.instruments += 1;
        current.tickCount += instrument.tickCount;
        byVenue.set(venue, current);
      });
    });

    return [...byVenue.entries()]
      .map(([venue, stats]) => ({
        venue,
        markets: stats.markets.size,
        instruments: stats.instruments,
        tickCount: stats.tickCount
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
          quoteLabel: quoteLabelForInstruments([instrumentA, instrumentB]),
          tickCount: instrumentA.tickCount + instrumentB.tickCount
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
            right.tickCount - left.tickCount ||
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
    return opportunityForPoint(point)?.value ?? null;
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

  function tickCountForInstruments(instruments: Market['instruments']) {
    return instruments.reduce((sum, instrument) => sum + instrument.tickCount, 0);
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
    return `${meta.bucketSeconds}s extrema`;
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
      <span class="status-dot" aria-hidden="true"></span>
      <div>
        <strong>SPREADDESK</strong>
        <span>跨交易所价差监控</span>
      </div>
    </div>
    <div class="topbar-meta" aria-live="polite">
      <span>数据源: ClickHouse</span>
      <span>{points.length > 0 ? `${formatInteger(points.length)} samples / ${sampleLabel(spread?.meta)}` : loadingMarkets ? '读取市场中' : '等待样本'}</span>
      <span>{refreshingSpread ? '同步当前组合中' : autoRefresh ? `实时 ${refreshSeconds}s` : '实时已暂停'}</span>
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
              <p class="sidebar-empty">正在读取 ClickHouse 市场目录。</p>
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
                    <span>{formatInteger(tickCountForInstruments(market.instruments))} ticks</span>
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
              <p class="sidebar-empty">正在读取 ClickHouse 市场目录。</p>
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
                    <span>{formatInteger(option.tickCount)} ticks</span>
                  </span>
                </button>
              {/each}
            {/if}
          </div>
        </section>
      {/if}

      <details class="sidebar-details">
        <summary>换算率 / 实时</summary>
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
          <p class="eyebrow">Spread curve</p>
          <h1>{selectedBase || 'No market'}/{spread?.meta.targetQuote ?? selectedInstrumentA?.quoteAsset ?? '-'}</h1>
          <p>{selectedLabel(selectedA)} vs {selectedLabel(selectedB)}</p>
        </div>
        <div class="latest-card" class:positive={latestOpportunity?.tone === 'positive'} class:negative={latestOpportunity?.tone === 'negative'}>
          <span>Latest best</span>
          <strong>{formatSignedBp(latestOpportunity?.bp ?? null)}</strong>
          <small>{spreadStatus}</small>
        </div>
      </div>

      <div class="control-strip">
        <div class="segmented" aria-label="时间范围">
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

        <div class="segmented display-mode" aria-label="曲线显示方式">
          <button
            type="button"
            class:active={displayMode === 'best'}
            aria-pressed={displayMode === 'best'}
            on:click={() => void setDisplayModeAndQuery('best')}
          >
            单边最大价差
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

        {#if displayMode === 'both'}
          <div class="series-pills" aria-label="双边方向">
            <button type="button" class:active={showAToB} aria-pressed={showAToB} on:click={() => void toggleSeriesAndQuery('aToB')}>
              A bid - B ask
            </button>
            <button type="button" class:active={showBToA} aria-pressed={showBToA} on:click={() => void toggleSeriesAndQuery('bToA')}>
              B bid - A ask
            </button>
          </div>
        {/if}

        <button
          class="primary-button"
          type="button"
          disabled={spreadBusy || loadingMarkets || currentInstruments.length < 2}
          on:click={() => void loadSpread({ slideWindow: true })}
        >
          {spreadBusy ? '同步中' : '运行查询'}
        </button>
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
        </div>

        <p id="chart-help" class="chart-help">
          鼠标悬停或点击可锁定任意一点；聚焦图表后可用 Left / Right / Home / End 查看样本。
        </p>

        {#if loadingSpread}
          <div class="empty-state">正在查询 ClickHouse。</div>
        {:else if points.length === 0}
          <div class="empty-state">当前组合没有可比较的盘口状态样本。请调整交易所腿或时间范围。</div>
        {:else}
          <svg
            class="spread-chart"
            viewBox={`0 0 ${CHART.width} ${CHART.height}`}
            role="button"
            tabindex="0"
            aria-label="Cross venue spread chart. Hover, click, or use arrow keys to inspect samples."
            aria-describedby="chart-help"
            on:pointermove={handlePointerMove}
            on:pointerleave={() => (hoverIndex = -1)}
            on:click={handleChartClick}
            on:keydown={handleChartKeydown}
          >
            <rect class="plot-bg" x={CHART.left} y={CHART.top} width={plotWidth()} height={plotHeight()} />
            {#each [0, 0.25, 0.5, 0.75, 1] as tick}
              <line
                class="grid-line"
                x1={CHART.left}
                x2={CHART.width - CHART.right}
                y1={CHART.top + tick * plotHeight()}
                y2={CHART.top + tick * plotHeight()}
              />
            {/each}
            {#each [0, 0.25, 0.5, 0.75, 1] as tick}
              <line
                class="grid-line vertical"
                x1={CHART.left + tick * plotWidth()}
                x2={CHART.left + tick * plotWidth()}
                y1={CHART.top}
                y2={CHART.height - CHART.bottom}
              />
            {/each}
            <line class="zero-line" x1={CHART.left} x2={CHART.width - CHART.right} y1={zeroY} y2={zeroY} />
            {#if averageSpreadValue !== null}
              {@const avgY = yScale(averageSpreadValue, yBounds)}
              <line
                class="average-line"
                x1={CHART.left}
                x2={CHART.width - CHART.right}
                y1={avgY}
                y2={avgY}
              />
              <text
                class="average-label"
                x={CHART.width - CHART.right - 8}
                y={clamp(avgY - 7, CHART.top + 14, CHART.height - CHART.bottom - 6)}
              >
                AVG {formatNumber(averageSpreadValue)}
              </text>
            {/if}
            {#if displayMode === 'best'}
              <path class="spread-line best" d={bestPath} />
            {:else}
              {#if showAToB}
                <path class="spread-line a" d={aPath} />
              {/if}
              {#if showBToA}
                <path class="spread-line b" d={bPath} />
              {/if}
            {/if}
            <text class="axis-label y top" x="12" y={CHART.top + 4}>{formatNumber(yBounds.max)}</text>
            <text class="axis-label y middle" x="12" y={zeroY + 4}>0</text>
            <text class="axis-label y bottom" x="12" y={CHART.height - CHART.bottom}>
              {formatNumber(yBounds.min)}
            </text>
            <text class="axis-label x" x={CHART.left} y={CHART.height - 12}>
              {formatAxisTime(xBounds.min)}
            </text>
            <text class="axis-label x end" x={CHART.width - CHART.right} y={CHART.height - 12}>
              {formatAxisTime(xBounds.max)}
            </text>
            {#if activePoint}
              {@const activeBestValue = bestSpreadValue(activePoint)}
              <line
                class="cursor-line"
                x1={xScale(activePoint.tsMs, xBounds)}
                x2={xScale(activePoint.tsMs, xBounds)}
                y1={CHART.top}
                y2={CHART.height - CHART.bottom}
              />
              {#if displayMode === 'best' && activeBestValue !== null}
                <rect
                  class="point best"
                  x={xScale(activePoint.tsMs, xBounds) - 5}
                  y={yScale(activeBestValue, yBounds) - 5}
                  width="10"
                  height="10"
                />
              {:else}
                {#if showAToB && activePoint.aToB !== null}
                  <rect
                    class="point a"
                    x={xScale(activePoint.tsMs, xBounds) - 5}
                    y={yScale(activePoint.aToB, yBounds) - 5}
                    width="10"
                    height="10"
                  />
                {/if}
                {#if showBToA && activePoint.bToA !== null}
                  <rect
                    class="point b"
                    x={xScale(activePoint.tsMs, xBounds) - 5}
                    y={yScale(activePoint.bToA, yBounds) - 5}
                    width="10"
                    height="10"
                  />
                {/if}
              {/if}
            {/if}
          </svg>
        {/if}

        <p class="chart-caption">
          {#if spread?.meta.granularity === 'raw'}
            当前短窗口使用数据库逐 tick BBO 更新计算价差；每个样本来自 A/B 任一侧的新盘口，并与另一侧最新盘口对齐。
          {:else}
            当前先按事件流维护 A/B 最新盘口，再在每个 bucket 内选择可交易价差最极端的真实状态；盘口超过 {formatMaybeDuration(spread?.meta.maxStaleMs ?? null)} 未更新的状态会被跳过。
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
          <strong>{formatInteger(currentInstruments.length)} venues / {formatInteger(totalTicks)} ticks</strong>
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
                <div><dt>Ticks</dt><dd>{formatInteger(instrument.tickCount)}</dd></div>
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
          <span>Live</span>
          <strong>{autoRefresh ? `${refreshSeconds}s` : 'Paused'}</strong>
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
      Geist, Inter, "Helvetica Neue", Helvetica, "Noto Sans SC", "Microsoft YaHei UI", Arial, sans-serif;
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
    background:
      radial-gradient(circle at 80% 0%, rgba(48, 214, 151, 0.05), transparent 30rem),
      var(--background);
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

  .status-dot {
    width: 8px;
    height: 8px;
    flex: 0 0 auto;
    border-radius: 999px;
    background: var(--primary);
    box-shadow: 0 0 18px rgba(48, 214, 151, 0.7);
  }

  .brand-lockup div {
    display: grid;
    gap: 1px;
  }

  .brand-lockup strong,
  .topbar-meta,
  .eyebrow,
  .sidebar-heading,
  .section-heading,
  .stats-heading,
  label span,
  dt,
  .chart-help,
  .chart-caption,
  .market-meta,
  .meta-row span {
    font-family: "Geist Mono", "SFMono-Regular", Consolas, monospace;
  }

  .brand-lockup strong {
    font-size: 0.8rem;
    letter-spacing: 0.14em;
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
    letter-spacing: 0.08em;
    text-transform: uppercase;
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
    font-family: "Geist Mono", "SFMono-Regular", Consolas, monospace;
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
    font-family: "Geist Mono", "SFMono-Regular", Consolas, monospace;
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
    letter-spacing: 0.08em;
    text-transform: uppercase;
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
  button:focus-visible,
  .spread-chart:focus-visible {
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

  .eyebrow {
    margin: 0 0 6px;
    color: var(--muted-foreground);
    font-size: 0.74rem;
    font-weight: 600;
    letter-spacing: 0.12em;
    text-transform: uppercase;
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

  .pair-header p:not(.eyebrow) {
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
    font-family: "Geist Mono", "SFMono-Regular", Consolas, monospace;
    font-size: 1.15rem;
    font-variant-numeric: tabular-nums;
  }

  .control-strip {
    flex-wrap: wrap;
    gap: 10px;
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
    min-height: 36px;
    border: 0;
    border-right: 1px solid var(--border);
    border-radius: 0;
    background: transparent;
  }

  .segmented button:last-child,
  .series-pills button:last-child {
    border-right: 0;
  }

  .display-mode {
    margin-left: auto;
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
    font-family: "Geist Mono", "SFMono-Regular", Consolas, monospace;
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

  .spread-chart {
    display: block;
    width: 100%;
    height: min(44vh, 430px);
    min-height: 320px;
    cursor: crosshair;
    touch-action: none;
  }

  .plot-bg {
    fill: #0c0f14;
  }

  .grid-line {
    stroke: rgba(255, 255, 255, 0.08);
    stroke-width: 1;
  }

  .grid-line.vertical {
    stroke-dasharray: 2 10;
  }

  .zero-line {
    stroke: rgba(255, 255, 255, 0.32);
    stroke-dasharray: 8 8;
    stroke-width: 1.1;
  }

  .average-line {
    stroke: rgba(233, 235, 239, 0.58);
    stroke-dasharray: 4 7;
    stroke-width: 1.2;
  }

  .average-label {
    fill: var(--foreground);
    font-family: "Geist Mono", "SFMono-Regular", Consolas, monospace;
    font-size: 12px;
    font-weight: 650;
    paint-order: stroke;
    stroke: var(--background);
    stroke-linejoin: round;
    stroke-width: 4px;
    text-anchor: end;
  }

  .spread-line {
    fill: none;
    stroke-linecap: round;
    stroke-linejoin: round;
    stroke-width: 2.3;
  }

  .spread-line.best,
  .spread-line.a {
    stroke: var(--primary);
  }

  .spread-line.b {
    stroke: var(--warning);
  }

  .cursor-line {
    stroke: rgba(233, 235, 239, 0.8);
    stroke-width: 1;
  }

  .point {
    stroke: var(--background);
    stroke-width: 2;
  }

  .point.best,
  .point.a {
    fill: var(--primary);
  }

  .point.b {
    fill: var(--warning);
  }

  .axis-label {
    fill: var(--muted-foreground);
    font-family: "Geist Mono", "SFMono-Regular", Consolas, monospace;
    font-size: 12px;
  }

  .axis-label.x.end {
    text-anchor: end;
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
    font-family: "Geist Mono", "SFMono-Regular", Consolas, monospace;
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
    font-family: "Geist Mono", "SFMono-Regular", Consolas, monospace;
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
    letter-spacing: 0.06em;
    text-transform: uppercase;
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
    text-transform: uppercase;
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

    .display-mode {
      margin-left: 0;
    }
  }

  @media (max-width: 760px) {
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
    .segmented,
    .series-pills {
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

    .custom-range,
    .rate-row,
    .venue-grid dl div,
    .point-ledger div,
    .stat-list div,
    .meta-row {
      grid-template-columns: 1fr;
    }

    .spread-chart {
      min-height: 280px;
      height: 340px;
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
</style>
