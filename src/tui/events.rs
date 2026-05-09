use crate::trader::errors::StreamError;
use crate::trader::event::TraderEvent;
use async_trait::async_trait;
use futures::stream::Stream;
use std::pin::Pin;

pub struct TraderEventTail {
    pub history: Vec<TraderEvent>,
    pub live: Pin<Box<dyn Stream<Item = TraderEvent> + Send>>,
}

#[async_trait]
pub trait TraderEventStream: Send + Sync {
    async fn tail(&self, n: usize) -> Result<TraderEventTail, StreamError>;
}

pub const STREAM_KEY: &str = "poly:prod:trader:events";
pub const STREAM_MAXLEN: usize = 1000;

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn key_namespace_is_prod() {
        assert!(STREAM_KEY.starts_with("poly:prod:trader:"));
    }
}
