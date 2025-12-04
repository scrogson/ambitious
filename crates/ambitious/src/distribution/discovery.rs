//! Node discovery trait for pluggable discovery mechanisms.
//!
//! Users can implement this trait to provide custom node discovery,
//! such as Kubernetes API, Consul, mDNS, or gossip protocols.

use std::net::SocketAddr;
use std::time::Duration;

/// Trait for node discovery mechanisms.
///
/// Implement this trait to provide custom node discovery for your environment.
///
/// # Example
///
/// ```ignore
/// use ambitious::dist::NodeDiscovery;
/// use std::net::SocketAddr;
/// use std::time::Duration;
///
/// struct K8sDiscovery {
///     namespace: String,
///     service: String,
/// }
///
/// #[async_trait::async_trait]
/// impl NodeDiscovery for K8sDiscovery {
///     async fn discover(&self) -> Vec<SocketAddr> {
///         // Query Kubernetes API for endpoints
///         todo!()
///     }
///
///     fn interval(&self) -> Duration {
///         Duration::from_secs(30)
///     }
/// }
///
/// // Register the discovery provider
/// ambitious::dist::set_discovery(K8sDiscovery {
///     namespace: "default".to_string(),
///     service: "my-app".to_string(),
/// });
/// ```
#[async_trait::async_trait]
pub trait NodeDiscovery: Send + Sync + 'static {
    /// Discover available node addresses.
    ///
    /// Called periodically at the interval specified by `interval()`.
    /// Return addresses of nodes that should be connected to.
    async fn discover(&self) -> Vec<SocketAddr>;

    /// How often to run discovery.
    ///
    /// Default is 30 seconds.
    fn interval(&self) -> Duration {
        Duration::from_secs(30)
    }

    /// Called when a discovered node is successfully connected.
    ///
    /// Override to track connection status.
    fn on_connected(&self, _addr: SocketAddr) {}

    /// Called when a connection to a discovered node fails.
    ///
    /// Override to implement retry logic or remove from discovery.
    fn on_connect_failed(&self, _addr: SocketAddr, _error: &str) {}
}

// Note: The discovery loop and set_discovery function would be implemented
// when we add the full discovery feature. For now, users connect manually
// with ambitious::dist::connect().
