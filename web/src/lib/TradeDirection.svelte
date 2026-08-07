<script lang="ts">
  import {
    directionLabel,
    directionLegs,
    type SpreadDirection,
    type SpreadLeg
  } from './spread-direction';

  export let direction: SpreadDirection;
  export let venueA = '';
  export let venueB = '';
  export let showVenues = false;

  $: legs = directionLegs(direction);
  $: label = showVenues
    ? `${sideLabel('BUY', legs.buy)}，${sideLabel('SELL', legs.sell)}`
    : directionLabel(direction);

  function venue(leg: SpreadLeg) {
    return leg === 'A' ? venueA : venueB;
  }

  function sideLabel(action: 'BUY' | 'SELL', leg: SpreadLeg) {
    const exchange = venue(leg);
    return `${action} ${leg}${exchange ? ` ${exchange}` : ''}`;
  }
</script>

<span class="trade-direction" aria-label={label}>
  <span class="side buy">
    <b>BUY</b>
    <span>{legs.buy}</span>
    {#if showVenues && venue(legs.buy)}
      <em>{venue(legs.buy)}</em>
    {/if}
  </span>
  <i>·</i>
  <span class="side sell">
    <b>SELL</b>
    <span>{legs.sell}</span>
    {#if showVenues && venue(legs.sell)}
      <em>{venue(legs.sell)}</em>
    {/if}
  </span>
</span>

<style>
  .trade-direction,
  .side {
    display: inline-flex;
    align-items: center;
  }

  .trade-direction {
    min-width: 0;
    gap: 5px;
    font-family: "IBM Plex Mono", "SFMono-Regular", Consolas, monospace;
    white-space: nowrap;
  }

  .side {
    min-width: 0;
    gap: 4px;
  }

  b {
    padding: 1px 4px;
    border-radius: 2px;
    font-size: 0.72em;
    letter-spacing: 0.04em;
    line-height: 1.35;
  }

  .buy b {
    color: var(--trade-buy, #287760);
    background: var(--trade-buy-bg, #dcebe5);
  }

  .sell b {
    color: var(--trade-sell, #9a493f);
    background: var(--trade-sell-bg, #f1dfdc);
  }

  .side > span {
    font-weight: 750;
  }

  em {
    overflow: hidden;
    max-width: 11em;
    color: inherit;
    font-size: 0.86em;
    font-style: normal;
    font-weight: 550;
    opacity: 0.78;
    text-overflow: ellipsis;
  }

  i {
    color: currentColor;
    font-style: normal;
    opacity: 0.38;
  }
</style>
