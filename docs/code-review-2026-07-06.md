# Code Review — 2026-07-06 (第二轮)

> 审阅范围：全项目 39 个源文件（含 28 个 git diff 变更文件）
> 原则：/ponytail — 删比加好，标库优先，最懒方案赢

---

## 🔴 阻断级

### 1. `config.toml` 格式不兼容

`config.toml` 用的是旧格式，新版 `VenueConfig` 已删除 `name`/`markets` 字段，改为 `adapter`/`venue_instance_id`/`instruments`。

当前 config.toml 中有 5 处 `name =`，0 处 `adapter =`。TOML 反序列化时 `name` 被忽略，`adapter` 默认为 `""`，导致 `build_adapter("")` 匹配失败，四个 venue 全部不识别。程序启动后输出 `"no venues enabled; waiting for Ctrl-C"` 然后空转。

**修复**：将 `config.toml` 对齐 `config.example.toml` 的新格式。

---

## 🔴 逻辑错误

### 2. `pipeline/normalizer.rs` — note 字段被无条件覆盖

```rust
// normalizer.rs:15
if spread.value() < 0 {
    tick.quality.inconsistent = true;
    tick.quality.note = Some("negative spread".to_string());  // ← 无条件覆盖
}
```

RiseX adapter 在 `risex/orderbook.rs:80` 已经设置了 checksum mismatch note：

```rust
if let Some(note) = checksum_note {
    tick.quality.note = Some(note);  // 例如 "RiseX checksum could not be verified..."
}
```

如果同一个 tick 既有 checksum 不匹配又是负 spread，checksum note 被静默覆盖。虽然概率低，但丢数据就是丢数据。

**修复**：

```rust
tick.quality.note = match tick.quality.note.take() {
    Some(existing) => Some(format!("{existing}; negative spread")),
    None => Some("negative spread".to_string()),
};
```

### 3. `main.rs` — crypto provider 安装失败静默忽略

```rust
fn install_crypto_provider() {
    let _ = rustls::crypto::ring::default_provider().install_default();
}
```

如果 ring 安装失败，后续 TLS 连接会 panic 或给迷惑错误。应该在启动阶段直接炸：

```rust
fn install_crypto_provider() {
    rustls::crypto::ring::default_provider()
        .install_default()
        .expect("install rustls ring crypto provider");
}
```

### 4. `exchange/zero_one/orderbook.rs` — `build_tick` 隐性 panic

```rust
fn build_tick(recv_ts_ns: i128, sequence: Option<i128>, book: &MarketBook) -> BboTick {
    BboTick::new(
        book.instrument.clone().expect("snapshot stores instrument before tick"),
        // ...
    )
}
```

目前安全（`apply_snapshot` 和 `apply_delta` 都确保先设 `book.instrument`），但靠注释保证的 invariant 是 timer bomb。参考 `risex/orderbook.rs` 的做法：

```rust
let Some(instrument) = book.instrument.clone() else {
    return ApplyResult::... // or return None
};
```

---

## 🟡 死代码 / 可删除

### 5. `ingest/heartbeat.rs` — 整个文件没人用

`HeartbeatPolicy` 只有两个方法（`hyperliquid()`、`lighter()`），缺少 risex 和 zero_one，且全项目零引用。四个 adapter 各自在 `run_once` 里硬编码了 interval。直接删文件。

### 6. `domain/market.rs` — `Venue` 枚举无引用

```rust
pub enum Venue { Hyperliquid, Lighter, Risex, ZeroOne }
```

`BboTick` 已从 `venue: Venue` 改为 `instrument: InstrumentRef`。`Venue` 只在定义文件和 `mod.rs` 的 re-export 中出现。`as_str()` 方法也死。

删掉 `Venue` 枚举（约 20 行）并移除 `domain/mod.rs` 中的 re-export。

### 7. `config/model.rs` — `PipelineConfig` / `stale_after_ms` 全项目无引用

```rust
pub struct PipelineConfig {
    pub channel_capacity: usize,  // 用了
    pub stale_after_ms: i64,      // 没用
}
```

`channel_capacity` 在 `runner.rs` 读了，`stale_after_ms` 搜索整个代码库只在 `config/model.rs` 自身出现。要么删掉，要么配合同样未实现的 `DataQuality.stale` 字段加上 stale 检测逻辑。

### 8. `domain/quality.rs` — `stale` / `gap` 字段从未被设为 `true`

```rust
pub struct DataQuality {
    pub gap: bool,           // 永远 false
    pub stale: bool,         // 永远 false
    pub inconsistent: bool,  // normalizer 设
    pub note: Option<String>, // risex checksum 设
}
```

全项目搜索 `quality.gap` / `quality.stale`，只有序列化和 `ClickHouseRow` 转换——无一处在运行时设置为 `true`。

### 9. `ingest/ws.rs` — 7 行薄封装

```rust
pub async fn connect(url: &str) -> anyhow::Result<(WebSocketStream<MaybeTlsStream<TcpStream>>, Response)> {
    Ok(connect_async(url).await?)
}
```

只是把 `tokio_tungstenite::connect_async` 改了个名。4 个 adapter 里各用一次。ponytail 说要么 inline 要么留着——留着也行，但别假装它有独立价值。

---

## 🟡 防御性/浪费

### 10. 四个 adapter 的 `run()` 重连循环完全一样

Hyperliquid、Lighter、RiseX、ZeroOne 的 `run()` 方法逐字复制了这段：

```rust
let mut backoff = Backoff::default();
while !*shutdown.borrow() {
    match self.run_once(tx.clone(), shutdown.clone()).await {
        Ok(()) => return Ok(()),
        Err(err) => {
            let sleep = backoff.next_delay();
            warn!(...);
            time::sleep(sleep).await;
        }
    }
}
Ok(())
```

抽成 `ExchangeAdapter` trait 的一个默认方法或者独立函数。

### 11. `CatalogClickHouseRow::from_catalog` — 无意义 serde 往返

```rust
product_type: serde_json::to_value(catalog.product_type)?
    .as_str()
    .unwrap_or("unknown")
    .to_string(),
```

`ProductType` 已经有 `as_str()` → `"perp"`。走 JSON 序列化再反序列化浪费且脆弱。同理 `price_convention` 和 `size_unit`。

```rust
// 应该：
product_type: catalog.product_type.as_str().to_string(),
```

### 12. `QuoteRateBook::rate` — 每次 same-currency 都 parse

```rust
pub fn rate(&self, from: &str, to: &str) -> Option<Fixed> {
    if from == to {
        return "1".parse().ok();  // TUI 每 250ms 可能调多次
    }
}
```

```rust
// 应该：
const ONE: Fixed = Fixed::new(1, 0);
if from == to { return Some(ONE); }
```

### 13. `storage/clickhouse.rs` — `batch_size.max(1)` 静默改写 0

```rust
batch_size: config.batch_size.max(1),
buffer: Mutex::new(Vec::with_capacity(config.batch_size.max(1))),
```

用户设 `batch_size = 0` 可能是想禁用批处理，但代码悄无声息改成 1。至少打个 warning。

### 14. `telemetry/mod.rs` — `try_init` 错误被吞

```rust
let _ = fmt().with_env_filter(filter).try_init();
```

除了 "already initialized" 之外的其他失败完全不可见。

### 15. `exchange/risex/adapter.rs` — 空 market_ids 分支不可达

```rust
let payload = if market_ids.is_empty() {
    json!({"method": "subscribe", "params": {"channel": "orderbook"}})
} else {
    json!({"method": "subscribe", "params": { "channel": "orderbook", "market_ids": market_ids }})
};
```

如果 instruments 为空，adapter 根本不会 spawn → `if` 的 true 分支永远不执行。删掉这个分支。

### 16. `exchange/lighter/parser.rs` — `micros_to_millis` 魔法阈值

```rust
fn micros_to_millis(value: i64) -> i64 {
    if value > 10_000_000_000_000 { value / 1_000 } else { value }
}
```

目前阈值在微秒 (~1.8×10^15) 和毫秒 (~1.8×10^12) 之间，正确。但 ~2286 年毫秒时间戳将超过此阈值，heuristic 反转。

加注释标记天花板：`// ponytail: flips when epoch ms > 10^13 (~2286 CE), upgrade to explicit flag`

### 17. `storage/clickhouse.rs` — `catalog_table` 用 `ReplacingMergeTree(inserted_time)` 但无 `FINAL`

```sql
ENGINE = ReplacingMergeTree(inserted_time)
ORDER BY (venue_instance_id, instrument_id, catalog_id)
```

`ReplacingMergeTree` 在 merge 时去重，但不保证查询时已去重。如果查询 catalog 表需要最新记录，要加 `FINAL` 修饰符或等待后台 merge。不过这属于数据工程层面的注意事项，当前代码只写不读，暂时不影响。

---

## 🔴 安全隐患

### 18. `config.toml` — 硬编码明文 ClickHouse 密码

```
password = "<redacted>"
```

`.gitignore` 虽然排除了 `config.toml`，但本地明文存储仍是风险。`config.example.toml` 已经示范了正确的 `password_env = "CLICKHOUSE_PASSWORD"` 用法。

---

## 总结

| 严重程度 | 数量 | 关键项 |
|---------|------|--------|
| 阻断    | 1    | config.toml 格式不兼容 |
| 高      | 3    | note 覆盖、crypto 静默、build_tick panic |
| 中      | 4    | 死代码（heartbeat.rs、Venue 枚举） |
| 低      | 10   | 效率浪费、防御性编程、注释缺失 |
