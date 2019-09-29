use futures_util::future;
use futures_util::TryFutureExt;
use log::{debug, log_enabled, trace};
use reqwest::header::{HeaderValue, CONTENT_TYPE};
use reqwest::multipart::Form;
use serde::de::DeserializeOwned;
use url::Url;

use crate::auth::Credentials;
use crate::error::{self, Kind, Result};
use crate::routing::{AuthMethod, Route};
use crate::types::ModioErrorResponse;
use crate::Modio;

pub struct RequestBuilder {
    modio: Modio,
    request: Request,
}

struct Request {
    pub(crate) route: Route,
    pub(crate) query: Option<String>,
    pub(crate) body: Option<Body>,
}

pub enum Body {
    Form(String),
    Multipart(Form),
}

impl RequestBuilder {
    pub(crate) fn new(modio: Modio, route: Route) -> Self {
        Self {
            modio,
            request: Request {
                route,
                query: None,
                body: None,
            },
        }
    }

    pub fn query(mut self, query: String) -> Self {
        self.request.query = Some(query);
        self
    }

    pub fn body<T>(mut self, body: T) -> Self
    where
        Body: From<T>,
    {
        self.request.body = Some(body.into());
        self
    }

    pub async fn send<Out>(self) -> Result<Out>
    where
        Out: DeserializeOwned + Send,
    {
        let (method, auth_method, path) = self.request.route.pieces();

        match (auth_method, &self.modio.credentials) {
            (AuthMethod::ApiKey, Credentials::Token(_, _)) => return Err(error::apikey_required()),
            (AuthMethod::Token, Credentials::ApiKey(_)) => return Err(error::token_required()),
            _ => {}
        }
        let url = match self.request.query {
            Some(query) => format!("{}{}?{}", self.modio.host, path, query),
            None => format!("{}{}", self.modio.host, path),
        };

        let mut url = if let Credentials::ApiKey(ref api_key) = self.modio.credentials {
            Url::parse_with_params(&url, Some(("api_key", api_key))).map_err(error::builder)?
        } else {
            url.parse().map_err(error::builder)?
        };

        if let Some("") = url.query() {
            url.set_query(None);
        }

        debug!("request: {} {}", method, url);
        let mut req = self.modio.client.request(method, url);

        if let Credentials::Token(ref token, _) = self.modio.credentials {
            req = req.bearer_auth(token);
        }

        match self.request.body {
            Some(Body::Form(s)) => {
                trace!("body: {}", s);
                req = req.header(
                    CONTENT_TYPE,
                    HeaderValue::from_static("application/x-www-form-urlencoded"),
                );
                req = req.body(s);
            }
            Some(Body::Multipart(mp)) => {
                trace!("{:?}", mp);
                req = req.multipart(mp);
            }
            None => {
                req = req.header(
                    CONTENT_TYPE,
                    HeaderValue::from_static("application/x-www-form-urlencoded"),
                );
            }
        }

        let response = req.send().map_err(error::builder_or_request).await?;

        let status = response.status();

        let (remaining, reset) = if status.is_success() {
            (None, None)
        } else {
            let remaining = response
                .headers()
                .get(super::X_RATELIMIT_REMAINING)
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.parse::<u64>().ok());
            let reset = response
                .headers()
                .get(super::X_RATELIMIT_RETRY_AFTER)
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.parse::<u64>().ok());
            (remaining, reset)
        };

        let body = response.bytes().map_err(error::request).await?;

        if log_enabled!(log::Level::Trace) {
            match std::str::from_utf8(&body) {
                Ok(s) => trace!("status: {}, response: {}", status, s),
                Err(_) => trace!("status: {}, response: {:?}", status, body),
            }
        }

        if status.is_success() {
            serde_json::from_slice::<Out>(&body).map_err(error::decode)
        } else {
            match (remaining, reset) {
                (Some(remaining), Some(reset)) if remaining == 0 => {
                    debug!("ratelimit reached: reset in {} mins", reset);
                    Err(error::ratelimit(reset))
                }
                _ => serde_json::from_slice::<ModioErrorResponse>(&body)
                    .map(|mer| Err(error::error_for_status(status, mer.error)))
                    .map_err(error::decode)?,
            }
        }
    }

    pub async fn delete(self) -> Result<()> {
        self.send()
            .or_else(|e| match e.kind() {
                Kind::Decode => future::ok(()),
                _ => future::err(e),
            })
            .await
    }
}

impl From<String> for Body {
    fn from(s: String) -> Body {
        Body::Form(s)
    }
}

impl From<Form> for Body {
    fn from(form: Form) -> Body {
        Body::Multipart(form)
    }
}
