//! Shared concrete HTTP client state for CLI and MCP application calls.

pub(crate) struct ApiClient {
    pub(crate) client: reqwest::Client,
    pub(crate) base_url: String,
    pub(crate) access_token: Option<String>,
}

impl ApiClient {
    pub(crate) fn new(base_url: &str) -> Self {
        Self {
            client: reqwest::Client::new(),
            base_url: base_url.trim_end_matches('/').to_string(),
            access_token: std::env::var("AETHER_ACCESS_TOKEN")
                .ok()
                .filter(|value| !value.trim().is_empty() && value.trim() == value),
        }
    }

    #[cfg(test)]
    pub(crate) fn with_access_token(base_url: &str, access_token: &str) -> Self {
        Self {
            client: reqwest::Client::new(),
            base_url: base_url.trim_end_matches('/').to_string(),
            access_token: Some(access_token.to_string()),
        }
    }

    #[cfg(test)]
    pub(crate) fn without_access_token(base_url: &str) -> Self {
        Self {
            client: reqwest::Client::new(),
            base_url: base_url.trim_end_matches('/').to_string(),
            access_token: None,
        }
    }

    pub(crate) fn apply_auth(
        &self,
        request: reqwest::RequestBuilder,
    ) -> anyhow::Result<reqwest::RequestBuilder> {
        match &self.access_token {
            Some(token) => {
                crate::transport_security::require_secure_bearer_transport(&self.base_url)?;
                Ok(request.bearer_auth(token))
            },
            None => Ok(request),
        }
    }

    pub(crate) fn require_governed_auth(
        &self,
        confirmed: bool,
        confirmation_error: &'static str,
        token_error: &'static str,
    ) -> anyhow::Result<&str> {
        if !confirmed {
            anyhow::bail!(confirmation_error);
        }
        crate::transport_security::require_secure_bearer_transport(&self.base_url)?;
        self.access_token
            .as_deref()
            .ok_or_else(|| anyhow::anyhow!(token_error))
    }
}

macro_rules! authenticated_api_client {
    ($visibility:vis $name:ident) => {
        $visibility struct $name(crate::api_client::ApiClient);

        impl std::ops::Deref for $name {
            type Target = crate::api_client::ApiClient;

            fn deref(&self) -> &Self::Target {
                &self.0
            }
        }

        impl $name {
            $visibility fn new(base_url: &str) -> anyhow::Result<Self> {
                Ok(Self(crate::api_client::ApiClient::new(base_url)))
            }

            #[cfg(test)]
            pub(crate) fn with_access_token(
                base_url: &str,
                access_token: &str,
            ) -> anyhow::Result<Self> {
                Ok(Self(crate::api_client::ApiClient::with_access_token(
                    base_url,
                    access_token,
                )))
            }

            #[cfg(test)]
            pub(crate) fn without_access_token(base_url: &str) -> Self {
                Self(crate::api_client::ApiClient::without_access_token(base_url))
            }
        }
    };
}

pub(crate) use authenticated_api_client;
