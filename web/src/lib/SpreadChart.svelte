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
    type IChartApi,
    type ISeriesApi,
    type Time,
    type UTCTimestamp
  } from 'lightweight-charts';
  import type { SpreadPoint } from './types';

  export let points: SpreadPoint[] = [];

  type DisplayPoint = {
    tsMs: number;
    openBp: number;
    closeBp: number;
    netBp: number;
  };

  let container: HTMLDivElement;
  let chart: IChartApi | null = null;
  let openSeries: ISeriesApi<'Line'> | null = null;
  let closeSeries: ISeriesApi<'Line'> | null = null;
  let netSeries: ISeriesApi<'Baseline'> | null = null;
  let rendered: SpreadPoint[] | null = null;
  let pointsBySecond = new Map<number, DisplayPoint>();
  let latestPoint: DisplayPoint | null = null;
  let activePoint: DisplayPoint | null = null;
  let crosshairSecond: number | null = null;
  let localZone = '浏览器本地时区';
  let locale = 'zh-CN';

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
      title: '开仓 B → A',
      priceLineVisible: false,
      lastValueVisible: true
    });
    closeSeries = chart.addSeries(LineSeries, {
      color: '#2e6f95',
      lineWidth: 2,
      title: '平仓 A → B',
      priceLineVisible: false,
      lastValueVisible: true
    });
    netSeries = chart.addSeries(BaselineSeries, {
      baseValue: { type: 'price', price: 0 },
      relativeGradient: true,
      topLineColor: '#287760',
      topFillColor1: 'rgba(40, 119, 96, 0.14)',
      topFillColor2: 'rgba(40, 119, 96, 0.015)',
      bottomLineColor: '#a14942',
      bottomFillColor1: 'rgba(161, 73, 66, 0.015)',
      bottomFillColor2: 'rgba(161, 73, 66, 0.12)',
      lineWidth: 3,
      title: '总净收益',
      priceLineVisible: false,
      lastValueVisible: true
    });
    netSeries.createPriceLine({
      price: 0,
      color: '#8fa0ad',
      lineWidth: 1,
      lineStyle: LineStyle.Dashed,
      axisLabelVisible: true,
      title: '零线'
    });
    chart.subscribeCrosshairMove((parameter) => {
      crosshairSecond = timestampSeconds(parameter.time);
      activePoint =
        crosshairSecond === null
          ? latestPoint
          : pointsBySecond.get(crosshairSecond) ?? latestPoint;
    });
    render();
    chart.timeScale().fitContent();
    return () => {
      chart?.remove();
      chart = null;
    };
  });

  $: if (chart && points !== rendered) render();

  function render() {
    rendered = points;
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
    pointsBySecond = new Map(
      sorted.map(([time, point]) => [
        time,
        {
          tsMs: time * 1_000,
          openBp: point.bToABp,
          closeBp: point.aToBBp,
          netBp: point.bToABp + point.aToBBp
        }
      ])
    );
    latestPoint = sorted.length > 0 ? pointsBySecond.get(sorted.at(-1)![0]) ?? null : null;
    activePoint =
      crosshairSecond === null
        ? latestPoint
        : pointsBySecond.get(crosshairSecond) ?? latestPoint;

    openSeries?.setData(
      sorted.map(([time, point]) => ({
        time: time as UTCTimestamp,
        value: point.bToABp
      }))
    );
    closeSeries?.setData(
      sorted.map(([time, point]) => ({
        time: time as UTCTimestamp,
        value: point.aToBBp
      }))
    );
    netSeries?.setData(
      sorted.map(([time, point]) => ({
        time: time as UTCTimestamp,
        value: point.bToABp + point.aToBBp
      }))
    );
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

  function formatBp(value: number | undefined) {
    if (value === undefined || !Number.isFinite(value)) return '—';
    return `${value > 0 ? '+' : ''}${value.toFixed(2)} bp`;
  }
</script>

<div class="readout">
  <div class="readout-time">
    <span>
      本地时间 · {localZone}
      {activePoint ? ` · ${describeUtcOffset(activePoint.tsMs)}` : ''}
    </span>
    <strong>{activePoint ? formatLocalDateTime(activePoint.tsMs) : '—'}</strong>
  </div>
  <dl>
    <div class="open">
      <dt><i></i>开仓 · B → A</dt>
      <dd>{formatBp(activePoint?.openBp)}</dd>
    </div>
    <div class="close">
      <dt><i></i>平仓 · A → B</dt>
      <dd>{formatBp(activePoint?.closeBp)}</dd>
    </div>
    <div class:profitable={activePoint !== null && activePoint.netBp >= 0} class="net">
      <dt><i></i>总净收益</dt>
      <dd>{formatBp(activePoint?.netBp)}</dd>
    </div>
  </dl>
</div>

<div
  class="chart"
  bind:this={container}
  role="img"
  aria-label="开仓价差、平仓价差与总净收益时间序列，单位为基点，时间按浏览器本地时区显示"
></div>

<style>
  .readout {
    min-height: 58px;
    display: grid;
    grid-template-columns: minmax(210px, 1.25fr) minmax(420px, 2fr);
    align-items: stretch;
    margin: 0 12px;
    border: 1px solid #d1dbe1;
    background: #f1f5f7;
  }

  .readout-time {
    min-width: 0;
    display: grid;
    align-content: center;
    gap: 4px;
    padding: 9px 12px;
    border-right: 1px solid #d1dbe1;
  }

  .readout-time span,
  dt {
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

  dl {
    min-width: 0;
    display: grid;
    grid-template-columns: repeat(3, minmax(0, 1fr));
    margin: 0;
  }

  dl > div {
    min-width: 0;
    display: grid;
    align-content: center;
    gap: 4px;
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

  .net dt i {
    height: 3px;
    background: #a14942;
  }

  .net.profitable dt i {
    background: #287760;
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

  .net dd {
    color: #a14942;
  }

  .net.profitable dd {
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

    .net {
      grid-column: 1 / -1;
      border-top: 1px solid #d1dbe1;
    }

    .chart {
      height: 320px;
    }
  }
</style>
