use crate::ServiceEnvironment;
use url::{Host, Url};

pub struct Config {
    host: Host,
    service_environment: ServiceEnvironment,
    url: Url,
}

impl Config {
    pub fn new(host: Host, service_environment: ServiceEnvironment) -> Self {
        let host_base = host.to_string();
        match service_environment {
            ServiceEnvironment::Live => Self {
                host: host,
                service_environment: service_environment,
                url: Url::parse(&format!("https://chat.{}", host_base)).unwrap(),
            },
            ServiceEnvironment::Staging => Self {
                host: host,
                service_environment: service_environment,
                url: Url::parse(&format!("https://chat.staging.{}", host_base)).unwrap(),
            },
        }
    }

    pub fn host(&self) -> &Host {
        &self.host
    }

    pub fn service_environment(&self) -> &ServiceEnvironment {
        &self.service_environment
    }

    pub fn url(&self) -> &Url {
        &self.url
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_host(s: &str) -> Host {
        Host::parse(s).expect("valid host")
    }

    #[test]
    fn live_environment_uses_chat_subdomain() {
        let cfg = Config::new(parse_host("signal.org"), ServiceEnvironment::Live);
        assert_eq!(cfg.url().as_str(), "https://chat.signal.org/");
        assert_eq!(cfg.host().to_string(), "signal.org");
    }

    #[test]
    fn staging_environment_uses_chat_staging_subdomain() {
        let cfg = Config::new(parse_host("signal.org"), ServiceEnvironment::Staging);
        assert_eq!(cfg.url().as_str(), "https://chat.staging.signal.org/");
    }

    #[test]
    fn url_uses_https_scheme_in_both_environments() {
        for env in &[ServiceEnvironment::Live, ServiceEnvironment::Staging] {
            let cfg = Config::new(parse_host("signal.org"), env.clone());
            assert_eq!(cfg.url().scheme(), "https");
        }
    }
}
