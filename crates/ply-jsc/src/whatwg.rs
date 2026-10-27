//! WHATWG URLs for the runtime's `URL` and `URLSearchParams` (RFC 0013):
//! the `url` crate parses and serialises, through the getters and setters
//! of its `quirks` module, which follow the URL standard's API.

use serde_json::{Value, json};
use url::{Url, form_urlencoded, quirks};

use crate::runtime::OpResult;

fn parts(url: &Url) -> Value {
    json!({
        "href": quirks::href(url), "origin": quirks::origin(url), "protocol": quirks::protocol(url),
        "username": quirks::username(url), "password": quirks::password(url),
        "host": quirks::host(url), "hostname": quirks::hostname(url), "port": quirks::port(url),
        "pathname": quirks::pathname(url), "search": quirks::search(url), "hash": quirks::hash(url),
    })
}

fn invalid() -> OpResult {
    OpResult::error("ERR_INVALID_URL", "Invalid URL")
}

pub(crate) fn op(op: &str, args: &Value) -> OpResult {
    let s = |key: &str| args.get(key).and_then(Value::as_str).unwrap_or("");
    match op {
        "url.parse" => {
            let base = match args.get("base").and_then(Value::as_str) {
                Some(base) => match Url::parse(base) { Ok(base) => Some(base), Err(_) => return invalid() },
                None => None,
            };
            match Url::options().base_url(base.as_ref()).parse(s("input")) {
                Ok(url) => OpResult::Json(parts(&url)),
                Err(_) => invalid(),
            }
        }
        // A setter the standard ignores on bad input leaves the URL as it was.
        "url.set" => {
            let Ok(mut url) = Url::parse(s("href")) else { return invalid() };
            let value = s("value");
            match s("part") {
                "href" => if quirks::set_href(&mut url, value).is_err() { return invalid() },
                "protocol" => { let _ = quirks::set_protocol(&mut url, value); }
                "username" => { let _ = quirks::set_username(&mut url, value); }
                "password" => { let _ = quirks::set_password(&mut url, value); }
                "host" => { let _ = quirks::set_host(&mut url, value); }
                "hostname" => { let _ = quirks::set_hostname(&mut url, value); }
                "port" => { let _ = quirks::set_port(&mut url, value); }
                "pathname" => quirks::set_pathname(&mut url, value),
                "search" => quirks::set_search(&mut url, value),
                "hash" => quirks::set_hash(&mut url, value),
                _ => return OpResult::error("ERR_PLY_INVALID_CALL", "unknown URL part"),
            }
            OpResult::Json(parts(&url))
        }
        "url.form_parse" => OpResult::Json(json!(form_urlencoded::parse(s("input").as_bytes()).map(|(k, v)| [k, v]).collect::<Vec<_>>())),
        "url.form_serialize" => {
            let pairs = args.get("pairs").and_then(Value::as_array).into_iter().flatten().filter_map(|pair| {
                Some((pair.get(0)?.as_str()?, pair.get(1)?.as_str()?))
            });
            OpResult::Json(json!(form_urlencoded::Serializer::new(String::new()).extend_pairs(pairs).finish()))
        }
        _ => OpResult::error("ERR_PLY", format!("unknown host op {op}")),
    }
}
