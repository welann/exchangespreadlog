<script lang="ts">
  import { createEventDispatcher, onMount } from 'svelte';
  import {
    BaselineSeries,
    ColorType,
    CrosshairMode,
    LineSeries,
    LineStyle,
    createChart,
    type IChartApi,
    type IPriceLine,
    type ISeriesApi,
    type MouseEventParams,
    type Time,
    type UTCTimestamp
  } from 'lightweight-charts';
  import type { SpreadPoint } from '$lib/types';

  type DisplayMode = 'best' | 'both';

  type AverageLine = {
    id: string;
    label: string;
    tone: 'best' | 'a' | 'b';
    value: number;
  };

  type IndexedChartPoint = {
    time: UTCTimestamp;
    point: SpreadPoint;
    index: number;
  };

  type LineApi = ISeriesApi<'Line'>;
  type BaselineApi = ISeriesApi<'Baseline'>;
  type SpreadSeriesApi = LineApi | BaselineApi;

  export let points: SpreadPoint[] = [];
  export let displayMode: DisplayMode = 'best';
  export let showAToB = true;
  export let showBToA = true;
  export let averageLines: AverageLine[] = [];
  export let selectedIndex = -1;
  export let labelA = 'Exchange A';
  export let labelB = 'Exchange B';
  export let viewKey = '';
  export let descriptionId = 'chart-help';

  const dispatch = createEventDispatcher<{
    hover: { index: number };
    select: { index: number };
    navigate: { key: 'ArrowLeft' | 'ArrowRight' | 'Home' | 'End' };
    granularityChange: { fromMs: number; toMs: number };
  }>();

  let container: HTMLDivElement;
  let chart: IChartApi | null = null;
  let bestSeries: BaselineApi | null = null;
  let aToBSeries: LineApi | null = null;
  let bToASeries: LineApi | null = null;
  let thresholdSeries: LineApi | null = null;
  let aMidSeries: LineApi | null = null;
  let bMidSeries: LineApi | null = null;
  let chartPoints: IndexedChartPoint[] = [];
  let timeToIndex = new Map<number, number>();
  let renderedPoints: SpreadPoint[] | null = null;
  let renderedMode: DisplayMode | null = null;
  let renderedShowAToB: boolean | null = null;
  let renderedShowBToA: boolean | null = null;
  let renderedAverageKey = '';
  let renderedSelectedIndex = -2;
  let renderedLabelA = '';
  let renderedLabelB = '';
  let renderedViewKey = '';
  let visibleRangeTimer: ReturnType<typeof setTimeout> | null = null;
  let priceLines: Array<{ series: SpreadSeriesApi; line: IPriceLine }> = [];

  onMount(() => {
    chart = createChart(container, {
      autoSize: true,
      layout: {
        attributionLogo: true,
        background: { type: ColorType.Solid, color: '#0a111b' },
        textColor: '#758397',
        fontFamily:
          'ui-monospace, "SFMono-Regular", "Roboto Mono", Consolas, monospace',
        fontSize: 11,
        panes: {
          separatorColor: '#1c2b3d',
          separatorHoverColor: '#2a4058',
          enableResize: true
        }
      },
      grid: {
        vertLines: { color: '#132033', style: LineStyle.Dotted },
        horzLines: { color: '#132033', style: LineStyle.Dotted }
      },
      crosshair: {
        mode: CrosshairMode.Normal,
        vertLine: {
          color: '#95a4b7',
          width: 1,
          style: LineStyle.Dashed,
          labelBackgroundColor: '#26364a'
        },
        horzLine: {
          color: '#53657b',
          width: 1,
          style: LineStyle.Dotted,
          labelBackgroundColor: '#26364a'
        }
      },
      timeScale: {
        borderColor: '#1c2b3d',
        timeVisible: true,
        secondsVisible: true,
        rightOffset: 3,
        barSpacing: 7,
        minBarSpacing: 0.6,
        lockVisibleTimeRangeOnResize: true
      },
      rightPriceScale: {
        borderColor: '#1c2b3d',
        minimumWidth: 76,
        scaleMargins: { top: 0.12, bottom: 0.12 }
      },
      handleScroll: {
        mouseWheel: true,
        pressedMouseMove: true,
        horzTouchDrag: true,
        vertTouchDrag: false
      },
      handleScale: {
        axisPressedMouseMove: true,
        mouseWheel: true,
        pinch: true
      }
    });

    bestSeries = chart.addSeries(BaselineSeries, {
      title: 'Best route',
      baseValue: { type: 'price', price: 0 },
      topLineColor: '#29d9c2',
      topFillColor1: 'rgba(41, 217, 194, 0.18)',
      topFillColor2: 'rgba(41, 217, 194, 0.01)',
      bottomLineColor: '#e26d6a',
      bottomFillColor1: 'rgba(226, 109, 106, 0.01)',
      bottomFillColor2: 'rgba(226, 109, 106, 0.14)',
      lineWidth: 2,
      priceLineVisible: false,
      lastValueVisible: true,
      priceFormat: {
        type: 'custom',
        minMove: 0.01,
        formatter: (value: number) => `${value.toFixed(2)} bp`
      }
    });

    aToBSeries = chart.addSeries(LineSeries, {
      title: 'A bid − B ask',
      color: '#29d9c2',
      lineWidth: 2,
      priceLineVisible: false,
      lastValueVisible: true,
      priceFormat: {
        type: 'custom',
        minMove: 0.01,
        formatter: (value: number) => `${value.toFixed(2)} bp`
      }
    });

    bToASeries = chart.addSeries(LineSeries, {
      title: 'B bid − A ask',
      color: '#f0a94b',
      lineWidth: 2,
      priceLineVisible: false,
      lastValueVisible: true,
      priceFormat: {
        type: 'custom',
        minMove: 0.01,
        formatter: (value: number) => `${value.toFixed(2)} bp`
      }
    });

    thresholdSeries = chart.addSeries(LineSeries, {
      title: '',
      color: '#43546a',
      lineWidth: 1,
      lineStyle: LineStyle.Solid,
      priceLineVisible: false,
      lastValueVisible: false,
      crosshairMarkerVisible: false
    });
    thresholdSeries.createPriceLine({
      price: 0,
      color: '#43546a',
      lineWidth: 1,
      lineStyle: LineStyle.Solid,
      axisLabelVisible: true,
      title: '0 bp'
    });

    aMidSeries = chart.addSeries(
      LineSeries,
      {
        title: 'A midpoint',
        color: '#29d9c2',
        lineWidth: 2,
        priceLineVisible: false,
        lastValueVisible: true,
        priceFormat: {
          type: 'custom',
          minMove: 0.000001,
          formatter: formatPrice
        }
      },
      1
    );

    bMidSeries = chart.addSeries(
      LineSeries,
      {
        title: 'B midpoint',
        color: '#f0a94b',
        lineWidth: 2,
        priceLineVisible: false,
        lastValueVisible: true,
        priceFormat: {
          type: 'custom',
          minMove: 0.000001,
          formatter: formatPrice
        }
      },
      1
    );

    chart.panes()[0]?.setHeight(330);
    chart.panes()[1]?.setHeight(130);
    chart.subscribeCrosshairMove(handleCrosshair);
    chart.subscribeClick(handleClick);

    // ── Zoom listener: detect when the user zooms in/out enough to warrant a granularity switch ──
    chart.timeScale().subscribeVisibleTimeRangeChange(handleVisibleTimeRangeChange);

    syncChart(true);

    return () => {
      chart?.unsubscribeCrosshairMove(handleCrosshair);
      chart?.unsubscribeClick(handleClick);
      chart?.timeScale().unsubscribeVisibleTimeRangeChange(handleVisibleTimeRangeChange);
      if (visibleRangeTimer) clearTimeout(visibleRangeTimer);
      chart?.remove();
      chart = null;
    };
  });

  $: if (chart) {
    syncChart(false);
  }

  function syncChart(initial: boolean) {
    if (
      !chart ||
      !bestSeries ||
      !aToBSeries ||
      !bToASeries ||
      !thresholdSeries ||
      !aMidSeries ||
      !bMidSeries
    ) {
      return;
    }

    if (renderedPoints !== points) {
      const nextChartPoints = normalizeChartPoints(points);
      const appendFrom = appendStartIndex(chartPoints, nextChartPoints);
      chartPoints = nextChartPoints;
      timeToIndex = new Map(chartPoints.map((entry) => [entry.time as number, entry.index]));

      const bestData = chartPoints.map(({ time, point }) => {
          const value = bestBpValue(point);
          return value === null ? { time } : { time, value };
        });
      const aToBData = chartPoints.map(({ time, point }) =>
          point.aToBBp === null ? { time } : { time, value: point.aToBBp }
        );
      const bToAData = chartPoints.map(({ time, point }) =>
          point.bToABp === null ? { time } : { time, value: point.bToABp }
        );
      const thresholdData = chartPoints.map(({ time }) => ({ time, value: 0 }));
      const aMidData = chartPoints.map(({ time, point }) =>
          point.aMid === null ? { time } : { time, value: point.aMid }
        );
      const bMidData = chartPoints.map(({ time, point }) =>
          point.bMid === null ? { time } : { time, value: point.bMid }
        );

      if (appendFrom >= 0) {
        for (let index = appendFrom; index < chartPoints.length; index += 1) {
          bestSeries.update(bestData[index]);
          aToBSeries.update(aToBData[index]);
          bToASeries.update(bToAData[index]);
          thresholdSeries.update(thresholdData[index]);
          aMidSeries.update(aMidData[index]);
          bMidSeries.update(bMidData[index]);
        }
      } else {
        bestSeries.setData(bestData);
        aToBSeries.setData(aToBData);
        bToASeries.setData(bToAData);
        thresholdSeries.setData(thresholdData);
        aMidSeries.setData(aMidData);
        bMidSeries.setData(bMidData);
      }

      renderedPoints = points;
      if ((initial || renderedViewKey !== viewKey) && chartPoints.length > 0) {
        chart.timeScale().fitContent();
      }
      renderedViewKey = viewKey;
    }

    if (
      initial ||
      renderedMode !== displayMode ||
      renderedShowAToB !== showAToB ||
      renderedShowBToA !== showBToA
    ) {
      bestSeries.applyOptions({ visible: displayMode === 'best' });
      aToBSeries.applyOptions({ visible: displayMode === 'both' && showAToB });
      bToASeries.applyOptions({ visible: displayMode === 'both' && showBToA });
      renderedMode = displayMode;
      renderedShowAToB = showAToB;
      renderedShowBToA = showBToA;
    }

    if (renderedLabelA !== labelA || renderedLabelB !== labelB) {
      aToBSeries.applyOptions({ title: 'A→B' });
      bToASeries.applyOptions({ title: 'B→A' });
      aMidSeries.applyOptions({ title: 'Mid A' });
      bMidSeries.applyOptions({ title: 'Mid B' });
      renderedLabelA = labelA;
      renderedLabelB = labelB;
    }

    const averageKey = averageLines
      .map((line) => `${line.id}:${line.value}:${line.label}`)
      .join('|');
    if (averageKey !== renderedAverageKey) {
      priceLines.forEach(({ series, line }) => series.removePriceLine(line));
      priceLines = [];
      averageLines.forEach((average) => {
        const series =
          average.tone === 'a'
            ? aToBSeries!
            : average.tone === 'b'
              ? bToASeries!
              : bestSeries!;
        const color =
          average.tone === 'a' ? '#29d9c2' : average.tone === 'b' ? '#f0a94b' : '#a9b6c7';
        const line = series.createPriceLine({
          price: average.value,
          color,
          lineWidth: 1,
          lineStyle: LineStyle.Dashed,
          axisLabelVisible: true,
          title: average.label
        });
        priceLines.push({ series, line });
      });
      renderedAverageKey = averageKey;
    }

    if (selectedIndex !== renderedSelectedIndex) {
      const selected = chartPoints.find((entry) => entry.index === selectedIndex);
      if (selected) {
        const point = selected.point;
        const selectedSeries =
          displayMode === 'best'
            ? bestSeries
            : showAToB && point.aToBBp !== null
              ? aToBSeries
              : bToASeries;
        const selectedPrice =
          displayMode === 'best'
            ? bestBpValue(point)
            : showAToB && point.aToBBp !== null
              ? point.aToBBp
              : point.bToABp;
        if (selectedPrice !== null) {
          chart.setCrosshairPosition(selectedPrice, selected.time, selectedSeries);
        }
      }
      renderedSelectedIndex = selectedIndex;
    }
  }

  function normalizeChartPoints(input: SpreadPoint[]) {
    const bySecond = new Map<number, IndexedChartPoint>();
    input.forEach((point, index) => {
      const time = Math.floor(point.tsMs / 1000) as UTCTimestamp;
      bySecond.set(time as number, { time, point, index });
    });
    return [...bySecond.values()].sort((left, right) => (left.time as number) - (right.time as number));
  }

  function appendStartIndex(
    previous: IndexedChartPoint[],
    next: IndexedChartPoint[]
  ): number {
    if (previous.length === 0 || next.length < previous.length) return -1;
    const stableLength = Math.max(0, previous.length - 1);
    for (let index = 0; index < stableLength; index += 1) {
      if (
        previous[index].time !== next[index]?.time ||
        previous[index].point.id !== next[index]?.point.id
      ) {
        return -1;
      }
    }
    if (stableLength === 0) return 0;
    return stableLength;
  }

  function handleVisibleTimeRangeChange(range: { from: Time; to: Time } | null) {
    if (!range || typeof range.from !== 'number' || typeof range.to !== 'number') return;
    const fromMs = range.from * 1000;
    const toMs = range.to * 1000;
    if (!Number.isFinite(fromMs) || !Number.isFinite(toMs) || fromMs >= toMs) return;
    if (visibleRangeTimer) clearTimeout(visibleRangeTimer);
    visibleRangeTimer = setTimeout(() => {
      dispatch('granularityChange', {
        fromMs: Math.floor(fromMs),
        toMs: Math.ceil(toMs)
      });
    }, 300);
  }

  function handleCrosshair(param: MouseEventParams<Time>) {
    if (typeof param.time !== 'number') {
      dispatch('hover', { index: -1 });
      return;
    }
    dispatch('hover', { index: timeToIndex.get(param.time) ?? -1 });
  }

  function handleClick(param: MouseEventParams<Time>) {
    if (typeof param.time !== 'number') return;
    const index = timeToIndex.get(param.time);
    if (index !== undefined) dispatch('select', { index });
  }

  function handleKeydown(event: KeyboardEvent) {
    if (
      event.key === 'ArrowLeft' ||
      event.key === 'ArrowRight' ||
      event.key === 'Home' ||
      event.key === 'End'
    ) {
      dispatch('navigate', { key: event.key });
      event.preventDefault();
    }
  }

  function bestBpValue(point: SpreadPoint) {
    const values = [point.aToBBp, point.bToABp].filter(
      (value): value is number => value !== null && Number.isFinite(value)
    );
    return values.length > 0 ? Math.max(...values) : null;
  }

  function formatPrice(value: number) {
    return new Intl.NumberFormat(undefined, {
      maximumFractionDigits: Math.abs(value) >= 100 ? 2 : Math.abs(value) >= 1 ? 4 : 6
    }).format(value);
  }
</script>

<div class="chart-wrap">
  <div
    class="chart"
    bind:this={container}
    role="img"
    aria-describedby={descriptionId}
    aria-label={`${labelA} 和 ${labelB} 的价差图。可用鼠标滚轮缩放并拖动平移。`}
  ></div>
  <button
    class="keyboard-target"
    type="button"
    aria-describedby={descriptionId}
    aria-label="键盘浏览价差样本"
    on:keydown={handleKeydown}
  >
    键盘浏览
  </button>
</div>

<style>
  .chart-wrap {
    position: relative;
    width: 100%;
    min-width: 0;
    overflow: hidden;
  }

  .chart {
    width: 100%;
    min-width: 0;
    height: clamp(410px, 54vh, 560px);
    min-height: 410px;
    outline: 1px solid transparent;
  }

  .keyboard-target {
    position: absolute;
    top: 6px;
    right: 82px;
    z-index: 3;
    min-height: 28px;
    border: 1px solid #2a4058;
    border-radius: 2px;
    padding: 4px 8px;
    color: #95a4b7;
    background: rgba(10, 17, 27, 0.9);
    font-size: 0.7rem;
  }

  .keyboard-target:focus-visible {
    outline: 2px solid #29d9c2;
    outline-offset: 2px;
  }

  @media (max-width: 760px) {
    .chart {
      height: 430px;
      min-height: 430px;
    }
  }
</style>
