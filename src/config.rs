use std::time::Duration;

#[derive(Debug, Clone)]
pub struct Config {
    pub bind_address: String,
    pub schedule_lead_time: Duration,
    /// Serve over HTTPS (self-signed, auto-generated) instead of plain HTTP.
    /// Needed for any client reaching the server over a LAN IP rather than
    /// localhost, since browsers only expose AudioWorklet (and other
    /// powerful APIs) in a secure context - localhost is exempted from that
    /// requirement, a LAN IP is not, regardless of network trust.
    pub tls_enabled: bool,
    /// Where the self-signed cert/key (and the SAN list they were generated
    /// for) are cached across restarts, so a dev cert doesn't retrigger a
    /// fresh "untrusted certificate" browser warning every time.
    pub tls_cert_dir: String,
}

const DEFAULT_BIND_ADDRESS: &str = "127.0.0.1:3000";
const DEFAULT_SCHEDULE_LEAD_TIME_MS: u64 = 150;
const DEFAULT_TLS_CERT_DIR: &str = "certs";

impl Config {
    pub fn from_env() -> Self {
        let bind_address =
            std::env::var("BIND_ADDRESS").unwrap_or_else(|_| DEFAULT_BIND_ADDRESS.to_string());

        let lead_time_ms = std::env::var("SCHEDULE_LEAD_TIME_MS")
            .ok()
            .and_then(|raw| raw.parse::<u64>().ok())
            .filter(|ms| *ms > 0)
            .unwrap_or(DEFAULT_SCHEDULE_LEAD_TIME_MS);

        let tls_enabled = std::env::var("TLS_ENABLED")
            .map(|raw| matches!(raw.trim(), "1" | "true" | "TRUE" | "yes"))
            .unwrap_or(false);

        let tls_cert_dir =
            std::env::var("TLS_CERT_DIR").unwrap_or_else(|_| DEFAULT_TLS_CERT_DIR.to_string());

        Self {
            bind_address,
            schedule_lead_time: Duration::from_millis(lead_time_ms),
            tls_enabled,
            tls_cert_dir,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    // Environment variables are process-global; serialize tests that touch
    // them so they don't race with each other under the parallel test runner.
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn defaults_are_applied_when_env_vars_absent() {
        let _guard = ENV_LOCK.lock().unwrap();
        std::env::remove_var("BIND_ADDRESS");
        std::env::remove_var("SCHEDULE_LEAD_TIME_MS");
        std::env::remove_var("TLS_ENABLED");
        std::env::remove_var("TLS_CERT_DIR");

        let config = Config::from_env();

        assert_eq!(config.bind_address, DEFAULT_BIND_ADDRESS);
        assert_eq!(config.schedule_lead_time, Duration::from_millis(150));
        assert!(!config.tls_enabled);
        assert_eq!(config.tls_cert_dir, DEFAULT_TLS_CERT_DIR);
    }

    #[test]
    fn tls_enabled_accepts_common_truthy_spellings() {
        let _guard = ENV_LOCK.lock().unwrap();
        for value in ["1", "true", "TRUE", "yes"] {
            std::env::set_var("TLS_ENABLED", value);
            assert!(
                Config::from_env().tls_enabled,
                "expected {value:?} to enable TLS"
            );
        }
        std::env::remove_var("TLS_ENABLED");
    }

    #[test]
    fn tls_enabled_defaults_false_for_unrecognized_values() {
        let _guard = ENV_LOCK.lock().unwrap();
        std::env::set_var("TLS_ENABLED", "nope");
        assert!(!Config::from_env().tls_enabled);
        std::env::remove_var("TLS_ENABLED");
    }

    #[test]
    fn invalid_lead_time_falls_back_to_default() {
        let _guard = ENV_LOCK.lock().unwrap();
        std::env::set_var("SCHEDULE_LEAD_TIME_MS", "not-a-number");
        let config = Config::from_env();
        assert_eq!(config.schedule_lead_time, Duration::from_millis(150));
        std::env::remove_var("SCHEDULE_LEAD_TIME_MS");
    }

    #[test]
    fn zero_lead_time_falls_back_to_default() {
        let _guard = ENV_LOCK.lock().unwrap();
        std::env::set_var("SCHEDULE_LEAD_TIME_MS", "0");
        let config = Config::from_env();
        assert_eq!(config.schedule_lead_time, Duration::from_millis(150));
        std::env::remove_var("SCHEDULE_LEAD_TIME_MS");
    }
}
