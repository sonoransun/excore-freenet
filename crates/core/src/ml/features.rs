//! Feature engineering for ML models
//!
//! This module extracts features from Freenet's existing telemetry data:
//! - Transport metrics (RTT, bandwidth, packet loss)
//! - Routing history and performance
//! - Peer behavior patterns
//! - Contract execution metrics

use super::FeatureVector;
use crate::ring::{Distance, Location, PeerKeyLocation};
use crate::transport::metrics::TransportMetricsSnapshot;
use std::time::{Duration, Instant};
use std::collections::HashMap;

/// Feature extractor for routing decisions
pub struct RoutingFeatureExtractor {
    /// Historical routing performance per peer
    peer_history: HashMap<PeerKeyLocation, PeerPerformanceHistory>,
    /// Global network statistics
    network_stats: NetworkStatistics,
    /// Feature extraction config
    config: FeatureConfig,
}

impl RoutingFeatureExtractor {
    /// Create new feature extractor
    pub fn new(config: FeatureConfig) -> Self {
        Self {
            peer_history: HashMap::new(),
            network_stats: NetworkStatistics::default(),
            config,
        }
    }

    /// Extract features for peer routing decision
    pub fn extract_routing_features(
        &mut self,
        peer: &PeerKeyLocation,
        target_location: Location,
        transport_metrics: Option<&TransportMetricsSnapshot>,
    ) -> FeatureVector {
        let mut features = Vec::new();
        let mut names = Vec::new();

        // 1. Spatial features
        let distance = peer.location().distance(&target_location);
        features.push(distance as f32);
        names.push("distance".to_string());

        // 2. Peer performance history
        let peer_stats = self.peer_history.get(peer).cloned().unwrap_or_default();

        features.push(peer_stats.avg_rtt.as_millis() as f32);
        names.push("avg_rtt_ms".to_string());

        features.push(peer_stats.success_rate);
        names.push("success_rate".to_string());

        features.push(peer_stats.avg_bandwidth);
        names.push("avg_bandwidth_bps".to_string());

        features.push(peer_stats.packet_loss_rate);
        names.push("packet_loss_rate".to_string());

        // 3. Recent performance trends
        features.push(peer_stats.recent_rtt_trend);
        names.push("rtt_trend".to_string());

        features.push(peer_stats.recent_success_trend);
        names.push("success_trend".to_string());

        // 4. Network context features
        features.push(self.network_stats.avg_network_rtt.as_millis() as f32);
        names.push("network_avg_rtt_ms".to_string());

        features.push(self.network_stats.network_load);
        names.push("network_load".to_string());

        features.push(self.network_stats.active_peers as f32);
        names.push("active_peers".to_string());

        // 5. Transport layer features (if available)
        if let Some(metrics) = transport_metrics {
            if let Some(peer_stats) = metrics.per_peer_stats.get(&peer.addr()) {
                features.push(peer_stats.bytes_sent as f32);
                names.push("bytes_sent".to_string());

                features.push(peer_stats.bytes_received as f32);
                names.push("bytes_received".to_string());

                features.push(peer_stats.transfers_completed as f32);
                names.push("transfers_completed".to_string());

                features.push(peer_stats.transfers_failed as f32);
                names.push("transfers_failed".to_string());
            } else {
                // Peer not in transport metrics - add zeros
                features.extend_from_slice(&[0.0, 0.0, 0.0, 0.0]);
                names.extend_from_slice(&[
                    "bytes_sent".to_string(),
                    "bytes_received".to_string(),
                    "transfers_completed".to_string(),
                    "transfers_failed".to_string(),
                ]);
            }
        } else {
            // No transport metrics - add zeros
            features.extend_from_slice(&[0.0, 0.0, 0.0, 0.0]);
            names.extend_from_slice(&[
                "bytes_sent".to_string(),
                "bytes_received".to_string(),
                "transfers_completed".to_string(),
                "transfers_failed".to_string(),
            ]);
        }

        // 6. Time-based features
        let hour_of_day = chrono::Utc::now().hour() as f32 / 24.0; // Normalized 0-1
        features.push(hour_of_day);
        names.push("hour_of_day".to_string());

        let day_of_week = chrono::Utc::now().weekday().number_from_monday() as f32 / 7.0;
        features.push(day_of_week);
        names.push("day_of_week".to_string());

        // 7. Peer stability features
        features.push(peer_stats.connection_stability);
        names.push("connection_stability".to_string());

        features.push(peer_stats.uptime_hours);
        names.push("uptime_hours".to_string());

        FeatureVector::new(features, names)
    }

    /// Update peer performance history with routing outcome
    pub fn update_peer_performance(
        &mut self,
        peer: &PeerKeyLocation,
        rtt: Duration,
        success: bool,
        bytes_transferred: u64,
    ) {
        let entry = self.peer_history.entry(*peer).or_default();
        entry.update(rtt, success, bytes_transferred);

        // Update global network statistics
        self.network_stats.update(rtt, success);
    }

    /// Update network-wide statistics
    pub fn update_network_stats(&mut self, active_peers: usize, network_load: f32) {
        self.network_stats.active_peers = active_peers;
        self.network_stats.network_load = network_load;
    }

    /// Clean old peer history to prevent memory growth
    pub fn cleanup_old_history(&mut self) {
        let cutoff = Instant::now() - self.config.max_history_age;
        self.peer_history.retain(|_, history| history.last_updated > cutoff);
    }
}

/// Configuration for feature extraction
#[derive(Clone, Debug)]
pub struct FeatureConfig {
    /// Maximum age of peer performance history
    pub max_history_age: Duration,
    /// Window size for computing trends
    pub trend_window_size: usize,
    /// Whether to normalize features
    pub normalize_features: bool,
}

impl Default for FeatureConfig {
    fn default() -> Self {
        Self {
            max_history_age: Duration::from_secs(3600), // 1 hour
            trend_window_size: 10,
            normalize_features: true,
        }
    }
}

/// Historical performance data for a peer
#[derive(Clone, Debug, Default)]
pub struct PeerPerformanceHistory {
    /// Average RTT
    pub avg_rtt: Duration,
    /// Success rate (0.0 to 1.0)
    pub success_rate: f32,
    /// Average bandwidth (bytes per second)
    pub avg_bandwidth: f32,
    /// Packet loss rate (0.0 to 1.0)
    pub packet_loss_rate: f32,
    /// Recent RTT trend (-1.0 = getting worse, 1.0 = getting better)
    pub recent_rtt_trend: f32,
    /// Recent success trend (-1.0 = getting worse, 1.0 = getting better)
    pub recent_success_trend: f32,
    /// Connection stability (0.0 to 1.0)
    pub connection_stability: f32,
    /// Uptime in hours
    pub uptime_hours: f32,
    /// Last update timestamp
    pub last_updated: Instant,
    /// Internal tracking
    rtt_samples: Vec<Duration>,
    success_samples: Vec<bool>,
    total_bytes: u64,
    total_samples: usize,
}

impl PeerPerformanceHistory {
    /// Update history with new performance data
    pub fn update(&mut self, rtt: Duration, success: bool, bytes_transferred: u64) {
        self.last_updated = Instant::now();
        self.total_samples += 1;
        self.total_bytes += bytes_transferred;

        // Update RTT tracking
        self.rtt_samples.push(rtt);
        if self.rtt_samples.len() > 100 {
            self.rtt_samples.remove(0);
        }

        // Update success tracking
        self.success_samples.push(success);
        if self.success_samples.len() > 100 {
            self.success_samples.remove(0);
        }

        // Recompute averages
        self.update_averages();

        // Update trends
        self.update_trends();
    }

    fn update_averages(&mut self) {
        if !self.rtt_samples.is_empty() {
            let total_rtt: Duration = self.rtt_samples.iter().sum();
            self.avg_rtt = total_rtt / self.rtt_samples.len() as u32;
        }

        if !self.success_samples.is_empty() {
            let successes = self.success_samples.iter().filter(|&&s| s).count();
            self.success_rate = successes as f32 / self.success_samples.len() as f32;
        }

        if self.total_samples > 0 {
            let duration_hours = self.last_updated.elapsed().as_secs_f32() / 3600.0;
            if duration_hours > 0.0 {
                self.avg_bandwidth = self.total_bytes as f32 / duration_hours;
                self.uptime_hours = duration_hours;
            }
        }

        // Estimate packet loss (simplified)
        self.packet_loss_rate = 1.0 - self.success_rate;

        // Connection stability based on variance in RTT
        if self.rtt_samples.len() > 1 {
            let avg_rtt_ms = self.avg_rtt.as_millis() as f32;
            let variance = self.rtt_samples.iter()
                .map(|rtt| {
                    let diff = rtt.as_millis() as f32 - avg_rtt_ms;
                    diff * diff
                })
                .sum::<f32>() / self.rtt_samples.len() as f32;

            // Stability is inverse of coefficient of variation
            let cv = variance.sqrt() / avg_rtt_ms;
            self.connection_stability = (1.0 / (1.0 + cv)).clamp(0.0, 1.0);
        }
    }

    fn update_trends(&mut self) {
        const TREND_WINDOW: usize = 10;

        // RTT trend (negative = getting worse, positive = getting better)
        if self.rtt_samples.len() >= TREND_WINDOW {
            let recent = &self.rtt_samples[self.rtt_samples.len() - TREND_WINDOW..];
            let older = &self.rtt_samples[self.rtt_samples.len() - TREND_WINDOW * 2..self.rtt_samples.len() - TREND_WINDOW];

            if !older.is_empty() {
                let recent_avg: Duration = recent.iter().sum::<Duration>() / recent.len() as u32;
                let older_avg: Duration = older.iter().sum::<Duration>() / older.len() as u32;

                // Negative trend = RTT increasing (worse), positive = RTT decreasing (better)
                self.recent_rtt_trend = (older_avg.as_millis() as f32 - recent_avg.as_millis() as f32) / older_avg.as_millis() as f32;
                self.recent_rtt_trend = self.recent_rtt_trend.clamp(-1.0, 1.0);
            }
        }

        // Success rate trend
        if self.success_samples.len() >= TREND_WINDOW {
            let recent = &self.success_samples[self.success_samples.len() - TREND_WINDOW..];
            let older = &self.success_samples[self.success_samples.len() - TREND_WINDOW * 2..self.success_samples.len() - TREND_WINDOW];

            if !older.is_empty() {
                let recent_success_rate = recent.iter().filter(|&&s| s).count() as f32 / recent.len() as f32;
                let older_success_rate = older.iter().filter(|&&s| s).count() as f32 / older.len() as f32;

                self.recent_success_trend = recent_success_rate - older_success_rate;
                self.recent_success_trend = self.recent_success_trend.clamp(-1.0, 1.0);
            }
        }
    }
}

/// Network-wide statistics for context features
#[derive(Clone, Debug, Default)]
pub struct NetworkStatistics {
    /// Average network RTT
    pub avg_network_rtt: Duration,
    /// Network load indicator (0.0 to 1.0)
    pub network_load: f32,
    /// Number of active peers
    pub active_peers: usize,
    /// Internal tracking
    rtt_samples: Vec<Duration>,
    success_samples: Vec<bool>,
}

impl NetworkStatistics {
    /// Update network statistics
    pub fn update(&mut self, rtt: Duration, success: bool) {
        self.rtt_samples.push(rtt);
        self.success_samples.push(success);

        // Keep only recent samples
        const MAX_SAMPLES: usize = 1000;
        if self.rtt_samples.len() > MAX_SAMPLES {
            self.rtt_samples.remove(0);
        }
        if self.success_samples.len() > MAX_SAMPLES {
            self.success_samples.remove(0);
        }

        // Update averages
        if !self.rtt_samples.is_empty() {
            let total_rtt: Duration = self.rtt_samples.iter().sum();
            self.avg_network_rtt = total_rtt / self.rtt_samples.len() as u32;
        }

        // Estimate network load from RTT variance and success rate
        if self.rtt_samples.len() > 10 && !self.success_samples.is_empty() {
            let rtt_variance = self.compute_rtt_variance();
            let success_rate = self.success_samples.iter().filter(|&&s| s).count() as f32 / self.success_samples.len() as f32;

            // High variance and low success rate indicate high load
            let rtt_cv = rtt_variance.sqrt() / self.avg_network_rtt.as_millis() as f32;
            self.network_load = (rtt_cv * (1.0 - success_rate)).clamp(0.0, 1.0);
        }
    }

    fn compute_rtt_variance(&self) -> f32 {
        if self.rtt_samples.len() < 2 {
            return 0.0;
        }

        let avg_rtt_ms = self.avg_network_rtt.as_millis() as f32;
        let variance = self.rtt_samples.iter()
            .map(|rtt| {
                let diff = rtt.as_millis() as f32 - avg_rtt_ms;
                diff * diff
            })
            .sum::<f32>() / self.rtt_samples.len() as f32;

        variance
    }
}

/// Feature extractor for congestion control
pub struct CongestionFeatureExtractor {
    /// Window of recent network measurements
    measurement_history: Vec<CongestionMeasurement>,
    /// Maximum history size
    max_history_size: usize,
}

impl CongestionFeatureExtractor {
    pub fn new(max_history_size: usize) -> Self {
        Self {
            measurement_history: Vec::new(),
            max_history_size,
        }
    }

    /// Extract features for congestion control decision
    pub fn extract_congestion_features(&self, current_measurement: &CongestionMeasurement) -> FeatureVector {
        let mut features = Vec::new();
        let mut names = Vec::new();

        // Current state features
        features.push(current_measurement.rtt.as_millis() as f32);
        names.push("current_rtt_ms".to_string());

        features.push(current_measurement.bandwidth_estimate);
        names.push("bandwidth_estimate_bps".to_string());

        features.push(current_measurement.packet_loss_rate);
        names.push("packet_loss_rate".to_string());

        features.push(current_measurement.queue_delay.as_millis() as f32);
        names.push("queue_delay_ms".to_string());

        features.push(current_measurement.cwnd as f32);
        names.push("congestion_window".to_string());

        // Historical trend features
        if self.measurement_history.len() >= 5 {
            let recent_measurements = &self.measurement_history[self.measurement_history.len()-5..];

            // RTT trend
            let rtt_trend = self.compute_trend(recent_measurements.iter().map(|m| m.rtt.as_millis() as f32));
            features.push(rtt_trend);
            names.push("rtt_trend".to_string());

            // Bandwidth trend
            let bw_trend = self.compute_trend(recent_measurements.iter().map(|m| m.bandwidth_estimate));
            features.push(bw_trend);
            names.push("bandwidth_trend".to_string());

            // Loss trend
            let loss_trend = self.compute_trend(recent_measurements.iter().map(|m| m.packet_loss_rate));
            features.push(loss_trend);
            names.push("loss_trend".to_string());
        } else {
            features.extend_from_slice(&[0.0, 0.0, 0.0]);
            names.extend_from_slice(&["rtt_trend".to_string(), "bandwidth_trend".to_string(), "loss_trend".to_string()]);
        }

        FeatureVector::new(features, names)
    }

    /// Add new measurement to history
    pub fn add_measurement(&mut self, measurement: CongestionMeasurement) {
        self.measurement_history.push(measurement);

        if self.measurement_history.len() > self.max_history_size {
            self.measurement_history.remove(0);
        }
    }

    /// Compute trend from sequence of values
    fn compute_trend<I>(&self, values: I) -> f32
    where
        I: Iterator<Item = f32>,
    {
        let values: Vec<f32> = values.collect();
        if values.len() < 2 {
            return 0.0;
        }

        // Simple linear regression slope
        let n = values.len() as f32;
        let x_mean = (n - 1.0) / 2.0; // x values are 0, 1, 2, ..., n-1
        let y_mean = values.iter().sum::<f32>() / n;

        let numerator = values.iter().enumerate()
            .map(|(i, &y)| (i as f32 - x_mean) * (y - y_mean))
            .sum::<f32>();

        let denominator = (0..values.len())
            .map(|i| (i as f32 - x_mean).powi(2))
            .sum::<f32>();

        if denominator == 0.0 {
            0.0
        } else {
            numerator / denominator
        }
    }
}

/// Measurement for congestion control features
#[derive(Clone, Debug)]
pub struct CongestionMeasurement {
    pub rtt: Duration,
    pub bandwidth_estimate: f32,
    pub packet_loss_rate: f32,
    pub queue_delay: Duration,
    pub cwnd: u32,
    pub timestamp: Instant,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::SocketAddr;

    #[test]
    fn test_routing_feature_extraction() {
        let mut extractor = RoutingFeatureExtractor::new(FeatureConfig::default());

        let peer = PeerKeyLocation::new([0u8; 32], Location(0.5), "127.0.0.1:8000".parse::<SocketAddr>().unwrap());
        let target = Location(0.7);

        let features = extractor.extract_routing_features(&peer, target, None);

        assert!(!features.features.is_empty());
        assert_eq!(features.features.len(), features.names.len());
        assert!(features.features[0] > 0.0); // Distance should be > 0
    }

    #[test]
    fn test_peer_performance_history() {
        let mut history = PeerPerformanceHistory::default();

        // Add some performance data
        history.update(Duration::from_millis(50), true, 1000);
        history.update(Duration::from_millis(60), true, 2000);
        history.update(Duration::from_millis(55), false, 1500);

        assert!(history.avg_rtt.as_millis() > 0);
        assert!(history.success_rate > 0.0);
        assert!(history.success_rate < 1.0); // One failure
    }

    #[test]
    fn test_congestion_feature_extraction() {
        let extractor = CongestionFeatureExtractor::new(100);

        let measurement = CongestionMeasurement {
            rtt: Duration::from_millis(50),
            bandwidth_estimate: 1_000_000.0,
            packet_loss_rate: 0.01,
            queue_delay: Duration::from_millis(10),
            cwnd: 100,
            timestamp: Instant::now(),
        };

        let features = extractor.extract_congestion_features(&measurement);

        assert_eq!(features.features.len(), 8); // 5 current + 3 trends
        assert_eq!(features.features.len(), features.names.len());
    }
}