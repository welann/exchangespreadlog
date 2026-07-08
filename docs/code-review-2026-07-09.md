# Code Review — 2026-07-09 (第三轮)

> 审阅范围：全项目 47 个 `.rs` 文件 + `config.toml` + `config.example.toml`
> 原则：/ponytail — 删比加好，标库优先，最懒方案赢

---

## 上轮修复确认 ✅

14 个问题已被修复：

| # | 问题 | 状态 |
|---|------|------|
| 1 | `config.toml` 格式不兼容 | ✅ 已更新为新格式 |
| 2 | 明文密码 | ✅ 改为 `password_env` |
| 3 | normalizer note 覆盖 | ✅ 新增 `DataQuality::add_note()` 用 `; ` 拼接 |
| 4 | crypto provider 静默失败 | ✅ `.expect()` |
| 5 | `heartbeat.rs` 死代码 | ✅ 整个文件已删除 |
| 6 | `Venue` 枚举死代码 | ✅ 整个枚举已删除 |
| 7 | stale_after_ms 未用 | ✅ 新增 `mark_stale()` 函数 |
| 8 | DataQuality.stale 未填充 | ✅ 同上 |
| 9 | adapter 重连循环重复 | ✅ 新增 `run_with_reconnect` / `run_with_reconnect_backoff`，4/5 adapter 已迁移 |
| 10 | `CatalogClickHouseRow` serde 往返 | ✅ 改为 `catalog.product_type.as_str()` |
| 11 | `QuoteRateBook::rate` 重复 parse | ✅ 改为 `Fixed::new(1, 0)` |
| 12 | `batch_size.max(1)` 静默改写 | ✅ 加了 warning |
| 13 | `telemetry` 吞错误 | ✅ 改为条件判断，只屏蔽 "already set" 错误 |
| 14 | `micros_to_millis` 魔数 | ✅ 加了注释标明 2286 年天花板 |

---

## 本轮新发现

### 🔴 1. `LighterAdapter::run()` 在重连时不刷新 catalog

```rust
// lighter/adapter.rs:267
async fn run(&self, tx, shutdown) -> Result<()> {
    let catalog = self.bootstrap_catalog().await?;           // ← 只在首次 run() 时调用
    let backoff = Backoff::new(Duration::from_secs(15), ...);
    run_with_reconnect_backoff("lighter", tx, shutdown, backoff, |tx, shutdown| {
        self.run_once(&catalog, tx, shutdown)                // ← run_once 复用同一个 catalog
    })
}
```

`bootstrap_catalog` 只调一次。如果 Lighter 上线了新交易对，adapter 重连后仍然用旧的 catalog，新交易对永远不会被订阅。对比 Hyperliquid：

```rust
// hyperliquid/adapter.rs:251
async fn run(&self, tx, shutdown) -> Result<()> {
    run_with_reconnect("hyperliquid", tx, shutdown, |tx, shutdown| {
        self.run_once(tx, shutdown)  // ← run_once 内部每次重连都调用 bootstrap_catalog
    })
}
```

Hyperliquid 每次 `run_once` 都 `self.bootstrap_catalog().await?`，而 Lighter 只调一次。Ethereal 也是 Hyperliquid 模式。**Lighter 的 catalog 不会随重连更新。**

**修复**：把 `bootstrap_catalog` 移到 `run_once` 内部，和 Hyperliquid/Ethereal 一致。

### 🟡 2. `BboClickHouseRow` 有 3 个向后兼容冗余字段

```rust
// storage/clickhouse.rs:320
/// Compatibility fields for older ClickHouse tables created before catalog_id
/// became the tick identity.
venue: String,
market_id: String,
market_symbol: Option<String>,
```

这三个字段是 `venue_instance_id` / `instrument_id` 的拷贝，每行多 3 次 clone + 序列化。注释说旧表兼容，但 ClickHouse `JSONEachRow` 本来就忽略未知字段——旧表有多余列，新表没这几列也不报错。

除非有旧表依赖这些列做查询，否则可以考虑删掉。如果确实需要保留，至少给个明确的 sunset 计划。

### 🟡 3. `CatalogIndex::new` 同一个 key 插三次

```rust
// exchange/mod.rs:175
for instrument in &instruments {
    let instrument_ref = instrument.instrument_ref();
    refs_by_feed_key.insert(instrument.instrument_id.clone(), instrument_ref.clone());
    refs_by_feed_key.insert(instrument.raw_symbol.clone(), instrument_ref.clone());
    if let Some(feed_symbol) = &instrument.feed_symbol {
        refs_by_feed_key.insert(feed_symbol.clone(), instrument_ref);
    }
}
```

对 Hyperliquid（`instrument_id = "BTC"`, `raw_symbol = "BTC"`, `feed_symbol = Some("BTC")`），同一个 `InstrumentRef` 被 clone 3 次然后插 3 次 → 2 次无意义 clone + HashMap overwrite。每个 `InstrumentRef` 内部有 3 个 heap-allocated `String`，所以每次 clone = 3 次 alloc + 3 次 memcpy + 3 次 drop（被覆盖时）。

每个 adapter 最多 ~50 个 instrument，初始化时一次性的开销，对启动延迟几乎没有影响。但代码意图不清晰。

可以用 `entry().or_insert_with()` 或先去重 keys 再 insert：

```rust
for instrument in &instruments {
    let instrument_ref = instrument.instrument_ref();
    for key in catalog_keys(instrument) {
        refs_by_feed_key.entry(key).or_insert_with(|| instrument_ref.clone());
    }
}
```

不过 `catalog_keys` 已经实现了去重（`sort + dedup`），正好重用。

### 🟡 4. `ethereal/orderbook.rs` — `.expect()` 依赖控制流不变式

```rust
// ethereal/orderbook.rs:52
let current_ts_ms = book.timestamp_ms
    .expect("missing Ethereal book timestamp initializes as snapshot");
```

只有在前 7 行的 guard（`if update.is_snapshot || book.timestamp_ms.is_none()`）返回 `false` 时才到达这里。在这个上下文中 invariant 显然成立——5 行之前才刚刚检查过 `book.timestamp_ms.is_none()`。比上轮的 `zero_one/build_tick` 安全得多。

但还是建议用 `let Some(current_ts_ms) = book.timestamp_ms` 替代 `.expect()`，少一个潜在的 panic 路径。

### 🟡 5. `exchange/hyperliquid/adapter.rs` — `run_once` 内部 bootstrap 冗余 reconnect 场景

```rust
// hyperliquid/adapter.rs:63
async fn run_once(&self, tx, shutdown) -> Result<()> {
    let catalog = self.bootstrap_catalog().await?;  // 每次重连都拉 metadata
    for instrument in catalog.instruments() {
        tx.send(MarketEvent::Catalog { ... }).await?; // 每次重连都发 catalog
    }
    ...
}
```

WS 瞬断重连只需要 1 秒（`TRANSIENT_WS_RECONNECT_DELAY`），但 `bootstrap_catalog` 会打一个 HTTP 请求到 Hyperliquid REST API。每次重连都重新拉 metadata + 重新发送 catalog event。如果 exchange metadata 没变，这些都是浪费。

不过 Hyperliquid 的 metadata endpoint 轻量，且重连频率在 `STABLE_CONNECTION_RESET_AFTER = 30s` 之后才复位 backoff——实际上瞬断 1 秒重连后稳定连接不太可能再断。所以实际浪费可忽略。Low priority。

### 🟡 6. `config.toml` 缺少 ethereal venue

`config.example.toml` 有完整的 ethereal venue 配置（`venue_instance_id = "ethereal"`, L2Book, metadata fetch），但 `config.toml` 没有。如果用户想启用 ethereal，需要手动加。

这不是 bug——ethereal 默认 `enabled` 可能就是不需要的——但两个配置文件的 venue 数量不对称很容易让人困惑。

### 🟡 7. `ingest/ws.rs` 仍然是 7 行薄封装

未变。`tokio_tungstenite::connect_async` 的返回值类型签名确实丑，留着也行。但别假装它有独立存在价值。

---

## 代码质量提升项（非问题）

### ✅ `run_with_reconnect_backoff` 区分瞬断 vs 错误

```rust
// exchange/mod.rs
fn reconnect_decision(backoff, err, uptime) -> ReconnectDecision {
    let was_stable_connection = uptime >= STABLE_CONNECTION_RESET_AFTER;
    // 瞬断: 1s 重连 + cap 5s
    // 错误: 指数退避
}
```

设计很好。Lighter 用了自定义 backoff（15s → 300s），其他 adapter 用默认（1s → 30s）。

### ✅ `DataQuality::add_note` 拼接语义

```rust
pub fn add_note(&mut self, note: impl Into<String>) {
    self.note = match self.note.take() {
        Some(existing) => Some(format!("{existing}; {note}")),
        None => Some(note),
    };
}
```

同时解决了 note 覆盖问题 + stale 检测可以叠加标记。

### ✅ `CatalogSource` 枚举（Config / Exchange）

```rust
pub enum CatalogSource { Config, Exchange }
```

允许用户选择是用静态配置还是从 exchange REST API 动态拉 metadata。Hyperliquid 和 Lighter 支持动态拉取，RiseX/ZeroOne 用纯 config。

### ✅ `merge_configured_catalog` — 静态配置定义订阅集，exchange metadata 补充规则

```rust
pub fn merge_configured_catalog(configured, fetched) -> Vec<InstrumentCatalog> {
    // configured 的 instruments 保持顺序，用 fetched 的 price_tick/size_tick 补充
}
```

很好的设计：用户控制订阅什么，exchange metadata 补充精度规则，两者不冲突。

---

## 总结

| 严重程度 | 数量 | 关键项 |
|---------|------|--------|
| 高 | 1 | Lighter catalog 不随重连刷新 |
| 低 | 6 | 向后兼容字段、HashMap 重复插入、`.expect()` 风格等 |

整体质量相比三天前提升了非常多。14/18 的问题被修掉，新增的 `run_with_reconnect` 共享函数消除了适配器之间的重复代码，`CatalogSource` 和 `merge_configured_catalog` 是干净的设计。只剩 Lighter 那个重连不刷新 catalog 是实际会导致问题的 bug。
