use reqwest::header::{AUTHORIZATION, HeaderMap, HeaderValue};
use reqwest::{Error, IntoUrl, Method, Request, RequestBuilder, Response};
use serde::de::DeserializeOwned;
use url::Url;

use crate::Result;
use crate::appservice::types::Config;

#[derive(Debug, Clone)]
pub struct Client {
    base_url: Url,
    client: reqwest::Client,
}

impl Client {
    pub fn new(config: &Config) -> Result<Self> {
        let token = format!("Bearer {}", &config.appservice.as_token);
        let mut headers = HeaderMap::new();
        let mut token = HeaderValue::from_str(&token)?;
        token.set_sensitive(true);
        headers.insert(AUTHORIZATION, token);

        let client = reqwest::Client::builder()
            .use_rustls_tls()
            .default_headers(headers)
            .user_agent(&config.appservice.id)
            .build()?;

        Ok(Self { base_url: config.homeserver.url.clone(), client })
    }

    pub fn delete<U: IntoUrl>(&self, url: U) -> RequestBuilder {
        let request_url = join_url(&self.base_url, url.as_str());
        self.client.delete(request_url)
    }

    pub fn get<U: IntoUrl>(&self, url: U) -> RequestBuilder {
        let request_url = join_url(&self.base_url, url.as_str());
        self.client.get(request_url)
    }

    pub fn head<U: IntoUrl>(&self, url: U) -> RequestBuilder {
        let request_url = join_url(&self.base_url, url.as_str());
        self.client.head(request_url)
    }

    pub fn patch<U: IntoUrl>(&self, url: U) -> RequestBuilder {
        let request_url = join_url(&self.base_url, url.as_str());
        self.client.patch(request_url)
    }

    pub fn post<U: IntoUrl>(&self, url: U) -> RequestBuilder {
        let request_url = join_url(&self.base_url, url.as_str());
        self.client.post(request_url)
    }

    pub fn put<U: IntoUrl>(&self, url: U) -> RequestBuilder {
        let request_url = join_url(&self.base_url, url.as_str());
        self.client.put(request_url)
    }

    pub fn request<U: IntoUrl>(&self, method: Method, url: U) -> RequestBuilder {
        let request_url = join_url(&self.base_url, url.as_str());
        self.client.request(method, request_url)
    }

    pub fn execute(&self, request: Request) -> impl Future<Output = core::result::Result<Response, Error>> {
        self.client.execute(request)
    }
}

fn join_url(base_url: &Url, url: &str) -> Url {
    base_url.join(url).unwrap_or(base_url.to_owned())
}

pub async fn parse_response<T>(response: Response) -> Result<T>
where
    T: DeserializeOwned,
{
    match response.error_for_status() {
        Ok(response) => Ok(response.json().await?),
        Err(error) => Err(error.into()),
    }
}

pub async fn discard_response(response: Response) -> Result<()> {
    match response.error_for_status() {
        Ok(_) => Ok(()),
        Err(error) => Err(error.into()),
    }
}
