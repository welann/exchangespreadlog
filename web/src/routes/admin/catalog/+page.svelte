<script lang="ts">
  import { onMount } from 'svelte';
  import type { AssetGroup, CatalogInstrument, InstrumentKey } from '$lib/types';

  type CatalogMode = 'instruments' | 'automatic';
  type AutomaticScope = 'all' | 'monitored' | 'excluded';
  type AutomaticGroup = {
    symbol: string;
    members: CatalogInstrument[];
    venues: string[];
    result: 'monitored' | 'missing-lighter' | 'single-venue';
  };

  let instruments: CatalogInstrument[] = [];
  let groups: AssetGroup[] = [];
  let search = '';
  let venue = '';
  let view: 'all' | 'unmatched' | 'mapped' | 'inactive' = 'all';
  let catalogMode: CatalogMode = 'instruments';
  let automaticScope: AutomaticScope = 'all';
  let editingId: number | null = null;
  let groupSymbol = '';
  let selectedKeys: string[] = [];
  let loading = true;
  let saving = false;
  let refreshing = false;
  let message = '';
  let error = '';
  let rawInstrument: CatalogInstrument | null = null;

  $: venues = [...new Set(instruments.map((instrument) => instrument.venue))].sort();
  $: selectedMembers = instruments.filter((instrument) =>
    selectedKeys.includes(instrumentKey(instrument))
  );
  $: automaticGroups = buildAutomaticGroups(instruments);
  $: filteredAutomaticGroups = automaticGroups.filter((group) => {
    const query = search.trim().toLowerCase();
    const matchesSearch =
      !query ||
      group.symbol.toLowerCase().includes(query) ||
      group.members.some(
        (instrument) =>
          instrument.venue.toLowerCase().includes(query) ||
          instrument.symbol.toLowerCase().includes(query) ||
          instrument.instrumentId.toLowerCase().includes(query)
      );
    const matchesVenue = !venue || group.venues.includes(venue);
    const matchesScope =
      automaticScope === 'all' ||
      (automaticScope === 'monitored' && group.result === 'monitored') ||
      (automaticScope === 'excluded' && group.result !== 'monitored');
    return matchesSearch && matchesVenue && matchesScope;
  });
  $: filtered = instruments.filter((instrument) => {
    const query = search.trim().toLowerCase();
    const matchesSearch =
      !query ||
      instrument.symbol.toLowerCase().includes(query) ||
      instrument.instrumentId.toLowerCase().includes(query) ||
      instrument.normalizedSymbol.toLowerCase().includes(query) ||
      instrument.assetGroupSymbol?.toLowerCase().includes(query);
    const matchesVenue = !venue || instrument.venue === venue;
    const matchesView =
      view === 'all' ||
      (view === 'unmatched' && !instrument.assetGroupId) ||
      (view === 'mapped' && Boolean(instrument.assetGroupId)) ||
      (view === 'inactive' && (!instrument.present || instrument.status !== 'active'));
    return matchesSearch && matchesVenue && matchesView;
  });
  $: unmatchedCount = instruments.filter((instrument) => !instrument.assetGroupId).length;
  $: activeCount = instruments.filter(
    (instrument) => instrument.present && instrument.status === 'active'
  ).length;
  $: automaticMonitoredCount = automaticGroups.filter(
    (group) => group.result === 'monitored'
  ).length;

  onMount(() => void loadCatalog());

  async function loadCatalog() {
    loading = true;
    error = '';
    try {
      const [instrumentResponse, groupResponse] = await Promise.all([
        fetch('/v1/admin/catalog/instruments'),
        fetch('/v1/admin/catalog/groups')
      ]);
      if (!instrumentResponse.ok) throw new Error(await publicError(instrumentResponse));
      if (!groupResponse.ok) throw new Error(await publicError(groupResponse));
      instruments = ((await instrumentResponse.json()) as { instruments: CatalogInstrument[] })
        .instruments;
      groups = ((await groupResponse.json()) as { groups: AssetGroup[] }).groups;
    } catch (cause) {
      error = errorMessage(cause);
    } finally {
      loading = false;
    }
  }

  function clearEditor() {
    editingId = null;
    groupSymbol = '';
    selectedKeys = [];
    message = '';
  }

  function editGroup(group: AssetGroup) {
    editingId = group.id;
    groupSymbol = group.symbol;
    selectedKeys = group.members.map(memberKey);
    message = '';
  }

  function toggleInstrument(instrument: CatalogInstrument) {
    const previousSuggestion = suggestedSymbol(selectedMembers);
    const key = instrumentKey(instrument);
    if (selectedKeys.includes(key)) {
      selectedKeys = selectedKeys.filter((value) => value !== key);
    } else {
      const sameVenue = selectedMembers.find((member) => member.venue === instrument.venue);
      selectedKeys = selectedKeys.filter(
        (value) => !sameVenue || value !== instrumentKey(sameVenue)
      );
      selectedKeys = [...selectedKeys, key];
    }
    if (editingId === null && (!groupSymbol || groupSymbol === previousSuggestion)) {
      groupSymbol = suggestedSymbol(
        instruments.filter((candidate) => selectedKeys.includes(instrumentKey(candidate)))
      );
    }
  }

  async function saveGroup() {
    const symbol = groupSymbol.trim();
    if (!symbol) {
      error = '请输入资产组名称。';
      return;
    }
    saving = true;
    error = '';
    message = '';
    try {
      const members: InstrumentKey[] = selectedMembers.map((instrument) => ({
        venue: instrument.venue,
        instrumentId: instrument.instrumentId
      }));
      const wasEditing = editingId !== null;
      const response = await fetch(
        editingId === null
          ? '/v1/admin/catalog/groups'
          : `/v1/admin/catalog/groups/${editingId}`,
        {
          method: editingId === null ? 'POST' : 'PUT',
          headers: { 'Content-Type': 'application/json' },
          body: JSON.stringify({ symbol, members })
        }
      );
      if (!response.ok) throw new Error(await publicError(response));
      message = `${symbol.toUpperCase()} 已保存，正在应用订阅目录。`;
      await loadCatalog();
      if (wasEditing) {
        const saved = groups.find((group) => group.symbol === symbol.toUpperCase());
        if (saved) editGroup(saved);
      } else {
        editingId = null;
        groupSymbol = '';
        selectedKeys = [];
      }
    } catch (cause) {
      error = errorMessage(cause);
    } finally {
      saving = false;
    }
  }

  async function deleteGroup() {
    if (editingId === null) return;
    const response = await fetch(`/v1/admin/catalog/groups/${editingId}`, { method: 'DELETE' });
    if (!response.ok) {
      error = await publicError(response);
      return;
    }
    clearEditor();
    message = '资产组已删除，原交易对恢复自动匹配。';
    await loadCatalog();
  }

  async function refreshCatalog() {
    refreshing = true;
    error = '';
    const response = await fetch('/v1/admin/catalog/refresh', { method: 'POST' });
    if (!response.ok) {
      error = await publicError(response);
      refreshing = false;
      return;
    }
    message = '已请求刷新全部交易所目录。';
    window.setTimeout(async () => {
      await loadCatalog();
      refreshing = false;
    }, 2500);
  }

  function instrumentKey(instrument: CatalogInstrument) {
    return memberKey({ venue: instrument.venue, instrumentId: instrument.instrumentId });
  }

  function memberKey(member: InstrumentKey) {
    return `${member.venue}\u0000${member.instrumentId}`;
  }

  function suggestedSymbol(members: CatalogInstrument[]) {
    return (
      members.find((instrument) => instrument.venue === 'lighter')?.normalizedSymbol ??
      members[0]?.normalizedSymbol ??
      ''
    );
  }

  function buildAutomaticGroups(source: CatalogInstrument[]): AutomaticGroup[] {
    const grouped = new Map<string, CatalogInstrument[]>();
    for (const instrument of source) {
      if (!instrument.present || !instrument.eligible) continue;
      const members = grouped.get(instrument.normalizedSymbol) ?? [];
      members.push(instrument);
      grouped.set(instrument.normalizedSymbol, members);
    }
    return [...grouped.entries()]
      .map(([symbol, members]) => {
        const venues = [...new Set(members.map((instrument) => instrument.venue))].sort();
        const result = !venues.includes('lighter')
          ? venues.length > 1
            ? 'missing-lighter'
            : 'single-venue'
          : venues.length > 1
            ? 'monitored'
            : 'single-venue';
        return { symbol, members, venues, result } as AutomaticGroup;
      })
      .sort(
        (left, right) =>
          Number(right.result === 'monitored') - Number(left.result === 'monitored') ||
          left.symbol.localeCompare(right.symbol)
      );
  }

  function automaticResultLabel(group: AutomaticGroup) {
    if (group.result === 'monitored') return '进入监控';
    if (group.result === 'missing-lighter') return '缺少 Lighter';
    return '仅单交易所';
  }

  function selectAutomaticGroup(group: AutomaticGroup) {
    const byVenue = new Map<string, CatalogInstrument>();
    for (const instrument of group.members) {
      if (!byVenue.has(instrument.venue)) byVenue.set(instrument.venue, instrument);
    }
    editingId = null;
    groupSymbol = group.symbol;
    selectedKeys = [...byVenue.values()].map(instrumentKey);
    catalogMode = 'instruments';
    search = group.symbol;
    venue = '';
    view = 'all';
    message = `${group.symbol} 的自动分组成员已带入右侧，可直接新建或调整。`;
  }

  function manualGroupLabels(group: AutomaticGroup) {
    return [
      ...new Set(
        group.members
          .map((instrument) => instrument.assetGroupSymbol)
          .filter((symbol): symbol is string => Boolean(symbol))
      )
    ];
  }

  function eligibilityLabel(instrument: CatalogInstrument) {
    if (!instrument.present) return '目录缺失';
    if (instrument.eligible) return '可订阅';
    if (instrument.eligibilityReason === 'inactive') return '未激活';
    if (instrument.eligibilityReason === 'unsupported_product') return '产品类型不支持';
    if (instrument.eligibilityReason === 'unsupported_quote') return '报价资产不支持';
    return instrument.eligibilityReason ?? '不可订阅';
  }

  function formatTime(timestamp: number) {
    return new Date(timestamp).toLocaleString();
  }

  async function publicError(response: Response) {
    try {
      const payload = await response.json();
      return payload.error?.message ?? `请求失败（${response.status}）`;
    } catch {
      return `请求失败（${response.status}）`;
    }
  }

  function errorMessage(cause: unknown) {
    return cause instanceof Error ? cause.message : '请求失败。';
  }
</script>

<svelte:head>
  <title>交易对审核 · Spread Observatory</title>
</svelte:head>

<div class="catalog-shell">
  <header class="masthead">
    <div class="identity">
      <a href="/">Spread Observatory</a>
      <span>交易对审核台</span>
    </div>
    <div class="actions">
      <span>{activeCount} 个活跃合约</span>
      <span class="divider"></span>
      <span>{unmatchedCount} 个未人工分组</span>
      <button on:click={refreshCatalog} disabled={refreshing}>
        {refreshing ? '刷新中' : '刷新目录'}
      </button>
    </div>
  </header>

  {#if error || message}
    <div class:error class:success={Boolean(message) && !error} class="notice">
      {error || message}
      <button on:click={() => { error = ''; message = ''; }} aria-label="关闭提示">×</button>
    </div>
  {/if}

  <main>
    <section class="inventory">
      <header class="section-heading">
        <div>
          <span>{catalogMode === 'instruments' ? 'Exchange inventory' : 'Automatic resolution'}</span>
          <h1>{catalogMode === 'instruments' ? '全部交易对' : '自动分组结果'}</h1>
        </div>
        <strong>
          {catalogMode === 'instruments'
            ? `${filtered.length} / ${instruments.length}`
            : `${filteredAutomaticGroups.length} / ${automaticGroups.length}`}
        </strong>
      </header>

      <div class="catalog-modes" aria-label="目录视图">
        <button class:active={catalogMode === 'instruments'} on:click={() => (catalogMode = 'instruments')}>
          交易对目录
        </button>
        <button class:active={catalogMode === 'automatic'} on:click={() => (catalogMode = 'automatic')}>
          自动分组
          <small>{automaticMonitoredCount} 个进入监控</small>
        </button>
      </div>

      {#if catalogMode === 'automatic'}
        <p class="mode-note">
          这里仅使用程序自动识别的资产名，不应用人工编组。跨所同名且包含 Lighter 的分组会进入监控。
        </p>
      {/if}

      <div class="filters">
        <label class="search">
          <span>搜索</span>
          <input
            bind:value={search}
            placeholder={catalogMode === 'instruments'
              ? 'symbol / instrument ID / 资产组'
              : '自动资产名 / 交易所 / 合约'}
          />
        </label>
        <label>
          <span>交易所</span>
          <select bind:value={venue}>
            <option value="">全部交易所</option>
            {#each venues as option}
              <option value={option}>{option}</option>
            {/each}
          </select>
        </label>
        {#if catalogMode === 'instruments'}
          <div class="view-tabs" aria-label="目录筛选">
            {#each [
              ['all', '全部'],
              ['unmatched', '未匹配'],
              ['mapped', '已匹配'],
              ['inactive', '非活跃']
            ] as option}
              <button
                class:active={view === option[0]}
                on:click={() => (view = option[0] as typeof view)}
              >{option[1]}</button>
            {/each}
          </div>
        {:else}
          <div class="view-tabs" aria-label="自动分组筛选">
            {#each [
              ['all', '全部'],
              ['monitored', '进入监控'],
              ['excluded', '未进入']
            ] as option}
              <button
                class:active={automaticScope === option[0]}
                on:click={() => (automaticScope = option[0] as AutomaticScope)}
              >{option[1]}</button>
            {/each}
          </div>
        {/if}
      </div>

      <div class="table-wrap">
        {#if catalogMode === 'instruments'}
          <table>
            <thead>
              <tr>
                <th aria-label="选择"></th>
                <th>交易所 / 合约</th>
                <th>原始 symbol</th>
                <th>自动识别</th>
                <th>类型 / quote</th>
                <th>状态</th>
                <th>人工资产组</th>
                <th>最近发现</th>
                <th></th>
              </tr>
            </thead>
            <tbody>
              {#if loading}
                {#each Array(8) as _}
                  <tr class="skeleton"><td colspan="9"><span></span></td></tr>
                {/each}
              {:else if filtered.length === 0}
                <tr><td colspan="9" class="empty">没有符合条件的交易对。</td></tr>
              {:else}
                {#each filtered as instrument}
                  <tr class:selected={selectedKeys.includes(instrumentKey(instrument))}>
                    <td>
                      <input
                        type="checkbox"
                        checked={selectedKeys.includes(instrumentKey(instrument))}
                        on:change={() => toggleInstrument(instrument)}
                        aria-label={`选择 ${instrument.venue} ${instrument.symbol}`}
                      />
                    </td>
                    <td>
                      <strong>{instrument.venue}</strong>
                      <small>{instrument.instrumentId}</small>
                    </td>
                    <td class="symbol">{instrument.symbol}</td>
                    <td><code>{instrument.normalizedSymbol}</code></td>
                    <td>
                      <span>{instrument.productType}</span>
                      <small>{instrument.quoteAsset}</small>
                    </td>
                    <td>
                      <span class:eligible={instrument.eligible} class="status">
                        {eligibilityLabel(instrument)}
                      </span>
                    </td>
                    <td>
                      {#if instrument.assetGroupSymbol}
                        <button class="group-link" on:click={() => {
                          const group = groups.find((value) => value.id === instrument.assetGroupId);
                          if (group) editGroup(group);
                        }}>{instrument.assetGroupSymbol}</button>
                      {:else}
                        <span class="muted">—</span>
                      {/if}
                    </td>
                    <td><small>{formatTime(instrument.lastSeenMs)}</small></td>
                    <td><button class="raw" on:click={() => (rawInstrument = instrument)}>JSON</button></td>
                  </tr>
                {/each}
              {/if}
            </tbody>
          </table>
        {:else}
          <table class="automatic-table">
            <thead>
              <tr>
                <th>自动资产名</th>
                <th>程序结果</th>
                <th>交易所</th>
                <th>归入的交易对</th>
                <th>人工覆盖</th>
                <th></th>
              </tr>
            </thead>
            <tbody>
              {#if loading}
                {#each Array(8) as _}
                  <tr class="skeleton"><td colspan="6"><span></span></td></tr>
                {/each}
              {:else if filteredAutomaticGroups.length === 0}
                <tr><td colspan="6" class="empty">没有符合条件的自动分组。</td></tr>
              {:else}
                {#each filteredAutomaticGroups as group}
                  {@const manualLabels = manualGroupLabels(group)}
                  <tr>
                    <td>
                      <strong class="automatic-symbol">{group.symbol}</strong>
                      <small>{group.members.length} 个合约</small>
                    </td>
                    <td>
                      <span
                        class="auto-result"
                        class:monitored={group.result === 'monitored'}
                        class:excluded={group.result !== 'monitored'}
                      >{automaticResultLabel(group)}</span>
                    </td>
                    <td>
                      <div class="venue-stack">
                        {#each group.venues as groupVenue}<span>{groupVenue}</span>{/each}
                      </div>
                    </td>
                    <td>
                      <div class="automatic-members">
                        {#each group.members as instrument}
                          <span><b>{instrument.venue}</b>{instrument.symbol}</span>
                        {/each}
                      </div>
                    </td>
                    <td>
                      {#if manualLabels.length > 0}
                        {#each manualLabels as label}<code>{label}</code>{/each}
                      {:else}
                        <span class="muted">—</span>
                      {/if}
                    </td>
                    <td>
                      <button class="use-group" on:click={() => selectAutomaticGroup(group)}>
                        带入人工编组
                      </button>
                    </td>
                  </tr>
                {/each}
              {/if}
            </tbody>
          </table>
        {/if}
      </div>
    </section>

    <aside class="mapping-panel">
      <header class="section-heading compact">
        <div>
          <span>Manual mapping</span>
          <h2>{editingId === null ? '用所选交易对新建' : '编辑人工编组'}</h2>
        </div>
        <strong>{selectedMembers.length} 个交易所</strong>
      </header>

      <section class="mapping-editor">
        <label>
          <span>{editingId === null ? '新资产名称' : '资产组名称'}</span>
          <input bind:value={groupSymbol} placeholder="例如 SKHYNIX" />
        </label>

        <div class="mapping-rail">
          {#if selectedMembers.length === 0}
            <p>先从左侧勾选交易对。程序会优先采用所选 Lighter 合约的自动资产名，你也可以在上方修改。</p>
          {:else}
            {#each selectedMembers as instrument}
              <article>
                <i></i>
                <div>
                  <span>{instrument.venue}</span>
                  <strong>{instrument.symbol}</strong>
                  <small>{instrument.instrumentId}</small>
                </div>
                <button on:click={() => toggleInstrument(instrument)} aria-label="移出资产组">×</button>
              </article>
            {/each}
          {/if}
        </div>

        <div class="editor-actions">
          {#if editingId !== null}
            <button class="delete" on:click={deleteGroup}>删除</button>
            <button class="cancel" on:click={clearEditor}>取消编辑</button>
          {:else if selectedMembers.length > 0}
            <button class="cancel" on:click={clearEditor}>清空所选</button>
          {/if}
          <button
            class="save"
            on:click={saveGroup}
            disabled={saving || selectedMembers.length === 0 || !groupSymbol.trim()}
          >
            {saving ? '保存中' : editingId === null ? '新建编组' : '保存修改'}
          </button>
        </div>
        <small class="editor-note">保存后立即刷新订阅计划；同一人工资产组中，每家交易所只保留一个合约。</small>
      </section>

      <section class="saved-groups">
        <header>
          <div>
            <span>Reviewed overrides</span>
            <h3>已有人工编组</h3>
          </div>
          <strong>{groups.length}</strong>
        </header>
        <div class="group-list">
          {#each groups as group}
            <button class:active={editingId === group.id} on:click={() => editGroup(group)}>
              <strong>{group.symbol}</strong>
              <span>{group.members.length} 个交易所</span>
            </button>
          {/each}
          {#if groups.length === 0}
            <p>还没有人工编组。勾选左侧交易对后，点击“新建编组”。</p>
          {/if}
        </div>
      </section>
    </aside>
  </main>
</div>

{#if rawInstrument}
  <div class="raw-overlay" role="presentation" on:click={() => (rawInstrument = null)}>
    <dialog open aria-label="交易所原始元数据" on:click|stopPropagation>
      <header>
        <div><strong>{rawInstrument.venue}</strong><span>{rawInstrument.symbol}</span></div>
        <button on:click={() => (rawInstrument = null)}>关闭</button>
      </header>
      <pre>{JSON.stringify(JSON.parse(rawInstrument.rawJson), null, 2)}</pre>
    </dialog>
  </div>
{/if}

<style>
  :global(*) { box-sizing: border-box; }
  :global(html) { color-scheme: light; background: #e7edf1; }
  :global(body) {
    margin: 0;
    min-width: 320px;
    color: #13243a;
    background: #e7edf1;
    font-family: "Avenir Next", Avenir, "Segoe UI", sans-serif;
  }
  button, input, select { font: inherit; }
  button { color: inherit; }
  button:focus-visible, input:focus-visible, select:focus-visible {
    outline: 2px solid #2e6f95;
    outline-offset: 2px;
  }
  .catalog-shell {
    width: min(1760px, calc(100% - 24px));
    min-height: calc(100vh - 24px);
    margin: 12px auto;
    border: 1px solid #aebdc8;
    background: #f3f6f8;
  }
  .masthead {
    min-height: 66px;
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 20px;
    padding: 0 20px;
    border-bottom: 1px solid #aebdc8;
    background: rgba(248, 250, 251, 0.96);
  }
  .identity a, h1, h2, .group-list strong {
    font-family: "Avenir Next Condensed", "Arial Narrow", sans-serif;
  }
  .identity a { color: #13243a; font-size: 18px; font-weight: 700; letter-spacing: .04em; text-decoration: none; }
  .identity span { display: block; margin-top: 2px; color: #657789; font-size: 11px; letter-spacing: .08em; }
  .actions { display: flex; align-items: center; gap: 10px; color: #526579; font: 11px "IBM Plex Mono", monospace; }
  .actions button, .raw, .group-link, .use-group {
    border: 1px solid #aebdc8;
    background: #f8fafb;
    cursor: pointer;
  }
  .actions button { padding: 7px 11px; }
  .divider { width: 1px; height: 16px; background: #c9d3da; }
  .notice { display: flex; justify-content: space-between; padding: 10px 18px; border-bottom: 1px solid #dcb6ae; background: #f6e9e6; color: #6c312b; font-size: 13px; }
  .notice.success { border-color: #b8d2c8; background: #e9f2ef; color: #2f6657; }
  .notice button { border: 0; background: transparent; cursor: pointer; font-size: 18px; }
  main { display: grid; grid-template-columns: minmax(720px, 1fr) 360px; min-height: calc(100vh - 92px); }
  .inventory { min-width: 0; padding: 22px; }
  .mapping-panel { border-left: 1px solid #aebdc8; background: #edf2f5; }
  .section-heading { display: flex; align-items: flex-end; justify-content: space-between; margin-bottom: 18px; }
  .section-heading span { color: #6f8090; font: 10px "IBM Plex Mono", monospace; letter-spacing: .12em; text-transform: uppercase; }
  .section-heading h1, .section-heading h2 { margin: 2px 0 0; font-size: 27px; letter-spacing: .02em; }
  .section-heading h2 { font-size: 22px; }
  .section-heading > strong { color: #6a7b8b; font: 12px "IBM Plex Mono", monospace; }
  .section-heading.compact { padding: 20px 18px 0; margin-bottom: 14px; }
  .section-heading.compact > strong { padding-bottom: 3px; }
  .catalog-modes { display: flex; margin: -4px 0 14px; border-bottom: 1px solid #b9c6cf; }
  .catalog-modes button { display: flex; align-items: baseline; gap: 9px; padding: 9px 13px; border: 0; border-bottom: 3px solid transparent; background: transparent; color: #657789; cursor: pointer; font-weight: 700; }
  .catalog-modes button.active { border-bottom-color: #c97842; color: #13243a; }
  .catalog-modes small { color: #7b8b99; font: 9px "IBM Plex Mono", monospace; font-weight: 400; }
  .mode-note { margin: -4px 0 14px; padding: 9px 11px; border-left: 3px solid #c97842; background: #edf2f5; color: #5c6f80; font-size: 11px; line-height: 1.5; }
  .filters { display: grid; grid-template-columns: minmax(260px, 1fr) 170px auto; gap: 10px; align-items: end; margin-bottom: 14px; }
  label > span { display: block; margin-bottom: 5px; color: #657789; font-size: 10px; letter-spacing: .08em; text-transform: uppercase; }
  input, select { width: 100%; min-height: 36px; padding: 7px 10px; border: 1px solid #b9c6cf; border-radius: 0; background: #fbfcfd; color: #13243a; }
  .view-tabs { display: flex; }
  .view-tabs button { min-height: 36px; padding: 0 11px; border: 1px solid #b9c6cf; border-left: 0; background: #edf2f5; cursor: pointer; font-size: 12px; }
  .view-tabs button:first-child { border-left: 1px solid #b9c6cf; }
  .view-tabs button.active { color: #fff; background: #264b66; }
  .table-wrap { overflow: auto; border: 1px solid #b9c6cf; background: #fbfcfd; max-height: calc(100vh - 272px); }
  table { width: 100%; border-collapse: collapse; font-size: 12px; }
  th { position: sticky; top: 0; z-index: 2; padding: 9px 10px; color: #657789; background: #e8eef2; border-bottom: 1px solid #b9c6cf; font-size: 10px; letter-spacing: .05em; text-align: left; white-space: nowrap; }
  td { padding: 9px 10px; border-bottom: 1px solid #d8e0e5; vertical-align: middle; }
  tbody tr:hover, tbody tr.selected { background: #f4eadf; }
  td strong, td small { display: block; }
  td small { margin-top: 2px; color: #718191; font: 10px "IBM Plex Mono", monospace; white-space: nowrap; }
  td.symbol, code { font: 12px "IBM Plex Mono", monospace; white-space: nowrap; }
  code { padding: 2px 5px; background: #edf2f5; }
  .status { display: inline-block; padding: 3px 6px; color: #80514b; background: #f3e5e1; white-space: nowrap; }
  .status.eligible { color: #316b5a; background: #dfede8; }
  .group-link { padding: 4px 7px; color: #8a4d26; border-color: #d6b18f; background: #f8eee4; font-weight: 700; }
  .muted { color: #93a0aa; }
  .raw { padding: 4px 7px; font: 10px "IBM Plex Mono", monospace; }
  .automatic-table { min-width: 980px; }
  .automatic-table tbody tr:hover { background: #f5f8fa; }
  .automatic-symbol { font: 18px "Avenir Next Condensed", "Arial Narrow", sans-serif; letter-spacing: .02em; }
  .auto-result { display: inline-block; padding: 4px 7px; white-space: nowrap; }
  .auto-result.monitored { color: #316b5a; background: #dfede8; }
  .auto-result.excluded { color: #80514b; background: #f3e5e1; }
  .venue-stack, .automatic-members { display: flex; flex-wrap: wrap; gap: 5px; }
  .venue-stack span { padding: 3px 6px; background: #e6edf2; font: 10px "IBM Plex Mono", monospace; }
  .automatic-members { max-width: 480px; }
  .automatic-members span { display: inline-flex; gap: 5px; padding: 4px 6px; border: 1px solid #d2dce3; background: #f7f9fa; font: 10px "IBM Plex Mono", monospace; }
  .automatic-members b { color: #6e7f8e; font-weight: 500; }
  .automatic-table td code { display: inline-block; margin: 2px; color: #8a4d26; background: #f8eee4; }
  .use-group { padding: 6px 8px; white-space: nowrap; }
  .empty { height: 180px; color: #718191; text-align: center; }
  .skeleton span { display: block; height: 22px; background: linear-gradient(90deg, #edf2f5, #f8fafb, #edf2f5); background-size: 200% 100%; animation: pulse 1.4s infinite; }
  .group-list { max-height: 280px; overflow: auto; border-top: 1px solid #c5d0d8; border-bottom: 1px solid #c5d0d8; }
  .group-list > button { width: 100%; display: flex; align-items: center; justify-content: space-between; padding: 10px 18px; border: 0; border-bottom: 1px solid #d4dde3; background: transparent; cursor: pointer; text-align: left; }
  .group-list > button.active { box-shadow: inset 4px 0 #c97842; background: #f8fafb; }
  .group-list strong { font-size: 16px; }
  .group-list span, .group-list p { color: #708191; font-size: 11px; }
  .group-list p { padding: 0 18px 10px; }
  .mapping-editor { padding: 18px; border-top: 1px solid #c5d0d8; border-bottom: 1px solid #c5d0d8; background: #f2f6f8; }
  .mapping-rail { position: relative; min-height: 96px; margin: 18px 0; padding-left: 20px; }
  .mapping-rail::before { content: ''; position: absolute; left: 5px; top: 7px; bottom: 7px; width: 2px; background: #c97842; }
  .mapping-rail > p { color: #708191; font-size: 12px; line-height: 1.5; }
  .mapping-rail article { position: relative; display: grid; grid-template-columns: 1fr auto; gap: 8px; align-items: center; margin-bottom: 9px; padding: 9px 9px 9px 12px; border: 1px solid #c5d0d8; background: #f8fafb; }
  .mapping-rail article i { position: absolute; left: -20px; top: 50%; width: 20px; height: 1px; background: #c97842; }
  .mapping-rail article span, .mapping-rail article small { display: block; color: #708191; font: 10px "IBM Plex Mono", monospace; }
  .mapping-rail article strong { display: block; margin: 2px 0; }
  .mapping-rail article button { border: 0; background: transparent; cursor: pointer; font-size: 18px; }
  .editor-actions { display: flex; justify-content: flex-end; gap: 8px; }
  .editor-actions button { padding: 9px 12px; border: 1px solid #aebdc8; cursor: pointer; }
  .editor-actions .save { color: #fff; border-color: #264b66; background: #264b66; }
  .editor-actions .delete { margin-right: auto; color: #81453f; background: #f5e8e5; }
  .editor-actions .cancel { background: #f8fafb; }
  .editor-actions button:disabled { opacity: .45; cursor: default; }
  .editor-note { display: block; margin-top: 10px; color: #748493; line-height: 1.4; }
  .saved-groups > header { display: flex; align-items: flex-end; justify-content: space-between; padding: 17px 18px 11px; }
  .saved-groups > header span { color: #6f8090; font: 9px "IBM Plex Mono", monospace; letter-spacing: .1em; text-transform: uppercase; }
  .saved-groups h3 { margin: 2px 0 0; font: 18px "Avenir Next Condensed", "Arial Narrow", sans-serif; }
  .saved-groups > header strong { color: #6a7b8b; font: 12px "IBM Plex Mono", monospace; }
  .raw-overlay { position: fixed; inset: 0; z-index: 20; display: grid; place-items: center; padding: 20px; background: rgba(19, 36, 58, .48); }
  .raw-overlay dialog { position: static; width: min(760px, 100%); max-height: 84vh; margin: 0; padding: 0; border: 1px solid #8295a4; background: #f8fafb; box-shadow: 0 24px 70px rgba(19, 36, 58, .25); }
  .raw-overlay header { display: flex; align-items: center; justify-content: space-between; padding: 12px 15px; border-bottom: 1px solid #c5d0d8; }
  .raw-overlay header strong, .raw-overlay header span { display: block; }
  .raw-overlay header span { color: #708191; font: 11px "IBM Plex Mono", monospace; }
  .raw-overlay header button { border: 0; background: transparent; cursor: pointer; }
  pre { max-height: calc(84vh - 58px); margin: 0; padding: 16px; overflow: auto; color: #243c52; background: #eef3f6; font: 11px/1.55 "IBM Plex Mono", monospace; }
  @keyframes pulse { to { background-position: -200% 0; } }
  @media (prefers-reduced-motion: reduce) { .skeleton span { animation: none; } }
  @media (max-width: 1080px) {
    main { grid-template-columns: 1fr; }
    .mapping-panel { border-top: 1px solid #aebdc8; border-left: 0; }
    .table-wrap { max-height: 62vh; }
  }
  @media (max-width: 720px) {
    .catalog-shell { width: 100%; min-height: 100vh; margin: 0; border-right: 0; border-left: 0; }
    .masthead { align-items: flex-start; flex-direction: column; padding: 14px; }
    .actions { width: 100%; flex-wrap: wrap; }
    .inventory { padding: 14px; }
    .filters { grid-template-columns: 1fr; }
    .view-tabs { overflow-x: auto; }
    .table-wrap { max-height: 58vh; }
  }
</style>
