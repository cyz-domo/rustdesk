use hbb_common::ResultType;
use serde::de::DeserializeOwned;
use serde_json::{Map, Value};

#[cfg(feature = "flutter")]
pub mod account;
pub mod downloader;
mod http_client;
pub mod record_upload;
pub mod sync;
pub use http_client::{
    create_http_client_async, create_http_client_async_with_url_strict,
    create_http_client_with_url, create_http_client_with_url_strict, get_url_for_tls,
};

#[derive(Debug)]
pub enum HbbHttpResponse<T> {
    ErrorFormat,
    Error(String),
    DataTypeFormat,
    Data(T),
}

impl<T: DeserializeOwned> HbbHttpResponse<T> {
    pub fn parse(body: &str) -> ResultType<Self> {
        let map = serde_json::from_str::<Map<String, Value>>(body)?;
        if let Some(error) = map.get("error") {
            if let Some(err) = error.as_str() {
                Ok(Self::Error(err.to_owned()))
            } else {
                Ok(Self::ErrorFormat)
            }
        } else {
            match serde_json::from_value(Value::Object(map)) {
                Ok(v) => Ok(Self::Data(v)),
                Err(_) => Ok(Self::DataTypeFormat),
            }
        }
    }
}

/// Login state (access_token, user_info) belonging to the currently active
/// server's api-server: the owning profile's state, with the legacy global
/// slot as fallback for non-profile setups.
pub fn current_login_state() -> (String, String) {
    let api = crate::common::get_api_server(
        hbb_common::config::Config::get_option("api-server"),
        hbb_common::config::Config::get_option("custom-rendezvous-server"),
    );
    base::server_profile::get_login_by_api(&api)
}
