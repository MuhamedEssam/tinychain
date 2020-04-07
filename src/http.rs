use std::collections::{HashMap, HashSet};
use std::convert::Infallible;
use std::sync::Arc;

use hyper::service::{make_service_fn, service_fn};
use hyper::{Body, Method, Request, Response, Server, StatusCode};

use crate::context::{Link, TCResponse, TCResult, TCValue};
use crate::error;
use crate::host::Host;
use crate::transaction;

pub async fn listen(
    host: Arc<Host>,
    port: u16,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let make_svc = make_service_fn(|_conn| {
        let host = host.clone();
        async { Ok::<_, Infallible>(service_fn(move |req| handle(host.clone(), req))) }
    });

    let addr = ([127, 0, 0, 1], port).into();
    let server = Server::bind(&addr).serve(make_svc);

    println!("Listening on http://{}", addr);

    if let Err(e) = server.await {
        eprintln!("server error: {}", e);
    }

    Ok(())
}

async fn handle(host: Arc<Host>, req: Request<Body>) -> Result<Response<Body>, hyper::Error> {
    let _path = match Link::to(req.uri().path()) {
        Ok(link) => link,
        Err(cause) => {
            return transform_error(Err(cause));
        }
    };

    let params: HashMap<String, String> = req
        .uri()
        .query()
        .map(|v| {
            url::form_urlencoded::parse(v.as_bytes())
                .into_owned()
                .collect()
        })
        .unwrap_or_else(HashMap::new);

    match *req.method() {
        Method::POST => {
            let mut capture: HashSet<&str> = HashSet::new();
            if let Some(param) = params.get("capture") {
                for id in param.split(',') {
                    capture.insert(&id);
                }
            }

            let body = &hyper::body::to_bytes(req.into_body()).await?;
            let graph = match serde_json::from_slice::<transaction::Request>(body) {
                Ok(graph) => graph,
                Err(cause) => {
                    return transform_error(Err(error::bad_request(
                        "Unable to parse request",
                        cause,
                    )));
                }
            };

            let txn = match host.clone().new_transaction(graph) {
                Ok(txn) => txn,
                Err(cause) => {
                    return transform_error(Err(cause));
                }
            };

            let mut results: HashMap<String, TCValue> = HashMap::new();
            match txn.execute(capture).await {
                Ok(responses) => {
                    for (id, r) in responses {
                        match r {
                            TCResponse::Value(val) => {
                                results.insert(id, val);
                            }
                            other => {
                                return transform_error(Err(error::bad_request(
                                    "Attempt to capture an unserializable value",
                                    other,
                                )));
                            }
                        }
                    }
                }
                Err(cause) => {
                    return transform_error(Err(cause));
                }
            };

            let result = serde_json::to_string_pretty(&results)
                .and_then(|s| Ok(s.into_bytes()))
                .or_else(|e| {
                    let msg = "Your request completed successfully but there was an error serializing the response";
                    Err(error::bad_request(msg, e))
                });

            transform_error(result)
        }
        _ => {
            let mut response = Response::new(Body::from(""));
            *response.status_mut() = StatusCode::NOT_FOUND;
            Ok(response)
        }
    }
}

fn transform_error(result: TCResult<Vec<u8>>) -> Result<Response<Body>, hyper::Error> {
    match result {
        Ok(contents) => Ok(Response::new(Body::from(contents))),
        Err(cause) => {
            let mut response = Response::new(Body::from(cause.message().to_string()));
            *response.status_mut() = match cause.reason() {
                error::Code::BadRequest => StatusCode::BAD_REQUEST,
                error::Code::Internal => StatusCode::INTERNAL_SERVER_ERROR,
                error::Code::MethodNotAllowed => StatusCode::METHOD_NOT_ALLOWED,
                error::Code::NotFound => StatusCode::NOT_FOUND,
                error::Code::NotImplemented => StatusCode::NOT_IMPLEMENTED,
            };
            Ok(response)
        }
    }
}
