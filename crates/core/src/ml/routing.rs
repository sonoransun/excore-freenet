//! ML-Enhanced Router Implementation
//!
//! This module provides machine learning enhancements to the existing router
//! using the Enhanced<T, E> pattern for backward compatibility.

use super::{Enhanced, MLBackend, FeatureVector, Prediction, MLConfig};
use super::backend::BackendManager;
use super::features::RoutingFeatureExtractor;

use crate::ring::{Location, PeerKeyHash};
use crate::router::{Router, RoutingTable, ConnectionInfo};
use crate::transport::TransportStats;

use anyhow::{Result, Context};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};
use parking_lot::RwLock;

/// ML-enhanced router that augments isotonic regression with neural networks
pub type MLRouter = Enhanced<Router, MLRoutingEnhancer>;

impl MLRouter {
    /// Create a new ML-enhanced router
    pub fn new(base_router: Router, config: MLConfig) -> Result<Self> {
        let enhancement = if config.enabled {
            Some(MLRoutingEnhancer::new(config.clone())?)
        } else {
            None
        };

        Ok(Enhanced::new(base_router, enhancement, config))
    }

    /// Enhanced peer selection with ML predictions
    pub async fn select_optimal_peers(
        &self,
        target: &Location,
        candidates: &[PeerKeyHash],
        operation_type: OperationType,
    ) -> Result<Vec<PeerKeyHash>> {
        if let Some(ref enhancer) = self.enhancement {
            // Try ML-enhanced selection first
            match enhancer.ml_select_peers(&self.base, target, candidates, operation_type).await {
                Ok(peers) => return Ok(peers),
                Err(e) if self.config.fallback_on_error => {
                    tracing::warn!("ML peer selection failed, falling back to isotonic: {}", e);
                }
                Err(e) => return Err(e),
            }
        }

        // Fallback to original isotonic regression
        self.base.select_optimal_peers(target, candidates, operation_type).await
    }

    /// Enhanced route calculation with performance predictions
    pub async fn calculate_route(
        &self,
        target: &Location,
        max_hops: u8,
    ) -> Result<Vec<PeerKeyHash>> {
        if let Some(ref enhancer) = self.enhancement {
            match enhancer.ml_calculate_route(&self.base, target, max_hops).await {
                Ok(route) => return Ok(route),
                Err(e) if self.config.fallback_on_error => {
                    tracing::warn!("ML route calculation failed, falling back: {}", e);
                }
                Err(e) => return Err(e),
            }
        }

        // Fallback to original routing algorithm
        self.base.calculate_route(target, max_hops).await
    }

    /// Get route performance prediction
    pub async fn predict_route_performance(
        &self,
        route: &[PeerKeyHash],
        operation_type: OperationType,
    ) -> Option<RoutePrediction> {
        if let Some(ref enhancer) = self.enhancement {
            enhancer.predict_route_performance(route, operation_type).await.ok()
        } else {
            None
        }
    }
}

/// ML routing enhancer that augments the base router
pub struct MLRoutingEnhancer {
    backend_manager: BackendManager,
    feature_extractor: RoutingFeatureExtractor,
    prediction_cache: Arc<RwLock<HashMap<RouteKey, CachedPrediction>>>,
    config: MLConfig,
}

impl MLRoutingEnhancer {
    /// Create new ML routing enhancer
    pub fn new(config: MLConfig) -> Result<Self> {
        let backend_manager = BackendManager::new(
            Duration::from_millis(config.max_inference_latency_ms)
        );
        let feature_extractor = RoutingFeatureExtractor::new();

        Ok(Self {
            backend_manager,
            feature_extractor,
            prediction_cache: Arc::new(RwLock::new(HashMap::new())),
            config,
        })
    }

    /// Initialize ML backend from model file
    #[cfg(feature = "ml-routing")]
    pub fn initialize_backend(&mut self, model_path: &std::path::Path) -> Result<()> {
        use super::backend::CandleBackend;
        let backend = CandleBackend::from_file(model_path)
            .context("Failed to load ML routing model")?;
        self.backend_manager.set_primary(backend);
        Ok(())
    }

    /// ML-enhanced peer selection
    async fn ml_select_peers(
        &self,
        base_router: &Router,
        target: &Location,
        candidates: &[PeerKeyHash],
        operation_type: OperationType,
    ) -> Result<Vec<PeerKeyHash>> {
        let mut scored_peers = Vec::new();

        for peer in candidates {
            // Check cache first
            let route_key = RouteKey::new(*peer, *target, operation_type);
            if let Some(cached) = self.get_cached_prediction(&route_key) {
                scored_peers.push(ScoredPeer {
                    peer: *peer,
                    score: cached.primary_value(),
                    confidence: cached.confidence,
                });
                continue;
            }

            // Extract features for this peer-target pair
            let features = self.feature_extractor.extract_routing_features(
                base_router,
                peer,
                target,
                operation_type,
            )?;

            // Get ML prediction
            let prediction = self.backend_manager.predict(features).await;

            // Cache the prediction
            self.cache_prediction(route_key, prediction.clone());

            scored_peers.push(ScoredPeer {
                peer: *peer,
                score: prediction.primary_value(),
                confidence: prediction.confidence,
            });
        }

        // Sort by predicted performance (lower is better for retrieval time)
        scored_peers.sort_by(|a, b| a.score.partial_cmp(&b.score).unwrap());

        // Apply confidence-based selection
        let selected = self.select_by_confidence(scored_peers, operation_type);

        Ok(selected.into_iter().map(|s| s.peer).collect())
    }

    /// ML-enhanced route calculation
    async fn ml_calculate_route(
        &self,
        base_router: &Router,
        target: &Location,
        max_hops: u8,
    ) -> Result<Vec<PeerKeyHash>> {
        // For now, use greedy approach with ML scoring
        // Future: implement full path optimization with A*
        let mut route = Vec::new();
        let mut current_location = base_router.my_location();

        for hop in 0..max_hops {
            // Get candidates closer to target
            let candidates = base_router.get_candidates_towards(target, &current_location)?;

            if candidates.is_empty() {
                break;
            }

            // Use ML to select best next hop
            let selected = self.ml_select_peers(
                base_router,
                target,
                &candidates,
                OperationType::Forwarding,
            ).await?;

            if let Some(next_peer) = selected.first() {
                route.push(*next_peer);

                // Update current location for next iteration
                if let Some(peer_info) = base_router.get_peer_info(next_peer) {
                    current_location = peer_info.location;
                }
            }

            // Check if we're close enough to target
            if current_location.distance(target) < Location::MIN_DISTANCE {
                break;
            }
        }

        Ok(route)
    }

    /// Predict performance for a complete route
    async fn predict_route_performance(
        &self,
        route: &[PeerKeyHash],
        operation_type: OperationType,
    ) -> Result<RoutePrediction> {
        let mut total_latency = 0.0;
        let mut min_confidence = 1.0;
        let mut hop_predictions = Vec::new();

        for (i, peer) in route.iter().enumerate() {
            // For route prediction, we need target location
            // In practice, this would be passed or derived from context
            let target_location = Location::random(); // Placeholder

            let features = FeatureVector::new(
                vec![
                    i as f32,                    // hop_index
                    route.len() as f32,          // total_hops
                    0.5,                         // estimated_distance (placeholder)
                    100.0,                       // estimated_rtt (placeholder)
                    0.01,                        // packet_loss_rate (placeholder)
                ],
                vec![
                    "hop_index".to_string(),
                    "total_hops".to_string(),
                    "distance".to_string(),
                    "rtt".to_string(),
                    "packet_loss".to_string(),
                ],
            );

            let prediction = self.backend_manager.predict(features).await;
            total_latency += prediction.primary_value();
            min_confidence = min_confidence.min(prediction.confidence);
            hop_predictions.push(prediction);
        }

        Ok(RoutePrediction {
            total_latency,
            confidence: min_confidence,
            hop_predictions,
            predicted_success_rate: min_confidence, // Simplified
        })
    }

    /// Select peers based on confidence thresholds
    fn select_by_confidence(
        &self,
        mut scored_peers: Vec<ScoredPeer>,
        operation_type: OperationType,
    ) -> Vec<ScoredPeer> {
        let confidence_threshold = match operation_type {
            OperationType::Retrieval => 0.7,    // High confidence for GET operations
            OperationType::Storage => 0.6,      // Medium confidence for PUT operations
            OperationType::Forwarding => 0.5,   // Lower threshold for routing
        };

        // Filter by confidence and take top candidates
        scored_peers.retain(|peer| peer.confidence >= confidence_threshold);

        // Limit selection based on operation type
        let max_peers = match operation_type {
            OperationType::Retrieval => 5,
            OperationType::Storage => 8,
            OperationType::Forwarding => 3,
        };

        scored_peers.truncate(max_peers);
        scored_peers
    }

    /// Get cached prediction if available and fresh
    fn get_cached_prediction(&self, key: &RouteKey) -> Option<Prediction> {
        let cache = self.prediction_cache.read();
        if let Some(cached) = cache.get(key) {
            if cached.timestamp.elapsed() < Duration::from_secs(30) { // 30s TTL
                return Some(cached.prediction.clone());
            }
        }
        None
    }

    /// Cache prediction with timestamp
    fn cache_prediction(&self, key: RouteKey, prediction: Prediction) {
        let mut cache = self.prediction_cache.write();
        cache.insert(key, CachedPrediction {
            prediction,
            timestamp: Instant::now(),
        });

        // Cleanup old entries periodically
        if cache.len() > 10000 {
            cache.retain(|_, v| v.timestamp.elapsed() < Duration::from_secs(300));
        }
    }
}

/// Operation types for routing optimization
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum OperationType {
    /// GET operation - prioritize low latency
    Retrieval,
    /// PUT operation - prioritize reliability
    Storage,
    /// Message forwarding - balance latency and reliability
    Forwarding,
}

/// Peer with ML-predicted score
#[derive(Debug, Clone)]
struct ScoredPeer {
    peer: PeerKeyHash,
    score: f32,        // Predicted retrieval time (ms)
    confidence: f32,   // Prediction confidence (0-1)
}

/// Route performance prediction
#[derive(Debug, Clone)]
pub struct RoutePrediction {
    /// Total predicted latency (ms)
    pub total_latency: f32,
    /// Overall confidence (0-1)
    pub confidence: f32,
    /// Per-hop predictions
    pub hop_predictions: Vec<Prediction>,
    /// Predicted success rate (0-1)
    pub predicted_success_rate: f32,
}

/// Cache key for route predictions
#[derive(Debug, Clone, Hash, PartialEq, Eq)]
struct RouteKey {
    peer: PeerKeyHash,
    target: Location,
    operation_type: OperationType,
}

impl RouteKey {
    fn new(peer: PeerKeyHash, target: Location, operation_type: OperationType) -> Self {
        Self { peer, target, operation_type }
    }
}

/// Cached prediction with timestamp
#[derive(Debug, Clone)]
struct CachedPrediction {
    prediction: Prediction,
    timestamp: Instant,
}

/// Training data collection for continuous improvement
pub struct RoutingTrainer {
    feature_extractor: RoutingFeatureExtractor,
    training_data: Arc<RwLock<Vec<TrainingExample>>>,
}

impl RoutingTrainer {
    /// Create new routing trainer
    pub fn new() -> Self {
        Self {
            feature_extractor: RoutingFeatureExtractor::new(),
            training_data: Arc::new(RwLock::new(Vec::new())),
        }
    }

    /// Record actual routing outcome for training
    pub fn record_outcome(
        &self,
        peer: PeerKeyHash,
        target: Location,
        operation_type: OperationType,
        features: FeatureVector,
        actual_latency: f32,
        success: bool,
    ) {
        let example = TrainingExample {
            features,
            target_latency: actual_latency,
            success,
            timestamp: Instant::now(),
        };

        let mut data = self.training_data.write();
        data.push(example);

        // Keep only recent data for training
        data.retain(|e| e.timestamp.elapsed() < Duration::from_hours(24));
    }

    /// Export training data for model updates
    pub fn export_training_data(&self) -> Vec<TrainingExample> {
        self.training_data.read().clone()
    }
}

/// Training example for model improvement
#[derive(Debug, Clone)]
pub struct TrainingExample {
    pub features: FeatureVector,
    pub target_latency: f32,
    pub success: bool,
    pub timestamp: Instant,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::simulation::SimulationTimeSource;

    #[test]
    fn test_route_key_hash() {
        let peer1 = PeerKeyHash::random();
        let peer2 = PeerKeyHash::random();
        let target = Location::random();

        let key1 = RouteKey::new(peer1, target, OperationType::Retrieval);
        let key2 = RouteKey::new(peer1, target, OperationType::Retrieval);
        let key3 = RouteKey::new(peer2, target, OperationType::Retrieval);

        assert_eq!(key1, key2);
        assert_ne!(key1, key3);
    }

    #[tokio::test]
    async fn test_ml_router_fallback() {
        let base_router = Router::new(Location::random());
        let config = MLConfig {
            enabled: false,
            ..Default::default()
        };

        let ml_router = MLRouter::new(base_router, config).unwrap();
        assert!(!ml_router.has_enhancement());

        // Should work without ML enhancement
        let target = Location::random();
        let candidates = vec![PeerKeyHash::random(), PeerKeyHash::random()];
        let result = ml_router.select_optimal_peers(
            &target,
            &candidates,
            OperationType::Retrieval,
        ).await;
        assert!(result.is_ok());
    }

    #[test]
    fn test_operation_type_confidence_thresholds() {
        let enhancer = MLRoutingEnhancer::new(MLConfig::default()).unwrap();

        let peers = vec![
            ScoredPeer { peer: PeerKeyHash::random(), score: 50.0, confidence: 0.8 },
            ScoredPeer { peer: PeerKeyHash::random(), score: 60.0, confidence: 0.6 },
            ScoredPeer { peer: PeerKeyHash::random(), score: 70.0, confidence: 0.4 },
        ];

        let retrieval_selected = enhancer.select_by_confidence(
            peers.clone(),
            OperationType::Retrieval,
        );
        assert_eq!(retrieval_selected.len(), 1); // Only high confidence

        let storage_selected = enhancer.select_by_confidence(
            peers.clone(),
            OperationType::Storage,
        );
        assert_eq!(storage_selected.len(), 2); // Medium and high confidence
    }

    #[test]
    fn test_training_data_retention() {
        let trainer = RoutingTrainer::new();

        let features = FeatureVector::new(vec![1.0, 2.0, 3.0], vec!["a".to_string(), "b".to_string(), "c".to_string()]);
        trainer.record_outcome(
            PeerKeyHash::random(),
            Location::random(),
            OperationType::Retrieval,
            features,
            100.0,
            true,
        );

        let data = trainer.export_training_data();
        assert_eq!(data.len(), 1);
        assert_eq!(data[0].target_latency, 100.0);
        assert!(data[0].success);
    }
}