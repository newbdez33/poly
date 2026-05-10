use serde::Deserialize;

#[derive(Clone, Debug, Deserialize)]
pub struct Config {
    pub polymarket_private_key: String,
    #[serde(default = "default_redis_url")]
    pub redis_url: String,
    #[serde(default = "default_refresh_interval")]
    pub refresh_interval_secs: u64,
    #[serde(default = "default_clob_host")]
    pub clob_host: String,
    #[serde(default = "default_log_level")]
    pub log_level: String,
    #[serde(default = "default_polygon_rpc_url")]
    pub polygon_rpc_url: String,
}

fn default_redis_url() -> String { "redis://127.0.0.1:6379".to_string() }
fn default_refresh_interval() -> u64 { 30 }
// Polymarket retired the `clob-v2` hostname; it now 301-redirects to `clob`,
// and reqwest downgrades POST→GET on 301 follow → /order returns 405. Point at
// the canonical host directly so signed orders reach the CLOB intact.
fn default_clob_host() -> String { "https://clob.polymarket.com".to_string() }
fn default_log_level() -> String { "info".to_string() }
fn default_polygon_rpc_url() -> String { "https://polygon-rpc.com".to_string() }

impl Config {
    /// Load from process environment (caller is expected to have run `dotenvy::dotenv()` first).
    pub fn from_env() -> Result<Self, envy::Error> {
        let cfg: Config = envy::from_env()?;
        cfg.validate_clob_host()?;
        Ok(cfg)
    }

    /// Hard-fail at startup if `CLOB_HOST` points at the retired `clob-v2` host.
    /// reqwest downgrades POST→GET on 301 follow → /order returns 405 silently.
    /// We've been bitten once; better to refuse to start than surface as
    /// "every order rejected with no real explanation".
    fn validate_clob_host(&self) -> Result<(), envy::Error> {
        if self.clob_host.contains("clob-v2.polymarket.com") {
            return Err(envy::Error::Custom(format!(
                "CLOB_HOST points at retired host '{}' — Polymarket 301-redirects v2 \
                 to clob.polymarket.com and POST→GET downgrade returns HTTP 405. \
                 Update CLOB_HOST to https://clob.polymarket.com",
                self.clob_host
            )));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn with_env<F: FnOnce()>(vars: &[(&str, &str)], f: F) {
        let saved: Vec<(String, Option<String>)> = vars.iter()
            .map(|(k, _)| (k.to_string(), std::env::var(k).ok()))
            .collect();
        // SAFETY: tests run with --test-threads=1 so no concurrent env mutation.
        // std::env::set_var is deprecated outside unsafe in Rust 1.86+ due to
        // unsoundness in multi-threaded contexts; the single-thread constraint
        // makes this safe here.
        for (k, v) in vars {
            unsafe { std::env::set_var(k, v) };
        }
        f();
        for (k, v) in saved {
            match v {
                Some(val) => unsafe { std::env::set_var(&k, val) },
                None => unsafe { std::env::remove_var(&k) },
            }
        }
    }

    #[test]
    fn validate_clob_host_rejects_retired_v2() {
        // Test the validator directly to avoid env-var leakage with parallel tests.
        let cfg = Config {
            polymarket_private_key: "0xabc".into(),
            redis_url: "redis://x".into(),
            refresh_interval_secs: 30,
            clob_host: "https://clob-v2.polymarket.com".into(),
            log_level: "info".into(),
            polygon_rpc_url: "https://polygon-rpc.com".into(),
        };
        let err = cfg.validate_clob_host().unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("clob-v2"), "actual: {msg}");
        assert!(msg.contains("clob.polymarket.com"), "actual: {msg}");
    }

    #[test]
    fn validate_clob_host_accepts_canonical() {
        let cfg = Config {
            polymarket_private_key: "0xabc".into(),
            redis_url: "redis://x".into(),
            refresh_interval_secs: 30,
            clob_host: "https://clob.polymarket.com".into(),
            log_level: "info".into(),
            polygon_rpc_url: "https://polygon-rpc.com".into(),
        };
        assert!(cfg.validate_clob_host().is_ok());
    }

    #[test]
    fn loads_required_with_defaults() {
        with_env(&[("POLYMARKET_PRIVATE_KEY", "0xabc")], || {
            let cfg = Config::from_env().unwrap();
            assert_eq!(cfg.polymarket_private_key, "0xabc");
            assert_eq!(cfg.redis_url, "redis://127.0.0.1:6379");
            assert_eq!(cfg.refresh_interval_secs, 30);
            assert_eq!(cfg.clob_host, "https://clob.polymarket.com");
            assert_eq!(cfg.log_level, "info");
            assert_eq!(cfg.polygon_rpc_url, "https://polygon-rpc.com");
        });
    }

    #[test]
    fn missing_private_key_errors() {
        unsafe { std::env::remove_var("POLYMARKET_PRIVATE_KEY") };
        assert!(Config::from_env().is_err());
    }
}
