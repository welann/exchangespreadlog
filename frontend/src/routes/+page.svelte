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
    { label: '5m', value: '5m', ms: 5 * 60 * 1000 },
    { label: '15m', value: '15m', ms: 15 * 60 * 1000 },
    { label: '1h', value: '1h', ms: 60 * 60 * 1000 },
    { label: '6h', value: '6h', ms: 6 * 60 * 60 * 1000 },
    { label: '24h', value: '24h', ms: 24 * 60 * 60 * 1000 },
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

  type WorkMode = 'compare' | 'qualify' | 'normalize';

  let markets: Market[] = [];
  let selectedBase = '';
  let selectedA = '';
  let selectedB = '';
  let selectedPreset = '15m';
  let customStart = toDateInput(Date.now() - 60 * 60 * 1000);
  let customEnd = toDateInput(Date.now());
  let rangeAnchorMs = Date.now();
  let rates: QuoteRate[] = structuredClone(defaultRates);
  let hydrated = false;
  let nowMs = Date.now();
  let showAToB = true;
  let showBToA = true;
  let autoRefresh = false;
  let refreshSeconds = 30;
  let refreshTimer: ReturnType<typeof setInterval> | null = null;
  let activeMode: WorkMode = 'compare';

  let marketError = '';
  let queryError = '';
  let loadingMarkets = false;
  let loadingSpread = false;
  let spread: SpreadResponse | null = null;
  let selectedIndex = -1;
  let hoverIndex = -1;

  $: currentMarket = markets.find((market) => market.baseAsset === selectedBase);
  $: currentInstruments = currentMarket?.instruments ?? [];
  $: marketLatestMs = latestForInstruments(currentInstruments);
  $: marketFreshness = freshnessFor(marketLatestMs, nowMs);
  $: totalTicks = currentInstruments.reduce((sum, instrument) => sum + instrument.tickCount, 0);
  $: selectedInstrumentA = currentInstruments.find((instrument) => instrument.catalogId === selectedA) ?? null;
  $: selectedInstrumentB = currentInstruments.find((instrument) => instrument.catalogId === selectedB) ?? null;
  $: selectedRange = currentRange(selectedPreset, customStart, customEnd, rangeAnchorMs);
  $: points = spread?.points ?? [];
  $: xBounds = computeXBounds(points, selectedRange);
  $: yBounds = computeYBounds(points, showAToB, showBToA);
  $: aPath = showAToB ? linePath(points, 'aToB', xBounds, yBounds) : '';
  $: bPath = showBToA ? linePath(points, 'bToA', xBounds, yBounds) : '';
  $: zeroY = yScale(0, yBounds);
  $: activeIndex = hoverIndex >= 0 ? hoverIndex : selectedIndex;
  $: activePoint = points[activeIndex] ?? null;
  $: latestPoint = points.length > 0 ? points[points.length - 1] : null;
  $: latestOpportunity = opportunityForPoint(latestPoint);
  $: activeOpportunity = opportunityForPoint(activePoint);
  $: pointRows = pointTableRows(points, activeIndex);
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
      ? 'Actionable on latest joined sample'
      : latestOpportunity.tone === 'negative'
        ? 'No positive cross on latest sample'
        : 'Flat at latest joined sample'
    : marketError
      ? 'ClickHouse connection is not ready'
      : 'Waiting for comparable samples';

  onMount(() => {
    hydrated = true;
    loadStoredRates();
    const queryState = readQueryState();
    void loadMarkets(queryState);
    const clockTimer = setInterval(() => {
      nowMs = Date.now();
    }, 30_000);

    return () => {
      clearInterval(clockTimer);
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
        await loadSpread();
      } else {
        marketError = 'No comparable markets were found. Check /api/health for ClickHouse table status.';
      }
    } catch (error) {
      marketError = error instanceof Error ? error.message : 'Failed to load markets';
    } finally {
      loadingMarkets = false;
    }
  }

  async function loadSpread() {
    if (!selectedA || !selectedB || selectedA === selectedB) {
      queryError = 'Choose two different instruments from the same market';
      return;
    }

    const range = currentRange(selectedPreset, customStart, customEnd, rangeAnchorMs);
    if (!Number.isFinite(range.fromMs) || !Number.isFinite(range.toMs) || range.fromMs >= range.toMs) {
      queryError = 'Choose a valid time range with From before To';
      return;
    }

    loadingSpread = true;
    queryError = '';
    hoverIndex = -1;
    try {
      const response = await fetch('/api/spread', {
        method: 'POST',
        headers: { 'content-type': 'application/json' },
        body: JSON.stringify({
          catalogA: selectedA,
          catalogB: selectedB,
          fromMs: range.fromMs,
          toMs: range.toMs,
          rates: cleanRates(rates)
        })
      });
      const body = await response.json();
      if (!response.ok) throw new Error(body.error ?? 'Failed to query spread');
      spread = body as SpreadResponse;
      selectedIndex = spread.points.length > 0 ? spread.points.length - 1 : -1;
      syncQueryState();
    } catch (error) {
      spread = null;
      selectedIndex = -1;
      queryError = error instanceof Error ? error.message : 'Failed to query spread';
    } finally {
      loadingSpread = false;
    }
  }

  function selectBase(baseAsset: string) {
    applySelectionState({ ...captureQueryState(), baseAsset });
  }

  function selectLegA(catalogId: string) {
    selectedA = catalogId;
    if (selectedA === selectedB) {
      selectedB = currentInstruments.find((instrument) => instrument.catalogId !== catalogId)?.catalogId ?? '';
    }
    selectedIndex = -1;
    hoverIndex = -1;
  }

  function selectLegB(catalogId: string) {
    selectedB = catalogId;
    if (selectedA === selectedB) {
      selectedA = currentInstruments.find((instrument) => instrument.catalogId !== catalogId)?.catalogId ?? '';
    }
    selectedIndex = -1;
    hoverIndex = -1;
  }

  function swapLegs() {
    if (!selectedA || !selectedB) return;
    const previousA = selectedA;
    selectedA = selectedB;
    selectedB = previousA;
    selectedIndex = -1;
    hoverIndex = -1;
  }

  async function refreshData() {
    nowMs = Date.now();
    await loadMarkets(captureQueryState());
  }

  function toggleAutoRefresh(enabled: boolean) {
    autoRefresh = enabled;
    configureAutoRefresh();
  }

  function updateRefreshSeconds(value: string) {
    const parsed = Number(value);
    refreshSeconds = Number.isFinite(parsed) ? parsed : 30;
    configureAutoRefresh();
  }

  function configureAutoRefresh() {
    stopAutoRefresh();
    if (!autoRefresh) return;
    refreshTimer = setInterval(() => {
      if (!loadingMarkets && !loadingSpread) void refreshData();
    }, refreshSeconds * 1000);
  }

  function stopAutoRefresh() {
    if (refreshTimer) clearInterval(refreshTimer);
    refreshTimer = null;
  }

  function toggleSeries(series: 'aToB' | 'bToA') {
    if (series === 'aToB') {
      showAToB = !showAToB;
      if (!showAToB && !showBToA) showBToA = true;
    } else {
      showBToA = !showBToA;
      if (!showAToB && !showBToA) showAToB = true;
    }
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

  function computeYBounds(data: SpreadPoint[], includeAToB: boolean, includeBToA: boolean) {
    const values = data
      .flatMap((point) => [
        includeAToB ? point.aToB : null,
        includeBToA ? point.bToA : null
      ])
      .filter((value): value is number => value !== null && Number.isFinite(value));

    if (values.length === 0) return { min: -1, max: 1 };
    const min = Math.min(0, ...values);
    const max = Math.max(0, ...values);
    if (Math.abs(max - min) < Number.EPSILON) return { min: -1, max: 1 };
    const padding = Math.max((max - min) * 0.12, 0.0001);
    return { min: min - padding, max: max + padding };
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

  function latestForInstruments(instruments: Market['instruments']) {
    const latest = instruments
      .map((instrument) => instrument.latestRecvMs)
      .filter((value): value is number => value !== null && Number.isFinite(value));
    return latest.length > 0 ? Math.max(...latest) : null;
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

  function freshnessFor(ms: number | null, now: number) {
    if (ms === null) {
      return { label: 'No ticks', detail: 'No market data in ClickHouse', className: 'stale' };
    }
    const ageMs = Math.max(0, now - ms);
    const className = ageMs <= 2 * 60 * 1000 ? 'fresh' : ageMs <= 30 * 60 * 1000 ? 'lagging' : 'stale';
    return {
      label: `${formatDuration(ageMs)} old`,
      detail: formatTime(ms),
      className
    };
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

  function formatBp(value: number | null) {
    if (value === null || !Number.isFinite(value)) return '-';
    return `${new Intl.NumberFormat(undefined, {
      minimumFractionDigits: 2,
      maximumFractionDigits: 2
    }).format(value)} bp`;
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

  function formatLatest(ms: number | null) {
    if (ms === null) return 'no ticks';
    return `latest ${formatTime(ms)}`;
  }

  function clamp(value: number, min: number, max: number) {
    return Math.min(max, Math.max(min, value));
  }
</script>

<svelte:head>
  <title>Exchange Spread Log</title>
  <meta
    name="description"
    content="Inspect cross-venue BBO spread curves from ClickHouse."
  />
</svelte:head>

<a class="skip-link" href="#spread-main">Skip to main content</a>

<main id="spread-main" class="swiss-shell">
  <header class="chrome">
    <div class="chrome-left">
      <span>Exchange Spread Console</span>
      <span>{selectedBase || 'No market'}</span>
    </div>
    <div class="chrome-right" aria-live="polite">
      {#if loadingMarkets}
        Loading markets
      {:else if points.length > 0}
        {formatInteger(points.length)} joined samples / {spread?.meta.bucketSeconds}s bucket
      {:else}
        {formatLatest(marketLatestMs)}
      {/if}
    </div>
  </header>

  {#if marketError}
    <section class="notice error" role="alert">{marketError}</section>
  {/if}
  {#if queryError}
    <section class="notice error" role="alert">{queryError}</section>
  {/if}

  <section class="decision-grid" aria-label="Latest trading decision">
    <div class="hero-copy">
      <p class="kicker">DECIDE / latest joined BBO</p>
      <h1>
        {routeCode}
        <span>{formatBp(latestOpportunity?.bp ?? null)}</span>
      </h1>
      <p class="decision-line">{spreadStatus}</p>
      <div class="route-line">
        <strong>{latestOpportunity?.label ?? 'Choose a comparable pair'}</strong>
        <span>{latestOpportunity?.route ?? 'Run a query to build the spread tape.'}</span>
      </div>
    </div>

    <aside class="hero-ledger" aria-label="Current market ledger">
      <div class="ledger-row">
        <span>Spread</span>
        <strong>{formatNumber(latestOpportunity?.value ?? null)} {spread?.meta.targetQuote ?? ''}</strong>
      </div>
      <div class="ledger-row">
        <span>Freshness</span>
        <strong>{marketFreshness.label}</strong>
      </div>
      <div class="ledger-row">
        <span>Coverage</span>
        <strong>{formatInteger(currentInstruments.length)} venues / {formatInteger(totalTicks)} ticks</strong>
      </div>
      <div class="ledger-row">
        <span>Window</span>
        <strong>{hydrated ? `${formatTime(selectedRange.fromMs)} -> ${formatTime(selectedRange.toMs)}` : '-'}</strong>
      </div>
    </aside>
  </section>

  <section class="query-board" aria-label="Spread query controls">
    <label>
      <span>Market</span>
      <select
        name="market"
        value={selectedBase}
        disabled={loadingMarkets || markets.length === 0}
        on:change={(event) => selectBase(selectValue(event))}
      >
        {#each markets as market}
          <option value={market.baseAsset}>
            {market.baseAsset} / {formatLatest(latestForInstruments(market.instruments))}
          </option>
        {/each}
      </select>
    </label>

    <label>
      <span>Leg A</span>
      <select
        name="leg-a"
        value={selectedA}
        disabled={currentInstruments.length < 2}
        on:change={(event) => selectLegA(selectValue(event))}
      >
        {#each currentInstruments as instrument}
          <option value={instrument.catalogId}>{instrument.label}</option>
        {/each}
      </select>
    </label>

    <button
      class="axis-button"
      type="button"
      disabled={!selectedA || !selectedB || selectedA === selectedB}
      on:click={swapLegs}
    >
      Swap
    </button>

    <label>
      <span>Leg B</span>
      <select
        name="leg-b"
        value={selectedB}
        disabled={currentInstruments.length < 2}
        on:change={(event) => selectLegB(selectValue(event))}
      >
        {#each currentInstruments as instrument}
          <option value={instrument.catalogId}>{instrument.label}</option>
        {/each}
      </select>
    </label>

    <div class="preset-grid" aria-label="Time range presets">
      {#each presets as preset}
        <button
          type="button"
          class:active={selectedPreset === preset.value}
          on:click={() => handlePreset(preset.value)}
        >
          {preset.label}
        </button>
      {/each}
    </div>

    {#if selectedPreset === 'custom'}
      <label>
        <span>From</span>
        <input type="datetime-local" name="spread-from" autocomplete="off" bind:value={customStart} />
      </label>
      <label>
        <span>To</span>
        <input type="datetime-local" name="spread-to" autocomplete="off" bind:value={customEnd} />
      </label>
    {/if}

    <button
      class="query-button"
      type="button"
      disabled={loadingSpread || loadingMarkets || currentInstruments.length < 2}
      on:click={loadSpread}
    >
      {loadingSpread ? 'Querying' : 'Run Query'}
    </button>
  </section>

  <nav class="mode-rail" aria-label="Analysis workspaces">
    <button
      type="button"
      class:active={activeMode === 'compare'}
      aria-pressed={activeMode === 'compare'}
      on:click={() => (activeMode = 'compare')}
    >
      <span>COMPARE</span>
      <small>curve / tape / point</small>
    </button>
    <button
      type="button"
      class:active={activeMode === 'qualify'}
      aria-pressed={activeMode === 'qualify'}
      on:click={() => (activeMode = 'qualify')}
    >
      <span>QUALIFY</span>
      <small>venue health / legs</small>
    </button>
    <button
      type="button"
      class:active={activeMode === 'normalize'}
      aria-pressed={activeMode === 'normalize'}
      on:click={() => (activeMode = 'normalize')}
    >
      <span>NORMALIZE</span>
      <small>quote rates / refresh</small>
    </button>
  </nav>

  {#if activeMode === 'compare'}
    <section class="workbench compare-layout" aria-label="Spread comparison">
      <section class="chart-panel">
        <div class="section-head">
          <div>
            <p class="kicker">COMPARE / {spread?.meta.targetQuote ?? selectedInstrumentA?.quoteAsset ?? '-'}</p>
            <h2>{selectedLabel(selectedA)} against {selectedLabel(selectedB)}</h2>
          </div>
          <div class="series-controls">
            <button
              type="button"
              class:active={showAToB}
              aria-pressed={showAToB}
              on:click={() => toggleSeries('aToB')}
            >
              A bid - B ask
            </button>
            <button
              type="button"
              class:active={showBToA}
              aria-pressed={showBToA}
              on:click={() => toggleSeries('bToA')}
            >
              B bid - A ask
            </button>
          </div>
        </div>
        <p id="chart-help" class="chart-help">
          Click to pin. Use Left / Right / Home / End when the chart has focus.
        </p>

        {#if loadingSpread}
          <div class="empty-state">Querying ClickHouse.</div>
        {:else if points.length === 0}
          <div class="empty-state">No joined BBO samples. Change pair or time range.</div>
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
            {#if showAToB}
              <path class="spread-line a" d={aPath} />
            {/if}
            {#if showBToA}
              <path class="spread-line b" d={bPath} />
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
              <line
                class="cursor-line"
                x1={xScale(activePoint.tsMs, xBounds)}
                x2={xScale(activePoint.tsMs, xBounds)}
                y1={CHART.top}
                y2={CHART.height - CHART.bottom}
              />
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
          </svg>
        {/if}
      </section>

      <aside class="point-panel">
        <div class="section-head compact">
          <div>
            <p class="kicker">POINT / pinned sample</p>
            <h2>{activePoint ? formatTime(activePoint.tsMs) : '-'}</h2>
          </div>
        </div>
        {#if activePoint && spread}
          <dl class="point-ledger">
            <div><dt>Best route</dt><dd>{activeOpportunity?.label ?? '-'}</dd></div>
            <div><dt>Best bp</dt><dd>{formatBp(activeOpportunity?.bp ?? null)}</dd></div>
            <div><dt>A -> B</dt><dd>{formatNumber(activePoint.aToB)} {spread.meta.targetQuote}</dd></div>
            <div><dt>B -> A</dt><dd>{formatNumber(activePoint.bToA)} {spread.meta.targetQuote}</dd></div>
            <div><dt>Mid diff</dt><dd>{formatNumber(activePoint.midDiff)} {spread.meta.targetQuote}</dd></div>
            <div><dt>A book</dt><dd>{formatNumber(activePoint.aBid)} / {formatNumber(activePoint.aAsk)}</dd></div>
            <div><dt>B book</dt><dd>{formatNumber(activePoint.bBid)} / {formatNumber(activePoint.bAsk)}</dd></div>
          </dl>
          <div class="point-actions">
            <button type="button" on:click={() => jumpPoint(-1)}>Previous</button>
            <button type="button" on:click={() => jumpPoint(1)}>Next</button>
            <button type="button" on:click={jumpLatest}>Latest</button>
          </div>
        {:else}
          <p class="empty-copy">Pin a sample from the curve or tape.</p>
        {/if}
      </aside>

      <section class="spread-tape" aria-label="Nearby spread samples">
        <div class="tape-head">
          <span>SPREAD TAPE</span>
          <span>{pointRows.length > 0 ? `${pointRows[0].index + 1}-${pointRows[pointRows.length - 1].index + 1}` : '0'} / {points.length}</span>
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
              <strong>{formatBp(rowOpportunity?.bp ?? null)}</strong>
              <small>{rowOpportunity?.label ?? '-'}</small>
            </button>
          {/each}
        </div>
      </section>
    </section>
  {:else if activeMode === 'qualify'}
    <section class="workbench qualify-layout" aria-label="Venue data quality">
      <div class="section-head">
        <div>
          <p class="kicker">QUALIFY / data source</p>
          <h2>Only compare legs with recent ticks and the same base asset.</h2>
        </div>
        <button type="button" on:click={refreshData} disabled={loadingMarkets || loadingSpread}>Refresh catalog</button>
      </div>
      <div class="venue-grid">
        {#each currentInstruments as instrument}
          {@const instrumentFreshness = freshnessFor(instrument.latestRecvMs, nowMs)}
          <article class:selected={instrument.catalogId === selectedA || instrument.catalogId === selectedB}>
            <div class="venue-top">
              <span>{instrument.venueInstanceId}</span>
              <strong>{instrument.rawSymbol}</strong>
            </div>
            <dl>
              <div><dt>Quote</dt><dd>{instrument.quoteAsset}</dd></div>
              <div><dt>Freshness</dt><dd>{instrumentFreshness.label}</dd></div>
              <div><dt>Ticks</dt><dd>{formatInteger(instrument.tickCount)}</dd></div>
              <div><dt>Status</dt><dd>{instrument.status}</dd></div>
            </dl>
            <div class="leg-actions" aria-label={`Set ${instrument.label} as chart leg`}>
              <button
                type="button"
                class:active={instrument.catalogId === selectedA}
                aria-pressed={instrument.catalogId === selectedA}
                on:click={() => selectLegA(instrument.catalogId)}
              >
                Set A
              </button>
              <button
                type="button"
                class:active={instrument.catalogId === selectedB}
                aria-pressed={instrument.catalogId === selectedB}
                on:click={() => selectLegB(instrument.catalogId)}
              >
                Set B
              </button>
            </div>
          </article>
        {/each}
      </div>
    </section>
  {:else}
    <section class="workbench normalize-layout" aria-label="Quote conversion and refresh settings">
      <div class="section-head">
        <div>
          <p class="kicker">NORMALIZE / quote rates</p>
          <h2>Rates apply before spread and bp are calculated.</h2>
        </div>
        <button type="button" on:click={resetRates}>Reset rates</button>
      </div>
      <div class="normal-grid">
        <section class="rate-editor">
          <div class="rate-grid heading" aria-hidden="true">
            <span>From</span>
            <span>To</span>
            <span>Rate</span>
            <span></span>
          </div>
          {#each rates as rate, index (index)}
            <div class="rate-grid">
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
              <button type="button" aria-label="Remove quote rate" on:click={() => removeRate(index)}>
                x
              </button>
            </div>
          {/each}
          <button class="add-rate" type="button" on:click={addRate}>Add rate</button>
        </section>

        <aside class="refresh-card">
          <p class="kicker">Refresh policy</p>
          <label class="checkbox-line">
            <input
              type="checkbox"
              name="auto-refresh"
              checked={autoRefresh}
              on:change={(event) => toggleAutoRefresh((event.currentTarget as HTMLInputElement).checked)}
            />
            Auto refresh catalog and spread
          </label>
          <label>
            <span>Interval</span>
            <select
              name="refresh-interval"
              aria-label="Auto refresh interval"
              value={refreshSeconds}
              disabled={!autoRefresh}
              on:change={(event) => updateRefreshSeconds(selectValue(event))}
            >
              <option value="15">15s</option>
              <option value="30">30s</option>
              <option value="60">60s</option>
            </select>
          </label>
          <button type="button" on:click={refreshData} disabled={loadingMarkets || loadingSpread}>
            Refresh now
          </button>
        </aside>
      </div>
    </section>
  {/if}
</main>

<style>
  :global(*) {
    box-sizing: border-box;
  }

  :global(body) {
    --paper: #fafaf8;
    --ink: #0a0a0a;
    --grey-1: #f0f0ee;
    --grey-2: #d4d4d2;
    --grey-3: #737373;
    --accent: #002fa7;
    --accent-on: #ffffff;
    --hairline: 1px solid var(--grey-2);
    margin: 0;
    min-width: 320px;
    overflow-x: hidden;
    color: var(--ink);
    background: var(--paper);
    font-family:
      Inter, "Helvetica Neue", Helvetica, "Noto Sans SC", "Microsoft YaHei UI", Arial, sans-serif;
    -webkit-tap-highlight-color: rgba(0, 47, 167, 0.14);
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
    z-index: 20;
    padding: 10px 12px;
    color: var(--accent-on);
    background: var(--accent);
    transform: translateY(-160%);
    transition: transform 160ms ease;
  }

  .skip-link:focus-visible {
    transform: translateY(0);
    outline: 2px solid var(--ink);
  }

  .swiss-shell {
    width: min(1680px, calc(100vw - 40px));
    margin: 0 auto;
    padding: 28px 0 56px;
  }

  .chrome {
    display: flex;
    justify-content: space-between;
    gap: 24px;
    border-bottom: var(--hairline);
    padding: 0 0 18px;
    font-family: "JetBrains Mono", "SFMono-Regular", Consolas, monospace;
    font-size: 0.78rem;
    font-weight: 600;
    letter-spacing: 0.1em;
    text-transform: uppercase;
  }

  .chrome-left,
  .chrome-right {
    display: flex;
    flex-wrap: wrap;
    gap: 18px;
  }

  .chrome-right {
    justify-content: flex-end;
    color: var(--grey-3);
  }

  .notice {
    margin-top: 16px;
    border: var(--hairline);
    border-left: 8px solid var(--ink);
    padding: 14px 16px;
    background: var(--paper);
    font-weight: 500;
  }

  .notice.error {
    border-left-color: var(--accent);
  }

  .decision-grid {
    display: grid;
    grid-template-columns: repeat(16, minmax(0, 1fr));
    gap: 16px;
    border-bottom: var(--hairline);
    padding: 24px 0 22px;
  }

  .hero-copy {
    grid-column: 1 / 11;
    display: grid;
    align-content: space-between;
    min-height: 310px;
  }

  .kicker,
  label span,
  .tape-head,
  .rate-grid.heading {
    color: var(--grey-3);
    font-family: "JetBrains Mono", "SFMono-Regular", Consolas, monospace;
    font-size: 0.78rem;
    font-weight: 600;
    letter-spacing: 0.12em;
    text-transform: uppercase;
  }

  .kicker {
    margin: 0;
  }

  h1,
  h2,
  p {
    margin: 0;
  }

  .hero-copy h1 {
    display: grid;
    gap: 0.02em;
    font-size: min(10vw, 16vh);
    font-weight: 200;
    letter-spacing: -0.07em;
    line-height: 0.83;
  }

  .hero-copy h1 span {
    color: var(--accent);
    font-size: min(5.8vw, 9.4vh);
    font-weight: 200;
    letter-spacing: -0.05em;
  }

  .decision-line {
    max-width: 36ch;
    font-size: clamp(1.35rem, 2.2vw, 2.6rem);
    font-weight: 300;
    line-height: 1.05;
    letter-spacing: -0.035em;
  }

  .route-line {
    display: grid;
    gap: 8px;
    border-top: 4px solid var(--accent);
    padding-top: 16px;
  }

  .route-line strong {
    font-size: clamp(1.2rem, 1.8vw, 2rem);
    font-weight: 400;
  }

  .route-line span {
    color: var(--grey-3);
    font-size: 1rem;
    line-height: 1.5;
  }

  .hero-ledger {
    grid-column: 11 / 17;
    align-self: stretch;
    border-left: var(--hairline);
    padding-left: 16px;
  }

  .ledger-row {
    display: grid;
    grid-template-columns: minmax(96px, 0.65fr) minmax(0, 1fr);
    gap: 14px;
    border-bottom: var(--hairline);
    padding: 18px 0;
  }

  .ledger-row span,
  dt {
    color: var(--grey-3);
    font-family: "JetBrains Mono", "SFMono-Regular", Consolas, monospace;
    font-size: 0.78rem;
    font-weight: 600;
    letter-spacing: 0.08em;
    text-transform: uppercase;
  }

  .ledger-row strong,
  dd {
    margin: 0;
    overflow-wrap: anywhere;
    font-family: "JetBrains Mono", "SFMono-Regular", Consolas, monospace;
    font-variant-numeric: tabular-nums;
    font-weight: 600;
  }

  .query-board {
    display: grid;
    grid-template-columns: repeat(16, minmax(0, 1fr));
    align-items: end;
    gap: 12px;
    border-bottom: var(--hairline);
    padding: 18px 0;
  }

  label {
    display: grid;
    grid-column: span 2;
    gap: 7px;
  }

  .query-board label:nth-of-type(1) {
    grid-column: span 3;
  }

  .query-board label:nth-of-type(2),
  .query-board label:nth-of-type(3) {
    grid-column: span 3;
  }

  .axis-button {
    grid-column: span 1;
  }

  .preset-grid {
    grid-column: span 3;
    display: grid;
    grid-template-columns: repeat(3, minmax(0, 1fr));
    gap: 0;
    border: var(--hairline);
  }

  .query-button {
    grid-column: span 3;
  }

  select,
  input,
  button {
    min-height: 44px;
    border: var(--hairline);
    border-radius: 0;
    color: var(--ink);
    background: var(--paper);
    outline: 2px solid transparent;
    touch-action: manipulation;
  }

  select,
  input {
    width: 100%;
    padding: 0 10px;
  }

  button {
    padding: 0 12px;
    cursor: pointer;
    font-weight: 600;
    transition:
      background 150ms ease,
      color 150ms ease,
      border-color 150ms ease;
  }

  button:hover:not(:disabled),
  button.active {
    color: var(--accent-on);
    border-color: var(--accent);
    background: var(--accent);
  }

  button:disabled {
    cursor: not-allowed;
    opacity: 0.45;
  }

  select:focus-visible,
  input:focus-visible,
  button:focus-visible,
  .spread-chart:focus-visible {
    border-color: var(--accent);
    outline-color: var(--accent);
  }

  .preset-grid button {
    min-height: 42px;
    border: 0;
    border-right: var(--hairline);
  }

  .preset-grid button:last-child {
    border-right: 0;
  }

  .mode-rail {
    display: grid;
    grid-template-columns: repeat(3, minmax(0, 1fr));
    border-bottom: var(--hairline);
  }

  .mode-rail button {
    display: grid;
    justify-items: start;
    min-height: 84px;
    border: 0;
    border-right: var(--hairline);
    padding: 14px 16px;
    text-align: left;
  }

  .mode-rail button:last-child {
    border-right: 0;
  }

  .mode-rail span {
    font-family: "JetBrains Mono", "SFMono-Regular", Consolas, monospace;
    letter-spacing: 0.1em;
  }

  .mode-rail small {
    color: var(--grey-3);
    font-weight: 500;
  }

  .workbench {
    border-bottom: var(--hairline);
    padding: 18px 0 0;
  }

  .compare-layout {
    display: grid;
    grid-template-columns: repeat(16, minmax(0, 1fr));
    gap: 16px;
  }

  .chart-panel {
    grid-column: 1 / 12;
    min-width: 0;
    border-right: var(--hairline);
    padding-right: 16px;
  }

  .point-panel {
    grid-column: 12 / 17;
    min-width: 0;
  }

  .section-head {
    display: flex;
    align-items: start;
    justify-content: space-between;
    gap: 18px;
    margin-bottom: 18px;
  }

  .section-head h2 {
    max-width: 26ch;
    font-size: clamp(1.8rem, 3.3vw, 4.8rem);
    font-weight: 200;
    line-height: 0.95;
    letter-spacing: -0.055em;
  }

  .section-head.compact h2 {
    font-size: clamp(1.2rem, 1.8vw, 2.2rem);
    letter-spacing: -0.035em;
  }

  .series-controls,
  .point-actions,
  .leg-actions {
    display: flex;
    flex-wrap: wrap;
    gap: 8px;
  }

  .series-controls button,
  .point-actions button,
  .leg-actions button {
    min-height: 38px;
  }

  .chart-help,
  .empty-copy {
    margin: 0 0 10px;
    color: var(--grey-3);
    font-family: "JetBrains Mono", "SFMono-Regular", Consolas, monospace;
    font-size: 0.78rem;
  }

  .empty-state {
    display: grid;
    min-height: 430px;
    place-items: center;
    border: var(--hairline);
    color: var(--grey-3);
    text-align: center;
  }

  .spread-chart {
    display: block;
    width: 100%;
    height: auto;
    cursor: crosshair;
    touch-action: none;
  }

  .plot-bg {
    fill: var(--paper);
  }

  .grid-line {
    stroke: var(--grey-2);
    stroke-width: 1;
  }

  .grid-line.vertical {
    stroke-dasharray: 1 10;
  }

  .zero-line {
    stroke: var(--grey-3);
    stroke-dasharray: 8 8;
    stroke-width: 1.2;
  }

  .spread-line {
    fill: none;
    stroke-linecap: square;
    stroke-linejoin: miter;
    stroke-width: 2.2;
  }

  .spread-line.a {
    stroke: var(--accent);
  }

  .spread-line.b {
    stroke: var(--ink);
    stroke-dasharray: 7 5;
  }

  .cursor-line {
    stroke: var(--ink);
    stroke-width: 1;
  }

  .point {
    stroke: var(--paper);
    stroke-width: 2;
  }

  .point.a {
    fill: var(--accent);
  }

  .point.b {
    fill: var(--ink);
  }

  .axis-label {
    fill: var(--grey-3);
    font-family: "JetBrains Mono", "SFMono-Regular", Consolas, monospace;
    font-size: 12px;
  }

  .axis-label.x.end {
    text-anchor: end;
  }

  .point-ledger {
    display: grid;
    margin: 0;
  }

  .point-ledger div {
    display: grid;
    grid-template-columns: minmax(92px, 0.62fr) minmax(0, 1fr);
    gap: 12px;
    border-bottom: var(--hairline);
    padding: 12px 0;
  }

  .point-actions {
    margin-top: 14px;
  }

  .spread-tape {
    grid-column: 1 / 17;
    border-top: var(--hairline);
    padding: 16px 0 0;
  }

  .tape-head {
    display: flex;
    justify-content: space-between;
    margin-bottom: 10px;
  }

  .tape-grid {
    display: grid;
    grid-template-columns: repeat(9, minmax(0, 1fr));
    gap: 0;
    border-left: var(--hairline);
    border-top: var(--hairline);
  }

  .tape-grid button {
    display: grid;
    min-height: 112px;
    align-content: space-between;
    justify-items: start;
    border: 0;
    border-right: var(--hairline);
    border-bottom: var(--hairline);
    padding: 10px;
    text-align: left;
  }

  .tape-grid strong {
    font-family: "JetBrains Mono", "SFMono-Regular", Consolas, monospace;
    font-size: 1rem;
  }

  .tape-grid small {
    color: var(--grey-3);
  }

  .qualify-layout,
  .normalize-layout {
    display: grid;
    gap: 18px;
  }

  .venue-grid {
    display: grid;
    grid-template-columns: repeat(3, minmax(0, 1fr));
    border-left: var(--hairline);
    border-top: var(--hairline);
  }

  .venue-grid article {
    display: grid;
    gap: 18px;
    border-right: var(--hairline);
    border-bottom: var(--hairline);
    padding: 16px;
  }

  .venue-grid article.selected {
    border-top: 8px solid var(--accent);
    padding-top: 8px;
  }

  .venue-top {
    display: grid;
    gap: 8px;
  }

  .venue-top span {
    color: var(--grey-3);
    font-family: "JetBrains Mono", "SFMono-Regular", Consolas, monospace;
    font-size: 0.78rem;
    font-weight: 600;
    text-transform: uppercase;
  }

  .venue-top strong {
    overflow-wrap: anywhere;
    font-size: clamp(1.4rem, 2.3vw, 3.2rem);
    font-weight: 200;
    line-height: 0.95;
    letter-spacing: -0.045em;
  }

  .venue-grid dl {
    display: grid;
    gap: 0;
    margin: 0;
  }

  .venue-grid dl div {
    display: grid;
    grid-template-columns: minmax(86px, 0.5fr) minmax(0, 1fr);
    gap: 12px;
    border-bottom: var(--hairline);
    padding: 9px 0;
  }

  .normal-grid {
    display: grid;
    grid-template-columns: minmax(0, 1fr) 360px;
    gap: 16px;
  }

  .rate-editor,
  .refresh-card {
    border-top: var(--hairline);
    padding-top: 12px;
  }

  .rate-grid {
    display: grid;
    grid-template-columns: 1fr 1fr 1.1fr 46px;
    gap: 0;
    border-left: var(--hairline);
    border-top: var(--hairline);
  }

  .rate-grid + .rate-grid {
    border-top: 0;
  }

  .rate-grid > * {
    border: 0;
    border-right: var(--hairline);
    border-bottom: var(--hairline);
  }

  .rate-grid.heading span {
    min-height: 38px;
    padding: 10px;
  }

  .rate-grid button {
    padding: 0;
  }

  .add-rate {
    width: 100%;
    margin-top: 12px;
  }

  .refresh-card {
    display: grid;
    align-content: start;
    gap: 16px;
  }

  .checkbox-line {
    display: flex;
    min-height: 44px;
    align-items: center;
    gap: 10px;
  }

  .checkbox-line input {
    width: 18px;
    min-height: 18px;
    accent-color: var(--accent);
  }

  @media (max-width: 1180px) {
    .decision-grid,
    .query-board,
    .compare-layout {
      grid-template-columns: repeat(8, minmax(0, 1fr));
    }

    .hero-copy,
    .hero-ledger,
    .chart-panel,
    .point-panel,
    .spread-tape {
      grid-column: 1 / -1;
    }

    .hero-ledger,
    .chart-panel {
      border-left: 0;
      border-right: 0;
      padding-left: 0;
      padding-right: 0;
    }

    .query-board label,
    .axis-button,
    .preset-grid,
    .query-button {
      grid-column: span 4 !important;
    }

    .tape-grid {
      grid-template-columns: repeat(3, minmax(0, 1fr));
    }

    .venue-grid {
      grid-template-columns: repeat(2, minmax(0, 1fr));
    }

    .normal-grid {
      grid-template-columns: 1fr;
    }
  }

  @media (max-width: 740px) {
    .swiss-shell {
      width: min(100vw - 20px, 1680px);
      padding-top: 14px;
    }

    .chrome,
    .section-head {
      display: grid;
    }

    .hero-copy {
      min-height: 360px;
    }

    .hero-copy h1 {
      font-size: min(26vw, 16vh);
    }

    .hero-copy h1 span {
      font-size: min(15vw, 9vh);
    }

    .query-board label,
    .axis-button,
    .preset-grid,
    .query-button {
      grid-column: 1 / -1 !important;
    }

    .mode-rail,
    .venue-grid {
      grid-template-columns: 1fr;
    }

    .mode-rail button {
      border-right: 0;
      border-bottom: var(--hairline);
    }

    .tape-grid {
      grid-template-columns: 1fr;
    }

    .rate-grid {
      grid-template-columns: 1fr;
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
