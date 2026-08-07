<script lang="ts">
  import { onMount } from 'svelte';
  import {
    BaselineSeries,
    ColorType,
    CrosshairMode,
    LineSeries,
    LineStyle,
    TickMarkType,
    createChart,
    createSeriesMarkers,
    type IChartApi,
    type ISeriesApi,
    type ISeriesMarkersPluginApi,
    type Time,
    type UTCTimestamp
  } from 'lightweight-charts';
  import {
    directionBp,
    directionLabel,
    grossCaptureBp,
    oppositeDirection,
    type SpreadDirection
  } from './spread-direction';
  import TradeDirection from './TradeDirection.svelte';
  import type { SpreadPoint } from './types';

  export let points: SpreadPoint[] = [];
  export let openDirection: SpreadDirection = 'bToA';
  export let legAVenue = '';
  export let legBVenue = '';

  type CaptureStats = {
    bestBp: number | null;
    bestTsMs: number | null;
    breakEvenTsMs: number | null;
  };

  let container: HTMLDivElement;
  let chart: IChartApi | null = null;
  let openSeries: ISeriesApi<'Line'> | null = null;
  let closeSeries: ISeriesApi<'Line'> | null = null;
  let captureSeries: ISeriesApi<'Baseline'> | null = null;
  let entryMarkers: ISeriesMarkersPluginApi<Time> | null = null;
  let rendered: SpreadPoint[] | null = null;
  let renderedDirection: SpreadDirection | null = null;
  let pointsBySecond = new Map<number, SpreadPoint>();
  let latestPoint: SpreadPoint | null = null;
  let activePoint: SpreadPoint | null = null;
  let crosshairSecond: number | null = null;
  let entrySecond: number | null = null;
  let entryPoint: SpreadPoint | null = null;
  let captureStats: CaptureStats = {
    bestBp: null,
    bestTsMs: null,
    breakEvenTsMs: null
  };
  let localZone = '浏览器本地时区';
  let locale = 'zh-CN';

  $: closeDirection = oppositeDirection(openDirection);
  $: openDirectionLabel = directionLabel(openDirection);
  $: closeDirectionLabel = directionLabel(closeDirection);
  $: openActionLabel = actionLabel(openDirection);
  $: closeActionLabel = actionLabel(closeDirection);
  $: displayedOpenPoint = entryPoint ?? activePoint;
  $: activeCaptureBp =
    entryPoint && activePoint
      ? grossCaptureBp(entryPoint, activePoint, openDirection)
      : null;
  $: activeHoldingMs =
    entryPoint && activePoint && activePoint.tsMs >= entryPoint.tsMs
      ? activePoint.tsMs - entryPoint.tsMs
      : null;

  onMount(() => {
    locale = navigator.language || 'zh-CN';
    localZone = describeLocalZone();
    chart = createChart(container, {
      autoSize: true,
      layout: {
        background: { type: ColorType.Solid, color: '#f8fafb' },
        textColor: '#526579',
        fontFamily: '"IBM Plex Mono", "SFMono-Regular", Consolas, monospace',
        fontSize: 11,
        attributionLogo: false
      },
      grid: {
        vertLines: { color: '#e4eaee', style: LineStyle.Dotted },
        horzLines: { color: '#dce4e9', style: LineStyle.Dotted }
      },
      crosshair: { mode: CrosshairMode.Normal },
      rightPriceScale: { borderColor: '#b9c6cf' },
      timeScale: {
        borderColor: '#b9c6cf',
        timeVisible: true,
        secondsVisible: false,
        rightOffset: 4,
        tickMarkFormatter: formatLocalTick
      },
      localization: {
        priceFormatter: (value: number) => `${value.toFixed(2)} bp`,
        timeFormatter: formatLocalCrosshairTime
      }
    });

    openSeries = chart.addSeries(LineSeries, {
      color: '#c97842',
      lineWidth: 2,
      title: `开仓 ${openDirectionLabel}`,
      priceLineVisible: false,
      lastValueVisible: true
    });
    closeSeries = chart.addSeries(LineSeries, {
      color: '#2e6f95',
      lineWidth: 2,
      title: `平仓 ${closeDirectionLabel}`,
      priceLineVisible: false,
      lastValueVisible: true
    });
    captureSeries = chart.addSeries(BaselineSeries, {
      baseValue: { type: 'price', price: 0 },
      relativeGradient: true,
      topLineColor: '#287760',
      topFillColor1: 'rgba(40, 119, 96, 0.14)',
      topFillColor2: 'rgba(40, 119, 96, 0.015)',
      bottomLineColor: '#a14942',
      bottomFillColor1: 'rgba(161, 73, 66, 0.015)',
      bottomFillColor2: 'rgba(161, 73, 66, 0.12)',
      lineWidth: 3,
      title: '可平仓毛收益',
      priceLineVisible: false,
      lastValueVisible: true
    });
    captureSeries.createPriceLine({
      price: 0,
      color: '#8fa0ad',
      lineWidth: 1,
      lineStyle: LineStyle.Dashed,
      axisLabelVisible: true,
      title: '回本线'
    });
    entryMarkers = createSeriesMarkers(openSeries, [], { autoScale: true });
    chart.subscribeCrosshairMove((parameter) => {
      crosshairSecond = timestampSeconds(parameter.time);
      activePoint =
        crosshairSecond === null
          ? latestPoint
          : pointsBySecond.get(crosshairSecond) ?? latestPoint;
    });
    chart.subscribeClick((parameter) => {
      const second = timestampSeconds(parameter.time);
      if (second !== null) selectEntry(second);
    });
    render();
    chart.timeScale().fitContent();
    return () => {
      chart?.remove();
      chart = null;
    };
  });

  $: if (chart && (points !== rendered || openDirection !== renderedDirection)) render();

  function render() {
    const directionChanged =
      renderedDirection !== null && renderedDirection !== openDirection;
    rendered = points;
    renderedDirection = openDirection;
    if (directionChanged) clearEntry();

    openSeries?.applyOptions({ title: `开仓 ${openDirectionLabel}` });
    closeSeries?.applyOptions({ title: `平仓 ${closeDirectionLabel}` });
    const unique = new Map<number, SpreadPoint>();
    for (const point of points) {
      if (
        Number.isFinite(point.tsMs) &&
        Number.isFinite(point.aToBBp) &&
        Number.isFinite(point.bToABp)
      ) {
        unique.set(Math.floor(point.tsMs / 1000), point);
      }
    }
    const sorted = [...unique.entries()].sort(([left], [right]) => left - right);
    pointsBySecond = new Map(sorted);
    latestPoint = sorted.length > 0 ? pointsBySecond.get(sorted.at(-1)![0]) ?? null : null;
    activePoint =
      crosshairSecond === null
        ? latestPoint
        : pointsBySecond.get(crosshairSecond) ?? latestPoint;

    if (entrySecond !== null) {
      entryPoint = pointsBySecond.get(entrySecond) ?? null;
      if (!entryPoint) entrySecond = null;
    }

    openSeries?.setData(
      sorted.map(([time, point]) => ({
        time: time as UTCTimestamp,
        value: directionBp(point, openDirection)
      }))
    );
    closeSeries?.setData(
      sorted.map(([time, point]) => ({
        time: time as UTCTimestamp,
        value: directionBp(point, closeDirection)
      }))
    );
    renderCaptureSeries(sorted);
    syncEntryMarker();
  }

  function renderCaptureSeries(sorted: Array<[number, SpreadPoint]>) {
    if (!entryPoint || entrySecond === null) {
      captureSeries?.setData([]);
      captureStats = {
        bestBp: null,
        bestTsMs: null,
        breakEvenTsMs: null
      };
      return;
    }

    let bestBp: number | null = null;
    let bestTsMs: number | null = null;
    let breakEvenTsMs: number | null = null;
    const captureData: Array<{ time: UTCTimestamp; value: number }> = [];

    for (const [time, point] of sorted) {
      if (time < entrySecond) continue;
      const value = grossCaptureBp(entryPoint, point, openDirection);
      if (value === null) continue;

      captureData.push({ time: time as UTCTimestamp, value });
      if (bestBp === null || value > bestBp) {
        bestBp = value;
        bestTsMs = point.tsMs;
      }
      if (breakEvenTsMs === null && value >= 0) {
        breakEvenTsMs = point.tsMs;
      }
    }

    captureSeries?.setData(captureData);
    captureStats = { bestBp, bestTsMs, breakEvenTsMs };
  }

  function selectEntry(second: number) {
    const point = pointsBySecond.get(second);
    if (!point) return;

    entrySecond = second;
    entryPoint = point;
    activePoint = point;
    crosshairSecond = second;
    renderCaptureSeries([...pointsBySecond.entries()].sort(([left], [right]) => left - right));
    syncEntryMarker();
  }

  function clearEntry() {
    entrySecond = null;
    entryPoint = null;
    captureSeries?.setData([]);
    captureStats = {
      bestBp: null,
      bestTsMs: null,
      breakEvenTsMs: null
    };
    entryMarkers?.setMarkers([]);
  }

  function syncEntryMarker() {
    if (!entryPoint || entrySecond === null) {
      entryMarkers?.setMarkers([]);
      return;
    }

    entryMarkers?.setMarkers([
      {
        time: entrySecond as UTCTimestamp,
        position: 'aboveBar',
        shape: 'arrowDown',
        color: '#9f572c',
        text: '开仓',
        size: 1.1
      }
    ]);
  }

  function timestampSeconds(time: Time | undefined): number | null {
    if (time === undefined) return null;
    if (typeof time === 'number') return Math.floor(time);
    const date =
      typeof time === 'string'
        ? new Date(time)
        : new Date(Date.UTC(time.year, time.month - 1, time.day));
    const seconds = date.getTime() / 1_000;
    return Number.isFinite(seconds) ? Math.floor(seconds) : null;
  }

  function dateFromTime(time: Time): Date | null {
    const seconds = timestampSeconds(time);
    return seconds === null ? null : new Date(seconds * 1_000);
  }

  function formatLocalTick(time: Time, tickType: TickMarkType) {
    const date = dateFromTime(time);
    if (!date) return null;

    if (tickType === TickMarkType.Year) {
      return new Intl.DateTimeFormat(locale, { year: 'numeric' }).format(date);
    }
    if (tickType === TickMarkType.Month) {
      return new Intl.DateTimeFormat(locale, { month: 'short' }).format(date);
    }
    if (tickType === TickMarkType.DayOfMonth) {
      return new Intl.DateTimeFormat(locale, { month: 'numeric', day: 'numeric' }).format(date);
    }
    return new Intl.DateTimeFormat(locale, {
      hour: '2-digit',
      minute: '2-digit',
      second: tickType === TickMarkType.TimeWithSeconds ? '2-digit' : undefined,
      hourCycle: 'h23'
    }).format(date);
  }

  function formatLocalCrosshairTime(time: Time) {
    const date = dateFromTime(time);
    return date ? formatLocalDateTime(date.getTime()) : '—';
  }

  function formatLocalDateTime(timestampMs: number) {
    return new Intl.DateTimeFormat(locale, {
      year: 'numeric',
      month: '2-digit',
      day: '2-digit',
      hour: '2-digit',
      minute: '2-digit',
      second: '2-digit',
      hourCycle: 'h23'
    }).format(new Date(timestampMs));
  }

  function describeLocalZone() {
    return Intl.DateTimeFormat().resolvedOptions().timeZone || '本地时区';
  }

  function describeUtcOffset(timestampMs: number) {
    const offsetMinutes = -new Date(timestampMs).getTimezoneOffset();
    const sign = offsetMinutes >= 0 ? '+' : '-';
    const hours = String(Math.floor(Math.abs(offsetMinutes) / 60)).padStart(2, '0');
    const minutes = String(Math.abs(offsetMinutes) % 60).padStart(2, '0');
    return `UTC${sign}${hours}:${minutes}`;
  }

  function actionLabel(direction: SpreadDirection) {
    const a = legAVenue ? ` ${legAVenue}` : '';
    const b = legBVenue ? ` ${legBVenue}` : '';
    return direction === 'aToB'
      ? `BUY B${b} · SELL A${a}`
      : `BUY A${a} · SELL B${b}`;
  }

  function formatBp(value: number | undefined) {
    if (value === undefined || !Number.isFinite(value)) return '—';
    return `${value > 0 ? '+' : ''}${value.toFixed(2)} bp`;
  }

  function formatDuration(value: number | null) {
    if (value === null || value < 0 || !Number.isFinite(value)) return '—';
    if (value === 0) return '0 分钟';
    if (value < 60_000) return '< 1 分钟';
    const totalMinutes = Math.floor(value / 60_000);
    if (totalMinutes < 60) return `${totalMinutes} 分钟`;
    const hours = Math.floor(totalMinutes / 60);
    const minutes = totalMinutes % 60;
    if (hours < 24) return minutes > 0 ? `${hours} 小时 ${minutes} 分钟` : `${hours} 小时`;
    const days = Math.floor(hours / 24);
    const remainingHours = hours % 24;
    return remainingHours > 0 ? `${days} 天 ${remainingHours} 小时` : `${days} 天`;
  }

  function observationNote(
    entry: SpreadPoint | null,
    active: SpreadPoint | null,
    holdingMs: number | null
  ) {
    if (!entry) return '点击图表任一时刻，锁定橙线开仓报价';
    if (!active || active.tsMs < entry.tsMs) return '开仓前报价，不计入收益';
    return `已持仓 ${formatDuration(holdingMs)}`;
  }

  function captureSummary(entry: SpreadPoint | null, stats: CaptureStats) {
    if (!entry) return '选择开仓点后计算';
    const bestHoldingMs =
      stats.bestTsMs === null ? null : stats.bestTsMs - entry.tsMs;
    const breakEvenHoldingMs =
      stats.breakEvenTsMs === null
        ? null
        : stats.breakEvenTsMs - entry.tsMs;
    const best = `最佳出现在 ${formatDuration(bestHoldingMs)}`;
    const breakEven =
      breakEvenHoldingMs === null
        ? '区间内尚未回本'
        : `首次回本 ${formatDuration(breakEvenHoldingMs)}`;
    return `${best} · ${breakEven}`;
  }
</script>

<div class:anchored={entryPoint !== null} class="readout">
  <div class="readout-time">
    <div>
      <span>
        观察时间 · {localZone}
        {activePoint ? ` · ${describeUtcOffset(activePoint.tsMs)}` : ''}
      </span>
      <strong>{activePoint ? formatLocalDateTime(activePoint.tsMs) : '—'}</strong>
      <small>{observationNote(entryPoint, activePoint, activeHoldingMs)}</small>
    </div>
    {#if entryPoint}
      <button on:click={clearEntry}>清除开仓点</button>
    {/if}
  </div>
  <dl>
    <div class="open">
      <dt>
        <i></i>{entryPoint ? '固定开仓' : '候选开仓'} ·
        <TradeDirection
          direction={openDirection}
          venueA={legAVenue}
          venueB={legBVenue}
          showVenues
        />
      </dt>
      <dd>
        {formatBp(displayedOpenPoint ? directionBp(displayedOpenPoint, openDirection) : undefined)}
      </dd>
      <small>
        {entryPoint ? formatLocalDateTime(entryPoint.tsMs) : '卖出腿 bid − 买入腿 ask'}
      </small>
    </div>
    <div class="close">
      <dt>
        <i></i>观察点平仓 ·
        <TradeDirection
          direction={closeDirection}
          venueA={legAVenue}
          venueB={legBVenue}
          showVenues
        />
      </dt>
      <dd>{formatBp(activePoint ? directionBp(activePoint, closeDirection) : undefined)}</dd>
      <small>原买入腿 bid − 原卖出腿 ask</small>
    </div>
    <div
      class:profitable={activeCaptureBp !== null && activeCaptureBp >= 0}
      class="capture"
    >
      <dt><i></i>可平仓毛收益</dt>
      <dd>{entryPoint ? formatBp(activeCaptureBp ?? undefined) : '等待选择'}</dd>
      <small>未扣手续费、资金费和滑点</small>
    </div>
    <div class="best">
      <dt><i></i>区间最佳毛收益</dt>
      <dd>{formatBp(captureStats.bestBp ?? undefined)}</dd>
      <small>{captureSummary(entryPoint, captureStats)}</small>
    </div>
  </dl>
</div>

<div
  class="chart"
  bind:this={container}
  role="img"
  aria-label={`开仓 ${openActionLabel}、平仓 ${closeActionLabel} 与跨时点可平仓毛收益时间序列；点击图表可选择开仓时刻，单位为基点，时间按浏览器本地时区显示`}
></div>

<style>
  .readout {
    min-height: 76px;
    display: grid;
    grid-template-columns: minmax(245px, 1.2fr) minmax(0, 2.8fr);
    align-items: stretch;
    margin: 0 12px;
    border: 1px solid #d1dbe1;
    background: #f1f5f7;
  }

  .readout.anchored {
    border-color: #b9c8d1;
    box-shadow: inset 3px 0 #c97842;
  }

  .readout-time {
    min-width: 0;
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 10px;
    padding: 9px 12px;
    border-right: 1px solid #d1dbe1;
  }

  .readout-time > div {
    min-width: 0;
    display: grid;
    gap: 4px;
  }

  .readout-time span,
  dt,
  dl small,
  .readout-time small {
    color: #718392;
    font-size: 9px;
    letter-spacing: 0.02em;
  }

  .readout-time strong,
  dd {
    font-family: "IBM Plex Mono", "SFMono-Regular", Consolas, monospace;
  }

  .readout-time strong {
    overflow: hidden;
    color: #243a4e;
    font-size: 11px;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .readout-time small {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .readout-time button {
    flex: 0 0 auto;
    padding: 6px 8px;
    color: #536878;
    border: 1px solid #aebdc8;
    border-radius: 2px;
    background: #f8fafb;
    cursor: pointer;
    font-size: 9px;
  }

  .readout-time button:hover {
    color: #823e28;
    border-color: #c18a6a;
  }

  dl {
    min-width: 0;
    display: grid;
    grid-template-columns: repeat(4, minmax(0, 1fr));
    margin: 0;
  }

  dl > div {
    min-width: 0;
    display: grid;
    align-content: center;
    gap: 3px;
    padding: 8px 11px;
    border-right: 1px solid #d1dbe1;
  }

  dl > div:last-child {
    border-right: 0;
  }

  dt {
    display: flex;
    align-items: center;
    gap: 6px;
  }

  dt i {
    width: 13px;
    height: 2px;
    flex: 0 0 auto;
    background: #c97842;
  }

  .close dt i {
    background: #2e6f95;
  }

  .capture dt i {
    height: 3px;
    background: #a14942;
  }

  .capture.profitable dt i {
    background: #287760;
  }

  .best dt i {
    height: 3px;
    background: linear-gradient(90deg, #a14942 0 48%, #287760 52% 100%);
  }

  dd {
    overflow: hidden;
    margin: 0;
    color: #263c50;
    font-size: 12px;
    font-weight: 700;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  dl small {
    overflow: hidden;
    min-height: 12px;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .best small {
    line-height: 1.25;
    white-space: normal;
  }

  .capture dd {
    color: #a14942;
  }

  .capture.profitable dd {
    color: #287760;
  }

  .chart {
    width: 100%;
    height: 400px;
  }

  @media (max-width: 760px) {
    .readout {
      grid-template-columns: 1fr;
      margin: 0 8px;
    }

    .readout-time {
      border-right: 0;
      border-bottom: 1px solid #d1dbe1;
    }

    dl {
      grid-template-columns: 1fr 1fr;
    }

    dl > div:nth-child(2) {
      border-right: 0;
    }

    dl > div:nth-child(-n + 2) {
      border-bottom: 1px solid #d1dbe1;
    }

    .best {
      border-right: 0;
    }

    .chart {
      height: 320px;
    }
  }
</style>
