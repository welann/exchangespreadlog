use anyhow::{Result, anyhow, bail};
use serde::Deserialize;
use serde_json::Value;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParsedMessage {
    SubscriptionResponse(Vec<SubscriptionAck>),
    L2Book(PerplBookUpdate),
    Ignore,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubscriptionAck {
    pub stream: String,
    pub sid: u64,
    pub error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PerplBookUpdate {
    pub sid: u64,
    pub exchange_ts_ms: Option<i64>,
    pub sequence: Option<i128>,
    pub is_snapshot: bool,
    pub bids: Vec<PerplLevelDelta>,
    pub asks: Vec<PerplLevelDelta>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PerplLevelDelta {
    pub price: i128,
    pub size: i128,
    pub order_count: Option<u32>,
}

#[derive(Debug, Deserialize)]
struct Envelope {
    mt: u32,
    #[serde(default)]
    sid: Option<u64>,
    #[serde(default)]
    sn: Option<i128>,
    #[serde(default)]
    at: Option<BlockTimestamp>,
    #[serde(default)]
    bid: Vec<WireLevel>,
    #[serde(default)]
    ask: Vec<WireLevel>,
    #[serde(default)]
    subs: Vec<WireSubscriptionAck>,
}

#[derive(Debug, Deserialize)]
struct BlockTimestamp {
    #[serde(default)]
    t: Option<i64>,
}

#[derive(Debug, Deserialize)]
struct WireLevel {
    p: Value,
    s: Value,
    #[serde(default)]
    o: Option<u32>,
}

#[derive(Debug, Deserialize)]
struct WireSubscriptionAck {
    stream: String,
    #[serde(default)]
    sid: Option<u64>,
    #[serde(default)]
    status: Option<WireSubscriptionStatus>,
}

#[derive(Debug, Deserialize)]
struct WireSubscriptionStatus {
    code: i32,
    #[serde(default)]
    error: Option<String>,
}

pub fn parse_message(text: &str) -> Result<ParsedMessage> {
    let envelope: Envelope = serde_json::from_str(text)?;

    match envelope.mt {
        2 | 3 | 7 | 8 | 9 | 10 | 11 | 12 | 17 | 18 | 100 => Ok(ParsedMessage::Ignore),
        6 => parse_subscription_response(envelope).map(ParsedMessage::SubscriptionResponse),
        15 | 16 => parse_l2_book(envelope).map(ParsedMessage::L2Book),
        other => Err(anyhow!("unsupported Perpl websocket message type {other}")),
    }
}

fn parse_subscription_response(envelope: Envelope) -> Result<Vec<SubscriptionAck>> {
    envelope
        .subs
        .into_iter()
        .map(|ack| {
            let error =
                ack.status
                    .as_ref()
                    .filter(|status| status.code != 0)
                    .map(|status| {
                        status.error.clone().unwrap_or_else(|| {
                            format!("subscription failed with code {}", status.code)
                        })
                    });

            if error.is_none() && ack.sid.is_none() {
                bail!("Perpl subscription ack missing sid for {}", ack.stream);
            }

            Ok(SubscriptionAck {
                stream: ack.stream,
                sid: ack.sid.unwrap_or_default(),
                error,
            })
        })
        .collect()
}

fn parse_l2_book(envelope: Envelope) -> Result<PerplBookUpdate> {
    let sid = envelope
        .sid
        .ok_or_else(|| anyhow!("Perpl L2Book message missing sid"))?;

    Ok(PerplBookUpdate {
        sid,
        exchange_ts_ms: envelope.at.and_then(|at| at.t),
        sequence: envelope.sn,
        is_snapshot: envelope.mt == 15,
        bids: parse_levels(envelope.bid)?,
        asks: parse_levels(envelope.ask)?,
    })
}

fn parse_levels(levels: Vec<WireLevel>) -> Result<Vec<PerplLevelDelta>> {
    levels
        .into_iter()
        .map(|level| {
            Ok(PerplLevelDelta {
                price: value_to_i128(level.p)?,
                size: value_to_i128(level.s)?,
                order_count: level.o,
            })
        })
        .collect()
}

fn value_to_i128(value: Value) -> Result<i128> {
    match value {
        Value::Number(value) => value
            .as_i64()
            .map(i128::from)
            .or_else(|| value.as_u64().map(i128::from))
            .ok_or_else(|| anyhow!("expected integer number, got {value}")),
        Value::String(value) => value
            .parse::<i128>()
            .map_err(|err| anyhow!("expected integer string, got {value}: {err}")),
        other => Err(anyhow!("expected integer number or string, got {other}")),
    }
}

#[cfg(test)]
mod tests {
    use super::{ParsedMessage, parse_message};

    #[test]
    fn parses_subscription_response() {
        let raw = r#"{
            "mt": 6,
            "sn": 1,
            "subs": [
                {"stream":"order-book@1","sid":1000001,"status":{"code":0}}
            ]
        }"#;

        let ParsedMessage::SubscriptionResponse(acks) = parse_message(raw).unwrap() else {
            panic!("expected subscription response");
        };

        assert_eq!(acks.len(), 1);
        assert_eq!(acks[0].stream, "order-book@1");
        assert_eq!(acks[0].sid, 1_000_001);
        assert_eq!(acks[0].error, None);
    }

    #[test]
    fn parses_l2_book_snapshot() {
        let raw = r#"{
            "mt": 15,
            "sid": 1000001,
            "sn": 86647976,
            "at": {"b":86647976,"t":1783610131000},
            "bid": [{"p":630343,"s":44658,"o":3}],
            "ask": [{"p":630352,"s":760,"o":5}]
        }"#;

        let ParsedMessage::L2Book(update) = parse_message(raw).unwrap() else {
            panic!("expected L2Book");
        };

        assert!(update.is_snapshot);
        assert_eq!(update.sid, 1_000_001);
        assert_eq!(update.sequence, Some(86_647_976));
        assert_eq!(update.exchange_ts_ms, Some(1_783_610_131_000));
        assert_eq!(update.bids[0].price, 630_343);
        assert_eq!(update.bids[0].size, 44_658);
        assert_eq!(update.asks[0].order_count, Some(5));
    }

    #[test]
    fn ignores_heartbeat() {
        assert_eq!(
            parse_message(r#"{"mt":100,"sn":123}"#).unwrap(),
            ParsedMessage::Ignore
        );
    }
}
