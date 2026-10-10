//! What the semantic conventions record about a request.

use std::fmt;

use http::{Method, Request, Version};

/// The method as the conventions name it: one of the HTTP methods, or
/// `_OTHER` with the method as sent, so made-up methods can't grow label
/// sets.
pub(crate) fn method_name(method: &Method) -> (&'static str, Option<&str>) {
    let known = match *method {
        Method::GET => "GET",
        Method::HEAD => "HEAD",
        Method::POST => "POST",
        Method::PUT => "PUT",
        Method::DELETE => "DELETE",
        Method::CONNECT => "CONNECT",
        Method::OPTIONS => "OPTIONS",
        Method::TRACE => "TRACE",
        Method::PATCH => "PATCH",
        // A known method in the conventions since v1.44, though `http` has
        // no constant for it.
        _ if method.as_str() == "QUERY" => "QUERY",
        _ => return ("_OTHER", Some(method.as_str())),
    };
    (known, None)
}

/// `network.protocol.version`.
pub(crate) fn protocol_version(version: Version) -> &'static str {
    match version {
        Version::HTTP_09 => "0.9",
        Version::HTTP_10 => "1.0",
        Version::HTTP_11 => "1.1",
        Version::HTTP_2 => "2",
        Version::HTTP_3 => "3",
        _ => "_OTHER",
    }
}

/// The host and port a request was addressed to: the URI's authority, else
/// the `Host` header. The port is left out when neither gives one.
pub(crate) fn server_address<B>(req: &Request<B>) -> (Option<&str>, Option<u16>) {
    let authority = req
        .uri()
        .authority()
        .map(|a| a.as_str())
        .or_else(|| req.headers().get(http::header::HOST)?.to_str().ok());
    let Some(authority) = authority else {
        return (None, None);
    };
    let authority = authority
        .rsplit_once('@')
        .map_or(authority, |(_, host)| host);
    if let Some(v6) = authority.strip_prefix('[') {
        let Some((host, rest)) = v6.split_once(']') else {
            return (None, None);
        };
        return (
            Some(host),
            rest.strip_prefix(':').and_then(|p| p.parse().ok()),
        );
    }
    match authority.rsplit_once(':') {
        Some((host, port)) => (Some(host), port.parse().ok()),
        None => (Some(authority), None),
    }
}

/// What an incoming request's span records, as the HTTP semantic
/// conventions name it. Handed to a span-making hook; see
/// [`server_span!`](crate::server_span).
#[derive(Debug)]
pub struct RequestInfo<'a> {
    /// `http.request.method`: a known method, or `_OTHER`.
    pub method: &'static str,
    /// `http.request.method_original`, for `_OTHER`.
    pub method_original: Option<&'a str>,
    /// `url.path`. The query isn't recorded; it may hold secrets.
    pub path: &'a str,
    /// `url.scheme`.
    pub scheme: &'a str,
    /// `http.route`: the route template that matched, such as
    /// `/items/{id}`, if the layer knows it.
    pub route: Option<&'a str>,
    /// `server.address`, from the URI or the `Host` header.
    pub server_address: Option<&'a str>,
    /// `server.port`.
    pub server_port: Option<u16>,
    /// `network.protocol.version`, such as `1.1` or `2`.
    pub protocol_version: &'static str,
    /// `user_agent.original`.
    pub user_agent: Option<&'a str>,
}

impl RequestInfo<'_> {
    /// The span's name: `{method} {route}`, or just the method without a
    /// route; `HTTP` stands for a method outside the standard ones.
    pub fn span_name(&self) -> SpanName<'_> {
        SpanName {
            method: if self.method == "_OTHER" {
                "HTTP"
            } else {
                self.method
            },
            route: self.route,
        }
    }
}

/// A span name, formatted without allocating until it's recorded.
#[derive(Debug, Clone, Copy)]
pub struct SpanName<'a> {
    method: &'a str,
    route: Option<&'a str>,
}

impl fmt::Display for SpanName<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.route {
            Some(route) => write!(f, "{} {route}", self.method),
            None => f.write_str(self.method),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(uri: &str, host: Option<&str>) -> Request<()> {
        let mut b = Request::builder().uri(uri);
        if let Some(host) = host {
            b = b.header("host", host);
        }
        b.body(()).unwrap()
    }

    #[test]
    fn addresses_come_from_the_authority_or_host() {
        assert_eq!(
            server_address(&request("/x", Some("api.example:8080"))),
            (Some("api.example"), Some(8080))
        );
        assert_eq!(
            server_address(&request("/x", Some("api.example"))),
            (Some("api.example"), None)
        );
        assert_eq!(
            server_address(&request("http://u:p@[::1]:9/x", None)),
            (Some("::1"), Some(9))
        );
        assert_eq!(server_address(&request("/x", None)), (None, None));
    }

    #[test]
    fn unknown_methods_are_other() {
        assert_eq!(method_name(&Method::GET), ("GET", None));
        let purge = Method::from_bytes(b"PURGE").unwrap();
        assert_eq!(method_name(&purge), ("_OTHER", Some("PURGE")));
        let query = Method::from_bytes(b"QUERY").unwrap();
        assert_eq!(method_name(&query), ("QUERY", None));
    }
}
