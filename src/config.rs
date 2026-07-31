use std::time::Duration;

#[derive(Debug, Clone)]
pub struct Config {
    pub bind_address: String,
    pub schedule_lead_time: Duration,
}

const DEFAULT_BIND_ADDRESS: &str = "127.0.0.1:3000";
const DEFAULT_SCHEDULE_LEAD_TIME_MS: u64 = 150;

impl Config {
    pub fn from_env() -> Self {
        let bind_address =
            std::env::var("BIND_ADDRESS").unwrap_or_else(|_| DEFAULT_BIND_ADDRESS.to_string());

        let lead_time_ms = std::env::var("SCHEDULE_LEAD_TIME_MS")
            .ok()
            .and_then(|raw| raw.parse::<u64>().ok())
            .filter(|ms| *ms > 0)
            .unwrap_or(DEFAULT_SCHEDULE_LEAD_TIME_MS);

        Self {
            bind_address,
            schedule_lead_time: Duration::from_millis(lead_time_ms),
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

        let config = Config::from_env();

        assert_eq!(config.bind_address, DEFAULT_BIND_ADDRESS);
        assert_eq!(config.schedule_lead_time, Duration::from_millis(150));
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
