# Code Review — 2026-07-09 (夜间更新)

> 审阅范围：第三轮基础上的增量变更

---

## 变更概览

8 个文件改动，+422/-127 行：

| 文件 | 变更 |
|------|------|
| `src/config/model.rs` | +69 行 — 新增 `price_convention`/`size_unit`，perpl venue 默认配置，ethereal 加入默认列表 |
| `src/app/runner.rs` | +4 行 — 注册 perpl adapter |
| `src/exchange/mod.rs` | +1 行 — `pub mod perpl` |
| `src/exchange/perpl/*` | 新增 — Perpl exchange adapter（parser + orderbook + adapter） |
| `src/domain/market.rs` | `new_with_units()` — 支持 `PriceConvention`/`SizeUnit` 参数 |
| `scripts/generate_config_from_lighter.py` | 新增 400+ 行 — 配置自动生成器 |
| `tests/test_generate_config_from_lighter.py` | 新增 200+ 行 — 5 个单元测试 |
| `config.example.toml` | +47 行 — perpl venue + 新字段 |

---

## 上轮问题修复确认 ✅

| # | 问题 | 状态 |
|---|------|------|
| 3 | `CatalogIndex::new` 重复 insert | ✅ 改为 `entry().or_insert_with()` + `catalog_keys()` 去重 |

其他 5 个低优先级问题未变（向后兼容字段、`.expect()` 风格等），不影响功能。

---

## 本轮新发现

### 🟡 1. `InstrumentCatalog::new_with_units` 的 version_seed 缺少 `price_convention` 和 `size_unit`

```rust
// domain/market.rs:170
let version_seed = [
    raw_symbol.as_str(),
    feed_symbol.as_deref().unwrap_or(""),
    product_type.as_str(),
    base_asset.as_str(),
    quote_asset.as_str(),
    settle_asset.as_str(),
    margin_asset.as_str(),
    price_tick_seed.as_str(),
    size_tick_seed.as_str(),
    min_size_seed.as_str(),
    status.as_str(),
    // ← 缺少 price_convention.as_str() 和 size_unit.as_str()
].join("|");
```

`new_with_units` 接受了 `price_convention` 和 `size_unit` 参数但**没有纳入 catalog_id 的 version_seed**。这意味着：
- 同一个 instrument，`price_convention=QuotePerBase` 和 `price_convention=QuotePerBase`（相同值）→ catalog_id 相同 ✅
- 同一个 instrument，`QuotePerBase` 和 `Contracts` → catalog_id 仍然相同 ❌

如果将来有 venue 对同一 instrument 用不同的 convention，catalog_id 不会反映这个差异。目前所有 venue 都用 `QuotePerBase + BaseAsset`，所以不影响功能。但如果 Perpl 的某个市场改用 `Contracts`，就会出现 catalog_id 碰撞。

**修复**：在 `version_seed` 中追加 `price_convention.as_str()` 和 `size_unit.as_str()`。

### 🟡 2. `generate_config_from_lighter.py` 的 `Instrument` 不支持 `price_convention` / `size_unit`

Python dataclass：
```python
@dataclass(frozen=True)
class Instrument:
    instrument_id: str
    raw_symbol: str
    feed_symbol: str
    base_asset: str
    status: str = "active"
    price_tick: Optional[str] = None
    size_tick: Optional[str] = None
    # ← 没有 price_convention, size_unit
```

这意味着生成出来的 TOML 不会包含这两个字段。目前对大多数 venue 默认值（`QuotePerBase` / `BaseAsset`）是正确的，但如果 Perpl 有 `Contracts` 计价的品种，生成的 config 就会漏掉。和问题 1 一样，目前不影响功能，但为以后埋了坑。

### 🟡 3. Perpl adapter 的 `MarketScale` 结构体暴露但未从 `orderbook.rs` 导出

```rust
// perpl/orderbook.rs:21
pub struct MarketScale {
    pub price_decimals: u32,
    pub size_decimals: u32,
}
```

`pub` 但没有在 `mod.rs` 里 re-export，也没有被外部引用。目前只在 `adapter.rs` 里 `use super::orderbook::{..., MarketScale, ...}` 内部使用。如果不需要外部可见，改 `pub(crate)` 更精确。

### ✅ Perpl 实现质量不错

- `build_tick` 无 `.expect()`，Option 字段自然处理
- 使用 `run_with_reconnect` 共享重连逻辑
- snapshot 忽略前跳过 delta（`last_sequence.is_none()` guard）
- 无 gap 检测（Perpl API 保证有序），简化了实现

---

## 总结

| 严重程度 | 数量 | 关键项 |
|---------|------|--------|
| 中 | 1 | version_seed 缺少 price_convention/size_unit |
| 低 | 2 | Python 脚本缺字段、MarketScale 可见性 |

整体改动质量很好。`CatalogIndex` 的去重修复干净，perpl adapter 遵循了已有的模式，生成脚本的测试覆盖到位。
唯一需要立刻确认的是 `version_seed` 是否该包含那两个新字段——现在不加的话，以后加会改变已有 catalog_id（breaking change），不如趁早补上。
