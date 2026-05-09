use thiserror::Error;

#[derive(Error, Debug)]
pub enum StateError {
    #[error("redis op failed: {0}")]
    Op(String),
    #[error("state value malformed: {0}")]
    Decode(String),
    #[error("lock contention: another instance owns the lock")]
    LockContention,
    #[error("lock lost during refresh")]
    LockLost,
}

#[derive(Error, Debug)]
pub enum EmitError {
    #[error("redis stream write failed: {0}")]
    Write(String),
    #[error("event encode failed: {0}")]
    Encode(String),
}

#[derive(Error, Debug)]
pub enum StreamError {
    #[error("redis stream read failed: {0}")]
    Read(String),
    #[error("stream entry decode failed: {0}")]
    Decode(String),
}

#[derive(Error, Debug)]
pub enum MarketError {
    #[error("market not found for window {window_ts}")]
    NotFound { window_ts: i64 },
    #[error("gamma-api unavailable: {0}")]
    Network(String),
    #[error("response decode failed: {0}")]
    Decode(String),
}

#[derive(Error, Debug)]
pub enum ExecError {
    #[error("fill-or-kill rejected (no liquidity or partial fill)")]
    FillOrKillFailed,
    #[error("CLOB request failed: {0}")]
    Network(String),
    #[error("response decode failed: {0}")]
    Decode(String),
    #[error("insufficient USDC")]
    InsufficientFunds,
}

#[derive(Error, Debug)]
pub enum ResolveError {
    #[error("polling timed out after {seconds}s")]
    Timeout { seconds: u64 },
    #[error("market discovery failed during polling: {0}")]
    Market(#[from] MarketError),
}
