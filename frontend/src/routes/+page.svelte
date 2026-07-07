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

<main id="spread-main" class="page-shell">
  <header class="topbar">
    <div>
      <p class="eyebrow">ClickHouse spread console</p>
      <h1>Cross-venue spread curves</h1>
    </div>
    <div class="topbar-actions">
      <div class="connection" aria-live="polite">
        <span class:live={points.length > 0} aria-hidden="true"></span>
        {#if loadingMarkets}
          Loading markets…
        {:else if points.length > 0}
          {formatInteger(points.length)} samples, {spread?.meta.bucketSeconds}s buckets
        {:else if currentMarket}
          {formatLatest(marketLatestMs)}
        {:else}
          Waiting for query
        {/if}
      </div>
      <button
        class="ghost-button"
        type="button"
        disabled={loadingMarkets || loadingSpread || markets.length === 0}
        on:click={refreshData}
      >
        Refresh Latest
      </button>
    </div>
  </header>

  {#if marketError}
    <section class="notice error" role="alert">{marketError}</section>
  {/if}

  <section class="control-strip" aria-label="Spread query controls">
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
            {market.baseAsset} · {formatLatest(latestForInstruments(market.instruments))}
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

    <button
      class="swap-button"
      type="button"
      disabled={!selectedA || !selectedB || selectedA === selectedB}
      on:click={swapLegs}
    >
      Swap Legs
    </button>

    <div class="preset-group" aria-label="Time range presets">
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
      <label class="time-input">
        <span>From</span>
        <input
          type="datetime-local"
          name="spread-from"
          autocomplete="off"
          bind:value={customStart}
        />
      </label>
      <label class="time-input">
        <span>To</span>
        <input
          type="datetime-local"
          name="spread-to"
          autocomplete="off"
          bind:value={customEnd}
        />
      </label>
    {/if}

    <button
      class="query-button"
      type="button"
      disabled={loadingSpread || loadingMarkets || currentInstruments.length < 2}
      on:click={loadSpread}
    >
      {loadingSpread ? 'Querying…' : 'Run Query'}
    </button>

    <div class="auto-refresh">
      <span>Refresh</span>
      <label class="checkbox-line">
        <input
          type="checkbox"
          name="auto-refresh"
          checked={autoRefresh}
          on:change={(event) => toggleAutoRefresh((event.currentTarget as HTMLInputElement).checked)}
        />
        Auto
      </label>
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
    </div>
  </section>

  {#if queryError}
    <section class="notice error" role="alert">{queryError}</section>
  {/if}

  <section class="metric-strip" aria-label="Current spread summary">
    <article class={`metric ${latestOpportunity?.tone ?? 'neutral'}`}>
      <p class="metric-label">Latest Best Route</p>
      <strong>{latestOpportunity?.label ?? 'No route'}</strong>
      <span>
        {formatNumber(latestOpportunity?.value ?? null)} {spread?.meta.targetQuote ?? ''}
        · {formatBp(latestOpportunity?.bp ?? null)}
      </span>
    </article>

    <article class="metric">
      <p class="metric-label">Selected Point</p>
      <strong>{activePoint ? formatTime(activePoint.tsMs) : 'No point selected'}</strong>
      <span>{activeOpportunity?.route ?? 'Hover, click, or use arrow keys on the chart'}</span>
    </article>

    <article class="metric">
      <p class="metric-label">Data Freshness</p>
      <strong>
        <span class={`freshness ${marketFreshness.className}`}>{marketFreshness.label}</span>
      </strong>
      <span>{marketFreshness.detail}</span>
    </article>

    <article class="metric">
      <p class="metric-label">Market Coverage</p>
      <strong>{formatInteger(currentInstruments.length)} venues</strong>
      <span>{formatInteger(totalTicks)} ticks · {selectedPreset} window</span>
    </article>
  </section>

  <section class="workspace">
    <section class="chart-panel">
      <div class="chart-title">
        <div>
          <p class="eyebrow">{selectedBase || 'No market selected'}</p>
          <h2>{selectedLabel(selectedA)} vs {selectedLabel(selectedB)}</h2>
          <p class="chart-subtitle">
            {hydrated ? `${formatTime(selectedRange.fromMs)} - ${formatTime(selectedRange.toMs)}` : 'Loading range…'}
            · target {spread?.meta.targetQuote ?? selectedInstrumentA?.quoteAsset ?? selectedInstrumentB?.quoteAsset ?? '-'}
          </p>
        </div>
        <div class="legend">
          <button
            class="series-toggle"
            class:inactive={!showAToB}
            type="button"
            aria-pressed={showAToB}
            on:click={() => toggleSeries('aToB')}
          >
            <i class="line-a" aria-hidden="true"></i>A bid - B ask
          </button>
          <button
            class="series-toggle"
            class:inactive={!showBToA}
            type="button"
            aria-pressed={showBToA}
            on:click={() => toggleSeries('bToA')}
          >
            <i class="line-b" aria-hidden="true"></i>B bid - A ask
          </button>
          <span><i class="zero" aria-hidden="true"></i>zero</span>
        </div>
      </div>
      <p id="chart-help" class="chart-help">
        Click the curve to pin a sample. Use Left, Right, Home, and End while focused on the chart.
      </p>

      {#if loadingSpread}
        <div class="empty-state">Querying ClickHouse…</div>
      {:else if points.length === 0}
        <div class="empty-state">
          No joined BBO samples for this pair and time range. Adjust the range or pair.
        </div>
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
          <rect
            class="plot-bg"
            x={CHART.left}
            y={CHART.top}
            width={plotWidth()}
            height={plotHeight()}
          />
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
          <line
            class="zero-line"
            x1={CHART.left}
            x2={CHART.width - CHART.right}
            y1={zeroY}
            y2={zeroY}
          />
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
              <circle
                class="point a"
                cx={xScale(activePoint.tsMs, xBounds)}
                cy={yScale(activePoint.aToB, yBounds)}
                r="5"
              />
            {/if}
            {#if showBToA && activePoint.bToA !== null}
              <circle
                class="point b"
                cx={xScale(activePoint.tsMs, xBounds)}
                cy={yScale(activePoint.bToA, yBounds)}
                r="5"
              />
            {/if}
          {/if}
        </svg>
      {/if}
    </section>

    <aside class="side-panel">
      <section class="readout">
        <p class="eyebrow">Point inspector</p>
        {#if activePoint && spread}
          <h3>{formatTime(activePoint.tsMs)}</h3>
          {#if activeOpportunity}
            <div class={`route-card ${activeOpportunity.tone}`}>
              <span>{activeOpportunity.label}</span>
              <strong>
                {formatNumber(activeOpportunity.value)} {spread.meta.targetQuote}
                · {formatBp(activeOpportunity.bp)}
              </strong>
              <small>{activeOpportunity.route}</small>
            </div>
          {/if}
          <dl>
            <div>
              <dt>A bid - B ask</dt>
              <dd>{formatNumber(activePoint.aToB)} {spread.meta.targetQuote}</dd>
            </div>
            <div>
              <dt>A -> B bp</dt>
              <dd>{formatBp(activePoint.aToBBp)}</dd>
            </div>
            <div>
              <dt>B bid - A ask</dt>
              <dd>{formatNumber(activePoint.bToA)} {spread.meta.targetQuote}</dd>
            </div>
            <div>
              <dt>B -> A bp</dt>
              <dd>{formatBp(activePoint.bToABp)}</dd>
            </div>
            <div>
              <dt>Mid diff</dt>
              <dd>{formatNumber(activePoint.midDiff)} {spread.meta.targetQuote}</dd>
            </div>
          </dl>
          <div class="book-snapshot">
            <span>A bid/ask {formatNumber(activePoint.aBid)} / {formatNumber(activePoint.aAsk)}</span>
            <span>B bid/ask {formatNumber(activePoint.bBid)} / {formatNumber(activePoint.bAsk)}</span>
          </div>
          <div class="point-actions">
            <button type="button" disabled={points.length === 0} on:click={() => jumpPoint(-1)}>
              Previous
            </button>
            <button type="button" disabled={points.length === 0} on:click={() => jumpPoint(1)}>
              Next
            </button>
            <button type="button" disabled={points.length === 0} on:click={jumpLatest}>Latest</button>
          </div>
        {:else}
          <p class="muted">Hover or click the curve after a query to inspect exact values.</p>
        {/if}
      </section>

      <section class="points-panel">
        <div class="panel-heading">
          <div>
            <p class="eyebrow">Sample table</p>
            <h3>Nearby points</h3>
          </div>
        </div>
        {#if pointRows.length === 0}
          <p class="muted">Run a query to show a keyboard-friendly table for the chart.</p>
        {:else}
          <div class="table-scroll">
            <table aria-label="Nearby spread samples">
              <thead>
                <tr>
                  <th scope="col">Time</th>
                  <th scope="col">A -> B</th>
                  <th scope="col">B -> A</th>
                  <th scope="col">Best bp</th>
                </tr>
              </thead>
              <tbody>
                {#each pointRows as row}
                  {@const rowOpportunity = opportunityForPoint(row.point)}
                  <tr class:active={row.index === activeIndex}>
                    <td>
                      <button
                        class="row-select"
                        type="button"
                        aria-pressed={row.index === activeIndex}
                        on:click={() => selectPoint(row.index)}
                      >
                        {formatAxisTime(row.point.tsMs)}
                      </button>
                    </td>
                    <td>{formatNumber(row.point.aToB)}</td>
                    <td>{formatNumber(row.point.bToA)}</td>
                    <td>{formatBp(rowOpportunity?.bp ?? null)}</td>
                  </tr>
                {/each}
              </tbody>
            </table>
          </div>
        {/if}
      </section>

      <section class="coverage-panel">
        <div class="panel-heading">
          <div>
            <p class="eyebrow">Market coverage</p>
            <h3>Venue data status</h3>
          </div>
        </div>
        <div class="coverage-list">
          {#each currentInstruments as instrument}
            {@const instrumentFreshness = freshnessFor(instrument.latestRecvMs, nowMs)}
            <div
              class="coverage-row"
              class:selected={instrument.catalogId === selectedA || instrument.catalogId === selectedB}
            >
              <div class="coverage-main">
                <strong>{instrument.venueInstanceId}</strong>
                <span>{instrument.rawSymbol} · {instrument.quoteAsset}</span>
              </div>
              <div class="coverage-meta">
                <span class={`freshness ${instrumentFreshness.className}`}>{instrumentFreshness.label}</span>
                <span>{formatInteger(instrument.tickCount)} ticks</span>
              </div>
              <div class="leg-actions" aria-label={`Set ${instrument.label} as chart leg`}>
                <button
                  type="button"
                  class:active={instrument.catalogId === selectedA}
                  aria-pressed={instrument.catalogId === selectedA}
                  on:click={() => selectLegA(instrument.catalogId)}
                >
                  A
                </button>
                <button
                  type="button"
                  class:active={instrument.catalogId === selectedB}
                  aria-pressed={instrument.catalogId === selectedB}
                  on:click={() => selectLegB(instrument.catalogId)}
                >
                  B
                </button>
              </div>
            </div>
          {/each}
        </div>
      </section>

      <section class="rates-panel">
        <div class="panel-heading">
          <div>
            <p class="eyebrow">Quote conversion</p>
            <h3>Rates used by this page</h3>
          </div>
          <button type="button" on:click={resetRates}>Reset</button>
        </div>

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
              placeholder="e.g. USDC…"
              on:input={(event) => updateRate(index, 'from', inputValue(event))}
            />
            <input
              aria-label="Quote to"
              name={`quote-to-${index}`}
              autocomplete="off"
              value={rate.to}
              placeholder="e.g. USD…"
              on:input={(event) => updateRate(index, 'to', inputValue(event))}
            />
            <input
              aria-label="Quote rate"
              name={`quote-rate-${index}`}
              value={rate.rate}
              inputmode="decimal"
              placeholder="e.g. 1…"
              on:input={(event) => updateRate(index, 'rate', inputValue(event))}
            />
            <button type="button" aria-label="Remove quote rate" on:click={() => removeRate(index)}>
              x
            </button>
          </div>
        {/each}
        <button class="add-rate" type="button" on:click={addRate}>Add rate</button>
      </section>
    </aside>
  </section>
</main>

<style>
  :global(*) {
    box-sizing: border-box;
  }

  :global(body) {
    margin: 0;
    min-width: 320px;
    overflow-x: hidden;
    color: #1e293b;
    background:
      linear-gradient(90deg, rgba(37, 99, 235, 0.055) 1px, transparent 1px),
      linear-gradient(180deg, rgba(37, 99, 235, 0.045) 1px, transparent 1px),
      #f8fafc;
    background-size: 28px 28px;
    font-family:
      Inter, ui-sans-serif, system-ui, -apple-system, BlinkMacSystemFont, "Segoe UI", sans-serif;
    -webkit-tap-highlight-color: rgba(37, 99, 235, 0.14);
  }

  :global(button),
  :global(input),
  :global(select) {
    font: inherit;
  }

  .page-shell {
    width: min(1480px, calc(100vw - 32px));
    margin: 0 auto;
    padding: 28px 0 36px;
  }

  .skip-link {
    position: fixed;
    top: 12px;
    left: 12px;
    z-index: 20;
    border-radius: 6px;
    padding: 10px 12px;
    color: #ffffff;
    background: #0f172a;
    transform: translateY(-160%);
    transition: transform 180ms ease;
  }

  .skip-link:focus-visible {
    transform: translateY(0);
    outline: 3px solid rgba(37, 99, 235, 0.35);
  }

  .topbar,
  .control-strip,
  .metric,
  .chart-panel,
  .readout,
  .points-panel,
  .coverage-panel,
  .rates-panel,
  .notice {
    border: 1px solid #dbe4ef;
    background: rgba(255, 255, 255, 0.94);
    box-shadow: 0 18px 46px rgba(15, 23, 42, 0.07);
  }

  .topbar {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 20px;
    padding: 18px 22px;
    border-radius: 8px 8px 0 0;
  }

  .eyebrow {
    margin: 0 0 7px;
    color: #64748b;
    font-family: "JetBrains Mono", "SFMono-Regular", Consolas, monospace;
    font-size: 0.72rem;
    font-weight: 700;
    letter-spacing: 0.08em;
    text-transform: uppercase;
  }

  h1,
  h2,
  h3 {
    margin: 0;
    letter-spacing: 0;
  }

  h1 {
    max-width: 760px;
    font-size: clamp(1.55rem, 3vw, 2.55rem);
    line-height: 1.05;
  }

  h2 {
    font-size: clamp(1.2rem, 2vw, 2rem);
  }

  h3 {
    font-size: 1.05rem;
  }

  .topbar-actions {
    display: grid;
    justify-items: end;
    gap: 10px;
  }

  .connection {
    display: inline-flex;
    align-items: center;
    gap: 10px;
    min-width: max-content;
    color: #475569;
    font-family: "JetBrains Mono", "SFMono-Regular", Consolas, monospace;
    font-size: 0.86rem;
  }

  .connection span {
    width: 10px;
    height: 10px;
    border-radius: 999px;
    background: #cbd5e1;
  }

  .connection span.live {
    background: #2563eb;
    box-shadow: 0 0 0 5px rgba(37, 99, 235, 0.14);
  }

  .notice {
    margin-top: 12px;
    padding: 12px 14px;
    border-radius: 8px;
  }

  .notice.error {
    border-color: #fecaca;
    color: #991b1b;
    background: #fff7f7;
  }

  .control-strip {
    display: grid;
    grid-template-columns: repeat(auto-fit, minmax(180px, 1fr));
    align-items: end;
    gap: 14px;
    padding: 18px;
    border-top: 0;
    border-radius: 0 0 8px 8px;
  }

  label {
    display: grid;
    gap: 7px;
  }

  label span,
  .auto-refresh > span {
    color: #64748b;
    font-family: "JetBrains Mono", "SFMono-Regular", Consolas, monospace;
    font-size: 0.72rem;
    font-weight: 700;
    text-transform: uppercase;
  }

  select,
  input {
    width: 100%;
    min-height: 44px;
    border: 1px solid #cbd5e1;
    border-radius: 6px;
    padding: 0 11px;
    color: #0f172a;
    background: #ffffff;
    outline: 2px solid transparent;
  }

  select:focus-visible,
  input:focus-visible,
  button:focus-visible,
  .spread-chart:focus-visible {
    border-color: #2563eb;
    box-shadow: 0 0 0 3px rgba(37, 99, 235, 0.18);
  }

  .preset-group {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    min-height: 44px;
    padding: 4px;
    border: 1px solid #cbd5e1;
    border-radius: 999px;
    background: #eff6ff;
  }

  button {
    min-height: 44px;
    border: 1px solid #cbd5e1;
    border-radius: 999px;
    padding: 0 13px;
    color: #0f172a;
    background: #ffffff;
    cursor: pointer;
    touch-action: manipulation;
    transition:
      border-color 180ms ease,
      background 180ms ease,
      color 180ms ease,
      opacity 180ms ease;
  }

  button:hover:not(:disabled) {
    border-color: #2563eb;
  }

  button:disabled {
    cursor: not-allowed;
    opacity: 0.55;
  }

  .preset-group button {
    min-height: 34px;
    border-color: transparent;
    background: transparent;
  }

  .preset-group button.active,
  .query-button {
    color: #ffffff;
    background: #0f172a;
  }

  .query-button,
  .swap-button,
  .ghost-button {
    min-height: 44px;
    border-radius: 6px;
    padding: 0 18px;
  }

  .ghost-button {
    min-width: 142px;
  }

  .time-input {
    min-width: 190px;
  }

  .auto-refresh {
    display: grid;
    grid-template-columns: auto minmax(82px, 1fr);
    align-items: end;
    gap: 7px 10px;
  }

  .auto-refresh > span {
    grid-column: 1 / -1;
  }

  .checkbox-line {
    display: inline-flex;
    min-height: 44px;
    align-items: center;
    gap: 8px;
    color: #334155;
  }

  .checkbox-line input {
    width: 18px;
    min-height: 18px;
    accent-color: #2563eb;
  }

  .metric-strip {
    display: grid;
    grid-template-columns: repeat(4, minmax(0, 1fr));
    gap: 12px;
    margin-top: 14px;
  }

  .metric {
    display: grid;
    min-height: 118px;
    align-content: space-between;
    gap: 8px;
    border-left: 4px solid #cbd5e1;
    border-radius: 8px;
    padding: 14px 16px;
  }

  .metric.positive {
    border-left-color: #0f766e;
  }

  .metric.negative {
    border-left-color: #f97316;
  }

  .metric-label {
    margin: 0;
    color: #64748b;
    font-family: "JetBrains Mono", "SFMono-Regular", Consolas, monospace;
    font-size: 0.72rem;
    font-weight: 700;
    text-transform: uppercase;
  }

  .metric strong {
    color: #0f172a;
    font-size: 1.08rem;
  }

  .metric span {
    color: #475569;
    font-size: 0.88rem;
  }

  .freshness {
    display: inline-flex;
    align-items: center;
    gap: 6px;
  }

  .freshness::before {
    width: 8px;
    height: 8px;
    border-radius: 999px;
    content: "";
  }

  .freshness.fresh::before {
    background: #0f766e;
  }

  .freshness.lagging::before {
    background: #f97316;
  }

  .freshness.stale::before {
    background: #dc2626;
  }

  .workspace {
    display: grid;
    grid-template-columns: minmax(0, 1fr) 390px;
    gap: 16px;
    margin-top: 16px;
    padding: 16px;
    border-radius: 8px;
    border: 1px solid #dbe4ef;
    background: rgba(239, 246, 255, 0.68);
  }

  .chart-panel,
  .readout,
  .points-panel,
  .coverage-panel,
  .rates-panel {
    border-radius: 8px;
  }

  .chart-panel {
    min-width: 0;
    overflow: hidden;
  }

  .chart-title {
    display: flex;
    align-items: start;
    justify-content: space-between;
    gap: 20px;
    padding: 20px 22px 8px;
  }

  .chart-subtitle,
  .chart-help {
    margin: 7px 0 0;
    color: #64748b;
    font-family: "JetBrains Mono", "SFMono-Regular", Consolas, monospace;
    font-size: 0.78rem;
  }

  .chart-help {
    margin: 0;
    padding: 0 22px 8px;
  }

  .legend {
    display: flex;
    flex-wrap: wrap;
    justify-content: end;
    gap: 12px;
    color: #475569;
    font-family: "JetBrains Mono", "SFMono-Regular", Consolas, monospace;
    font-size: 0.78rem;
  }

  .legend span,
  .series-toggle {
    display: inline-flex;
    align-items: center;
    gap: 7px;
  }

  .series-toggle {
    min-height: 34px;
    border-radius: 6px;
    padding: 0 9px;
    font-size: 0.78rem;
  }

  .series-toggle.inactive {
    color: #64748b;
    background: #f8fafc;
    opacity: 0.64;
  }

  .legend i {
    width: 24px;
    height: 3px;
    border-radius: 999px;
    display: inline-block;
  }

  .line-a {
    background: #2563eb;
  }

  .line-b {
    background: #f97316;
  }

  .zero {
    background: #94a3b8;
  }

  .empty-state {
    display: grid;
    min-height: 430px;
    place-items: center;
    padding: 32px;
    color: #64748b;
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
    fill: #f8fafc;
  }

  .grid-line {
    stroke: #dbe4ef;
    stroke-width: 1;
  }

  .grid-line.vertical {
    stroke-dasharray: 2 8;
  }

  .zero-line {
    stroke: #94a3b8;
    stroke-dasharray: 7 7;
    stroke-width: 1.5;
  }

  .spread-line {
    fill: none;
    stroke-linecap: round;
    stroke-linejoin: round;
    stroke-width: 2.4;
  }

  .spread-line.a {
    stroke: #2563eb;
  }

  .spread-line.b {
    stroke: #f97316;
  }

  .cursor-line {
    stroke: #0f172a;
    stroke-width: 1;
    stroke-dasharray: 4 5;
  }

  .point {
    stroke: #ffffff;
    stroke-width: 2.5;
  }

  .point.a {
    fill: #2563eb;
  }

  .point.b {
    fill: #f97316;
  }

  .axis-label {
    fill: #64748b;
    font-family: "JetBrains Mono", "SFMono-Regular", Consolas, monospace;
    font-size: 12px;
  }

  .axis-label.x.end {
    text-anchor: end;
  }

  .side-panel {
    display: grid;
    gap: 16px;
  }

  .readout,
  .points-panel,
  .coverage-panel,
  .rates-panel {
    padding: 18px;
  }

  .route-card {
    display: grid;
    gap: 4px;
    margin-top: 14px;
    border: 1px solid #dbe4ef;
    border-left: 4px solid #cbd5e1;
    border-radius: 6px;
    padding: 10px 12px;
    background: #f8fafc;
  }

  .route-card.positive {
    border-left-color: #0f766e;
  }

  .route-card.negative {
    border-left-color: #f97316;
  }

  .route-card span,
  .route-card small {
    color: #64748b;
  }

  .route-card strong {
    font-family: "JetBrains Mono", "SFMono-Regular", Consolas, monospace;
  }

  .readout dl {
    display: grid;
    gap: 10px;
    margin: 18px 0 0;
  }

  .readout dl div {
    display: flex;
    align-items: baseline;
    justify-content: space-between;
    gap: 12px;
    border-bottom: 1px solid #e2e8f0;
    padding-bottom: 9px;
  }

  dt {
    color: #64748b;
    font-size: 0.86rem;
  }

  dd {
    margin: 0;
    font-family: "JetBrains Mono", "SFMono-Regular", Consolas, monospace;
    font-variant-numeric: tabular-nums;
    font-weight: 700;
  }

  .book-snapshot {
    display: grid;
    gap: 8px;
    margin-top: 16px;
    color: #475569;
    font-family: "JetBrains Mono", "SFMono-Regular", Consolas, monospace;
    font-size: 0.78rem;
  }

  .point-actions {
    display: grid;
    grid-template-columns: repeat(3, minmax(0, 1fr));
    gap: 8px;
    margin-top: 16px;
  }

  .point-actions button {
    border-radius: 6px;
    padding: 0 8px;
  }

  .muted {
    margin: 10px 0 0;
    color: #64748b;
  }

  .panel-heading {
    display: flex;
    align-items: start;
    justify-content: space-between;
    gap: 12px;
    margin-bottom: 14px;
  }

  .panel-heading button {
    min-height: 36px;
  }

  .table-scroll {
    overflow-x: auto;
  }

  table {
    width: 100%;
    min-width: 520px;
    border-collapse: collapse;
    font-family: "JetBrains Mono", "SFMono-Regular", Consolas, monospace;
    font-size: 0.78rem;
  }

  th,
  td {
    border-bottom: 1px solid #e2e8f0;
    padding: 8px 7px;
    text-align: right;
    white-space: nowrap;
  }

  th:first-child,
  td:first-child {
    text-align: left;
  }

  th {
    color: #64748b;
    font-weight: 700;
  }

  tr.active {
    background: #eff6ff;
  }

  .row-select {
    min-height: 32px;
    border-radius: 6px;
    padding: 0 8px;
    font-family: inherit;
  }

  .coverage-list {
    display: grid;
    gap: 10px;
  }

  .coverage-row {
    display: grid;
    grid-template-columns: minmax(0, 1fr) auto auto;
    align-items: center;
    gap: 10px;
    border: 1px solid #e2e8f0;
    border-radius: 6px;
    padding: 10px;
    background: #ffffff;
  }

  .coverage-row.selected {
    border-color: #93c5fd;
    background: #eff6ff;
  }

  .coverage-main {
    display: grid;
    gap: 3px;
    min-width: 0;
  }

  .coverage-main strong,
  .coverage-main span {
    overflow-wrap: anywhere;
  }

  .coverage-main span,
  .coverage-meta {
    color: #64748b;
    font-size: 0.8rem;
  }

  .coverage-meta {
    display: grid;
    justify-items: end;
    gap: 4px;
    font-family: "JetBrains Mono", "SFMono-Regular", Consolas, monospace;
  }

  .leg-actions {
    display: inline-flex;
    gap: 4px;
  }

  .leg-actions button {
    min-width: 34px;
    min-height: 34px;
    border-radius: 6px;
    padding: 0;
  }

  .leg-actions button.active {
    color: #ffffff;
    border-color: #2563eb;
    background: #2563eb;
  }

  .rate-grid {
    display: grid;
    grid-template-columns: 1fr 1fr 1.1fr 34px;
    gap: 8px;
    margin-top: 8px;
  }

  .rate-grid.heading {
    margin-top: 0;
    color: #64748b;
    font-family: "JetBrains Mono", "SFMono-Regular", Consolas, monospace;
    font-size: 0.72rem;
    font-weight: 700;
    text-transform: uppercase;
  }

  .rate-grid input {
    min-height: 38px;
    padding: 0 8px;
  }

  .rate-grid button {
    min-width: 34px;
    min-height: 38px;
    border-radius: 6px;
    padding: 0;
  }

  .add-rate {
    width: 100%;
    margin-top: 12px;
    border-radius: 6px;
  }

  @media (max-width: 1180px) {
    .metric-strip {
      grid-template-columns: repeat(2, minmax(0, 1fr));
    }

    .workspace {
      grid-template-columns: 1fr;
    }

    .side-panel {
      grid-template-columns: repeat(2, minmax(0, 1fr));
    }
  }

  @media (max-width: 740px) {
    .page-shell {
      width: min(100vw - 20px, 1480px);
      padding-top: 10px;
    }

    .topbar,
    .chart-title,
    .control-strip {
      grid-template-columns: 1fr;
      align-items: stretch;
    }

    .topbar,
    .chart-title {
      display: grid;
    }

    .topbar-actions {
      justify-items: stretch;
    }

    .connection,
    .legend {
      justify-content: start;
    }

    .preset-group {
      overflow-x: auto;
      justify-content: start;
    }

    .metric-strip {
      grid-template-columns: 1fr;
    }

    .side-panel {
      grid-template-columns: 1fr;
    }

    .workspace {
      padding: 10px;
    }

    .coverage-row {
      grid-template-columns: 1fr;
      align-items: stretch;
    }

    .coverage-meta {
      justify-items: start;
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
