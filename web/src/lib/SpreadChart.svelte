<script lang="ts">
  import { onMount } from 'svelte';
  import {
    ColorType,
    CrosshairMode,
    LineSeries,
    LineStyle,
    createChart,
    type IChartApi,
    type ISeriesApi,
    type UTCTimestamp
  } from 'lightweight-charts';
  import type { SpreadPoint } from './types';

  export let points: SpreadPoint[] = [];

  let container: HTMLDivElement;
  let chart: IChartApi | null = null;
  let routeAB: ISeriesApi<'Line'> | null = null;
  let routeBA: ISeriesApi<'Line'> | null = null;
  let rendered: SpreadPoint[] | null = null;

  onMount(() => {
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
        secondsVisible: false
      },
      localization: {
        priceFormatter: (value: number) => `${value.toFixed(2)} bp`
      }
    });
    routeAB = chart.addSeries(LineSeries, {
      color: '#2e6f95',
      lineWidth: 2,
      title: 'A → B',
      priceLineVisible: false,
      lastValueVisible: true
    });
    routeBA = chart.addSeries(LineSeries, {
      color: '#c97842',
      lineWidth: 2,
      title: 'B → A',
      priceLineVisible: false,
      lastValueVisible: true
    });
    routeAB.createPriceLine({
      price: 0,
      color: '#8fa0ad',
      lineWidth: 1,
      lineStyle: LineStyle.Dashed,
      axisLabelVisible: true,
      title: '零线'
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
    routeAB?.setData(
      sorted.map(([time, point]) => ({
        time: time as UTCTimestamp,
        value: point.aToBBp
      }))
    );
    routeBA?.setData(
      sorted.map(([time, point]) => ({
        time: time as UTCTimestamp,
        value: point.bToABp
      }))
    );
  }
</script>

<div
  class="chart"
  bind:this={container}
  role="img"
  aria-label="两条跨所价差时间序列，单位为基点"
></div>

<style>
  .chart {
    width: 100%;
    height: 420px;
  }

  @media (max-width: 760px) {
    .chart {
      height: 330px;
    }
  }
</style>
