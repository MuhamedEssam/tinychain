use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;

use async_trait::async_trait;
use destream::de::FromStream;
use futures::{TryFutureExt, TryStreamExt};
use hyper::service::{make_service_fn, service_fn};
use hyper::{Body, Response};
use log::debug;
use serde::de::DeserializeOwned;
use transact::IntoView;

use auth::Token;
use error::*;
use generic::{NetworkTime, TCPathBuf};

use crate::gateway::Gateway;
use crate::state::State;
use crate::txn::*;

const CONTENT_TYPE: &str = "application/json";

type GetParams = HashMap<String, String>;

pub struct HTTPServer {
    gateway: Arc<Gateway>,
}

impl HTTPServer {
    pub fn new(gateway: Arc<Gateway>) -> Self {
        Self { gateway }
    }

    async fn handle(
        self: Arc<Self>,
        request: hyper::Request<Body>,
    ) -> Result<Response<Body>, hyper::Error> {
        let (params, txn) = match self.process_headers(&request).await {
            Ok((params, txn)) => (params, txn),
            Err(cause) => return Ok(transform_error(cause)),
        };

        match self.route(&txn, params, request).await {
            Ok(state) => match destream_json::encode(state.into_view(txn)) {
                Ok(response) => {
                    let mut response = Response::new(Body::wrap_stream(response));
                    response
                        .headers_mut()
                        .insert(hyper::header::CONTENT_TYPE, CONTENT_TYPE.parse().unwrap());

                    Ok(response)
                }
                Err(cause) => Ok(transform_error(TCError::internal(cause))),
            },
            Err(cause) => Ok(transform_error(cause)),
        }
    }

    async fn process_headers(
        &self,
        http_request: &hyper::Request<Body>,
    ) -> TCResult<(GetParams, Txn)> {
        let mut params = http_request
            .uri()
            .query()
            .map(|v| {
                debug!("param {}", v);
                url::form_urlencoded::parse(v.as_bytes())
                    .into_owned()
                    .collect()
            })
            .unwrap_or_else(HashMap::new);

        let token: Token = if let Some(header) = http_request.headers().get("Authorization") {
            let token = header
                .to_str()
                .map_err(|e| TCError::bad_request("Unable to parse Authorization header", e))?;

            self.gateway.authenticate(token).await?
        } else {
            self.gateway.issue_token()?
        };

        let txn_id = if let Some(txn_id) = get_param(&mut params, "txn_id")? {
            txn_id
        } else {
            TxnId::new(NetworkTime::now())
        };

        let request = Request::new(token, txn_id);
        let txn = self.gateway.new_txn(request).await?;
        Ok((params, txn))
    }

    async fn route(
        &self,
        txn: &Txn,
        mut params: GetParams,
        http_request: hyper::Request<Body>,
    ) -> TCResult<State> {
        let path: TCPathBuf = http_request.uri().path().parse()?;

        match http_request.method() {
            &hyper::Method::GET => {
                let key = get_param(&mut params, "key")?.unwrap_or_default();
                self.gateway.get(txn, path.into(), key).await
            }
            &hyper::Method::POST => {
                let data = http_request
                    .into_body()
                    .map_ok(|bytes| bytes.to_vec())
                    .map_err(destream::de::Error::custom);
                let mut decoder = destream_json::de::Decoder::from(data);
                let data = State::from_stream(txn.clone(), &mut decoder)
                    .map_err(|e| TCError::bad_request("error deserializing POST data", e))
                    .await?;

                self.gateway.post(txn, path.into(), data).await
            }
            other => Err(TCError::method_not_allowed(other)),
        }
    }
}

#[async_trait]
impl crate::gateway::Server for HTTPServer {
    type Error = hyper::Error;

    async fn listen(self, addr: SocketAddr) -> Result<(), Self::Error> {
        println!("HTTP server listening on {}", &addr);
        let server = Arc::new(self);

        let new_service = make_service_fn(move |_| {
            let server = server.clone();
            async {
                Ok::<_, hyper::Error>(service_fn(move |req| {
                    let server = server.clone();
                    HTTPServer::handle(server, req)
                }))
            }
        });

        hyper::Server::bind(&addr).serve(new_service).await
    }
}

fn get_param<T: DeserializeOwned>(
    params: &mut HashMap<String, String>,
    name: &str,
) -> TCResult<Option<T>> {
    if let Some(param) = params.remove(name) {
        let val: T = serde_json::from_str(&param).map_err(|e| {
            TCError::bad_request(&format!("Unable to parse URI parameter '{}'", name), e)
        })?;

        Ok(Some(val))
    } else {
        Ok(None)
    }
}

fn transform_error(err: TCError) -> hyper::Response<Body> {
    let mut response = hyper::Response::new(Body::from(format!("{}\r\n", err.message())));

    use error::ErrorType::*;
    use hyper::StatusCode;
    *response.status_mut() = match err.code() {
        BadRequest => StatusCode::BAD_REQUEST,
        Forbidden => StatusCode::FORBIDDEN,
        Conflict => StatusCode::CONFLICT,
        Internal => StatusCode::INTERNAL_SERVER_ERROR,
        MethodNotAllowed => StatusCode::METHOD_NOT_ALLOWED,
        NotFound => StatusCode::NOT_FOUND,
        NotImplemented => StatusCode::NOT_IMPLEMENTED,
        Timeout => StatusCode::REQUEST_TIMEOUT,
        Unauthorized => StatusCode::UNAUTHORIZED,
    };

    response
}
