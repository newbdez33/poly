use crate::trader::price::MidwindowPriceFetcher;
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::time::Duration;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExitConfig {
    pub tp_price: Decimal,
    pub sl_price: Decimal,
    pub poll: Duration,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ExitKind { Tp, Sl }

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExitTrigger {
    pub kind: ExitKind,
    pub bid: Decimal,
}

pub struct ExitWatcher {
    fetcher: Arc<dyn MidwindowPriceFetcher>,
    cfg: ExitConfig,
}

impl ExitWatcher {
    pub fn new(fetcher: Arc<dyn MidwindowPriceFetcher>, cfg: ExitConfig) -> Self {
        Self { fetcher, cfg }
    }

    /// Polls until trigger fires OR `deadline` reached.
    /// Returns `Some(trigger)` on TP/SL hit, `None` on deadline.
    pub async fn watch(
        &self,
        token_id: &str,
        deadline: tokio::time::Instant,
    ) -> Option<ExitTrigger> {
        loop {
            let now = tokio::time::Instant::now();
            if now >= deadline {
                return None;
            }
            let sleep_until = (now + self.cfg.poll).min(deadline);
            tokio::time::sleep_until(sleep_until).await;
            if tokio::time::Instant::now() >= deadline {
                return None;
            }
            match self.fetcher.current_bid(token_id).await {
                Err(e) => {
                    tracing::warn!("exit-watcher price fetch failed: {e}; skipping tick");
                    continue;
                }
                Ok(bid) => {
                    if bid >= self.cfg.tp_price {
                        return Some(ExitTrigger { kind: ExitKind::Tp, bid });
                    }
                    if bid <= self.cfg.sl_price {
                        return Some(ExitTrigger { kind: ExitKind::Sl, bid });
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::trader::errors::PriceError;
    use crate::trader::price::tests::StubPriceFetcher;
    use rust_decimal_macros::dec;
    use std::sync::Arc;

    fn cfg() -> ExitConfig {
        ExitConfig {
            tp_price: dec!(0.85),
            sl_price: dec!(0.45),
            poll: Duration::from_millis(100),
        }
    }

    #[tokio::test(start_paused = true)]
    async fn tp_triggers_on_first_crossing() {
        let f = Arc::new(StubPriceFetcher::new(vec![
            Ok(dec!(0.50)),
            Ok(dec!(0.70)),
            Ok(dec!(0.85)),
        ]));
        let w = ExitWatcher::new(f, cfg());
        let deadline = tokio::time::Instant::now() + Duration::from_secs(60);
        let t = w.watch("tok", deadline).await.expect("trigger");
        assert_eq!(t.kind, ExitKind::Tp);
        assert_eq!(t.bid, dec!(0.85));
    }

    #[tokio::test(start_paused = true)]
    async fn sl_triggers_on_first_crossing() {
        let f = Arc::new(StubPriceFetcher::new(vec![
            Ok(dec!(0.50)),
            Ok(dec!(0.45)),  // exactly at threshold counts
        ]));
        let w = ExitWatcher::new(f, cfg());
        let deadline = tokio::time::Instant::now() + Duration::from_secs(60);
        let t = w.watch("tok", deadline).await.expect("trigger");
        assert_eq!(t.kind, ExitKind::Sl);
        assert_eq!(t.bid, dec!(0.45));
    }

    #[tokio::test(start_paused = true)]
    async fn deadline_returns_none_when_no_trigger() {
        // Always 0.50 → never crosses tp=0.85 or sl=0.45
        let mut responses = vec![];
        for _ in 0..1000 { responses.push(Ok(dec!(0.50))); }
        let f = Arc::new(StubPriceFetcher::new(responses));
        let w = ExitWatcher::new(f, cfg());
        let deadline = tokio::time::Instant::now() + Duration::from_millis(350);
        let outcome = w.watch("tok", deadline).await;
        assert!(outcome.is_none());
    }

    #[tokio::test(start_paused = true)]
    async fn fetcher_error_is_skipped_and_polling_continues() {
        let f = Arc::new(StubPriceFetcher::new(vec![
            Err(PriceError::Network("502".into())),
            Err(PriceError::Decode("bad json".into())),
            Ok(dec!(0.85)),
        ]));
        let w = ExitWatcher::new(f, cfg());
        let deadline = tokio::time::Instant::now() + Duration::from_secs(60);
        let t = w.watch("tok", deadline).await.expect("trigger");
        assert_eq!(t.kind, ExitKind::Tp);
    }

    #[tokio::test(start_paused = true)]
    async fn tp_wins_when_both_crossed_simultaneously() {
        // bid=0.90 — above tp (0.85) AND would also be above sl (0.45). Tp checked first.
        // Construct a config where both would trigger to make the precedence explicit.
        let cfg_overlap = ExitConfig {
            tp_price: dec!(0.50),
            sl_price: dec!(0.95),  // intentionally inverted to force overlap
            poll: Duration::from_millis(100),
        };
        let f = Arc::new(StubPriceFetcher::new(vec![Ok(dec!(0.80))]));
        let w = ExitWatcher::new(f, cfg_overlap);
        let deadline = tokio::time::Instant::now() + Duration::from_secs(60);
        let t = w.watch("tok", deadline).await.expect("trigger");
        assert_eq!(t.kind, ExitKind::Tp,
                   "tp branch must be checked before sl branch");
    }

    #[tokio::test(start_paused = true)]
    async fn deadline_in_the_past_returns_none_immediately() {
        let f = Arc::new(StubPriceFetcher::new(vec![Ok(dec!(0.50))]));
        let w = ExitWatcher::new(f, cfg());
        let deadline = tokio::time::Instant::now() - Duration::from_secs(1);
        let outcome = w.watch("tok", deadline).await;
        assert!(outcome.is_none());
    }

    #[test]
    fn exit_kind_serializes_distinctly() {
        let tp = serde_json::to_string(&ExitKind::Tp).unwrap();
        let sl = serde_json::to_string(&ExitKind::Sl).unwrap();
        assert_ne!(tp, sl);
    }
}
