use crate::trader::errors::{EmitError, StreamError};
use crate::trader::event::{TraderEvent, TraderEventEmitter};
use crate::tui::events::{TraderEventStream, TraderEventTail, STREAM_KEY, STREAM_MAXLEN};
use async_trait::async_trait;
use fred::interfaces::{ClientLike, StreamsInterface};
use fred::prelude::{RedisClient, RedisConfig, RedisError};
use fred::types::{XCapKind, XCapTrim};

pub struct RedisTraderStream {
    client: RedisClient,
}

impl RedisTraderStream {
    pub async fn connect(url: &str) -> Result<Self, EmitError> {
        let config = RedisConfig::from_url(url)
            .map_err(|e| EmitError::Write(format!("bad redis url: {e}")))?;
        let client = RedisClient::new(config, None, None, None);
        client
            .init()
            .await
            .map_err(|e| EmitError::Write(format!("redis init: {e}")))?;
        Ok(Self { client })
    }
}

#[async_trait]
impl TraderEventEmitter for RedisTraderStream {
    async fn emit(&self, event: &TraderEvent) -> Result<(), EmitError> {
        let json = serde_json::to_string(event)
            .map_err(|e| EmitError::Encode(e.to_string()))?;
        // XADD STREAM_KEY MAXLEN ~ 1000 * payload <json>
        // fred 9.x xadd(key, nomkstream, cap, id, fields)
        // cap: (XCapKind, XCapTrim, threshold: i64, limit: Option<i64>)
        let cap = (XCapKind::MaxLen, XCapTrim::AlmostExact, STREAM_MAXLEN as i64, None::<i64>);
        let _: String = self
            .client
            .xadd(STREAM_KEY, false, cap, "*", vec![("payload", json)])
            .await
            .map_err(map_emit)?;
        Ok(())
    }
}

#[async_trait]
impl TraderEventStream for RedisTraderStream {
    async fn tail(&self, n: usize) -> Result<TraderEventTail, StreamError> {
        // XREVRANGE returns most-recent first; reverse to chronological order.
        // Signature: xrevrange(key, end, start, count: Option<u64>) -> R: FromRedis
        // We decode as Vec<(String, Vec<(String, String)>)> then parse manually.
        use fred::types::XReadResponse;

        // Use xrevrange_values for a strongly-typed decode:
        // xrevrange_values<Ri, Rk, Rv, K, E, S>(key, end, start, count)
        // -> Result<Vec<XReadValue<Ri, Rk, Rv>>> where XReadValue<I,K,V> = (I, HashMap<K, V>)
        let entries: Vec<fred::types::XReadValue<String, String, String>> = self
            .client
            .xrevrange_values(STREAM_KEY, "+", "-", Some(n as u64))
            .await
            .map_err(map_stream)?;

        let history: Vec<TraderEvent> = entries
            .iter()
            .rev()
            .filter_map(|(_id, fields)| fields.get("payload"))
            .filter_map(|p| serde_json::from_str::<TraderEvent>(p).ok())
            .collect();

        // Use the most-recent ID as the cursor for live polling.
        let last_id: String = entries
            .first()
            .map(|(id, _)| id.clone())
            .unwrap_or_else(|| "0-0".to_string());

        // Live subscription via repeated XREAD BLOCK.
        let client = self.client.clone();
        let live = async_stream::stream! {
            let mut cursor = last_id;
            loop {
                // xread_map(count, block_ms, keys, ids)
                // returns XReadResponse<K1, I, K2, V> = HashMap<K1, Vec<(I, HashMap<K2, V>)>>
                let resp: Result<XReadResponse<String, String, String, String>, RedisError> =
                    client
                        .xread_map(Some(10u64), Some(250u64), STREAM_KEY, cursor.as_str())
                        .await;
                match resp {
                    Ok(map) => {
                        for (_stream_key, records) in map {
                            for (id, fields) in records {
                                if let Some(payload) = fields.get("payload") {
                                    if let Ok(ev) = serde_json::from_str::<TraderEvent>(payload) {
                                        yield ev;
                                    }
                                }
                                cursor = id;
                            }
                        }
                    }
                    Err(_) => {
                        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                    }
                }
            }
        };

        Ok(TraderEventTail {
            history,
            live: Box::pin(live),
        })
    }
}

fn map_emit(e: RedisError) -> EmitError {
    EmitError::Write(e.to_string())
}

fn map_stream(e: RedisError) -> StreamError {
    StreamError::Read(e.to_string())
}
