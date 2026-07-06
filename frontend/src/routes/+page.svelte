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

  let markets: Market[] = [];
  let selectedBase = '';
  let selectedA = '';
  let selectedB = '';
  let selectedPreset = '15m';
  let customStart = toDateInput(Date.now() - 60 * 60 * 1000);
  let customEnd = toDateInput(Date.now());
  let rates: QuoteRate[] = structuredClone(defaultRates);

  let marketError = '';
  let queryError = '';
  let loadingMarkets = false;
  let loadingSpread = false;
  let spread: SpreadResponse | null = null;
  let selectedIndex = -1;
  let hoverIndex = -1;

  $: currentMarket = markets.find((market) => market.baseAsset === selectedBase);
  $: currentInstruments = currentMarket?.instruments ?? [];
  $: selectedRange = currentRange(selectedPreset, customStart, customEnd);
  $: points = spread?.points ?? [];
  $: xBounds = computeXBounds(points, selectedRange);
  $: yBounds = computeYBounds(points);
  $: aPath = linePath(points, 'aToB', xBounds, yBounds);
  $: bPath = linePath(points, 'bToA', xBounds, yBounds);
  $: zeroY = yScale(0, yBounds);
  $: activeIndex = hoverIndex >= 0 ? hoverIndex : selectedIndex;
  $: activePoint = points[activeIndex] ?? null;

  onMount(() => {
    loadStoredRates();
    void loadMarkets();
  });

  async function loadMarkets() {
    loadingMarkets = true;
    marketError = '';
    try {
      const response = await fetch('/api/markets');
      const body = await response.json();
      if (!response.ok) throw new Error(body.error ?? 'Failed to load markets');
      markets = body.markets ?? [];
      if (markets.length > 0) {
        selectBase(markets[0].baseAsset);
        await loadSpread();
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

    const range = currentRange(selectedPreset, customStart, customEnd);
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
    } catch (error) {
      spread = null;
      selectedIndex = -1;
      queryError = error instanceof Error ? error.message : 'Failed to query spread';
    } finally {
      loadingSpread = false;
    }
  }

  function selectBase(baseAsset: string) {
    selectedBase = baseAsset;
    const instruments = markets.find((market) => market.baseAsset === baseAsset)?.instruments ?? [];
    selectedA = instruments[0]?.catalogId ?? '';
    selectedB = instruments[1]?.catalogId ?? instruments[0]?.catalogId ?? '';
    selectedIndex = -1;
    hoverIndex = -1;
  }

  function selectLegA(catalogId: string) {
    selectedA = catalogId;
    if (selectedA === selectedB) {
      selectedB = currentInstruments.find((instrument) => instrument.catalogId !== catalogId)?.catalogId ?? '';
    }
  }

  function selectLegB(catalogId: string) {
    selectedB = catalogId;
    if (selectedA === selectedB) {
      selectedA = currentInstruments.find((instrument) => instrument.catalogId !== catalogId)?.catalogId ?? '';
    }
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

  function currentRange(presetValue: string, start: string, end: string) {
    const preset = presets.find((item) => item.value === presetValue);
    if (preset && preset.value !== 'custom') {
      const toMs = Date.now();
      return { fromMs: toMs - preset.ms, toMs };
    }

    return {
      fromMs: new Date(start).getTime(),
      toMs: new Date(end).getTime()
    };
  }

  function handlePreset(value: string) {
    selectedPreset = value;
    if (value !== 'custom') {
      const range = currentRange(value, customStart, customEnd);
      customStart = toDateInput(range.fromMs);
      customEnd = toDateInput(range.toMs);
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

  function computeYBounds(data: SpreadPoint[]) {
    const values = data
      .flatMap((point) => [point.aToB, point.bToA])
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

  function formatNumber(value: number | null, digits = 6) {
    if (value === null || !Number.isFinite(value)) return '-';
    if (Math.abs(value) >= 100) return value.toFixed(2);
    if (Math.abs(value) >= 1) return value.toFixed(4);
    return value.toFixed(digits);
  }

  function formatBp(value: number | null) {
    if (value === null || !Number.isFinite(value)) return '-';
    return `${value.toFixed(2)} bp`;
  }

  function formatTime(ms: number) {
    return new Intl.DateTimeFormat(undefined, {
      month: '2-digit',
      day: '2-digit',
      hour: '2-digit',
      minute: '2-digit',
      second: '2-digit'
    }).format(new Date(ms));
  }

  function formatAxisTime(ms: number) {
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
  <title>Exchange Spread Log</title>
  <meta
    name="description"
    content="Inspect cross-venue BBO spread curves from ClickHouse."
  />
</svelte:head>

<main class="page-shell">
  <header class="topbar">
    <div>
      <p class="eyebrow">ClickHouse spread console</p>
      <h1>Cross-venue spread curves</h1>
    </div>
    <div class="connection">
      <span class:live={points.length > 0}></span>
      {#if loadingMarkets}
        Loading markets
      {:else if points.length > 0}
        {points.length} samples, {spread?.meta.bucketSeconds}s buckets
      {:else}
        Waiting for query
      {/if}
    </div>
  </header>

  {#if marketError}
    <section class="notice error">{marketError}</section>
  {/if}

  <section class="control-strip" aria-label="Spread query controls">
    <label>
      <span>Market</span>
      <select
        value={selectedBase}
        disabled={loadingMarkets || markets.length === 0}
        on:change={(event) => selectBase(selectValue(event))}
      >
        {#each markets as market}
          <option value={market.baseAsset}>{market.baseAsset}</option>
        {/each}
      </select>
    </label>

    <label>
      <span>Leg A</span>
      <select
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
        value={selectedB}
        disabled={currentInstruments.length < 2}
        on:change={(event) => selectLegB(selectValue(event))}
      >
        {#each currentInstruments as instrument}
          <option value={instrument.catalogId}>{instrument.label}</option>
        {/each}
      </select>
    </label>

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
        <input type="datetime-local" bind:value={customStart} />
      </label>
      <label class="time-input">
        <span>To</span>
        <input type="datetime-local" bind:value={customEnd} />
      </label>
    {/if}

    <button
      class="query-button"
      type="button"
      disabled={loadingSpread || loadingMarkets || currentInstruments.length < 2}
      on:click={loadSpread}
    >
      {loadingSpread ? 'Querying...' : 'Run query'}
    </button>
  </section>

  {#if queryError}
    <section class="notice error">{queryError}</section>
  {/if}

  <section class="workspace">
    <section class="chart-panel">
      <div class="chart-title">
        <div>
          <p class="eyebrow">{selectedBase || 'No market selected'}</p>
          <h2>{selectedLabel(selectedA)} vs {selectedLabel(selectedB)}</h2>
        </div>
        <div class="legend">
          <span><i class="line-a"></i>A bid - B ask</span>
          <span><i class="line-b"></i>B bid - A ask</span>
          <span><i class="zero"></i>zero</span>
        </div>
      </div>

      {#if loadingSpread}
        <div class="empty-state">Querying ClickHouse...</div>
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
          <path class="spread-line a" d={aPath} />
          <path class="spread-line b" d={bPath} />

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
            {#if activePoint.aToB !== null}
              <circle
                class="point a"
                cx={xScale(activePoint.tsMs, xBounds)}
                cy={yScale(activePoint.aToB, yBounds)}
                r="5"
              />
            {/if}
            {#if activePoint.bToA !== null}
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
        {:else}
          <p class="muted">Hover or click the curve after a query to inspect exact values.</p>
        {/if}
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
              value={rate.from}
              placeholder="USDC"
              on:input={(event) => updateRate(index, 'from', inputValue(event))}
            />
            <input
              aria-label="Quote to"
              value={rate.to}
              placeholder="USD"
              on:input={(event) => updateRate(index, 'to', inputValue(event))}
            />
            <input
              aria-label="Quote rate"
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
    color: #151917;
    background:
      linear-gradient(90deg, rgba(28, 99, 72, 0.055) 1px, transparent 1px),
      linear-gradient(180deg, rgba(28, 99, 72, 0.045) 1px, transparent 1px),
      #f5f7f2;
    background-size: 28px 28px;
    font-family:
      Inter, ui-sans-serif, system-ui, -apple-system, BlinkMacSystemFont, "Segoe UI", sans-serif;
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

  .topbar,
  .control-strip,
  .workspace,
  .chart-panel,
  .side-panel,
  .readout,
  .rates-panel,
  .notice {
    border: 1px solid #cfd8d0;
    background: rgba(252, 253, 248, 0.92);
    box-shadow: 0 18px 50px rgba(35, 48, 40, 0.08);
  }

  .topbar {
    display: flex;
    align-items: end;
    justify-content: space-between;
    gap: 20px;
    padding: 22px 24px;
    border-radius: 8px 8px 0 0;
  }

  .eyebrow {
    margin: 0 0 7px;
    color: #617168;
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
    font-size: clamp(2rem, 4vw, 4.6rem);
    line-height: 0.95;
  }

  h2 {
    font-size: clamp(1.2rem, 2vw, 2rem);
  }

  h3 {
    font-size: 1.05rem;
  }

  .connection {
    display: inline-flex;
    align-items: center;
    gap: 10px;
    min-width: max-content;
    color: #3d4a43;
    font-family: "JetBrains Mono", "SFMono-Regular", Consolas, monospace;
    font-size: 0.86rem;
  }

  .connection span {
    width: 10px;
    height: 10px;
    border-radius: 999px;
    background: #b9c4bc;
  }

  .connection span.live {
    background: #23815e;
    box-shadow: 0 0 0 5px rgba(35, 129, 94, 0.14);
  }

  .notice {
    margin-top: 12px;
    padding: 12px 14px;
    border-radius: 8px;
  }

  .notice.error {
    border-color: #e1b8b8;
    color: #8e2c32;
    background: #fff7f5;
  }

  .control-strip {
    display: grid;
    grid-template-columns: minmax(120px, 0.7fr) minmax(220px, 1.1fr) minmax(220px, 1.1fr) auto auto;
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

  label span {
    color: #617168;
    font-family: "JetBrains Mono", "SFMono-Regular", Consolas, monospace;
    font-size: 0.72rem;
    font-weight: 700;
    text-transform: uppercase;
  }

  select,
  input {
    width: 100%;
    min-height: 42px;
    border: 1px solid #bfcbbf;
    border-radius: 6px;
    padding: 0 11px;
    color: #151917;
    background: #ffffff;
    outline: none;
  }

  select:focus,
  input:focus,
  button:focus-visible {
    border-color: #23815e;
    box-shadow: 0 0 0 3px rgba(35, 129, 94, 0.17);
  }

  .preset-group {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    min-height: 42px;
    padding: 4px;
    border: 1px solid #cbd5cb;
    border-radius: 999px;
    background: #edf2ed;
  }

  button {
    min-height: 36px;
    border: 1px solid #c3cec4;
    border-radius: 999px;
    padding: 0 13px;
    color: #17221d;
    background: #ffffff;
    cursor: pointer;
  }

  button:hover:not(:disabled) {
    border-color: #23815e;
  }

  button:disabled {
    cursor: not-allowed;
    opacity: 0.55;
  }

  .preset-group button {
    min-height: 32px;
    border-color: transparent;
    background: transparent;
  }

  .preset-group button.active,
  .query-button {
    color: #ffffff;
    background: #151917;
  }

  .query-button {
    min-height: 42px;
    border-radius: 6px;
    padding: 0 18px;
  }

  .time-input {
    min-width: 190px;
  }

  .workspace {
    display: grid;
    grid-template-columns: minmax(0, 1fr) 360px;
    gap: 16px;
    margin-top: 16px;
    padding: 16px;
    border-radius: 8px;
    background: rgba(235, 241, 235, 0.72);
  }

  .chart-panel,
  .readout,
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
    padding: 20px 22px 12px;
  }

  .legend {
    display: flex;
    flex-wrap: wrap;
    justify-content: end;
    gap: 12px;
    color: #4e5e55;
    font-family: "JetBrains Mono", "SFMono-Regular", Consolas, monospace;
    font-size: 0.78rem;
  }

  .legend span {
    display: inline-flex;
    align-items: center;
    gap: 7px;
  }

  .legend i {
    width: 24px;
    height: 3px;
    border-radius: 999px;
    display: inline-block;
  }

  .line-a {
    background: #23815e;
  }

  .line-b {
    background: #b33572;
  }

  .zero {
    background: #9aa59d;
  }

  .empty-state {
    display: grid;
    min-height: 430px;
    place-items: center;
    padding: 32px;
    color: #617168;
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
    fill: #f8faf6;
  }

  .grid-line {
    stroke: #dce5dd;
    stroke-width: 1;
  }

  .grid-line.vertical {
    stroke-dasharray: 2 8;
  }

  .zero-line {
    stroke: #6c7770;
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
    stroke: #23815e;
  }

  .spread-line.b {
    stroke: #b33572;
  }

  .cursor-line {
    stroke: #171d19;
    stroke-width: 1;
    stroke-dasharray: 4 5;
  }

  .point {
    stroke: #ffffff;
    stroke-width: 2.5;
  }

  .point.a {
    fill: #23815e;
  }

  .point.b {
    fill: #b33572;
  }

  .axis-label {
    fill: #617168;
    font-family: "JetBrains Mono", "SFMono-Regular", Consolas, monospace;
    font-size: 12px;
  }

  .axis-label.x.end {
    text-anchor: end;
  }

  .side-panel {
    display: grid;
    gap: 16px;
    border: 0;
    background: transparent;
    box-shadow: none;
  }

  .readout,
  .rates-panel {
    padding: 18px;
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
    border-bottom: 1px solid #e3e9e3;
    padding-bottom: 9px;
  }

  dt {
    color: #617168;
    font-size: 0.86rem;
  }

  dd {
    margin: 0;
    font-family: "JetBrains Mono", "SFMono-Regular", Consolas, monospace;
    font-weight: 700;
  }

  .book-snapshot {
    display: grid;
    gap: 8px;
    margin-top: 16px;
    color: #4d5b53;
    font-family: "JetBrains Mono", "SFMono-Regular", Consolas, monospace;
    font-size: 0.78rem;
  }

  .muted {
    margin: 10px 0 0;
    color: #617168;
  }

  .panel-heading {
    display: flex;
    align-items: start;
    justify-content: space-between;
    gap: 12px;
    margin-bottom: 14px;
  }

  .panel-heading button {
    min-height: 32px;
  }

  .rate-grid {
    display: grid;
    grid-template-columns: 1fr 1fr 1.1fr 34px;
    gap: 8px;
    margin-top: 8px;
  }

  .rate-grid.heading {
    margin-top: 0;
    color: #617168;
    font-family: "JetBrains Mono", "SFMono-Regular", Consolas, monospace;
    font-size: 0.72rem;
    font-weight: 700;
    text-transform: uppercase;
  }

  .rate-grid input {
    min-height: 36px;
    padding: 0 8px;
  }

  .rate-grid button {
    min-width: 34px;
    min-height: 36px;
    border-radius: 6px;
    padding: 0;
  }

  .add-rate {
    width: 100%;
    margin-top: 12px;
    border-radius: 6px;
  }

  @media (max-width: 1180px) {
    .control-strip {
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

    .connection,
    .legend {
      justify-content: start;
    }

    .preset-group {
      overflow-x: auto;
      justify-content: start;
    }

    .side-panel {
      grid-template-columns: 1fr;
    }

    .workspace {
      padding: 10px;
    }
  }
</style>
