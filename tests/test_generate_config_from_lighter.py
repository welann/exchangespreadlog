import importlib.util
import sys
import unittest
from decimal import Decimal
from pathlib import Path


SCRIPT_PATH = (
    Path(__file__).resolve().parents[1] / "scripts" / "generate_config_from_lighter.py"
)
SPEC = importlib.util.spec_from_file_location("generate_config_from_lighter", SCRIPT_PATH)
generator = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = generator
SPEC.loader.exec_module(generator)


class GenerateConfigFromLighterTest(unittest.TestCase):
    def test_normalize_base_handles_common_exchange_symbols(self):
        cases = {
            "BTC/USDC": "BTC",
            "ETH-USD": "ETH",
            "SOLUSD": "SOL",
            "HYPE": "HYPE",
            "xyz:SPCX": "SPCX",
            "1000SHIB": "1000SHIB",
        }
        for raw, expected in cases.items():
            with self.subTest(raw=raw):
                self.assertEqual(generator.normalize_base(raw), expected)

    def test_fetch_top_lighter_markets_sorts_active_perps_by_quote_volume(self):
        original = generator.fetch_lighter_details
        generator.fetch_lighter_details = lambda: [
            {
                "symbol": "ETH",
                "market_id": 0,
                "market_type": "perp",
                "status": "active",
                "daily_quote_token_volume": "50",
            },
            {
                "symbol": "BTC",
                "market_id": 1,
                "market_type": "perp",
                "status": "active",
                "daily_quote_token_volume": "100",
            },
            {
                "symbol": "BTC",
                "market_id": 10,
                "market_type": "perp",
                "status": "active",
                "daily_quote_token_volume": "90",
            },
            {
                "symbol": "SOL/USDC",
                "market_id": 2,
                "market_type": "spot",
                "status": "active",
                "daily_quote_token_volume": "1000",
            },
            {
                "symbol": "DOGE",
                "market_id": 3,
                "market_type": "perp",
                "status": "inactive",
                "daily_quote_token_volume": "900",
            },
        ]
        try:
            top = generator.fetch_top_lighter_markets(2)
        finally:
            generator.fetch_lighter_details = original

        self.assertEqual([market.base_asset for market in top], ["BTC", "ETH"])
        self.assertEqual(top[0].market_id, "1")
        self.assertEqual(top[0].volume, Decimal("100"))

    def test_fetch_hyperliquid_instruments_includes_hip3_fallback(self):
        original = generator.post_json
        generator.post_json = lambda _url, payload: [
            {
                "universe": [
                    {"name": "BTC", "szDecimals": 5},
                    {"name": "OLD", "isDelisted": True},
                ]
            },
            {
                "universe": [
                    {"name": "xyz:SPCX", "szDecimals": 3},
                    {"name": "hyna:BTC", "szDecimals": 5},
                ]
            },
        ]
        try:
            instruments = generator.fetch_hyperliquid_instruments(
                [
                    generator.LighterMarket("1", "BTC", "BTC", Decimal("100")),
                    generator.LighterMarket("194", "SPCX", "SPCX", Decimal("50")),
                ]
            )
        finally:
            generator.post_json = original

        self.assertEqual([item.instrument_id for item in instruments], ["BTC", "xyz:SPCX"])
        self.assertEqual([item.feed_symbol for item in instruments], ["BTC", "xyz:SPCX"])
        self.assertEqual([item.base_asset for item in instruments], ["BTC", "SPCX"])

    def test_render_config_writes_expected_venue_and_instrument_fields(self):
        top_markets = [
            generator.LighterMarket("1", "BTC", "BTC", Decimal("100")),
        ]
        venues = [
            generator.Venue(
                venue_instance_id="lighter",
                adapter="lighter",
                url="wss://mainnet.zklighter.elliot.ai/stream?readonly=true",
                channel="ticker",
                catalog_source="exchange",
                metadata_url=generator.LIGHTER_ORDERBOOKS_URL,
                default_quote_asset="USDC",
                default_settle_asset="USDC",
                default_margin_asset="USDC",
                instruments=[generator.instrument("1", "BTC", "1", "BTC")],
            )
        ]

        rendered = generator.render_config(top_markets, venues)

        self.assertIn('venue_instance_id = "lighter"', rendered)
        self.assertIn('metadata_url = "https://mainnet.zklighter.elliot.ai/api/v1/orderBooks"', rendered)
        self.assertIn('instrument_id = "1"', rendered)
        self.assertIn('feed_symbol = "1"', rendered)
        self.assertIn("# BTC:1:100", rendered)

    def test_fetch_ethereal_instruments_accepts_data_payload(self):
        original = generator.get_json
        generator.get_json = lambda _url: {
            "data": [
                {
                    "ticker": "BTCUSD",
                    "displayTicker": "BTC-USD",
                    "baseTokenName": "BTC",
                    "status": "ACTIVE",
                },
                {
                    "ticker": "ETHUSD",
                    "displayTicker": "ETH-USD",
                    "baseTokenName": "ETH",
                    "status": "DISABLED",
                },
            ]
        }
        try:
            instruments = generator.fetch_ethereal_instruments(
                [generator.LighterMarket("1", "BTC", "BTC", Decimal("100"))]
            )
        finally:
            generator.get_json = original

        self.assertEqual(len(instruments), 1)
        self.assertEqual(instruments[0].instrument_id, "BTCUSD")
        self.assertEqual(instruments[0].feed_symbol, "BTCUSD")

    def test_fetch_perpl_instruments_uses_context_market_ids_and_ticks(self):
        original = generator.get_json
        generator.get_json = lambda _url: {
            "markets": [
                {
                    "id": 1,
                    "symbol": "",
                    "name": "BTC",
                    "size_units": "BTC",
                    "config": {
                        "is_open": True,
                        "price_decimals": 1,
                        "size_decimals": 5,
                    },
                },
                {
                    "id": 20,
                    "symbol": "ETH",
                    "name": "ETH",
                    "size_units": "ETH",
                    "config": {
                        "is_open": False,
                        "price_decimals": 2,
                        "size_decimals": 3,
                    },
                },
            ]
        }
        try:
            instruments = generator.fetch_perpl_instruments(
                [
                    generator.LighterMarket("1", "BTC", "BTC", Decimal("100")),
                    generator.LighterMarket("0", "ETH", "ETH", Decimal("50")),
                ]
            )
        finally:
            generator.get_json = original

        self.assertEqual(len(instruments), 1)
        self.assertEqual(instruments[0].instrument_id, "1")
        self.assertEqual(instruments[0].feed_symbol, "1")
        self.assertEqual(instruments[0].base_asset, "BTC")
        self.assertEqual(instruments[0].price_tick, "0.1")
        self.assertEqual(instruments[0].size_tick, "0.00001")


if __name__ == "__main__":
    unittest.main()
