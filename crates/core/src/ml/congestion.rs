//! Reinforcement Learning Enhanced Congestion Control
//!
//! This module provides RL-based adaptive congestion control that enhances
//! the existing BBR and LEDBAT++ algorithms with learned parameter optimization.

use super::{Enhanced, MLConfig, FeatureVector, Prediction};
use super::backend::BackendManager;
use super::features::CongestionFeatureExtractor;

use crate::transport::congestion_control::{CongestionController, BBRController, LEDBATController};
use crate::transport::{TransportStats, ConnectionMetrics};

use anyhow::{Result, Context};
use std::collections::VecDeque;
use std::sync::Arc;
use std::time::{Duration, Instant};
use parking_lot::RwLock;

/// RL-enhanced congestion controller
pub type RLCongestionController = Enhanced<Box<dyn CongestionController>, RLCongestionEnhancer>;

impl RLCongestionController {
    /// Create new RL-enhanced congestion controller
    pub fn new(
        base_controller: Box<dyn CongestionController>,
        config: MLConfig,
    ) -> Result<Self> {
        let enhancement = if config.enabled {
            Some(RLCongestionEnhancer::new(config.clone())?)
        } else {
            None
        };

        Ok(Enhanced::new(base_controller, enhancement, config))
    }

    /// Enhanced bandwidth estimation with RL adaptation
    pub async fn estimate_bandwidth(&mut self, stats: &TransportStats) -> Result<f64> {
        if let Some(ref mut enhancer) = self.enhancement {
            match enhancer.rl_estimate_bandwidth(&mut self.base, stats).await {
                Ok(bandwidth) => return Ok(bandwidth),
                Err(e) if self.config.fallback_on_error => {
                    tracing::warn!("RL bandwidth estimation failed, falling back: {}", e);
                }
                Err(e) => return Err(e),
            }
        }

        // Fallback to base controller
        self.base.estimate_bandwidth(stats).await
    }

    /// Enhanced congestion window adaptation
    pub async fn adapt_congestion_window(
        &mut self,
        current_cwnd: u32,
        rtt: Duration,
        metrics: &ConnectionMetrics,
    ) -> Result<u32> {
        if let Some(ref mut enhancer) = self.enhancement {
            match enhancer.rl_adapt_congestion_window(
                &mut self.base,
                current_cwnd,
                rtt,
                metrics,
            ).await {
                Ok(new_cwnd) => return Ok(new_cwnd),
                Err(e) if self.config.fallback_on_error => {
                    tracing::warn!("RL cwnd adaptation failed, falling back: {}", e);
                }
                Err(e) => return Err(e),
            }
        }

        // Fallback to base controller adaptation
        self.base.adapt_congestion_window(current_cwnd, rtt, metrics).await
    }

    /// Get RL-based pacing recommendations
    pub async fn get_pacing_rate(&self, bandwidth_estimate: f64) -> Option<f64> {
        if let Some(ref enhancer) = self.enhancement {
            enhancer.get_pacing_rate(bandwidth_estimate).await.ok()
        } else {
            None
        }
    }
}

/// RL congestion control enhancer
pub struct RLCongestionEnhancer {
    backend_manager: BackendManager,
    feature_extractor: CongestionFeatureExtractor,
    state_history: Arc<RwLock<VecDeque<CongestionState>>>,
    action_history: Arc<RwLock<VecDeque<CongestionAction>>>,
    reward_history: Arc<RwLock<VecDeque<f32>>>,
    config: MLConfig,
    exploration_rate: f32,
}

impl RLCongestionEnhancer {
    /// Create new RL congestion enhancer
    pub fn new(config: MLConfig) -> Result<Self> {
        let backend_manager = BackendManager::new(
            Duration::from_millis(config.max_inference_latency_ms)
        );
        let feature_extractor = CongestionFeatureExtractor::new();

        Ok(Self {
            backend_manager,
            feature_extractor,
            state_history: Arc::new(RwLock::new(VecDeque::with_capacity(1000))),
            action_history: Arc::new(RwLock::new(VecDeque::with_capacity(1000))),
            reward_history: Arc::new(RwLock::new(VecDeque::with_capacity(1000))),
            config,
            exploration_rate: 0.1, // 10% exploration
        })
    }

    /// Initialize RL model from checkpoint
    #[cfg(feature = "rl-congestion")]
    pub fn initialize_model(&mut self, model_path: &std::path::Path) -> Result<()> {
        use super::backend::CandleBackend;
        let backend = CandleBackend::from_file(model_path)
            .context("Failed to load RL congestion model")?;
        self.backend_manager.set_primary(backend);
        Ok(())
    }

    /// RL-enhanced bandwidth estimation
    async fn rl_estimate_bandwidth(
        &mut self,
        base_controller: &mut Box<dyn CongestionController>,
        stats: &TransportStats,
    ) -> Result<f64> {
        // Extract current state features
        let state_features = self.feature_extractor.extract_congestion_features(stats)?;

        // Get current state
        let current_state = CongestionState::from_stats(stats);

        // Get RL action recommendation
        let action = self.select_action(&state_features).await?;

        // Apply action to get modified bandwidth estimate
        let base_estimate = base_controller.estimate_bandwidth(stats).await?;
        let rl_estimate = self.apply_bandwidth_action(base_estimate, &action);

        // Record state and action for learning
        self.record_state_action(current_state, action);

        Ok(rl_estimate)
    }

    /// RL-enhanced congestion window adaptation
    async fn rl_adapt_congestion_window(
        &mut self,
        base_controller: &mut Box<dyn CongestionController>,
        current_cwnd: u32,
        rtt: Duration,
        metrics: &ConnectionMetrics,
    ) -> Result<u32> {
        // Extract state features
        let state_features = self.feature_extractor.extract_window_features(
            current_cwnd,
            rtt,
            metrics,
        )?;

        // Get RL action
        let action = self.select_action(&state_features).await?;

        // Apply base controller logic first
        let base_cwnd = base_controller.adapt_congestion_window(
            current_cwnd,
            rtt,
            metrics,
        ).await?;

        // Apply RL modification
        let rl_cwnd = self.apply_cwnd_action(base_cwnd, &action);

        // Record for learning
        let state = CongestionState {
            cwnd: current_cwnd,
            rtt: rtt.as_millis() as f32,
            throughput: metrics.throughput_bps,
            loss_rate: metrics.packet_loss_rate,
            timestamp: Instant::now(),
        };

        self.record_state_action(state, action);

        Ok(rl_cwnd)
    }

    /// Get RL-based pacing rate recommendation
    async fn get_pacing_rate(&self, bandwidth_estimate: f64) -> Result<f64> {
        // Create feature vector for pacing decision
        let features = FeatureVector::new(
            vec![
                bandwidth_estimate as f32,
                self.get_recent_throughput(),
                self.get_recent_rtt(),
                self.get_recent_loss_rate(),
            ],
            vec![
                "bandwidth_estimate".to_string(),
                "recent_throughput".to_string(),
                "recent_rtt".to_string(),
                "recent_loss_rate".to_string(),
            ],
        );

        let prediction = self.backend_manager.predict(features).await;

        // Convert prediction to pacing rate multiplier
        let pacing_multiplier = prediction.primary_value().max(0.5).min(2.0);
        Ok(bandwidth_estimate * pacing_multiplier as f64)
    }

    /// Select action using epsilon-greedy policy
    async fn select_action(&self, state_features: &FeatureVector) -> Result<CongestionAction> {
        // Epsilon-greedy exploration
        if rand::random::<f32>() < self.exploration_rate {
            // Random exploration
            Ok(CongestionAction::random())
        } else {
            // Greedy action selection
            let prediction = self.backend_manager.predict(state_features.clone()).await;
            Ok(CongestionAction::from_prediction(&prediction))
        }
    }

    /// Apply bandwidth action to base estimate
    fn apply_bandwidth_action(&self, base_estimate: f64, action: &CongestionAction) -> f64 {
        let multiplier = match action.bandwidth_adjustment {
            BandwidthAdjustment::Increase => 1.1,
            BandwidthAdjustment::Decrease => 0.9,
            BandwidthAdjustment::Maintain => 1.0,
        };

        (base_estimate * multiplier).max(1000.0) // Minimum 1 Kbps
    }

    /// Apply congestion window action
    fn apply_cwnd_action(&self, base_cwnd: u32, action: &CongestionAction) -> u32 {
        let adjustment = match action.cwnd_adjustment {
            CwndAdjustment::Increase => (base_cwnd as f32 * 1.05) as u32,
            CwndAdjustment::Decrease => (base_cwnd as f32 * 0.95) as u32,
            CwndAdjustment::Maintain => base_cwnd,
        };

        adjustment.max(1).min(65535) // Clamp to valid range
    }

    /// Record state-action pair for learning
    fn record_state_action(&self, state: CongestionState, action: CongestionAction) {
        {
            let mut states = self.state_history.write();
            states.push_back(state);
            if states.len() > 1000 {
                states.pop_front();
            }
        }

        {
            let mut actions = self.action_history.write();
            actions.push_back(action);
            if actions.len() > 1000 {
                actions.pop_front();
            }
        }
    }

    /// Record reward for last action (called after observing outcome)
    pub fn record_reward(&self, reward: f32) {
        let mut rewards = self.reward_history.write();
        rewards.push_back(reward);
        if rewards.len() > 1000 {
            rewards.pop_front();
        }
    }

    /// Get recent throughput for context
    fn get_recent_throughput(&self) -> f32 {
        let states = self.state_history.read();
        states.iter()
            .rev()
            .take(10)
            .map(|s| s.throughput)
            .sum::<f32>() / 10.0.max(states.len() as f32)
    }

    /// Get recent RTT for context
    fn get_recent_rtt(&self) -> f32 {
        let states = self.state_history.read();
        states.iter()
            .rev()
            .take(10)
            .map(|s| s.rtt)
            .sum::<f32>() / 10.0.max(states.len() as f32)
    }

    /// Get recent loss rate for context
    fn get_recent_loss_rate(&self) -> f32 {
        let states = self.state_history.read();
        states.iter()
            .rev()
            .take(10)
            .map(|s| s.loss_rate)
            .sum::<f32>() / 10.0.max(states.len() as f32)
    }
}

/// Congestion control state representation
#[derive(Debug, Clone)]
pub struct CongestionState {
    pub cwnd: u32,
    pub rtt: f32,
    pub throughput: f32,
    pub loss_rate: f32,
    pub timestamp: Instant,
}

impl CongestionState {
    /// Create state from transport stats
    pub fn from_stats(stats: &TransportStats) -> Self {
        Self {
            cwnd: stats.congestion_window,
            rtt: stats.rtt.as_millis() as f32,
            throughput: stats.throughput_bps,
            loss_rate: stats.packet_loss_rate,
            timestamp: Instant::now(),
        }
    }
}

/// RL action for congestion control
#[derive(Debug, Clone)]
pub struct CongestionAction {
    pub bandwidth_adjustment: BandwidthAdjustment,
    pub cwnd_adjustment: CwndAdjustment,
    pub pacing_adjustment: PacingAdjustment,
}

impl CongestionAction {
    /// Generate random action for exploration
    pub fn random() -> Self {
        Self {
            bandwidth_adjustment: BandwidthAdjustment::random(),
            cwnd_adjustment: CwndAdjustment::random(),
            pacing_adjustment: PacingAdjustment::random(),
        }
    }

    /// Create action from ML prediction
    pub fn from_prediction(prediction: &Prediction) -> Self {
        let values = &prediction.values;

        Self {
            bandwidth_adjustment: if values.get(0).unwrap_or(&0.0) > &0.5 {
                BandwidthAdjustment::Increase
            } else if values.get(0).unwrap_or(&0.0) < &-0.5 {
                BandwidthAdjustment::Decrease
            } else {
                BandwidthAdjustment::Maintain
            },

            cwnd_adjustment: if values.get(1).unwrap_or(&0.0) > &0.5 {
                CwndAdjustment::Increase
            } else if values.get(1).unwrap_or(&0.0) < &-0.5 {
                CwndAdjustment::Decrease
            } else {
                CwndAdjustment::Maintain
            },

            pacing_adjustment: if values.get(2).unwrap_or(&0.0) > &0.5 {
                PacingAdjustment::Faster
            } else if values.get(2).unwrap_or(&0.0) < &-0.5 {
                PacingAdjustment::Slower
            } else {
                PacingAdjustment::Maintain
            },
        }
    }
}

/// Bandwidth adjustment actions
#[derive(Debug, Clone, Copy)]
pub enum BandwidthAdjustment {
    Increase,
    Decrease,
    Maintain,
}

impl BandwidthAdjustment {
    fn random() -> Self {
        match rand::random::<u8>() % 3 {
            0 => Self::Increase,
            1 => Self::Decrease,
            _ => Self::Maintain,
        }
    }
}

/// Congestion window adjustment actions
#[derive(Debug, Clone, Copy)]
pub enum CwndAdjustment {
    Increase,
    Decrease,
    Maintain,
}

impl CwndAdjustment {
    fn random() -> Self {
        match rand::random::<u8>() % 3 {
            0 => Self::Increase,
            1 => Self::Decrease,
            _ => Self::Maintain,
        }
    }
}

/// Pacing adjustment actions
#[derive(Debug, Clone, Copy)]
pub enum PacingAdjustment {
    Faster,
    Slower,
    Maintain,
}

impl PacingAdjustment {
    fn random() -> Self {
        match rand::random::<u8>() % 3 {
            0 => Self::Faster,
            1 => Self::Slower,
            _ => Self::Maintain,
        }
    }
}

/// Reward calculator for RL training
pub struct RewardCalculator {
    target_throughput: f32,
    target_rtt: f32,
    fairness_weight: f32,
}

impl RewardCalculator {
    /// Create new reward calculator
    pub fn new(target_throughput: f32, target_rtt: f32) -> Self {
        Self {
            target_throughput,
            target_rtt,
            fairness_weight: 0.3, // 30% weight for fairness
        }
    }

    /// Calculate reward based on network performance
    pub fn calculate_reward(
        &self,
        prev_state: &CongestionState,
        current_state: &CongestionState,
        action: &CongestionAction,
    ) -> f32 {
        // Throughput improvement reward
        let throughput_ratio = current_state.throughput / self.target_throughput;
        let throughput_reward = (throughput_ratio - 1.0).max(-1.0).min(1.0);

        // RTT penalty (lower is better)
        let rtt_ratio = current_state.rtt / self.target_rtt;
        let rtt_penalty = (1.0 - rtt_ratio).max(-1.0).min(1.0);

        // Loss penalty
        let loss_penalty = -current_state.loss_rate * 10.0; // Heavy penalty for losses

        // Stability reward (penalize oscillations)
        let cwnd_change = (current_state.cwnd as f32 - prev_state.cwnd as f32).abs();
        let stability_reward = -cwnd_change / prev_state.cwnd as f32 * 0.1;

        // Combined reward
        let total_reward = throughput_reward * 0.4
            + rtt_penalty * 0.3
            + loss_penalty * 0.2
            + stability_reward * 0.1;

        total_reward.max(-2.0).min(2.0) // Clamp to reasonable range
    }
}

/// Training data collector for continuous learning
pub struct RLTrainer {
    experience_buffer: Arc<RwLock<VecDeque<Experience>>>,
    reward_calculator: RewardCalculator,
}

impl RLTrainer {
    /// Create new RL trainer
    pub fn new(target_throughput: f32, target_rtt: f32) -> Self {
        Self {
            experience_buffer: Arc::new(RwLock::new(VecDeque::with_capacity(10000))),
            reward_calculator: RewardCalculator::new(target_throughput, target_rtt),
        }
    }

    /// Record experience tuple (s, a, r, s')
    pub fn record_experience(
        &self,
        state: CongestionState,
        action: CongestionAction,
        reward: f32,
        next_state: CongestionState,
    ) {
        let experience = Experience {
            state,
            action,
            reward,
            next_state,
            timestamp: Instant::now(),
        };

        let mut buffer = self.experience_buffer.write();
        buffer.push_back(experience);

        // Keep only recent experiences
        if buffer.len() > 10000 {
            buffer.pop_front();
        }
    }

    /// Export training batch for model updates
    pub fn sample_batch(&self, batch_size: usize) -> Vec<Experience> {
        let buffer = self.experience_buffer.read();
        let mut batch = Vec::with_capacity(batch_size);

        for _ in 0..batch_size.min(buffer.len()) {
            let idx = rand::random::<usize>() % buffer.len();
            batch.push(buffer[idx].clone());
        }

        batch
    }
}

/// RL experience tuple
#[derive(Debug, Clone)]
pub struct Experience {
    pub state: CongestionState,
    pub action: CongestionAction,
    pub reward: f32,
    pub next_state: CongestionState,
    pub timestamp: Instant,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transport::congestion_control::FixedRateController;

    #[test]
    fn test_congestion_action_from_prediction() {
        let prediction = Prediction::new(
            vec![0.8, -0.6, 0.2], // bandwidth+, cwnd-, pacing maintain
            0.9,
            "test".to_string(),
            Duration::from_millis(10),
        );

        let action = CongestionAction::from_prediction(&prediction);

        match action.bandwidth_adjustment {
            BandwidthAdjustment::Increase => {},
            _ => panic!("Expected bandwidth increase"),
        }

        match action.cwnd_adjustment {
            CwndAdjustment::Decrease => {},
            _ => panic!("Expected cwnd decrease"),
        }
    }

    #[test]
    fn test_reward_calculation() {
        let calculator = RewardCalculator::new(1000000.0, 50.0); // 1 Mbps, 50ms RTT

        let prev_state = CongestionState {
            cwnd: 10,
            rtt: 100.0,
            throughput: 500000.0,
            loss_rate: 0.01,
            timestamp: Instant::now(),
        };

        let current_state = CongestionState {
            cwnd: 12,
            rtt: 60.0,
            throughput: 800000.0,
            loss_rate: 0.005,
            timestamp: Instant::now(),
        };

        let action = CongestionAction::random();
        let reward = calculator.calculate_reward(&prev_state, &current_state, &action);

        // Should be positive due to improved throughput and RTT
        assert!(reward > 0.0);
    }

    #[tokio::test]
    async fn test_rl_congestion_controller_fallback() {
        let base_controller = Box::new(FixedRateController::new(1000000)) as Box<dyn CongestionController>;
        let config = MLConfig {
            enabled: false,
            ..Default::default()
        };

        let mut rl_controller = RLCongestionController::new(base_controller, config).unwrap();
        assert!(!rl_controller.has_enhancement());

        // Should work without RL enhancement
        let stats = TransportStats::default();
        let result = rl_controller.estimate_bandwidth(&stats).await;
        assert!(result.is_ok());
    }

    #[test]
    fn test_bandwidth_adjustment_application() {
        let enhancer = RLCongestionEnhancer::new(MLConfig::default()).unwrap();
        let base_estimate = 1000000.0; // 1 Mbps

        let increase_action = CongestionAction {
            bandwidth_adjustment: BandwidthAdjustment::Increase,
            cwnd_adjustment: CwndAdjustment::Maintain,
            pacing_adjustment: PacingAdjustment::Maintain,
        };

        let result = enhancer.apply_bandwidth_action(base_estimate, &increase_action);
        assert!(result > base_estimate);

        let decrease_action = CongestionAction {
            bandwidth_adjustment: BandwidthAdjustment::Decrease,
            cwnd_adjustment: CwndAdjustment::Maintain,
            pacing_adjustment: PacingAdjustment::Maintain,
        };

        let result = enhancer.apply_bandwidth_action(base_estimate, &decrease_action);
        assert!(result < base_estimate);
    }
}