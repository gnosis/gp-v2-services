//! Code for reporting per-url metrics.
//!
//! The only correct way to report a request is by wrapping the top-level
//! warp filter into a `warp::log::custom`. Other ways (logging with `map`
//! or wrapping API filters separately) have some pitfalls. Specifically,
//! they don't report requests that didn't match any filter, they also
//! may report a request multiple times.
//!
//! When using `warp::log::custom`, we get a full request path for reporting.
//! The issue is that we can't create a separate metric for each requested
//! path. This is because our paths contain variable parameters. For example,
//! consider two requests, `/api/v1/orders/id1` and `/api/v1/orders/id2`.
//! If we were to use a request path as a metric label, we'd get two metrics,
//! one for `id1` and another one for `id2`.
//!
//! Therefore, we need to sanitize the path and replace all variable parameters
//! with asterisks, so our metric label looks like `/api/v1/orders/*`.
//! We also need to strip any unexpected parts of the url, in case someone
//! sends a request to a path that doesn't exist. So `/api/v1/some-junk`
//! should become `/api/v1/..`.
//!
//! We do this by taking a list of allowed path patterns, and comparing
//! all requested paths to them.
//!
//! Patterns are just regular paths with asterisks in places
//! of variable parameters. For the example above, the pattern
//! would be `/api/v1/orders/*`.

use shared::metrics::get_metric_storage_registry;
use std::collections::HashMap;

/// Creates a wrapper for warp filters that reports per-request metrics.
///
/// See the module documentation for more info.
pub fn handle_metrics<Paths, Path>(paths: Paths) -> warp::log::Log<impl Fn(warp::log::Info) + Clone>
where
    Paths: IntoIterator<Item = Path>,
    Path: AsRef<str>,
{
    handle_metrics_impl(PathSegmentTree::from_paths(paths))
}

fn handle_metrics_impl(tree: PathSegmentTree) -> warp::log::Log<impl Fn(warp::log::Info) + Clone> {
    let metrics = ApiMetrics::instance(get_metric_storage_registry()).unwrap();

    warp::log::custom(move |info: warp::log::Info| {
        let path = tree.sanitize(info.path());
        metrics
            .requests_complete
            .with_label_values(&[&path, info.method().as_str(), info.status().as_str()])
            .inc();
        metrics
            .requests_duration_seconds
            .with_label_values(&[&path, info.method().as_str()])
            .observe(info.elapsed().as_secs_f64());
    })
}

#[derive(prometheus_metric_storage::MetricStorage, Clone, Debug)]
#[metric(subsystem = "api")]
struct ApiMetrics {
    /// Number of completed API requests.
    #[metric(labels("url", "method", "status_code"))]
    requests_complete: prometheus::CounterVec,

    /// Execution time for each API request.
    #[metric(labels("url", "method"))]
    requests_duration_seconds: prometheus::HistogramVec,
}

#[derive(Default, Debug, Clone)]
struct PathSegmentTree {
    root: PathSegmentTreeNode,
}

#[derive(Default, Debug, Clone)]
struct PathSegmentTreeNode {
    leaves: HashMap<String, PathSegmentTreeNode>,
}

impl PathSegmentTree {
    fn from_paths<Paths, Path>(paths: Paths) -> Self
    where
        Paths: IntoIterator<Item = Path>,
        Path: AsRef<str>,
    {
        let mut tree = PathSegmentTree::default();
        for path in paths {
            tree.add_path(path.as_ref());
        }
        tree
    }

    fn add_path(&mut self, mut path: &str) {
        let mut segments = &mut self.root;

        // Path always starts with a slash, so we ignore it.
        path = path.strip_prefix('/').unwrap_or(path);

        while !path.is_empty() {
            let (head, tail) = path.split_once('/').unwrap_or((path, ""));
            segments = segments
                .leaves
                .entry(head.into())
                .or_insert_with(Default::default);
            path = tail;
        }
    }

    fn sanitize(&self, mut path: &str) -> String {
        let mut segments = &self.root;

        // Path always starts with a slash, so we ignore it.
        path = path.strip_prefix('/').unwrap_or(path);

        if path.is_empty() {
            return "/".into();
        }

        let mut sanitized = String::with_capacity(128);

        while !path.is_empty() {
            let (head, tail) = path.split_once('/').unwrap_or((path, ""));
            if let Some(subsegments) = segments.leaves.get(head) {
                sanitized.push('/');
                sanitized.push_str(head);
                segments = subsegments;
            } else if let Some(subsegments) = segments.leaves.get("*") {
                sanitized.push_str("/*");
                segments = subsegments;
            } else {
                sanitized.push_str("/..");
                break;
            }
            path = tail;
        }

        sanitized
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sanitize() {
        let segments =
            PathSegmentTree::from_paths(["/orders", "/orders/*", "/orders/*/status", "/fee"]);

        assert_eq!(segments.sanitize("/"), "/");
        assert_eq!(segments.sanitize("/orders"), "/orders");
        assert_eq!(segments.sanitize("/orders/1"), "/orders/*");
        assert_eq!(segments.sanitize("/orders/1/status"), "/orders/*/status");
        assert_eq!(
            segments.sanitize("/orders/1/status/whatever"),
            "/orders/*/status/.."
        );
        assert_eq!(
            segments.sanitize("/orders/1/status/x/y/z"),
            "/orders/*/status/.."
        );
        assert_eq!(segments.sanitize("/orders/1/x/y/z"), "/orders/*/..");
        assert_eq!(segments.sanitize("/fee"), "/fee");
        assert_eq!(segments.sanitize("/fee/x"), "/fee/..");
        assert_eq!(segments.sanitize("/other/url"), "/..");

        assert_eq!(segments.sanitize("/fee"), "/fee");
        assert_eq!(segments.sanitize("/fee/"), "/fee");
        assert_eq!(segments.sanitize("/fee//"), "/fee/..");
    }
}
