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
        envy::from_env::<Config>()
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
